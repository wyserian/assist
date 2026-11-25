//! The assist core for the Android app (PLAN.md Phase 4), through UniFFI: the same engine,
//! dialog, policy and scheduler as the desktop, built by `assistant::assemble`. Kotlin calls
//! [`Assistant::say`] and shows the answer; jobs run on the core's own tokio runtime. Phone
//! actions call back into the app through [`phone::Phone`].

pub mod phone;

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use assist_core::assistant::{assemble, Host};
use assist_core::config::Config;
use assist_core::dialog::{Dialog, LiveEngine};
use assist_core::policy::Principal;
use assist_core::scheduler::Scheduler;
use assist_core::store::Store;

use phone::Phone;

uniffi::setup_scaffolding!();

/// Threads for jobs; a phone runs few at a time.
const WORKERS: usize = 2;

/// Most contacts and apps taken from the phone, and the longest name kept.
const MAX_ENTRIES: usize = 5000;
const MAX_NAME: usize = 100;

/// A contact from the phone's address book.
#[derive(Debug, Clone, uniffi::Record)]
pub struct Contact {
    /// Display name, as the user says it.
    pub name: String,
    /// Number to call or text.
    pub number: String,
}

/// An app on the phone's launcher.
#[derive(Debug, Clone, uniffi::Record)]
pub struct App {
    /// Label, as the user says it.
    pub name: String,
    /// Package name to launch.
    pub package_name: String,
}

/// Why the assistant could not start.
#[derive(Debug, thiserror::Error, uniffi::Error)]
#[uniffi(flat_error)]
pub enum AssistError {
    /// The config, data directory or engine failed.
    #[error("{0}")]
    Start(String),
}

/// What the assistant answers.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct Answer {
    /// Lines to show or speak, in order.
    pub say: Vec<String>,
    /// Jobs it started.
    pub jobs: Vec<Job>,
    /// It waits for an answer (yes or no, a name, ...).
    pub waiting: bool,
}

/// A started job.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct Job {
    /// Job id, for [`Assistant::output`].
    pub id: u64,
    /// What it runs, as read back.
    pub title: String,
}

/// One conversation with the core on this device.
#[derive(uniffi::Object)]
pub struct Assistant {
    runtime: tokio::runtime::Runtime,
    scheduler: Scheduler,
    engine: Arc<LiveEngine>,
    store: Store,
    dialog: Mutex<Dialog>,
    owner: Principal,
    home: PathBuf,
}

#[uniffi::export]
impl Assistant {
    /// Starts the core with `config_toml` (the contents of config.toml), its data in
    /// `data_dir` (the app's private files directory), the phone's contacts and apps for
    /// "call mom" and "open camera", and `phone` to do it.
    #[uniffi::constructor]
    pub fn new(
        config_toml: String,
        data_dir: String,
        contacts: Vec<Contact>,
        apps: Vec<App>,
        phone: Arc<dyn Phone>,
    ) -> Result<Arc<Self>, AssistError> {
        let failed = |err: &dyn std::fmt::Display| AssistError::Start(err.to_string());
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(WORKERS)
            .enable_all()
            .build()
            .map_err(|e| failed(&e))?;
        let mut config = Config::from_toml(&config_toml).map_err(|e| failed(&e))?;
        add_phone_lists(&mut config, contacts, apps);
        let config = Arc::new(config);
        let home = PathBuf::from(data_dir);
        let mut actions = assist_actions::builtin();
        actions.extend(phone::all(&phone));
        let host = Host {
            actions,
            ..Host::default()
        };
        // The scheduler spawns its tasks on the runtime it is built in.
        let parts = {
            let _inside = runtime.enter();
            assemble(config, &home, host).map_err(|e| failed(&e))?
        };
        let owner = Principal::local("android");
        Ok(Arc::new(Self {
            dialog: Mutex::new(Dialog::new(parts.engine.get(), owner.clone())),
            runtime,
            scheduler: parts.scheduler,
            engine: parts.engine,
            store: parts.store,
            owner,
            home,
        }))
    }

    /// Handles what the user typed or said and starts the jobs it asks for.
    pub fn say(&self, text: String) -> Answer {
        let engine = self.engine.get();
        let reply = {
            let mut dialog = self.dialog.lock().unwrap_or_else(PoisonError::into_inner);
            dialog.use_engine(Arc::clone(&engine));
            dialog.handle(&self.owner, &text, Instant::now())
        };
        let waiting = self
            .dialog
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_waiting();
        let mut answer = Answer {
            say: reply.say,
            jobs: Vec::new(),
            waiting,
        };
        let _inside = self.runtime.enter();
        for pipe in reply.run {
            let run = engine.submission(pipe);
            match self
                .scheduler
                .submit_pipe(run, self.owner.clone(), self.home.clone())
            {
                Ok(ids) => answer.jobs.extend(ids.into_iter().map(|id| Job {
                    id,
                    title: self.scheduler.get(id).map(|j| j.title).unwrap_or_default(),
                })),
                Err(err) => answer.say.push(err.to_string()),
            }
        }
        answer
    }

    /// A job's last output lines and whether it has ended.
    pub fn output(&self, job: u64) -> JobOutput {
        let info = self.scheduler.get(job);
        let mut lines = self.scheduler.tail(job).unwrap_or_default();
        // Why it failed, such as a missing permission, is for the user to read too.
        if let Some(err) = info.as_ref().and_then(|j| j.error.as_ref()) {
            lines.push(err.message.clone());
        }
        JobOutput {
            lines,
            ended: info.as_ref().is_none_or(|j| j.state.is_finished()),
            state: info.map_or_else(|| "gone".to_owned(), |j| j.state.as_str().to_owned()),
        }
    }
}

/// What a job printed so far.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct JobOutput {
    /// Its last lines, oldest first.
    pub lines: Vec<String>,
    /// `queued`, `running`, `done`, `failed`, ... or `gone` when it is no longer kept.
    pub state: String,
    /// It finished or is no longer kept: no more lines will come.
    pub ended: bool,
}

/// Adds the phone's contacts and apps to the `[contacts]` and `[apps]` lists the matcher
/// uses. Entries in config.toml win; names are trimmed and bounded, numbers taken as given.
fn add_phone_lists(config: &mut Config, contacts: Vec<Contact>, apps: Vec<App>) {
    let name = |raw: &str| {
        let name = raw.trim();
        (!name.is_empty() && name.chars().count() <= MAX_NAME).then(|| name.to_owned())
    };
    for contact in contacts.into_iter().take(MAX_ENTRIES) {
        if let Some(key) = name(&contact.name) {
            config.contacts.entry(key).or_insert(contact.number);
        }
    }
    for app in apps.into_iter().take(MAX_ENTRIES) {
        if let Some(key) = name(&app.name) {
            config
                .apps
                .entry(key)
                .or_insert(toml::Value::String(app.package_name));
        }
    }
}

impl Drop for Assistant {
    fn drop(&mut self) {
        // Writes pending job history before the app's process goes away.
        self.store.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phone::PhoneError;

    /// Records what the core asked the phone to do.
    #[derive(Default)]
    struct FakePhone(Mutex<Vec<String>>);

    impl FakePhone {
        fn log(&self, line: String) -> Result<(), PhoneError> {
            self.0.lock().unwrap().push(line);
            Ok(())
        }
    }

    impl Phone for FakePhone {
        fn open_app(&self, package_name: String) -> Result<(), PhoneError> {
            self.log(format!("open {package_name}"))
        }
        fn call(&self, number: String) -> Result<(), PhoneError> {
            self.log(format!("call {number}"))
        }
        fn send_sms(&self, number: String, text: String) -> Result<(), PhoneError> {
            self.log(format!("sms {number} {text}"))
        }
        fn set_volume(&self, _percent: u8) -> Result<(), PhoneError> {
            Err(PhoneError::Failed {
                reason: "Volume is not allowed.".to_owned(),
            })
        }
    }

    /// An assistant in a fresh data directory; remove it with [`finish`].
    fn start(name: &str, phone: Arc<FakePhone>) -> (Arc<Assistant>, PathBuf) {
        let dir = std::env::temp_dir().join(format!("assist-ffi-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let contacts = vec![Contact {
            name: "Mom".to_owned(),
            number: "555 0100".to_owned(),
        }];
        let apps = vec![App {
            name: "Camera".to_owned(),
            package_name: "com.android.camera2".to_owned(),
        }];
        let assistant = Assistant::new(
            String::new(),
            dir.display().to_string(),
            contacts,
            apps,
            phone,
        )
        .unwrap();
        (assistant, dir)
    }

    fn finish(assistant: Arc<Assistant>, dir: PathBuf) {
        drop(assistant);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Waits for the job to end and returns its output.
    fn wait(assistant: &Assistant, job: u64) -> JobOutput {
        for _ in 0..100 {
            let out = assistant.output(job);
            if out.ended {
                return out;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("job {job} did not end");
    }

    #[test]
    fn remembers_and_answers_like_the_desktop() {
        let (assistant, dir) = start("memory", Arc::default());
        let answer = assistant.say("remember that the car is on level 3".to_owned());
        assert_eq!(answer.jobs.len(), 1);
        wait(&assistant, answer.jobs[0].id);
        let recalled = assistant.say("where is the car".to_owned());
        assert_eq!(recalled.say, ["The car is on level 3."]);
        finish(assistant, dir);
    }

    #[test]
    fn calls_and_texts_phone_contacts_after_confirming() {
        let phone = Arc::new(FakePhone::default());
        let (assistant, dir) = start("call", Arc::clone(&phone));
        let asked = assistant.say("call mom".to_owned());
        assert!(asked.waiting && asked.jobs.is_empty(), "{asked:?}");
        let yes = assistant.say("yes".to_owned());
        wait(&assistant, yes.jobs[0].id);
        assistant.say("text mom saying running late".to_owned());
        let yes = assistant.say("yes".to_owned());
        wait(&assistant, yes.jobs[0].id);
        assert_eq!(
            *phone.0.lock().unwrap(),
            ["call 555 0100", "sms 555 0100 running late"]
        );
        finish(assistant, dir);
    }

    #[test]
    fn opens_phone_apps_and_shows_why_a_command_failed() {
        let phone = Arc::new(FakePhone::default());
        let (assistant, dir) = start("apps", Arc::clone(&phone));
        let open = assistant.say("open camera".to_owned());
        wait(&assistant, open.jobs[0].id);
        assert_eq!(*phone.0.lock().unwrap(), ["open com.android.camera2"]);
        let volume = assistant.say("volume 40".to_owned());
        let out = wait(&assistant, volume.jobs[0].id);
        assert_eq!(out.state, "failed");
        assert_eq!(out.lines, ["Volume is not allowed."]);
        finish(assistant, dir);
    }

    #[test]
    fn config_entries_win_and_names_are_bounded() {
        let mut config = Config::from_toml(
            "[contacts]
mom = \"1\"
",
        )
        .unwrap();
        let contact = |name: &str| Contact {
            name: name.to_owned(),
            number: "2".to_owned(),
        };
        let long = "x".repeat(MAX_NAME + 1);
        add_phone_lists(
            &mut config,
            vec![
                contact("mom"),
                contact("  "),
                contact(&long),
                contact(" Sam "),
            ],
            vec![],
        );
        let names: Vec<_> = config.contacts.iter().collect();
        assert_eq!(
            names,
            [
                (&"Sam".to_owned(), &"2".to_owned()),
                (&"mom".to_owned(), &"1".to_owned())
            ]
        );
    }
}
