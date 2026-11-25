//! [`assist_proto::rpc`] methods. Blocking work (store, keystore) runs on `spawn_blocking`.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use assist_core::config::{edit, setting_applies, Applies, SettingValue};
use assist_core::dialog::SHELL;
use assist_core::phrases::add_user_phrase;
use assist_core::policy::Trust;
use assist_core::scheduler::JobInfo;
use assist_core::{autopilot, memory};
use assist_proto::rpc::{
    self, AuditPage, AutopilotSet, ClientRef, CommandEntry, CommandSetting, DialogAnswer, Explain,
    GoalStatus, History, JobRef, MemoryForget, Miss, MissText, Otp, PhraseAdd, SecretEntry,
    SecretName, SecretSet, SettingChange, SettingResult, Settings,
};
use assist_proto::{JobState, Rpc, RpcResult};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::conn::Conn;
use crate::Credential;

/// Bytes of an old job's log returned by `jobs.tail`.
const TAIL_BYTES: u64 = 64 * 1024;
/// Most jobs returned by one `jobs.history` call.
const MAX_HISTORY: usize = 500;
/// Unmatched sentences returned by `misses.list`.
const MAX_MISSES: usize = 50;
/// Longest phrase `phrases.add` accepts.
const MAX_PHRASE: usize = 200;
/// How long `dialogs.answer` waits for the other client to take the answer.
const ANSWER_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

pub(crate) async fn call(conn: &mut Conn, call: Rpc) -> RpcResult {
    match dispatch(conn, &call.method, call.params).await {
        Ok(result) => result,
        Err(error) => error,
    }
}

async fn dispatch(conn: &mut Conn, method: &str, params: Value) -> Result<RpcResult, RpcResult> {
    let services = &conn.state.services;
    let scheduler = &services.scheduler;
    Ok(match method {
        rpc::JOBS_LIST => {
            let mut jobs: Vec<_> = scheduler.list().iter().map(JobInfo::summary).collect();
            let active: Vec<u64> = jobs
                .iter()
                .filter(|j| matches!(j.state, JobState::Running | JobState::Paused))
                .map(|j| j.id)
                .collect();
            if !active.is_empty() {
                let measure = scheduler.clone();
                let usage = blocking(move || Ok::<_, String>(measure.usage(&active))).await?;
                for job in &mut jobs {
                    if let Some(used) = usage.get(&job.id) {
                        job.cpu = Some(used.cpu);
                        job.memory = Some(used.memory);
                    }
                }
            }
            RpcResult::ok(jobs)
        }
        rpc::JOBS_HISTORY => {
            let page: History = parse(params)?;
            let store = services.store.clone();
            let limit = page.limit.unwrap_or(50).min(MAX_HISTORY);
            let jobs = blocking(move || store.history(limit, page.before)).await?;
            RpcResult::ok(jobs)
        }
        rpc::JOBS_TAIL => {
            let job: JobRef = parse(params)?;
            match scheduler.tail(job.id) {
                Some(lines) => RpcResult::ok(lines),
                None => {
                    let store = services.store.clone();
                    RpcResult::ok(blocking(move || store.log_tail(job.id, TAIL_BYTES)).await?)
                }
            }
        }
        rpc::JOBS_STOP => {
            let job: JobRef = parse(params)?;
            RpcResult::ok(scheduler.cancel(job.id))
        }
        rpc::JOBS_PAUSE | rpc::JOBS_RESUME => {
            let job: JobRef = parse(params)?;
            let done = if method == rpc::JOBS_PAUSE {
                scheduler.pause(job.id)
            } else {
                scheduler.resume(job.id)
            };
            done.map_err(|err| RpcResult::err("failed", err.to_string()))?;
            RpcResult::ok(true)
        }
        rpc::JOBS_STOP_ALL => RpcResult::ok(scheduler.cancel_all()),
        rpc::SECRETS_LIST => {
            let secrets = services.secrets.as_ref().ok_or_else(no_secrets)?;
            let entries: Vec<SecretEntry> = secrets
                .list()
                .into_iter()
                .map(|(name, info)| SecretEntry {
                    name,
                    set: info.set,
                    last4: info.last4,
                })
                .collect();
            RpcResult::ok(entries)
        }
        rpc::SECRETS_SET => {
            require_local(conn)?;
            let secret: SecretSet = parse(params)?;
            let secrets = services.secrets.clone().ok_or_else(no_secrets)?;
            blocking(move || secrets.set(&secret.name, &secret.value)).await?;
            RpcResult::ok(true)
        }
        rpc::SECRETS_DELETE => {
            require_local(conn)?;
            let secret: SecretName = parse(params)?;
            let secrets = services.secrets.clone().ok_or_else(no_secrets)?;
            RpcResult::ok(blocking(move || secrets.delete(&secret.name)).await?)
        }
        rpc::AUTOPILOT_LIST => {
            require_local(conn)?;
            let engine = services.engine.get();
            let config = engine.config();
            let data = services.data_dir.clone();
            let state = blocking(move || Ok::<_, String>(autopilot::load(&data))).await?;
            let today = jiff::ZonedDateTime::now().date().to_string();
            let goals: Vec<GoalStatus> = config
                .autopilot
                .goals
                .iter()
                .map(|goal| {
                    let state_name = if !config.autopilot.enabled {
                        "disabled"
                    } else if today > goal.expires {
                        "expired"
                    } else if state.off {
                        "off"
                    } else if state.paused.contains(&goal.name) {
                        "paused"
                    } else {
                        "on"
                    };
                    GoalStatus {
                        name: goal.name.clone(),
                        objective: goal.objective.clone(),
                        run: goal.run.clone(),
                        state: state_name.to_owned(),
                        used_today: state.used_on(&goal.name, &today),
                        jobs_per_day: goal.budget.jobs_per_day,
                        expires: goal.expires.clone(),
                    }
                })
                .collect();
            RpcResult::ok(goals)
        }
        rpc::AUTOPILOT_SET => {
            require_local(conn)?;
            let set: AutopilotSet = parse(params)?;
            let data = services.data_dir.clone();
            blocking(move || {
                autopilot::update(&data, |s| match &set.goal {
                    None => s.off = !set.on,
                    Some(goal) if set.on => {
                        s.paused.remove(goal);
                    }
                    Some(goal) => {
                        s.paused.insert(goal.clone());
                    }
                })
            })
            .await?;
            RpcResult::ok(true)
        }
        rpc::MEMORY_LIST => {
            require_local(conn)?;
            let data = services.data_dir.clone();
            RpcResult::ok(blocking(move || Ok::<_, String>(memory::list(&data))).await?)
        }
        rpc::MEMORY_FORGET => {
            require_local(conn)?;
            let forget: MemoryForget = parse(params)?;
            let data = services.data_dir.clone();
            RpcResult::ok(blocking(move || memory::forget(&data, &forget.ids)).await?)
        }
        rpc::COMMANDS_EXPLAIN => {
            let explain: Explain = parse(params)?;
            RpcResult::ok(
                services
                    .engine
                    .get()
                    .explain(&conn.principal, &explain.text),
            )
        }
        rpc::CLIENTS_LIST => RpcResult::ok(conn.state.client_list()),
        rpc::CLIENTS_REVOKE => {
            require_local(conn)?;
            let client: ClientRef = parse(params)?;
            RpcResult::ok(conn.state.revoke(client.id))
        }
        rpc::AUDIT_TAIL => {
            let page: AuditPage = parse(params)?;
            let audit = services
                .audit
                .clone()
                .ok_or_else(|| RpcResult::err("unavailable", "this daemon has no audit log"))?;
            let limit = page.limit.unwrap_or(100).min(MAX_HISTORY);
            RpcResult::ok(blocking(move || audit.tail(limit)).await?)
        }
        rpc::MISSES_LIST => {
            let store = services.store.clone();
            let rows = blocking(move || store.misses(MAX_MISSES)).await?;
            let engine = services.engine.get();
            let misses: Vec<Miss> = rows
                .into_iter()
                .map(|(text, count, last)| Miss {
                    guess: engine.guess(&text),
                    text,
                    count,
                    last,
                })
                .collect();
            RpcResult::ok(misses)
        }
        rpc::MISSES_DISMISS => {
            let miss: MissText = parse(params)?;
            services.store.forget_miss(&miss.text);
            RpcResult::ok(true)
        }
        rpc::COMMANDS_LIST => {
            let engine = services.engine.get();
            let commands: Vec<CommandEntry> = engine
                .phrases()
                .commands()
                .map(|(name, spec)| CommandEntry {
                    name: name.to_owned(),
                    slots: spec.slots.clone(),
                    available: engine.available(name),
                })
                .collect();
            RpcResult::ok(commands)
        }
        rpc::PHRASES_ADD => {
            require_local(conn)?;
            let add: PhraseAdd = parse(params)?;
            let phrase = add.phrase.trim().to_owned();
            if phrase.is_empty() || phrase.len() > MAX_PHRASE {
                return Err(RpcResult::err(
                    "bad_request",
                    format!("a phrase is 1 to {MAX_PHRASE} characters"),
                ));
            }
            let file = services.user_phrases.clone().ok_or_else(|| {
                RpcResult::err("unavailable", "this daemon has no user phrase file")
            })?;
            let engine = services.engine.get();
            let not_found = || RpcResult::err("not_found", format!("no command {}", add.command));
            let spec = engine
                .phrases()
                .spec(&add.command)
                .cloned()
                .ok_or_else(not_found)?;
            let phrases = engine
                .phrases()
                .with_phrase(&add.command, &phrase)
                .ok_or_else(not_found)?;
            // Compiled before writing, so a broken phrase never reaches the file.
            let updated = engine
                .with_phrases(&phrases)
                .map_err(|err| RpcResult::err("bad_request", err.to_string()))?;
            let command = add.command.clone();
            blocking(move || add_user_phrase(&file, &command, &spec, &phrase)).await?;
            // shortcut: two dashboards adding at the same moment can drop one reload (both
            // read the same engine); the phrase is still in the file and loads on restart.
            services.engine.set(Arc::new(updated));
            if let Some(miss) = add.miss {
                services.store.forget_miss(&miss);
            }
            RpcResult::ok(true)
        }
        rpc::SETTINGS_GET => {
            let engine = services.engine.get();
            let config = engine.config();
            let mut names: Vec<String> = engine
                .phrases()
                .commands()
                .map(|(name, _)| name.to_owned())
                .collect();
            names.push(SHELL.to_owned());
            names.sort();
            let commands = names
                .into_iter()
                .map(|name| CommandSetting {
                    risky: config.commands.get(&name).is_some_and(|c| c.risky),
                    name,
                })
                .collect();
            RpcResult::ok(Settings {
                commands,
                lanes_interactive: config.lanes.interactive,
                lanes_background: config.lanes.background,
                jobs_timeout: format!("{}s", config.jobs.timeout.as_secs()),
                output_tail_kb: config.jobs.output_tail_kb,
                retention_days: config.jobs.retention_days,
                log_cap_mb: config.jobs.log_cap_mb,
                search_url: config.search.url.clone(),
                restart_pending: conn.state.restart_pending.load(Ordering::Relaxed),
            })
        }
        rpc::SETTINGS_SET => {
            require_local(conn)?;
            let change: SettingChange = parse(params)?;
            let value = match change.value {
                Value::Bool(b) => SettingValue::Bool(b),
                Value::Number(n) => SettingValue::Int(
                    n.as_i64()
                        .ok_or_else(|| RpcResult::err("bad_request", "numbers must be whole"))?,
                ),
                Value::String(text) => SettingValue::Text(text),
                _ => {
                    return Err(RpcResult::err(
                        "bad_request",
                        "use true/false, a number or text",
                    ))
                }
            };
            let applies = setting_applies(&change.key).ok_or_else(|| {
                RpcResult::err(
                    "bad_request",
                    format!("{} cannot be changed here", change.key),
                )
            })?;
            let file = services
                .config_file
                .clone()
                .ok_or_else(|| RpcResult::err("unavailable", "this daemon has no config file"))?;
            let read = file.clone();
            let src = blocking(move || match std::fs::read_to_string(&read) {
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
                other => other,
            })
            .await?;
            let (text, config) = edit(&src, &change.key, value)
                .map_err(|err| RpcResult::err("bad_request", err.to_string()))?;
            // Built before writing: a config naming an unknown command is refused here.
            let engine = services
                .engine
                .get()
                .with_config(Arc::new(config))
                .map_err(|err| RpcResult::err("bad_request", err.to_string()))?;
            blocking(move || write_atomically(&file, &text)).await?;
            services.engine.set(Arc::new(engine));
            let restart_needed = applies == Applies::AfterRestart;
            if restart_needed {
                conn.state.restart_pending.store(true, Ordering::Relaxed);
            }
            RpcResult::ok(SettingResult { restart_needed })
        }
        rpc::DIALOGS_PENDING => RpcResult::ok(conn.state.waiting()),
        rpc::FACTS_LIST => {
            let facts = conn.state.services.facts.all();
            let shown: BTreeMap<String, String> = facts
                .into_iter()
                .map(|(name, value)| (name, value.to_string()))
                .collect();
            RpcResult::ok(shown)
        }
        rpc::DIALOGS_ANSWER => {
            // A local user on the dashboard may answer any client's question (PLAN.md 3.5).
            require_local(conn)?;
            let answer: DialogAnswer = parse(params)?;
            if answer.client == conn.id {
                return Ok(RpcResult::ok(conn.converse(&answer.text, true)));
            }
            let reply = conn
                .state
                .answer(answer.client, answer.text)
                .ok_or_else(|| RpcResult::err("not_found", "that client is not waiting"))?;
            match tokio::time::timeout(ANSWER_WAIT, reply).await {
                Ok(Ok(reply)) => RpcResult::ok(reply),
                _ => RpcResult::err("failed", "the client did not take the answer"),
            }
        }
        rpc::PAIR_START => {
            // Pairing lets a device in as the owner: only the token holder may start it.
            if conn.credential != Credential::Token {
                return Err(RpcResult::err("denied", "only the CLI can pair devices"));
            }
            let nodes = services.nodes.as_ref().ok_or_else(|| {
                RpcResult::err(
                    "unavailable",
                    "the network did not start; see the daemon's output",
                )
            })?;
            RpcResult::ok(nodes.pairing.start(nodes.endpoint.id()).encode())
        }
        rpc::AUTH_OTP => {
            // Only the token holder can let a browser in.
            if conn.credential != Credential::Token {
                return Err(RpcResult::err(
                    "denied",
                    "only the CLI can create sign-in codes",
                ));
            }
            let code = conn
                .state
                .auth
                .new_otp()
                .map_err(|err| RpcResult::err("internal", err.to_string()))?;
            RpcResult::ok(Otp { code })
        }
        other => RpcResult::err("unknown_method", format!("no method {other}")),
    })
}

fn parse<T: DeserializeOwned>(params: Value) -> Result<T, RpcResult> {
    serde_json::from_value(params).map_err(|err| RpcResult::err("bad_request", err.to_string()))
}

fn require_local(conn: &Conn) -> Result<(), RpcResult> {
    if conn.principal.trust == Trust::Local {
        Ok(())
    } else {
        Err(RpcResult::err("denied", "only local clients can do this"))
    }
}

/// Replaces `file` with `text` via a temporary file, so a crash never leaves it half written.
fn write_atomically(file: &Path, text: &str) -> std::io::Result<()> {
    let tmp = file.with_extension("toml.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, file)
}

fn no_secrets() -> RpcResult {
    RpcResult::err("unavailable", "no keystore on this machine")
}

/// Runs blocking work off the async runtime; errors become `internal` results.
async fn blocking<T, E>(
    work: impl FnOnce() -> Result<T, E> + Send + 'static,
) -> Result<T, RpcResult>
where
    T: Send + 'static,
    E: std::fmt::Display + Send + 'static,
{
    match tokio::task::spawn_blocking(work).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(err)) => Err(RpcResult::err("failed", err.to_string())),
        Err(err) => Err(RpcResult::err("internal", err.to_string())),
    }
}
