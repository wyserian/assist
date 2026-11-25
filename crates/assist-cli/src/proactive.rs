//! Delivers the proactive engine's messages from the daemon (PLAN.md section 3.17): to
//! subscribed clients (`speak`: `assist listen` says them, the dashboard shows them) or as a
//! desktop notification (`notify`). The rules and their runner live in
//! `assist_core::proactive`.

use std::path::PathBuf;

use assist_core::config::{Config, Sink};
use assist_core::facts::Facts;
use assist_core::proactive::{self, Notice};
use assist_core::scheduler::Scheduler;
use assist_proto::Notice as Wire;
use tokio::sync::broadcast;

/// Starts the engine when `config` has rules. Runs until the runtime stops.
pub fn start(
    config: &Config,
    scheduler: Scheduler,
    facts: Facts,
    data: PathBuf,
    clients: broadcast::Sender<Wire>,
) {
    let title = config.persona.name.clone();
    proactive::spawn(config, scheduler, facts, data, move |notice| {
        deliver(notice, &clients, &title);
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
