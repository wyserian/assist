//! The assist core for the Android app (PLAN.md Phase 4), through UniFFI: the same engine,
//! dialog, policy and scheduler as the desktop, built by `assistant::assemble`. Kotlin calls
//! [`Assistant::say`] for typed and push-to-talk input and [`Assistant::hear`] for the wake
//! word, and shows the answer; jobs run on the core's own tokio runtime. Phone actions call
//! back into the app through [`phone::Phone`].

pub mod models;
pub mod phone;
pub mod voice;

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use assist_core::assistant::{assemble, Host};
use assist_core::config::{Config, Sink};
use assist_core::dialog::{Dialog, LiveEngine};
use assist_core::facts::{Facts, Value};
use assist_core::policy::Principal;
use assist_core::proactive;
use assist_core::replies::{Line, Replies};
use assist_core::scheduler::Scheduler;
use assist_core::store::Store;

use phone::Phone;

uniffi::setup_scaffolding!();

/// Threads for jobs; a phone runs few at a time.
const WORKERS: usize = 2;

/// The facts the app may set (PLAN.md section 3.10); others are refused.
const PHONE_FACTS: [&str; 3] = ["node.battery", "node.charging", "screen.locked"];

/// A fact's value, as the app reports it.
#[derive(Debug, Clone, PartialEq, uniffi::Enum)]
pub enum FactValue {
    /// Yes or no, e.g. `node.charging`.
    Bool {
        /// The value.
        value: bool,
    },
    /// A number, e.g. `node.battery` in percent.
    Number {
        /// The value.
        value: f64,
    },
}

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

/// Why the assistant or its voice could not start.
#[derive(Debug, thiserror::Error, uniffi::Error)]
#[uniffi(flat_error)]
pub enum AssistError {
    /// The config, data directory or engine failed.
    #[error("{0}")]
    Start(String),
    /// A voice model is missing, broken or failed its checksum.
    #[error("{0}")]
    Voice(String),
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

/// The core on this device.
#[derive(uniffi::Object)]
pub struct Assistant {
    runtime: tokio::runtime::Runtime,
    scheduler: Scheduler,
    engine: Arc<LiveEngine>,
    store: Store,
    /// Typed and push-to-talk: the owner on their own unlocked phone.
    typed: Conversation,
    /// The wake word: whoever speaks near the phone (voice trust, PLAN.md section 3.6).
    spoken: Conversation,
    home: PathBuf,
    facts: Facts,
    wake_phrases: Vec<String>,
    awake_seconds: u32,
    woke: String,
}

/// A dialog and who it speaks for.
struct Conversation {
    dialog: Mutex<Dialog>,
    principal: Principal,
}

impl Conversation {
    fn new(engine: &LiveEngine, principal: Principal) -> Self {
        Self {
            dialog: Mutex::new(Dialog::new(engine.get(), principal.clone())),
            principal,
        }
    }
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
        let wake_phrases = config.persona.wake_phrases();
        let awake_seconds = u32::try_from(config.voice.awake.as_secs()).unwrap_or(u32::MAX);
        let woke = Replies::new(&config).line(Line::Wake, &[]);
        let config = Arc::new(config);
        let home = PathBuf::from(data_dir);
        let mut actions = assist_actions::builtin();
        actions.extend(phone::all(&phone));
        let facts = Facts::default();
        let host = Host {
            actions,
            facts: facts.clone(),
            ..Host::default()
        };
        // The scheduler and the rules spawn their tasks on the runtime they are built in.
        let parts = {
            let _inside = runtime.enter();
            let parts = assemble(Arc::clone(&config), &home, host).map_err(|e| failed(&e))?;
            proactive::spawn(
                &config,
                parts.scheduler.clone(),
                facts.clone(),
                home.clone(),
                move |notice| {
                    let phone = Arc::clone(&phone);
                    let speak = notice.sink == Sink::Speak;
                    // The app posts it; keep the runtime's threads free meanwhile.
                    drop(tokio::task::spawn_blocking(move || {
                        phone.notice(notice.text, speak)
                    }));
                },
            );
            parts
        };
        Ok(Arc::new(Self {
            typed: Conversation::new(&parts.engine, Principal::local("android")),
            spoken: Conversation::new(&parts.engine, Principal::voice("android")),
            runtime,
            scheduler: parts.scheduler,
            engine: parts.engine,
            store: parts.store,
            home,
            facts,
            wake_phrases,
            awake_seconds,
            woke,
        }))
    }

    /// Handles what the owner typed or said with push-to-talk and starts the jobs it asks for.
    pub fn say(&self, text: String) -> Answer {
        self.converse(&self.typed, &text, false)
    }

    /// Handles what was heard after the wake word. `vouched` is true when the owner stands
    /// behind it, such as a tap on Yes on screen; a spoken "yes" alone does not confirm.
    pub fn hear(&self, text: String, vouched: bool) -> Answer {
        self.converse(&self.spoken, &text, vouched)
    }

    /// Sets a phone fact such as `node.battery`; false for a name the app may not set.
    pub fn set_fact(&self, name: String, value: FactValue) -> bool {
        if !PHONE_FACTS.contains(&name.as_str()) {
            return false;
        }
        let value = match value {
            FactValue::Bool { value } => Value::Bool(value),
            FactValue::Number { value } if value.is_finite() => Value::Number(value),
            FactValue::Number { .. } => return false,
        };
        self.facts.set(&name, value);
        true
    }

    /// The wake phrases from `[persona]`, for the hands-free listener.
    pub fn wake_phrases(&self) -> Vec<String> {
        self.wake_phrases.clone()
    }

    /// How long requests need no wake word after the last one (`[voice] awake`).
    pub fn awake_seconds(&self) -> u32 {
        self.awake_seconds
    }

    /// What to say when the wake word comes alone ("Yes, Prime?").
    pub fn woke_reply(&self) -> String {
        self.woke.clone()
    }

    /// Stops a job, such as a recording from its notification; false if it is not running.
    pub fn stop(&self, job: u64) -> bool {
        self.scheduler.cancel(job)
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

impl Assistant {
    fn converse(&self, with: &Conversation, text: &str, vouched: bool) -> Answer {
        let engine = self.engine.get();
        let (reply, waiting) = {
            let mut dialog = with.dialog.lock().unwrap_or_else(PoisonError::into_inner);
            dialog.use_engine(Arc::clone(&engine));
            let reply = dialog.handle_vouched(&with.principal, text, vouched, Instant::now());
            (reply, dialog.is_waiting())
        };
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
                .submit_pipe(run, with.principal.clone(), self.home.clone())
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
/// uses; config.toml entries win (`Config::add_found_apps`).
fn add_phone_lists(config: &mut Config, contacts: Vec<Contact>, apps: Vec<App>) {
    config.add_found_contacts(contacts.into_iter().map(|c| (c.name, c.number)));
    config.add_found_apps(apps.into_iter().map(|a| (a.name, a.package_name)));
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
        fn lock(&self) -> Result<(), PhoneError> {
            self.log("lock".to_owned())
        }
        fn screenshot(&self, name: String) -> Result<String, PhoneError> {
            self.log(format!("screenshot {name}"))?;
            Ok(format!("Pictures/Assist/{name}"))
        }
        fn capture(&self) -> Result<String, PhoneError> {
            Err(PhoneError::Failed {
                reason: "no screen in tests".to_owned(),
            })
        }
        fn offer_models(&self, feature: models::Feature) -> Result<(), PhoneError> {
            self.log(format!("offer {feature:?}"))
        }
        fn start_recording(
            &self,
            what: phone::Recording,
            name: String,
            _job: u64,
        ) -> Result<String, PhoneError> {
            self.log(format!("record {what:?} {name}"))?;
            Ok(format!("Music/Assist/{name}"))
        }
        fn stop_recording(&self) -> Result<String, PhoneError> {
            self.log("stop recording".to_owned())?;
            Ok("Music/Assist/saved".to_owned())
        }
        fn notice(&self, text: String, speak: bool) -> Result<(), PhoneError> {
            self.log(format!("notice {speak} {text}"))
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
    fn a_wake_word_call_needs_a_yes_on_screen() {
        let phone = Arc::new(FakePhone::default());
        let (assistant, dir) = start("voice", Arc::clone(&phone));
        assert!(assistant.hear("call mom".to_owned(), false).waiting);
        let spoken = assistant.hear("yes".to_owned(), false);
        assert!(spoken.jobs.is_empty() && spoken.waiting, "{spoken:?}");
        assert!(spoken.say[0].contains("Confirm on screen"), "{spoken:?}");
        let tapped = assistant.hear("yes".to_owned(), true);
        wait(&assistant, tapped.jobs[0].id);
        assert_eq!(*phone.0.lock().unwrap(), ["call 555 0100"]);
        assert!(!assistant.wake_phrases().is_empty());
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
    fn locks_takes_screenshots_and_offers_the_ocr_models() {
        let phone = Arc::new(FakePhone::default());
        let (assistant, dir) = start("screen", Arc::clone(&phone));
        let lock = assistant.say("lock the phone".to_owned());
        wait(&assistant, lock.jobs[0].id);
        let shot = assistant.say("take a screenshot".to_owned());
        let saved = wait(&assistant, shot.jobs[0].id);
        assert_eq!(saved.lines.len(), 1, "{saved:?}");
        assert!(
            saved.lines[0].starts_with("Pictures/Assist/assist-"),
            "{saved:?}"
        );
        let read = assistant.say("read the screen".to_owned());
        let refused = wait(&assistant, read.jobs[0].id);
        assert_eq!(refused.state, "failed");
        assert!(
            refused.lines[0].contains("text reading models"),
            "{refused:?}"
        );
        let log = phone.0.lock().unwrap().clone();
        assert_eq!(log[0], "lock");
        assert!(log[1].starts_with("screenshot assist-"));
        assert_eq!(log[2], "offer Ocr");
        finish(assistant, dir);
    }

    #[test]
    fn records_for_a_length_and_stops_when_the_job_is_stopped() {
        let phone = Arc::new(FakePhone::default());
        let (assistant, dir) = start("record", Arc::clone(&phone));
        let timed = assistant.say("record audio for 1s".to_owned());
        let out = wait(&assistant, timed.jobs[0].id);
        assert_eq!(out.state, "done", "{out:?}");
        let job = assistant.say("record screen to demo".to_owned()).jobs[0].id;
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(assistant.stop(job));
        wait(&assistant, job);
        // The guard stops the recorder from a blocking thread; give it a moment.
        std::thread::sleep(std::time::Duration::from_millis(200));
        let log = phone.0.lock().unwrap().clone();
        assert!(log[0].starts_with("record Audio assist-"), "{log:?}");
        assert_eq!(log[1], "stop recording");
        assert_eq!(log[2], "record Screen demo.mp4");
        assert_eq!(log[3], "stop recording");
        finish(assistant, dir);
    }

    #[test]
    fn a_low_battery_rule_speaks_on_the_phone() {
        let phone = Arc::new(FakePhone::default());
        let dir = std::env::temp_dir().join(format!("assist-ffi-facts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let config = "[[proactive.rules]]\nname = \"battery\"\non = \"node.battery < 20 and not node.charging\"\nsay = \"Battery is low.\"\n";
        let assistant = Assistant::new(
            config.to_owned(),
            dir.display().to_string(),
            Vec::new(),
            Vec::new(),
            Arc::clone(&phone) as Arc<dyn Phone>,
        )
        .unwrap();
        assert!(!assistant.set_fact("app.focused".to_owned(), FactValue::Bool { value: true }));
        assert!(assistant.set_fact("node.charging".to_owned(), FactValue::Bool { value: false }));
        assert!(assistant.set_fact("node.battery".to_owned(), FactValue::Number { value: 12.0 }));
        // The rules check facts when they start and then every 15 s.
        for _ in 0..400 {
            if !phone.0.lock().unwrap().is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert_eq!(*phone.0.lock().unwrap(), ["notice true Battery is low."]);
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
        let long = "x".repeat(101);
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
