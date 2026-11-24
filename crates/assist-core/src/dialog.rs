//! Conversation state machine (PLAN.md section 3.5).
//!
//! One [`Dialog`] per conversation. It turns text into [`Plan`]s to run: it asks for missing
//! slots, asks "A or B?" for ambiguous names, reads risky actions back for a "yes", handles
//! follow-ups ("close it"), corrections ("no, I meant dad") and "X and Y". `cancel` works in
//! every state, and an unanswered question expires.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use assist_proto::rpc::Explanation;

use crate::action::{DataKind, PipeShape};
use crate::condition::Condition;
use crate::config::{Config, ConfigError};
use crate::facts::Facts;
use crate::matcher::{BuildError, Intent, Matcher, RuleMatcher, SlotType};
use crate::normalize::{normalize, parse_number};
use crate::phrases::{CommandSpec, PhraseSet};
use crate::policy::{Capability, Decision, Policy, Principal, Request, Trust};
use crate::replies::{Line, Replies};
use crate::scheduler::Submission;

/// The built-in command for `$ ...` input; its slot `command` holds the exact text.
pub const SHELL: &str = "shell";

/// The phrase-only command for "run <saved pipe>"; the dialog expands it into the pipe.
pub const RUN_PIPE: &str = "run_pipe";

/// How long a question waits for its answer.
pub const ANSWER_TIMEOUT: Duration = Duration::from_secs(60);

const CANCEL: [&str; 6] = [
    "cancel",
    "stop",
    "abort",
    "never mind",
    "nevermind",
    "forget it",
];
const YES: [&str; 10] = [
    "yes", "y", "yeah", "yep", "sure", "ok", "okay", "confirm", "do it", "go ahead",
];
const NO: [&str; 4] = ["no", "n", "nope", "don't"];
const PRONOUNS: [&str; 5] = ["it", "that", "him", "her", "them"];
const CORRECTIONS: [&str; 6] = [
    "no i meant ",
    "no i mean ",
    "i meant ",
    "i mean ",
    "no it's ",
    "no ",
];
const SEPARATORS: [&str; 2] = ["and", "then"];
const ORDINALS: [&str; 5] = ["first", "second", "third", "fourth", "fifth"];

/// What a command needs from the policy; built from the action manifests.
#[derive(Debug, Clone)]
pub struct CommandInfo {
    /// Capabilities the command's action declares.
    pub capabilities: Vec<Capability>,
    /// The action only runs what the owner wrote in config (named scripts), so it counts
    /// as typed for the policy.
    pub owner_defined: bool,
    /// How it fits in a pipe.
    pub pipe: PipeShape,
}

/// Why an [`Engine`] could not be built.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// Config refers to unknown commands.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// Phrases do not compile.
    #[error(transparent)]
    Build(#[from] BuildError),
}

/// Everything conversations share: config, matcher, policy and the available commands.
#[derive(Debug)]
pub struct Engine {
    config: Arc<Config>,
    phrases: PhraseSet,
    matcher: RuleMatcher,
    policy: Policy,
    commands: BTreeMap<String, CommandInfo>,
    /// Command -> slot -> question.
    asks: BTreeMap<String, BTreeMap<String, String>>,
    replies: Replies,
    offer_shell: bool,
    /// Current device state, for saved pipes' `if`.
    facts: Facts,
}

impl Engine {
    /// Builds the engine. `commands` are the commands this device can run; `offer_shell`
    /// offers unmatched input as a shell command (desktop).
    pub fn new(
        config: Arc<Config>,
        phrases: &PhraseSet,
        commands: BTreeMap<String, CommandInfo>,
        offer_shell: bool,
        facts: Facts,
    ) -> Result<Self, EngineError> {
        config.validate_commands(phrases)?;
        let matcher = RuleMatcher::new(phrases, &config)?;
        let policy = Policy::new(&config.policy);
        let mut asks: BTreeMap<String, BTreeMap<String, String>> = phrases
            .commands()
            .map(|(name, spec)| (name.to_owned(), spec.ask.clone()))
            .collect();
        for (name, command) in &config.commands {
            asks.entry(name.clone())
                .or_default()
                .extend(command.ask.clone());
        }
        let engine = Self {
            replies: Replies::new(&config),
            config,
            phrases: phrases.clone(),
            matcher,
            policy,
            commands,
            asks,
            offer_shell,
            facts,
        };
        // A saved pipe that cannot work is a config error now, not when its trigger fires.
        // A stage this device lacks is not: the same config serves devices with other
        // actions, and running it says so.
        for name in engine.config.pipes.keys() {
            if let Err((line, text)) = engine.saved_pipe(name) {
                if line == Line::Unavailable {
                    continue;
                }
                let reason = engine.say(line, &[("text", &text)]);
                return Err(ConfigError::Invalid(format!("[pipes] {name}: {reason}")).into());
            }
        }
        Ok(engine)
    }

    /// The same engine with other phrases, e.g. after a phrase was added.
    pub fn with_phrases(&self, phrases: &PhraseSet) -> Result<Self, EngineError> {
        Self::new(
            Arc::clone(&self.config),
            phrases,
            self.commands.clone(),
            self.offer_shell,
            self.facts.clone(),
        )
    }

    /// The same engine with another config, e.g. after a setting changed. Fails if the
    /// config names unknown commands.
    pub fn with_config(&self, config: Arc<Config>) -> Result<Self, EngineError> {
        Self::new(
            config,
            &self.phrases,
            self.commands.clone(),
            self.offer_shell,
            self.facts.clone(),
        )
    }

    /// The config the engine was built from.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// The phrases the engine was built from.
    pub fn phrases(&self) -> &PhraseSet {
        &self.phrases
    }

    /// Whether this device can run `command`.
    pub fn available(&self, command: &str) -> bool {
        self.commands.contains_key(command)
    }

    /// The command an unmatched sentence most likely meant; see [`RuleMatcher::guess`].
    pub fn guess(&self, text: &str) -> Option<String> {
        self.matcher.guess(text)
    }

    /// What `text` would do for `principal`, without asking or running anything; `None` if
    /// it matches no command.
    pub fn explain(&self, principal: &Principal, text: &str) -> Option<Explanation> {
        let text = text.trim();
        let (plan, ambiguous) = match text.strip_prefix('$') {
            Some(command) => (Plan::shell(command.trim(), true), BTreeMap::new()),
            None => {
                let intent = self.matcher.best(text)?;
                let plan = Plan {
                    command: intent.command,
                    slots: intent.slots,
                    typed: false,
                };
                (plan, intent.ambiguous)
            }
        };
        let saved = (plan.command == RUN_PIPE)
            .then(|| {
                plan.slots
                    .get("pipe")
                    .map(|name| self.saved_pipe(name).ok())
            })
            .flatten()
            .flatten();
        let decision = match &saved {
            Some(pipe) => self.pipe_decision(principal, pipe),
            None => self.decision(principal, &plan),
        };
        let decision = decision.map_or("unavailable", Decision::as_str);
        Some(Explanation {
            readback: match &saved {
                Some(pipe) => self.readback_pipe(pipe),
                None => self.readback(&plan),
            },
            command: plan.command,
            slots: plan.slots,
            ambiguous,
            decision: decision.to_owned(),
        })
    }

    /// The fully resolved action as one line, for confirmation and logs.
    pub fn readback(&self, plan: &Plan) -> String {
        if plan.command == SHELL {
            let command = plan.slots.get("command").map_or("", String::as_str);
            return format!("$ {command}");
        }
        let slots: Vec<String> = plan
            .slots
            .iter()
            .map(|(slot, value)| format!("{slot} = {}", self.shown(&plan.command, slot, value)))
            .collect();
        if slots.is_empty() {
            plan.command.clone()
        } else {
            format!("{}: {}", plan.command, slots.join(", "))
        }
    }

    /// A pipe read back stage by stage, `run_script: script = build | count`, with what
    /// runs when it fails.
    pub fn readback_pipe(&self, pipe: &Pipe) -> String {
        let stages = |plans: &[Plan]| {
            let stages: Vec<String> = plans.iter().map(|plan| self.readback(plan)).collect();
            stages.join(" | ")
        };
        let mut text = stages(&pipe.stages);
        if pipe.retry > 0 {
            text.push_str(&format!(", retry {}", pipe.retry));
        }
        if !pipe.on_error.is_empty() {
            text.push_str(&format!(", on error {}", stages(&pipe.on_error)));
        }
        text
    }

    /// The pipe to submit, each stage with its title.
    pub fn submission(&self, pipe: Pipe) -> Submission {
        let titled = |plans: Vec<Plan>| -> Vec<(Plan, String)> {
            plans
                .into_iter()
                .map(|plan| {
                    let title = self.readback(&plan);
                    (plan, title)
                })
                .collect()
        };
        Submission {
            stages: titled(pipe.stages),
            retry: pipe.retry,
            on_error: titled(pipe.on_error),
        }
    }

    /// `a | b | c` as plans: every stage resolves, and each fits the output of the one
    /// before (PLAN.md section 3.11). The error is the reply line and the stage it is about.
    fn parse_pipe(&self, text: &str) -> Result<Vec<Plan>, (Line, String)> {
        let mut pipe: Vec<Plan> = Vec::new();
        for text in text.split('|').map(str::trim) {
            let Some(plan) = self.stage(text) else {
                return Err((Line::BadStage, text.to_owned()));
            };
            let shape = self.commands.get(&plan.command).map(|info| info.pipe);
            let problem = match (pipe.last(), shape) {
                (_, None) => Some(Line::Unavailable),
                (None, Some(shape)) if shape.is_transform() => Some(Line::PipeStart),
                (Some(_), Some(shape)) if !shape.is_transform() => Some(Line::NoInput),
                (Some(before), Some(_)) => {
                    let output = self.commands.get(&before.command).map(|i| i.pipe.output);
                    (output != Some(DataKind::Lines)).then_some(Line::NoOutput)
                }
                (None, Some(_)) => None,
            };
            if let Some(line) = problem {
                return Err((line, text.to_owned()));
            }
            pipe.push(plan);
        }
        Ok(pipe)
    }

    /// A saved pipe for a schedule or trigger, which nobody watches (PLAN.md section 3.4): it
    /// must be marked `unattended` and run without any confirmation. `Ok(None)` when its
    /// `if` does not hold; the error says why it may not run.
    pub fn unattended(&self, name: &str, principal: &Principal) -> Result<Option<Pipe>, String> {
        if !self.config.pipes.get(name).is_some_and(|p| p.unattended) {
            return Err(format!("{name} is not marked unattended"));
        }
        if self.unmet_condition(name).is_some() {
            return Ok(None);
        }
        let pipe = self
            .saved_pipe(name)
            .map_err(|(line, text)| self.say(line, &[("text", &text)]))?;
        match self.pipe_decision(principal, &pipe) {
            Some(Decision::Allow) => Ok(Some(pipe)),
            Some(Decision::Confirm) => Err(format!(
                "{name} needs a confirmation, which nobody can give unattended"
            )),
            Some(Decision::Deny) => Err(format!("{name} is not allowed")),
            None => Err(format!("{name} uses a command this device does not have")),
        }
    }

    /// A saved pipe's `if` when the current facts do not meet it.
    fn unmet_condition(&self, name: &str) -> Option<String> {
        let text = self.config.pipes.get(name)?.condition.as_deref()?;
        // Config loading has parsed every condition, so a failure here cannot happen.
        let met = Condition::parse(text).is_ok_and(|c| c.eval(&|fact| self.facts.get(fact)));
        (!met).then(|| text.to_owned())
    }

    /// A saved pipe from `[pipes]`; its `on_error` is a pipe or another saved pipe's name.
    fn saved_pipe(&self, name: &str) -> Result<Pipe, (Line, String)> {
        let saved = self
            .config
            .pipes
            .get(name)
            .ok_or_else(|| (Line::BadStage, name.to_owned()))?;
        let on_error = match saved.on_error.as_deref() {
            Some(text) => {
                let text = self.config.pipes.get(text).map_or(text, |p| p.run.as_str());
                self.parse_pipe(text)?
            }
            None => Vec::new(),
        };
        Ok(Pipe {
            stages: self.parse_pipe(&saved.run)?,
            retry: saved.retry,
            on_error,
        })
    }

    /// One pipe stage as a plan: a transform written `<name> <arg>`, or any command the
    /// matcher fully resolves (no question to ask, no choice to make).
    fn stage(&self, text: &str) -> Option<Plan> {
        let (word, rest) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
        let name = word.to_lowercase();
        if let Some(info) = self.commands.get(&name).filter(|i| i.pipe.is_transform()) {
            let rest = rest.trim();
            let slots = info
                .pipe
                .arg
                .filter(|_| !rest.is_empty())
                .map(|slot| (slot.to_owned(), rest.to_owned()))
                .into_iter()
                .collect();
            return Some(Plan {
                command: name,
                slots,
                typed: false,
            });
        }
        let intent = self.matcher.best(text)?;
        if !intent.ambiguous.is_empty() || self.missing(&intent).is_some() {
            return None;
        }
        Some(Plan {
            command: intent.command,
            slots: intent.slots,
            typed: false,
        })
    }

    /// The strictest decision over every stage that may run, the `on_error` pipe included;
    /// `None` if any stage is unavailable. Taint rule: once a stage's output is untrusted,
    /// a later stage that sends always asks (PLAN.md section 3.6).
    fn pipe_decision(&self, principal: &Principal, pipe: &Pipe) -> Option<Decision> {
        let mut worst = Decision::Allow;
        for plans in [&pipe.stages, &pipe.on_error] {
            let mut tainted = false;
            for plan in plans {
                let mut decision = self.decision(principal, plan)?;
                let info = self.commands.get(&plan.command)?;
                if tainted && info.capabilities.contains(&Capability::Send) {
                    decision = decision.max(Decision::Confirm);
                }
                tainted |= info.pipe.untrusted;
                worst = worst.max(decision);
            }
        }
        Some(worst)
    }

    /// A slot value as a confirmation shows it: text quoted, a contact with its number, so
    /// the owner sees exactly what runs.
    fn shown(&self, command: &str, slot: &str, value: &str) -> String {
        match self.matcher.slots(command).and_then(|t| t.get(slot)) {
            Some(SlotType::Text) => format!("{value:?}"),
            Some(SlotType::List(list)) if list == "contacts" => {
                match self.config.contacts.get(value) {
                    Some(number) => format!("{value} ({number})"),
                    None => value.to_owned(),
                }
            }
            _ => value.to_owned(),
        }
    }

    fn say(&self, line: Line, vars: &[(&str, &str)]) -> String {
        self.replies.line(line, vars)
    }

    /// The confirmation question for `pipe`: a single command's own `confirm` line with
    /// shown slot values, else the readback.
    fn confirm_question(&self, pipe: &Pipe) -> String {
        let own = match (pipe.stages.as_slice(), pipe.on_error.is_empty()) {
            ([plan], true) => self.command_line(plan, |spec| spec.confirm.as_deref(), true),
            _ => None,
        };
        own.unwrap_or_else(|| self.say(Line::Confirm, &[("action", &self.readback_pipe(pipe))]))
    }

    /// What is said when `plan` starts: the command's own `reply` line, else a short
    /// acknowledgement. Shell commands say nothing; their output speaks for them.
    fn started(&self, plan: &Plan) -> Option<String> {
        if plan.command == SHELL {
            return None;
        }
        self.command_line(plan, |spec| spec.reply.as_deref(), false)
            .or_else(|| Some(self.say(Line::Started, &[])))
    }

    fn command_line(
        &self,
        plan: &Plan,
        template: impl Fn(&CommandSpec) -> Option<&str>,
        shown: bool,
    ) -> Option<String> {
        let template = template(self.phrases.spec(&plan.command)?)?;
        let values: Vec<(&str, String)> = plan
            .slots
            .iter()
            .map(|(slot, value)| {
                let value = if shown {
                    self.shown(&plan.command, slot, value)
                } else {
                    value.clone()
                };
                (slot.as_str(), value)
            })
            .collect();
        let vars: Vec<(&str, &str)> = values.iter().map(|(k, v)| (*k, v.as_str())).collect();
        Some(self.replies.fill(template, &vars))
    }

    fn choice_question(&self, candidates: &[String]) -> String {
        self.say(Line::DidYouMean, &[("options", &options(candidates))])
    }

    fn decision(&self, principal: &Principal, plan: &Plan) -> Option<Decision> {
        let info = self.commands.get(&plan.command)?;
        let risky = self
            .config
            .commands
            .get(&plan.command)
            .is_some_and(|c| c.risky);
        Some(self.policy.decide(&Request {
            principal,
            capabilities: &info.capabilities,
            risky,
            typed: plan.typed || info.owner_defined,
        }))
    }

    /// The first missing slot that has a question, and the question.
    fn missing(&self, intent: &Intent) -> Option<(String, String)> {
        self.asks
            .get(&intent.command)?
            .iter()
            .find(|(slot, _)| !intent.slots.contains_key(*slot))
            .map(|(slot, question)| (slot.clone(), question.clone()))
    }

    fn slot_type(&self, command: &str, slot: &str) -> SlotType {
        self.matcher
            .slots(command)
            .and_then(|slots| slots.get(slot))
            .cloned()
            .unwrap_or(SlotType::Text)
    }
}

/// An action ready to be scheduled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Command name.
    pub command: String,
    /// Resolved slots; frozen once confirmed.
    pub slots: BTreeMap<String, String>,
    /// The user typed the exact command (`$ ls`).
    pub typed: bool,
}

/// What a reply runs: a pipe's stages (one for a plain command) and, for a saved pipe, how
/// to handle failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pipe {
    /// Stages in order; each one's lines feed the next.
    pub stages: Vec<Plan>,
    /// Runs again up to this many times when a stage fails.
    pub retry: u32,
    /// Runs when the last try failed; empty for nothing.
    pub on_error: Vec<Plan>,
}

impl From<Plan> for Pipe {
    fn from(plan: Plan) -> Self {
        Self {
            stages: vec![plan],
            retry: 0,
            on_error: Vec::new(),
        }
    }
}

impl Plan {
    fn shell(command: &str, typed: bool) -> Self {
        Self {
            command: SHELL.to_owned(),
            slots: BTreeMap::from([("command".to_owned(), command.to_owned())]),
            typed,
        }
    }
}

/// What the dialog wants done after one input.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Reply {
    /// Lines to show or speak, in order.
    pub say: Vec<String>,
    /// What to schedule now, in order: each entry is one plan, or the stages of a pipe whose
    /// lines flow from one stage to the next.
    pub run: Vec<Pipe>,
    /// The input, normalized, when it matched no command; callers record it so it can
    /// become a phrase.
    pub missed: Option<String>,
}

/// The current engine. Replaced when phrases change; each dialog picks up the new one on
/// its next input.
#[derive(Debug)]
pub struct LiveEngine(std::sync::RwLock<Arc<Engine>>);

impl LiveEngine {
    /// Starts with `engine`.
    pub fn new(engine: Arc<Engine>) -> Self {
        Self(std::sync::RwLock::new(engine))
    }

    /// The current engine.
    pub fn get(&self) -> Arc<Engine> {
        Arc::clone(
            &self
                .0
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    /// Replaces the engine.
    pub fn set(&self, engine: Arc<Engine>) {
        *self
            .0
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = engine;
    }
}

#[derive(Debug)]
enum State {
    Idle,
    Asking {
        intent: Intent,
        slot: String,
    },
    Choosing {
        intent: Intent,
        slot: String,
        candidates: Vec<String>,
    },
    Confirming {
        pipe: Pipe,
        intent: Option<Intent>,
    },
}

/// One conversation.
#[derive(Debug)]
pub struct Dialog {
    engine: Arc<Engine>,
    owner: Principal,
    state: State,
    queue: VecDeque<Intent>,
    last: Option<Intent>,
    deadline: Option<Instant>,
    /// The current input is vouched for by the owner (see [`Dialog::handle_vouched`]).
    vouched: bool,
}

impl Dialog {
    /// A new conversation for `owner`; only the owner's input is accepted.
    pub fn new(engine: Arc<Engine>, owner: Principal) -> Self {
        Self {
            engine,
            owner,
            state: State::Idle,
            queue: VecDeque::new(),
            last: None,
            deadline: None,
            vouched: false,
        }
    }

    /// Whether the dialog is waiting for an answer.
    pub fn is_waiting(&self) -> bool {
        !matches!(self.state, State::Idle)
    }

    /// Switches to `engine` (see [`LiveEngine`]) unless it is already the one in use.
    pub fn use_engine(&mut self, engine: Arc<Engine>) {
        if !Arc::ptr_eq(&self.engine, &engine) {
            self.engine = engine;
        }
    }

    /// Handles one input at time `now`.
    pub fn handle(&mut self, principal: &Principal, input: &str, now: Instant) -> Reply {
        self.handle_vouched(principal, input, false, now)
    }

    /// Like [`Dialog::handle`]. `vouched` says the owner stands behind this input: a voice
    /// that matched the owner's voiceprint, or an answer from a local user on the dashboard.
    /// A voice conversation's "yes" counts only when vouched (PLAN.md section 3.4).
    pub fn handle_vouched(
        &mut self,
        principal: &Principal,
        input: &str,
        vouched: bool,
        now: Instant,
    ) -> Reply {
        self.vouched = vouched;
        let mut reply = Reply::default();
        let raw = input.trim();
        if raw.is_empty() {
            return reply;
        }
        if principal != &self.owner {
            let line = self
                .engine
                .say(Line::OtherOwner, &[("source", &self.owner.source)]);
            reply.say.push(line);
            return reply;
        }
        if self.is_waiting() && self.deadline.is_some_and(|deadline| now > deadline) {
            self.reset();
            reply.say.push(self.engine.say(Line::Expired, &[]));
        }
        let norm = normalize(raw);
        if CANCEL.contains(&norm.as_str()) {
            let had_work = self.is_waiting() || !self.queue.is_empty();
            self.reset();
            let line = if had_work {
                Line::Cancelled
            } else {
                Line::NothingToCancel
            };
            reply.say.push(self.engine.say(line, &[]));
            return reply;
        }
        match std::mem::replace(&mut self.state, State::Idle) {
            State::Idle => self.fresh(raw, &norm, &mut reply),
            State::Asking { intent, slot } => self.answer(intent, slot, raw, &mut reply),
            State::Choosing {
                intent,
                slot,
                candidates,
            } => self.choose(intent, slot, candidates, &norm, &mut reply),
            State::Confirming { pipe, intent } => self.confirm(pipe, intent, &norm, &mut reply),
        }
        self.deadline = self.is_waiting().then(|| now + ANSWER_TIMEOUT);
        reply
    }

    fn reset(&mut self) {
        self.state = State::Idle;
        self.queue.clear();
        self.deadline = None;
    }

    fn fresh(&mut self, raw: &str, norm: &str, reply: &mut Reply) {
        if let Some(command) = raw.strip_prefix('$') {
            let command = command.trim();
            if command.is_empty() {
                reply.say.push(self.engine.say(Line::EmptyShell, &[]));
            } else {
                self.decide(Plan::shell(command, true).into(), None, reply);
            }
            return;
        }
        if raw.contains('|') {
            return self.pipe(raw, reply);
        }
        if let (Some(value), Some(last)) = (correction(norm), self.last.clone()) {
            return self.correct(last, value, reply);
        }
        let intents = self.split(norm);
        if !intents.is_empty() {
            self.queue.extend(intents);
            return self.next(reply);
        }
        if let Some(intent) = self.follow_up(norm) {
            self.queue.push_back(intent);
            return self.next(reply);
        }
        self.unmatched(raw, reply);
    }

    /// The whole input as one intent, or "X and Y" when every part matches.
    fn split(&self, norm: &str) -> Vec<Intent> {
        let matcher = &self.engine.matcher;
        if let Some(intent) = matcher.best(norm) {
            return vec![intent];
        }
        let mut parts: Vec<Vec<&str>> = vec![Vec::new()];
        for word in norm.split(' ') {
            if SEPARATORS.contains(&word) {
                parts.push(Vec::new());
            } else if let Some(part) = parts.last_mut() {
                part.push(word);
            }
        }
        parts.retain(|part| !part.is_empty());
        if parts.len() < 2 {
            return Vec::new();
        }
        parts
            .iter()
            .map(|part| matcher.best(&part.join(" ")))
            .collect::<Option<Vec<_>>>()
            .unwrap_or_default()
    }

    /// "close it" after "open firefox": a pronoun replaced by a value of the last command.
    fn follow_up(&self, norm: &str) -> Option<Intent> {
        let last = self.last.as_ref()?;
        let words: Vec<&str> = norm.split(' ').collect();
        let at = words.iter().position(|w| PRONOUNS.contains(w))?;
        last.slots.values().find_map(|value| {
            let mut words = words.clone();
            words[at] = value.as_str();
            self.engine.matcher.best(&words.join(" "))
        })
    }

    fn unmatched(&mut self, raw: &str, reply: &mut Reply) {
        reply.missed = Some(normalize(raw));
        if self.engine.offer_shell {
            let plan = Plan::shell(raw, false);
            let decision = self.engine.decision(&self.owner, &plan);
            if decision.is_some_and(|d| d != Decision::Deny) {
                reply
                    .say
                    .push(self.engine.say(Line::OfferShell, &[("text", raw)]));
                self.state = State::Confirming {
                    pipe: plan.into(),
                    intent: None,
                };
                return;
            }
        }
        reply.say.push(self.engine.say(Line::NotUnderstood, &[]));
    }

    fn next(&mut self, reply: &mut Reply) {
        while let Some(intent) = self.queue.pop_front() {
            self.advance(intent, reply);
            if self.is_waiting() {
                return;
            }
        }
    }

    fn advance(&mut self, mut intent: Intent, reply: &mut Reply) {
        let saved = intent.command == RUN_PIPE;
        if !saved && !self.engine.commands.contains_key(&intent.command) {
            reply.say.push(self.engine.say(Line::Unavailable, &[]));
            return;
        }
        if let Some((slot, candidates)) = intent.ambiguous.pop_first() {
            reply.say.push(self.engine.choice_question(&candidates));
            self.state = State::Choosing {
                intent,
                slot,
                candidates,
            };
            return;
        }
        if let Some((slot, question)) = self.engine.missing(&intent) {
            reply.say.push(question);
            self.state = State::Asking { intent, slot };
            return;
        }
        if saved {
            let name = intent.slots.get("pipe").cloned().unwrap_or_default();
            if let Some(condition) = self.engine.unmet_condition(&name) {
                let vars = [("text", name.as_str()), ("condition", condition.as_str())];
                return reply.say.push(self.engine.say(Line::Skipped, &vars));
            }
            return match self.engine.saved_pipe(&name) {
                Ok(pipe) => self.decide(pipe, None, reply),
                Err((line, text)) => reply.say.push(self.engine.say(line, &[("text", &text)])),
            };
        }
        let plan = Plan {
            command: intent.command.clone(),
            slots: intent.slots.clone(),
            typed: false,
        };
        self.decide(plan.into(), Some(intent), reply);
    }

    /// `a | b | c`, decided as one.
    fn pipe(&mut self, raw: &str, reply: &mut Reply) {
        match self.engine.parse_pipe(raw) {
            Ok(stages) => {
                let pipe = Pipe {
                    stages,
                    retry: 0,
                    on_error: Vec::new(),
                };
                self.decide(pipe, None, reply);
            }
            Err((line, text)) => reply.say.push(self.engine.say(line, &[("text", &text)])),
        }
    }

    fn decide(&mut self, pipe: Pipe, intent: Option<Intent>, reply: &mut Reply) {
        match self.engine.pipe_decision(&self.owner, &pipe) {
            None => reply.say.push(self.engine.say(Line::Unavailable, &[])),
            Some(Decision::Deny) => {
                let line = self
                    .engine
                    .say(Line::NotAllowed, &[("source", &self.owner.source)]);
                reply.say.push(line);
            }
            Some(Decision::Confirm) => {
                reply.say.push(self.engine.confirm_question(&pipe));
                self.state = State::Confirming { pipe, intent };
            }
            Some(Decision::Allow) => self.submit(pipe, intent, reply),
        }
    }

    fn submit(&mut self, pipe: Pipe, intent: Option<Intent>, reply: &mut Reply) {
        if intent.is_some() {
            self.last = intent;
        }
        if let Some(first) = pipe.stages.first() {
            reply.say.extend(self.engine.started(first));
        }
        reply.run.push(pipe);
    }

    fn answer(&mut self, mut intent: Intent, slot: String, raw: &str, reply: &mut Reply) {
        match self.engine.slot_type(&intent.command, &slot) {
            SlotType::Text => {
                intent.slots.insert(slot, raw.to_owned());
            }
            SlotType::Number => match parse_number(&normalize(raw)) {
                Some(n) => {
                    intent.slots.insert(slot, n.to_string());
                }
                None => {
                    reply.say.push(self.engine.say(Line::SayNumber, &[]));
                    self.state = State::Asking { intent, slot };
                    return;
                }
            },
            SlotType::List(list) => {
                let candidates = self.engine.matcher.candidates(&list, &normalize(raw));
                match candidates.as_slice() {
                    [] => {
                        let line = self.engine.say(Line::UnknownChoice, &[("text", raw)]);
                        reply.say.push(line);
                        self.state = State::Asking { intent, slot };
                        return;
                    }
                    [one] => {
                        intent.slots.insert(slot, one.clone());
                    }
                    [first, ..] => {
                        intent.slots.insert(slot.clone(), first.clone());
                        intent.ambiguous.insert(slot, candidates);
                    }
                }
            }
        }
        self.queue.push_front(intent);
        self.next(reply);
    }

    fn choose(
        &mut self,
        mut intent: Intent,
        slot: String,
        candidates: Vec<String>,
        norm: &str,
        reply: &mut Reply,
    ) {
        let words: Vec<&str> = norm.split(' ').collect();
        let by_position = words.iter().find_map(|w| {
            let index = match ORDINALS.iter().position(|o| o == w) {
                Some(i) => i,
                None => usize::try_from(parse_number(w)?).ok()?.checked_sub(1)?,
            };
            candidates.get(index)
        });
        let by_name: Vec<&String> = candidates
            .iter()
            .filter(|c| {
                let name = normalize(c);
                words.iter().all(|w| name.split(' ').any(|n| n == *w))
            })
            .collect();
        let pick = by_position.or(match by_name.as_slice() {
            [one] => Some(*one),
            _ => None,
        });
        match pick {
            Some(value) => {
                intent.slots.insert(slot, value.clone());
                self.queue.push_front(intent);
                self.next(reply);
            }
            None => {
                reply.say.push(self.engine.choice_question(&candidates));
                self.state = State::Choosing {
                    intent,
                    slot,
                    candidates,
                };
            }
        }
    }

    fn confirm(&mut self, pipe: Pipe, intent: Option<Intent>, norm: &str, reply: &mut Reply) {
        if YES.contains(&norm) {
            if self.owner.trust == Trust::Voice && !self.vouched {
                reply.say.push(self.engine.say(Line::UnknownVoice, &[]));
                self.state = State::Confirming { pipe, intent };
                return;
            }
            self.submit(pipe, intent, reply);
            return self.next(reply);
        }
        if let (Some(value), Some(intent)) = (correction(norm), intent.clone()) {
            return self.correct(intent, value, reply);
        }
        if NO.contains(&norm) {
            self.queue.clear();
            reply.say.push(self.engine.say(Line::Declined, &[]));
            return;
        }
        reply.say.push(self.engine.say(Line::YesOrNo, &[]));
        self.state = State::Confirming { pipe, intent };
    }

    /// Puts `value` into the slot it fits (a list that knows it, else the only slot) and
    /// goes through slots and policy again.
    fn correct(&mut self, mut intent: Intent, value: String, reply: &mut Reply) {
        let engine = Arc::clone(&self.engine);
        let declared = engine.matcher.slots(&intent.command);
        let fits_list = declared.and_then(|slots| {
            slots.iter().find_map(|(slot, ty)| match ty {
                SlotType::List(list) if !engine.matcher.candidates(list, &value).is_empty() => {
                    Some(slot.clone())
                }
                _ => None,
            })
        });
        let only_slot = match declared.map(|slots| slots.keys().collect::<Vec<_>>()) {
            Some(keys) if keys.len() == 1 => keys.first().map(|k| (*k).clone()),
            _ => None,
        };
        let Some(slot) = fits_list.or(only_slot) else {
            let line = engine.say(Line::UnclearCorrection, &[("text", &value)]);
            reply.say.push(line);
            return;
        };
        intent.slots.remove(&slot);
        intent.ambiguous.remove(&slot);
        self.answer(intent, slot, &value, reply);
    }
}

fn correction(norm: &str) -> Option<String> {
    CORRECTIONS
        .iter()
        .find_map(|prefix| norm.strip_prefix(prefix))
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// "a", "a or b", "a, b or c".
fn options(candidates: &[String]) -> String {
    match candidates.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} or {last}", rest.join(", ")),
        _ => candidates.join(""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::Capability as C;

    const CONFIG: &str = r#"
[apps]
firefox = {}
code = {}

[scripts]
build = {}

[contacts]
mom = "+910000000000"
dad = "+910000000001"
"sam lee" = "+910000000002"
"sam roy" = "+910000000003"

[commands.send_sms]
risky = true

[pipes]
report = { run = "run build | head 3 | count", retry = 1, on_error = "alert" }
alert = { run = "run build | save errors.txt" }
"#;

    fn commands() -> BTreeMap<String, CommandInfo> {
        let info = |caps: &[Capability], owner_defined| CommandInfo {
            capabilities: caps.to_vec(),
            owner_defined,
            pipe: PipeShape::SOURCE,
        };
        let shaped = |caps: &[Capability], pipe| CommandInfo {
            pipe,
            ..info(caps, false)
        };
        let lines = DataKind::Lines;
        BTreeMap::from([
            ("open_app".to_owned(), shaped(&[C::Apps], PipeShape::NONE)),
            ("close_app".to_owned(), shaped(&[C::Apps], PipeShape::NONE)),
            (
                "head".to_owned(),
                shaped(&[], PipeShape::transform(lines, Some("count"))),
            ),
            (
                "count".to_owned(),
                shaped(&[], PipeShape::transform(lines, None)),
            ),
            (
                "save".to_owned(),
                shaped(
                    &[C::FsWrite],
                    PipeShape::transform(DataKind::Empty, Some("path")),
                ),
            ),
            ("run_script".to_owned(), info(&[C::Shell], true)),
            ("search".to_owned(), info(&[C::Read], false)),
            ("send_sms".to_owned(), info(&[C::Send], false)),
            ("call".to_owned(), info(&[C::Send], false)),
            ("set_volume".to_owned(), info(&[C::Media], false)),
            (SHELL.to_owned(), info(&[C::Shell], false)),
        ])
    }

    fn engine(offer_shell: bool, commands: BTreeMap<String, CommandInfo>) -> Arc<Engine> {
        let config = Arc::new(Config::from_toml(CONFIG).unwrap());
        let phrases = PhraseSet::default_en().unwrap();
        Arc::new(Engine::new(config, &phrases, commands, offer_shell, Facts::default()).unwrap())
    }

    /// Runs `lines` through one dialog: `>` input, `<` reply, `!` scheduled plan.
    fn transcript(engine: Arc<Engine>, lines: &[&str]) -> String {
        let owner = Principal::local("cli");
        let mut dialog = Dialog::new(Arc::clone(&engine), owner.clone());
        let now = Instant::now();
        let mut out = String::new();
        for line in lines {
            out.push_str(&format!("> {line}\n"));
            let reply = dialog.handle(&owner, line, now);
            for say in reply.say {
                out.push_str(&format!("< {say}\n"));
            }
            for pipe in reply.run {
                out.push_str(&format!("! {}\n", engine.readback_pipe(&pipe)));
            }
        }
        out
    }

    #[test]
    fn transcripts() {
        let full = engine(true, commands());
        insta::assert_snapshot!(
            "basics",
            transcript(
                Arc::clone(&full),
                &[
                    "open firefox",
                    "close it",
                    "$ ls -la",
                    "run build",
                    "open firefox and search rust tokio",
                ]
            )
        );
        insta::assert_snapshot!(
            "pipes",
            transcript(
                Arc::clone(&full),
                &[
                    "run report",
                    "yes",
                    "run build | head 3 | count",
                    "Run build | save Out File.txt",
                    "yes",
                    "head 3 | count",
                    "open firefox | count",
                    "run build | open firefox",
                    "run build | fly away",
                    "$ ls | grep src",
                ]
            )
        );
        insta::assert_snapshot!(
            "ask_and_confirm",
            transcript(Arc::clone(&full), &["text mom", "I'm late", "yes"])
        );
        insta::assert_snapshot!(
            "choose_and_correct",
            transcript(
                Arc::clone(&full),
                &["text sam", "roy", "Hello there", "no, I meant mom", "yes"]
            )
        );
        insta::assert_snapshot!(
            "bad_answers",
            transcript(
                Arc::clone(&full),
                &["call sam", "bob", "2", "maybe", "no", "cancel"]
            )
        );
        insta::assert_snapshot!(
            "cancel_anywhere",
            transcript(
                Arc::clone(&full),
                &["text mom", "cancel", "call dad", "stop"]
            )
        );
        insta::assert_snapshot!(
            "shell_offer",
            transcript(
                Arc::clone(&full),
                &["make me a sandwich", "no", "echo hi there", "yes"]
            )
        );
        insta::assert_snapshot!(
            "no_shell_offer",
            transcript(engine(false, commands()), &["make me a sandwich", "$"])
        );
        let mut fewer = commands();
        fewer.remove("call");
        insta::assert_snapshot!(
            "unavailable",
            transcript(engine(true, fewer), &["call dad"])
        );
    }

    #[test]
    fn explain_matches_without_running() {
        let engine = engine(true, commands());
        let owner = Principal::local("cli");
        let call = engine.explain(&owner, "call sam").unwrap();
        assert_eq!(
            (call.command.as_str(), call.decision.as_str()),
            ("call", "confirm")
        );
        assert_eq!(call.ambiguous["contact"], ["sam lee", "sam roy"]);
        let shell = engine.explain(&owner, "$ ls").unwrap();
        assert_eq!(
            (shell.readback.as_str(), shell.decision.as_str()),
            ("$ ls", "allow")
        );
        assert!(engine.explain(&owner, "fly to the moon").is_none());
        let fewer = super::Engine::new(
            Arc::new(Config::from_toml(CONFIG).unwrap()),
            &PhraseSet::default_en().unwrap(),
            BTreeMap::new(),
            true,
            Facts::default(),
        )
        .unwrap();
        assert_eq!(
            fewer.explain(&owner, "call dad").unwrap().decision,
            "unavailable"
        );
    }

    #[test]
    fn unmatched_input_is_reported() {
        let owner = Principal::local("cli");
        let mut dialog = Dialog::new(engine(false, commands()), owner.clone());
        let reply = dialog.handle(&owner, "Make me a Sandwich!", Instant::now());
        assert_eq!(reply.missed.as_deref(), Some("make me a sandwich"));
        let reply = dialog.handle(&owner, "open firefox", Instant::now());
        assert_eq!(reply.missed, None);
    }

    #[test]
    fn voice_yes_counts_only_when_vouched() {
        let engine = engine(true, commands());
        let owner = Principal::voice("voice");
        let mut dialog = Dialog::new(engine, owner.clone());
        let now = Instant::now();
        let question = dialog.handle(&owner, "call mom", now);
        assert_eq!(question.say, ["Call mom (+910000000000)?"]);
        let refused = dialog.handle(&owner, "yes", now);
        assert_eq!(
            refused.say,
            ["Sorry Prime, I don't recognise your voice. Confirm on screen."]
        );
        assert!(refused.run.is_empty());
        assert!(dialog.is_waiting());
        let vouched = dialog.handle_vouched(&owner, "yes", true, now);
        assert_eq!(vouched.run.len(), 1);
    }

    #[test]
    fn a_saved_pipe_that_cannot_run_is_a_config_error() {
        let config = format!(
            "{CONFIG}bad = {{ run = \"head 3\" }}
"
        );
        let config = Arc::new(Config::from_toml(&config).unwrap());
        let phrases = PhraseSet::default_en().unwrap();
        let err = Engine::new(config, &phrases, commands(), true, Facts::default()).unwrap_err();
        assert_eq!(
            err.to_string(),
            "[pipes] bad: \"head 3\" needs input: put a command and | before it."
        );
    }

    #[test]
    fn untrusted_output_reaching_send_always_asks() {
        let mut commands = commands();
        let transform = PipeShape::transform(DataKind::Lines, None);
        let info = |capabilities: Vec<Capability>, untrusted| CommandInfo {
            capabilities,
            owner_defined: false,
            pipe: PipeShape {
                untrusted,
                ..transform
            },
        };
        commands.insert("fetch".to_owned(), info(vec![C::Read], true));
        commands.insert("mail".to_owned(), info(vec![C::Send], false));
        let config = format!(
            "{CONFIG}
[policy]
send = {{ local = \"allow\" }}
"
        );
        let config = Arc::new(Config::from_toml(&config).unwrap());
        let engine = Arc::new(
            Engine::new(
                config,
                &PhraseSet::default_en().unwrap(),
                commands,
                true,
                Facts::default(),
            )
            .unwrap(),
        );
        let owner = Principal::local("cli");
        let mut dialog = Dialog::new(engine, owner.clone());
        let clean = dialog.handle(&owner, "run build | mail", Instant::now());
        assert_eq!(clean.run.len(), 1);
        let tainted = dialog.handle(&owner, "run build | fetch | mail", Instant::now());
        assert!(tainted.run.is_empty());
        assert!(dialog.is_waiting());
    }

    #[test]
    fn a_saved_pipe_runs_only_when_its_if_holds() {
        let config = format!(
            "{CONFIG}plugged = {{ run = \"run build | count\", if = \"node.charging\" }}
"
        );
        let config = Arc::new(Config::from_toml(&config).unwrap());
        let facts = Facts::default();
        let engine = Arc::new(
            Engine::new(
                config,
                &PhraseSet::default_en().unwrap(),
                commands(),
                true,
                facts.clone(),
            )
            .unwrap(),
        );
        let owner = Principal::local("cli");
        let mut dialog = Dialog::new(engine, owner.clone());
        let skipped = dialog.handle(&owner, "run plugged", Instant::now());
        assert_eq!(
            skipped.say,
            ["Not running plugged: node.charging is not true."]
        );
        assert!(skipped.run.is_empty());
        facts.set("node.charging", crate::facts::Value::Bool(true));
        let ran = dialog.handle(&owner, "run plugged", Instant::now());
        assert_eq!(ran.run.len(), 1);
    }

    #[test]
    fn unattended_runs_need_the_flag_and_no_confirmation() {
        let extra = "auto = { run = \"run build | count\", unattended = true }
                     manual = { run = \"run build | count\" }
                     asks = { run = \"run build | save out.txt\", unattended = true }
                     later = { run = \"run build\", unattended = true, if = \"node.charging\" }
";
        let config = Arc::new(Config::from_toml(&format!("{CONFIG}{extra}")).unwrap());
        let engine = Engine::new(
            config,
            &PhraseSet::default_en().unwrap(),
            commands(),
            true,
            Facts::default(),
        )
        .unwrap();
        let timer = Principal::local("schedule");
        assert_eq!(
            engine
                .unattended("auto", &timer)
                .unwrap()
                .unwrap()
                .stages
                .len(),
            2
        );
        assert!(engine
            .unattended("manual", &timer)
            .unwrap_err()
            .contains("not marked"));
        assert!(engine
            .unattended("asks", &timer)
            .unwrap_err()
            .contains("confirmation"));
        assert_eq!(engine.unattended("later", &timer), Ok(None));
    }

    #[test]
    fn questions_expire() {
        let engine = engine(true, commands());
        let owner = Principal::local("cli");
        let mut dialog = Dialog::new(engine, owner.clone());
        let start = Instant::now();
        dialog.handle(&owner, "text mom", start);
        assert!(dialog.is_waiting());
        let later = start + ANSWER_TIMEOUT + Duration::from_secs(1);
        let reply = dialog.handle(&owner, "open firefox", later);
        assert_eq!(reply.say, ["That question expired.", "Opening firefox."]);
        assert_eq!(reply.run.len(), 1);
        assert!(!dialog.is_waiting());
    }

    #[test]
    fn only_the_owner_can_answer() {
        let engine = engine(true, commands());
        let owner = Principal::local("cli");
        let mut dialog = Dialog::new(engine, owner.clone());
        let now = Instant::now();
        dialog.handle(&owner, "call dad", now);
        let stranger = Principal {
            trust: Trust::OwnerRemote,
            ..Principal::local("telegram")
        };
        let reply = dialog.handle(&stranger, "yes", now);
        assert!(reply.run.is_empty());
        assert!(dialog.is_waiting());
    }
}
