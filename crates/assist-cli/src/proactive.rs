//! Runs the proactive engine in the daemon (PLAN.md section 3.17): feeds it ended jobs and
//! facts, and delivers its messages to subscribed clients (`speak`: `assist listen` says
//! them, the dashboard shows them) or as a desktop notification (`notify`). The rules live
//! in `assist_core::proactive`.

use std::path::PathBuf;
use std::time::Duration;

use assist_core::config::{Config, Sink};
use assist_core::facts::Facts;
use assist_core::proactive::{load_muted, Notice, Proactive};
use assist_core::replies::Replies;
use assist_core::scheduler::{JobEvent, Scheduler};
use assist_proto::Notice as Wire;
use jiff::ZonedDateTime;
use tokio::sync::broadcast::{self, error::RecvError};

/// How often fact rules are checked.
const TICK: Duration = Duration::from_secs(15);

/// Starts the engine when `config` has rules. Runs until the runtime stops.
pub fn start(
    config: &Config,
    scheduler: Scheduler,
    facts: Facts,
    data: PathBuf,
    clients: broadcast::Sender<Wire>,
) {
    if config.proactive.rules.is_empty() {
        return;
    }
    let engine = Proactive::new(&config.proactive);
    let replies = Replies::new(config);
    let title = config.persona.name.clone();
    tokio::spawn(async move {
        let mut engine = engine;
        let mut events = scheduler.subscribe();
        let mut tick = tokio::time::interval(TICK);
        loop {
            let notices = tokio::select! {
                event = events.recv() => match event {
                    Ok(JobEvent::State { id, state }) if state.is_finished() => {
                        let Some(job) = scheduler.get(id) else { continue };
                        engine.set_muted(load_muted(&data));
                        engine.on_job(&job, &facts, &replies, &ZonedDateTime::now())
                    }
                    Ok(_) | Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => return,
                },
                _ = tick.tick() => {
                    engine.set_muted(load_muted(&data));
                    engine.on_facts(&facts, &replies, &ZonedDateTime::now())
                }
            };
            for notice in notices {
                deliver(notice, &clients, &title);
            }
        }
    });
}

fn deliver(notice: Notice, clients: &broadcast::Sender<Wire>, title: &str) {
    println!("{}: {}", notice.rule, notice.text);
    match notice.sink {
        Sink::Speak => {
            // Nobody listening is fine; the console line above remains.
            let _ = clients.send(Wire {
                rule: notice.rule,
                text: notice.text,
            });
        }
        Sink::Notify => {
            let title = title.to_owned();
            tokio::task::spawn_blocking(move || {
                let shown = notify_rust::Notification::new()
                    .summary(&title)
                    .body(&notice.text)
                    .show();
                if let Err(err) = shown {
                    println!("{}: cannot show a notification: {err}", notice.rule);
                }
            });
        }
    }
}
