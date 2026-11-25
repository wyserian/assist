//! Config loading and validation (PLAN.md section 6).
//!
//! Only the sections the current phase uses are accepted; unknown keys are errors so typos
//! (`[contcts]`) never pass silently.

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer};

use crate::condition::Condition;
use crate::dialog::SHELL;
use crate::normalize::normalize;
use crate::phrases::PhraseSet;
use crate::policy::PolicyOverrides;
use crate::replies::Line;
use crate::trigger::{Missed, Schedule};

/// The config format version this build reads.
pub const CURRENT_VERSION: u32 = 1;

/// The parsed `config.toml`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Format version; must equal [`CURRENT_VERSION`].
    #[serde(default = "current_version")]
    pub config_version: u32,
    /// Apps by name; per-OS launch values are read by the open-app action.
    #[serde(default)]
    pub apps: BTreeMap<String, toml::Value>,
    /// Named scripts; values are read by the script action.
    #[serde(default)]
    pub scripts: BTreeMap<String, toml::Value>,
    /// Saved pipes, run by name ("run morning").
    #[serde(default)]
    pub pipes: BTreeMap<String, SavedPipe>,
    /// Contacts by name, value is the phone number.
    #[serde(default)]
    pub contacts: BTreeMap<String, String>,
    /// Per-command settings, keyed by command name.
    #[serde(default)]
    pub commands: BTreeMap<String, CommandConfig>,
    /// Overrides of the default policy table (PLAN.md section 3.6).
    #[serde(default)]
    pub policy: PolicyOverrides,
    /// How many jobs may run at once per lane.
    #[serde(default)]
    pub lanes: LanesConfig,
    /// Job limits.
    #[serde(default)]
    pub jobs: JobsConfig,
    /// Web search.
    #[serde(default)]
    pub search: SearchConfig,
    /// The local daemon.
    #[serde(default)]
    pub server: ServerConfig,
    /// Who the assistant is and how it addresses you.
    #[serde(default)]
    pub persona: PersonaConfig,
    /// Saved pipes run on a clock.
    #[serde(default)]
    pub schedules: Vec<ScheduleConfig>,
    /// Saved pipes run on fact changes or file changes.
    #[serde(default)]
    pub triggers: Vec<TriggerConfig>,
    /// Reply lines that replace the defaults, keyed as in `phrases/en/replies.toml`.
    #[serde(default)]
    pub replies: BTreeMap<String, String>,
    /// Voice input.
    #[serde(default)]
    pub voice: VoiceConfig,
    /// Messages the assistant sends unasked.
    #[serde(default)]
    pub proactive: ProactiveConfig,
    /// Standing goals that run with nobody watching.
    #[serde(default)]
    pub autopilot: AutopilotConfig,
    /// Ways in from outside: the Telegram bot.
    #[serde(default)]
    pub sources: SourcesConfig,
}

/// `[sources]`: adapters that bring requests in from outside this machine.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SourcesConfig {
    /// `[sources.telegram]`.
    pub telegram: TelegramConfig,
}

/// `[sources.telegram]`: the Telegram bot (PLAN.md section 3.13). Its token is the secret
/// `telegram_bot_token`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TelegramConfig {
    /// Runs the bot.
    pub enabled: bool,
    /// Telegram user ids allowed to talk to it, as the owner on a remote device; messages
    /// from anyone else are ignored.
    pub allowed_user_ids: Vec<i64>,
    /// Lets Telegram run shell commands and scripts (asked first); off by default.
    pub shell: bool,
}

/// `[autopilot]` (PLAN.md section 3.18).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AutopilotConfig {
    /// Goals run only when this is on.
    pub enabled: bool,
    /// The goals.
    pub goals: Vec<GoalConfig>,
}

/// `[[autopilot.goals]]`: a saved pipe run on a trigger, inside limits.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoalConfig {
    /// Name, for `assist autopilot pause <name>` and the audit log.
    pub name: String,
    /// What it is for, shown in lists.
    #[serde(default)]
    pub objective: String,
    /// The saved pipe it runs.
    pub run: String,
    /// When: one of `at`, `every`, `on` or `watch`, as in schedules and triggers.
    pub trigger: GoalTrigger,
    /// The only commands its pipe may use.
    pub allow: Vec<String>,
    /// Limits; going past one pauses the goal.
    #[serde(default)]
    pub budget: Budget,
    /// Last day it runs, `YYYY-MM-DD`; renew by moving it.
    pub expires: String,
}

/// When a goal runs.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoalTrigger {
    /// Local time and days, e.g. `07:30 mon-fri`.
    pub at: Option<String>,
    /// Interval, e.g. `1h`.
    pub every: Option<String>,
    /// What to do when the time passed while the device slept.
    #[serde(default)]
    pub missed: Missed,
    /// A condition over facts.
    pub on: Option<String>,
    /// A folder; `~` is the home folder.
    pub watch: Option<String>,
}

/// A goal's limits.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Budget {
    /// Jobs it may start per local day.
    pub jobs_per_day: u32,
    /// How long one run may take.
    #[serde(deserialize_with = "duration")]
    pub max_runtime: Duration,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            jobs_per_day: 50,
            max_runtime: Duration::from_secs(300),
        }
    }
}

/// Where a proactive message goes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Sink {
    /// Connected clients: `assist listen` says it, the dashboard shows it.
    #[default]
    Speak,
    /// A desktop notification.
    Notify,
}

/// `[proactive]`: rules that speak up unasked (PLAN.md section 3.17).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ProactiveConfig {
    /// `"23:00-07:00"`: nothing is said in this local time window.
    pub quiet_hours: Option<String>,
    /// Most messages per hour, all rules together.
    pub max_per_hour: usize,
    /// Where messages go unless a rule says otherwise.
    pub sink: Sink,
    /// The rules.
    pub rules: Vec<ProactiveRule>,
}

impl Default for ProactiveConfig {
    fn default() -> Self {
        Self {
            quiet_hours: None,
            max_per_hour: 4,
            sink: Sink::Speak,
            rules: Vec::new(),
        }
    }
}

/// `[[proactive.rules]]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProactiveRule {
    /// Name, for muting ("stop telling me about build-failed").
    pub name: String,
    /// When: a condition over facts, or over `job.*` when a job ends.
    pub on: String,
    /// What to say; `{command}`, `{error}`, fact names and `{address}` are filled in.
    pub say: String,
    /// Shortest time between two messages of this rule.
    #[serde(default = "ten_minutes", deserialize_with = "duration")]
    pub every: Duration,
    /// Where it goes, if not `[proactive] sink`.
    pub sink: Option<Sink>,
}

fn ten_minutes() -> Duration {
    Duration::from_secs(600)
}

/// `[voice]`: voice input settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct VoiceConfig {
    /// How close (0 to 1) an utterance must be to the owner's voiceprint to count as the
    /// owner. Higher refuses others more surely and the owner more often.
    pub speaker_threshold: f32,
    /// How long requests need no wake word after the last exchange.
    #[serde(deserialize_with = "duration")]
    pub awake: Duration,
}

impl Default for VoiceConfig {
    fn default() -> Self {
        // Phase 2 spike: the owner's 1 s answers scored 0.26 to 0.71, other voices at most
        // 0.12 (PLAN.md section 13).
        Self {
            speaker_threshold: 0.35,
            awake: Duration::from_secs(60),
        }
    }
}

/// `[persona]`: the assistant's name and how it addresses the owner.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct PersonaConfig {
    /// The assistant's name.
    pub name: String,
    /// How the assistant addresses you; empty for not at all.
    pub address: String,
    /// The wake phrase; empty for "hey <name>" or "okay <name>". A longer word such as
    /// "bumblebee" helps when a short name triggers by mistake.
    pub wake_word: String,
}

impl PersonaConfig {
    /// The phrases that wake the assistant.
    pub fn wake_phrases(&self) -> Vec<String> {
        if self.wake_word.trim().is_empty() {
            ["hey", "okay", "ok"]
                .iter()
                .map(|greeting| format!("{greeting} {}", self.name))
                .collect()
        } else {
            vec![self.wake_word.trim().to_owned()]
        }
    }
}

impl Default for PersonaConfig {
    fn default() -> Self {
        Self {
            name: "Bee".to_owned(),
            address: "Prime".to_owned(),
            wake_word: String::new(),
        }
    }
}

/// `[server]`: where `assist serve` listens.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ServerConfig {
    /// A loopback address and port; other interfaces are refused.
    pub bind: SocketAddr,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::from(([127, 0, 0, 1], 7720)),
        }
    }
}

/// `[search]`: the search engine.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SearchConfig {
    /// An `http(s)://` URL with `{query}` where the encoded query goes.
    pub url: String,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            url: "https://duckduckgo.com/?q={query}".to_owned(),
        }
    }
}

/// `[lanes]`: concurrent jobs per lane (PLAN.md section 3.8).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LanesConfig {
    /// Quick, user-facing jobs.
    pub interactive: usize,
    /// Long jobs: builds, scans, scrapes.
    pub background: usize,
}

impl Default for LanesConfig {
    fn default() -> Self {
        Self {
            interactive: 2,
            background: 3,
        }
    }
}

/// `[jobs]`: per-job limits.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct JobsConfig {
    /// A running job is stopped after this long, e.g. `"10m"`.
    #[serde(deserialize_with = "duration")]
    pub timeout: Duration,
    /// Output kept in memory per job for `jobs` listings; older lines are dropped.
    pub output_tail_kb: usize,
    /// Finished jobs and their logs are deleted after this many days.
    pub retention_days: u64,
    /// The oldest job logs are deleted while all logs together exceed this size.
    pub log_cap_mb: u64,
}

impl Default for JobsConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(600),
            output_tail_kb: 64,
            retention_days: 7,
            log_cap_mb: 500,
        }
    }
}

/// Parses `30s`, `10m` or `2h`.
pub fn parse_duration(text: &str) -> Option<Duration> {
    let text = text.trim();
    let (digits, unit) = [('s', 1), ('m', 60), ('h', 3600), ('d', 86_400)]
        .into_iter()
        .find_map(|(suffix, secs)| text.strip_suffix(suffix).map(|d| (d, secs)))?;
    let n: u64 = digits.parse().ok()?;
    Some(Duration::from_secs(n.checked_mul(unit)?))
}

fn duration<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Duration, D::Error> {
    let text = String::deserialize(deserializer)?;
    parse_duration(&text).ok_or_else(|| {
        D::Error::custom(format!("invalid duration {text:?}; use e.g. 30s, 10m, 2h"))
    })
}

/// Most times a saved pipe may be retried.
pub const MAX_RETRY: u32 = 10;

/// A saved pipe (`[pipes]`), e.g.
/// `report = { run = "run build | filter error | save errors.txt", retry = 2 }`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedPipe {
    /// The pipe, as typed in the shell.
    pub run: String,
    /// Runs again up to this many times when a stage fails.
    #[serde(default)]
    pub retry: u32,
    /// A pipe, or another saved pipe's name, to run when the last try failed.
    #[serde(default)]
    pub on_error: Option<String>,
    /// Runs only when this condition over facts holds, e.g. `node.charging`.
    #[serde(default, rename = "if")]
    pub condition: Option<String>,
    /// Schedules and triggers may run it with nobody watching (PLAN.md section 3.4).
    #[serde(default)]
    pub unattended: bool,
}

/// `[[schedules]]`: runs a saved pipe at a time of day or at an interval.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScheduleConfig {
    /// The saved pipe; it must be marked `unattended`.
    pub run: String,
    /// Local time and days, e.g. `07:30 mon-fri`.
    pub at: Option<String>,
    /// Interval, e.g. `1h`.
    pub every: Option<String>,
    /// What to do when the time passed while the device slept.
    #[serde(default)]
    pub missed: Missed,
}

/// `[[triggers]]`: runs a saved pipe when a fact condition turns true (`on`) or something
/// changes in a folder (`watch`).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TriggerConfig {
    /// The saved pipe; it must be marked `unattended`.
    pub run: String,
    /// A condition over facts, e.g. `node.battery < 20`.
    pub on: Option<String>,
    /// A folder; `~` is the home folder.
    pub watch: Option<String>,
}

/// Settings for one command (`[commands.<name>]`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandConfig {
    /// Always ask for confirmation before running.
    #[serde(default)]
    pub risky: bool,
    /// Question to ask per missing slot, e.g. `ask.text = "What's the message?"`.
    #[serde(default)]
    pub ask: BTreeMap<String, String>,
}

/// Why a config could not be loaded.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The file could not be read.
    #[error("cannot read {path}: {source}")]
    Io {
        /// The file that was read.
        path: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },
    /// The TOML is invalid or has unknown or wrongly typed keys.
    #[error("line {line}, column {column}: {message}")]
    Parse {
        /// 1-based line.
        line: usize,
        /// 1-based column.
        column: usize,
        /// The parser's message.
        message: String,
    },
    /// The file was written for another format version.
    #[error("unsupported config_version {found}; this build reads version {expected}")]
    Version {
        /// Version in the file.
        found: u32,
        /// Version this build reads.
        expected: u32,
    },
    /// A `[commands.<name>]` section names a command no phrase file defines.
    #[error("[commands.{0}] does not match any known command")]
    UnknownCommand(String),
    /// A value is well-formed but not allowed.
    #[error("{0}")]
    Invalid(String),
}

fn current_version() -> u32 {
    CURRENT_VERSION
}

impl Config {
    /// Parses and validates config text. An empty text gives an empty, valid config.
    pub fn from_toml(src: &str) -> Result<Self, ConfigError> {
        let config: Config = toml::from_str(src).map_err(|err| {
            let (line, column) = location(src, &err);
            ConfigError::Parse {
                line,
                column,
                message: err.message().to_owned(),
            }
        })?;
        if config.config_version != CURRENT_VERSION {
            return Err(ConfigError::Version {
                found: config.config_version,
                expected: CURRENT_VERSION,
            });
        }
        if config.lanes.interactive == 0 || config.lanes.background == 0 {
            return Err(ConfigError::Invalid(
                "[lanes] values must be at least 1".to_owned(),
            ));
        }
        // Only web URLs: the URL is handed to the OS "open" handler.
        let url = &config.search.url;
        let web = url.starts_with("https://") || url.starts_with("http://");
        if !web || !url.contains("{query}") {
            return Err(ConfigError::Invalid(
                "[search] url must start with https:// or http:// and contain {query}".to_owned(),
            ));
        }
        if let Some((name, _)) = config.pipes.iter().find(|(_, p)| p.retry > MAX_RETRY) {
            return Err(ConfigError::Invalid(format!(
                "[pipes] {name}: retry must be at most {MAX_RETRY}"
            )));
        }
        for (name, pipe) in &config.pipes {
            if let Some(Err(err)) = pipe.condition.as_deref().map(Condition::parse) {
                return Err(ConfigError::Invalid(format!("[pipes] {name}: {err}")));
            }
        }
        config.validate_triggers()?;
        config.validate_proactive()?;
        config.validate_goals()?;
        if let Some(key) = config.replies.keys().find(|k| Line::from_key(k).is_none()) {
            return Err(ConfigError::Invalid(format!(
                "[replies] {key} is not a known reply"
            )));
        }
        if !(0.0..1.0).contains(&config.voice.speaker_threshold) {
            return Err(ConfigError::Invalid(
                "[voice] speaker_threshold must be at least 0 and below 1".to_owned(),
            ));
        }
        if !(Duration::from_secs(5)..=Duration::from_secs(3600)).contains(&config.voice.awake) {
            return Err(ConfigError::Invalid(
                "[voice] awake must be between 5s and 1h".to_owned(),
            ));
        }
        if !config.server.bind.ip().is_loopback() {
            return Err(ConfigError::Invalid(
                "[server] bind must be a loopback address such as 127.0.0.1:7720".to_owned(),
            ));
        }
        Ok(config)
    }

    /// Reads and parses a config file.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let src = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.to_owned(),
            source,
        })?;
        Self::from_toml(&src)
    }

    /// Checks that every `[commands.<name>]` names a command defined in `phrases` (or the
    /// built-in `shell`), so a typo can never silently drop `risky = true`.
    pub fn validate_commands(&self, phrases: &PhraseSet) -> Result<(), ConfigError> {
        let unknown = |name: &&String| !phrases.contains(name) && name.as_str() != SHELL;
        match self.commands.keys().find(unknown) {
            Some(name) => Err(ConfigError::UnknownCommand(name.clone())),
            None => Ok(()),
        }
    }

    /// Schedules and triggers name saved pipes marked `unattended` and have exactly one
    /// valid rule each.
    fn validate_triggers(&self) -> Result<(), ConfigError> {
        let unattended = |section: &str, run: &str| {
            match self.pipes.get(run) {
            Some(pipe) if pipe.unattended => Ok(()),
            Some(_) => Err(ConfigError::Invalid(format!(
                "[[{section}]] {run}: mark the pipe unattended = true to run it with nobody watching"
            ))),
            None => Err(ConfigError::Invalid(format!(
                "[[{section}]] {run}: no saved pipe with that name in [pipes]"
            ))),
        }
        };
        for schedule in &self.schedules {
            unattended("schedules", &schedule.run)?;
            Schedule::parse(schedule.at.as_deref(), schedule.every.as_deref()).map_err(|e| {
                ConfigError::Invalid(format!("[[schedules]] {}: {e}", schedule.run))
            })?;
        }
        for trigger in &self.triggers {
            unattended("triggers", &trigger.run)?;
            let invalid = |reason: String| {
                ConfigError::Invalid(format!("[[triggers]] {}: {reason}", trigger.run))
            };
            match (&trigger.on, &trigger.watch) {
                (Some(on), None) => {
                    Condition::parse(on).map_err(|e| invalid(e.to_string()))?;
                }
                (None, Some(watch)) if !watch.trim().is_empty() => {}
                _ => return Err(invalid("give either on or watch".to_owned())),
            }
        }
        Ok(())
    }

    fn validate_goals(&self) -> Result<(), ConfigError> {
        let mut names = std::collections::BTreeSet::new();
        for goal in &self.autopilot.goals {
            let invalid = |reason: &str| {
                Err(ConfigError::Invalid(format!(
                    "[[autopilot.goals]] {}: {reason}",
                    goal.name
                )))
            };
            let named = !goal.name.is_empty()
                && goal.name.len() <= 64
                && goal
                    .name
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
            if !named {
                return invalid("name: use a-z, 0-9 and -");
            }
            if !names.insert(goal.name.as_str()) {
                return invalid("two goals have this name");
            }
            if !self.pipes.contains_key(&goal.run) {
                return invalid("run must name a saved pipe in [pipes]");
            }
            if goal.allow.is_empty() {
                return invalid("allow lists the commands it may use");
            }
            if jiff::civil::Date::strptime("%Y-%m-%d", &goal.expires).is_err() {
                return invalid("expires: use a date such as 2026-12-31");
            }
            let t = &goal.trigger;
            let rules = [
                t.at.is_some() || t.every.is_some(),
                t.on.is_some(),
                t.watch.is_some(),
            ];
            if rules.iter().filter(|r| **r).count() != 1 {
                return invalid("trigger: give one of at, every, on or watch");
            }
            if t.at.is_some() || t.every.is_some() {
                if let Err(e) = Schedule::parse(t.at.as_deref(), t.every.as_deref()) {
                    return invalid(&e);
                }
            }
            if let Some(Err(e)) = t.on.as_deref().map(Condition::parse) {
                return invalid(&e.to_string());
            }
        }
        Ok(())
    }

    fn validate_proactive(&self) -> Result<(), ConfigError> {
        let proactive = &self.proactive;
        let invalid = |reason: String| Err(ConfigError::Invalid(format!("[proactive] {reason}")));
        if proactive.max_per_hour == 0 {
            return invalid("max_per_hour must be at least 1".to_owned());
        }
        if let Some(quiet) = &proactive.quiet_hours {
            if crate::proactive::parse_quiet(quiet).is_none() {
                return invalid(format!("quiet_hours {quiet:?}: use e.g. 23:00-07:00"));
            }
        }
        let mut names = std::collections::BTreeSet::new();
        for rule in &proactive.rules {
            let ok = !rule.name.is_empty()
                && rule.name.len() <= 64
                && rule.name.bytes().all(|b| {
                    b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b' '
                });
            if !ok {
                return invalid(format!(
                    "rule name {:?}: use a-z, 0-9, spaces and -",
                    rule.name
                ));
            }
            if !names.insert(rule.name.as_str()) {
                return invalid(format!("two rules are named {:?}", rule.name));
            }
            if let Err(err) = Condition::parse(&rule.on) {
                return invalid(format!("{}: {err}", rule.name));
            }
        }
        Ok(())
    }

    /// Adds apps this device found (name, how to launch it) to `[apps]`. Entries from
    /// config.toml win, also over a found name that differs only in case or spacing; see
    /// [`add_found`].
    pub fn add_found_apps(&mut self, apps: impl IntoIterator<Item = (String, String)>) {
        add_found(&mut self.apps, apps, toml::Value::String);
    }

    /// Adds contacts this device found (name, number) to `[contacts]`, like
    /// [`Config::add_found_apps`].
    pub fn add_found_contacts(&mut self, contacts: impl IntoIterator<Item = (String, String)>) {
        add_found(&mut self.contacts, contacts, |number| number);
    }

    /// Value lists that `list:<name>` slots match against: `apps`, `contacts`, `scripts`,
    /// `pipes`.
    pub fn lists(&self) -> BTreeMap<String, Vec<String>> {
        BTreeMap::from([
            ("apps".to_owned(), self.apps.keys().cloned().collect()),
            (
                "contacts".to_owned(),
                self.contacts.keys().cloned().collect(),
            ),
            ("scripts".to_owned(), self.scripts.keys().cloned().collect()),
            ("pipes".to_owned(), self.pipes.keys().cloned().collect()),
            (
                "rules".to_owned(),
                self.proactive
                    .rules
                    .iter()
                    .map(|r| r.name.clone())
                    .collect(),
            ),
        ])
    }
}

/// Most entries a device may add to one list, and the longest name kept.
const MAX_FOUND: usize = 5000;
const MAX_FOUND_NAME: usize = 100;

/// Adds found entries to `list`: names trimmed and bounded, at most [`MAX_FOUND`] taken, and
/// a name that matches one already there (as the matcher compares them) is skipped, so the
/// owner's entry wins and two found spellings of one app do not make "open firefox" ask which.
fn add_found<V>(
    list: &mut BTreeMap<String, V>,
    found: impl IntoIterator<Item = (String, String)>,
    value: impl Fn(String) -> V,
) {
    let mut taken: BTreeSet<String> = list.keys().map(|k| normalize(k)).collect();
    for (name, launch) in found.into_iter().take(MAX_FOUND) {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > MAX_FOUND_NAME {
            continue;
        }
        if taken.insert(normalize(name)) {
            list.insert(name.to_owned(), value(launch));
        }
    }
}

/// A value for [`edit`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingValue {
    /// `true` / `false`.
    Bool(bool),
    /// A whole number.
    Int(i64),
    /// A string, e.g. `"10m"` or a URL.
    Text(String),
}

/// Whether a setting applies at once or only after the daemon restarts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applies {
    /// Read on every request: `commands.<name>.risky`.
    Now,
    /// Read when the scheduler and actions start: lanes, job limits, search URL.
    AfterRestart,
}

/// The keys [`edit`] may change, and when each applies.
pub fn setting_applies(key: &str) -> Option<Applies> {
    let mut parts = key.split('.');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some("commands"), Some(name), Some("risky"), None) if !name.is_empty() => {
            Some(Applies::Now)
        }
        (Some("lanes"), Some("interactive" | "background"), None, None)
        | (
            Some("jobs"),
            Some("timeout" | "output_tail_kb" | "retention_days" | "log_cap_mb"),
            None,
            None,
        )
        | (Some("search"), Some("url"), None, None) => Some(Applies::AfterRestart),
        _ => None,
    }
}

/// Sets one allowed `key` (see [`setting_applies`]) in config text `src`, keeping comments and
/// layout. Returns the new text and the parsed config, which passed every check; nothing is
/// written.
pub fn edit(src: &str, key: &str, value: SettingValue) -> Result<(String, Config), ConfigError> {
    if setting_applies(key).is_none() {
        return Err(ConfigError::Invalid(format!(
            "{key} cannot be changed here"
        )));
    }
    let mut doc: toml_edit::DocumentMut = src.parse().map_err(|err: toml_edit::TomlError| {
        ConfigError::Invalid(format!("config is not valid TOML: {}", err.message()))
    })?;
    let value = match value {
        SettingValue::Bool(b) => toml_edit::value(b),
        SettingValue::Int(n) => toml_edit::value(n),
        SettingValue::Text(text) => toml_edit::value(text),
    };
    let mut parts: Vec<&str> = key.split('.').collect();
    let Some(last) = parts.pop() else {
        return Err(ConfigError::Invalid(format!(
            "{key} cannot be changed here"
        )));
    };
    let mut table = doc.as_table_mut();
    for part in parts {
        let entry = table
            .entry(part)
            .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()));
        table = entry
            .as_table_mut()
            .ok_or_else(|| ConfigError::Invalid(format!("{part} in the config is not a table")))?;
    }
    table.insert(last, value);
    let text = doc.to_string();
    let config = Config::from_toml(&text)?;
    Ok((text, config))
}

/// 1-based line and column of a TOML error in `src`.
pub(crate) fn location(src: &str, err: &toml::de::Error) -> (usize, usize) {
    let offset = err.span().map_or(0, |span| span.start);
    let before = src.get(..offset).unwrap_or(src);
    let line = before.matches('\n').count() + 1;
    let column = before
        .rsplit('\n')
        .next()
        .unwrap_or_default()
        .chars()
        .count()
        + 1;
    (line, column)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn found_apps_never_shadow_config_entries_or_each_other() {
        let mut config = Config::from_toml("[apps]\nfirefox = \"firefox.exe\"\n").unwrap();
        let long = "x".repeat(MAX_FOUND_NAME + 1);
        config.add_found_apps([
            ("Firefox".to_owned(), "shell:AppsFolder\\Firefox".to_owned()),
            (" Google Chrome ".to_owned(), "chrome-id".to_owned()),
            ("google  chrome".to_owned(), "other-id".to_owned()),
            ("  ".to_owned(), "x".to_owned()),
            (long, "x".to_owned()),
        ]);
        let names: Vec<_> = config.apps.keys().map(String::as_str).collect();
        assert_eq!(names, ["Google Chrome", "firefox"]);
        assert_eq!(config.apps["firefox"].as_str(), Some("firefox.exe"));
        assert_eq!(config.apps["Google Chrome"].as_str(), Some("chrome-id"));
    }

    #[test]
    fn wake_phrases_follow_the_name_unless_set() {
        let config = Config::from_toml("[persona]\nname = \"Bee\"\n").unwrap();
        assert_eq!(
            config.persona.wake_phrases(),
            ["hey Bee", "okay Bee", "ok Bee"]
        );
        let config = Config::from_toml("[persona]\nwake_word = \" bumblebee \"\n").unwrap();
        assert_eq!(config.persona.wake_phrases(), ["bumblebee"]);
    }

    #[test]
    fn a_bad_pipe_condition_is_a_config_error() {
        let err =
            Config::from_toml("[pipes]\nx = { run = \"run build\", if = \"node.battery <\" }\n")
                .unwrap_err();
        assert!(err.to_string().starts_with("[pipes] x: condition"), "{err}");
    }

    #[test]
    fn empty_config_is_valid() {
        let config = Config::from_toml("").unwrap();
        assert_eq!(config.config_version, CURRENT_VERSION);
        assert!(config.apps.is_empty());
        assert_eq!(config.lists().len(), 5);
    }

    #[test]
    fn unknown_section_reports_its_line() {
        let err = Config::from_toml("config_version = 1\n\n[contcts]\nmom = \"1\"\n").unwrap_err();
        assert!(matches!(err, ConfigError::Parse { line: 3, .. }), "{err}");
    }

    #[test]
    fn wrong_type_is_an_error() {
        let err = Config::from_toml("[commands.send_sms]\nrisky = \"yes\"\n").unwrap_err();
        assert!(matches!(err, ConfigError::Parse { line: 2, .. }), "{err}");
    }

    #[test]
    fn other_version_is_rejected() {
        let err = Config::from_toml("config_version = 2").unwrap_err();
        assert!(
            matches!(
                err,
                ConfigError::Version {
                    found: 2,
                    expected: 1
                }
            ),
            "{err}"
        );
    }

    #[test]
    fn edit_keeps_comments_and_validates() {
        let src = "# mine\n[lanes]\ninteractive = 2 # quick jobs\n";
        let (text, config) = edit(src, "lanes.interactive", SettingValue::Int(4)).unwrap();
        assert_eq!(config.lanes.interactive, 4);
        assert!(text.contains("# mine"), "{text}");

        let (text, config) =
            edit(&text, "commands.send_sms.risky", SettingValue::Bool(false)).unwrap();
        assert!(!config.commands["send_sms"].risky);
        assert!(text.contains("[commands.send_sms]"), "{text}");

        let (_, config) = edit("", "jobs.timeout", SettingValue::Text("90s".into())).unwrap();
        assert_eq!(config.jobs.timeout, Duration::from_secs(90));

        for (key, value) in [
            ("lanes.interactive", SettingValue::Int(0)),
            ("jobs.timeout", SettingValue::Text("soon".into())),
            ("search.url", SettingValue::Text("file:///x?{query}".into())),
            ("server.bind", SettingValue::Text("0.0.0.0:1".into())),
            ("apps.evil", SettingValue::Text("x".into())),
        ] {
            assert!(edit(src, key, value).is_err(), "{key} must be refused");
        }
        assert_eq!(setting_applies("commands.call.risky"), Some(Applies::Now));
        assert_eq!(
            setting_applies("lanes.background"),
            Some(Applies::AfterRestart)
        );
    }

    #[test]
    fn misspelled_command_is_rejected() {
        let config = Config::from_toml("[commands.sned_sms]\nrisky = true\n").unwrap();
        let phrases = PhraseSet::default_en().unwrap();
        let err = config.validate_commands(&phrases).unwrap_err();
        assert!(matches!(err, ConfigError::UnknownCommand(ref name) if name == "sned_sms"));
    }

    #[test]
    fn job_limits_parse_and_validate() {
        let config =
            Config::from_toml("[lanes]\ninteractive = 1\n[jobs]\ntimeout = \"90s\"\n").unwrap();
        assert_eq!(config.lanes.interactive, 1);
        assert_eq!(config.lanes.background, 3);
        assert_eq!(config.jobs.timeout, Duration::from_secs(90));
        assert!(matches!(
            Config::from_toml("[lanes]\nbackground = 0\n"),
            Err(ConfigError::Invalid(_))
        ));
        assert!(matches!(
            Config::from_toml("[jobs]\ntimeout = \"soon\"\n"),
            Err(ConfigError::Parse { line: 2, .. })
        ));
        assert_eq!(parse_duration("2h"), Some(Duration::from_secs(7200)));
        assert_eq!(parse_duration("10"), None);
    }

    #[test]
    fn server_binds_to_loopback_only() {
        assert_eq!(Config::from_toml("").unwrap().server.bind.port(), 7720);
        assert!(Config::from_toml("[server]\nbind = \"[::1]:8000\"\n").is_ok());
        assert!(matches!(
            Config::from_toml("[server]\nbind = \"0.0.0.0:7720\"\n"),
            Err(ConfigError::Invalid(_))
        ));
    }

    #[test]
    fn search_url_must_be_a_web_url_with_a_query() {
        assert!(Config::from_toml("[search]\nurl = \"https://x.org/?q={query}\"\n").is_ok());
        for bad in [
            "file:///c:/x?{query}",
            "https://x.org/",
            "javascript:{query}",
        ] {
            let src = format!("[search]\nurl = \"{bad}\"\n");
            assert!(
                matches!(Config::from_toml(&src), Err(ConfigError::Invalid(_))),
                "{bad}"
            );
        }
    }

    #[test]
    fn missing_file_is_an_io_error() {
        let err = Config::load(Path::new("definitely/not/here.toml")).unwrap_err();
        assert!(matches!(err, ConfigError::Io { .. }), "{err}");
    }
}
