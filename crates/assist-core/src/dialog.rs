//! Conversation state machine (PLAN.md §3.5).
//!
//! One [`Dialog`] per conversation. It turns text into [`Plan`]s to run: it asks for missing
//! slots, asks "A or B?" for ambiguous names, reads risky actions back for a "yes", handles
//! follow-ups ("close it"), corrections ("no, I meant dad") and "X and Y". `cancel` works in
//! every state, and an unanswered question expires.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::config::{Config, ConfigError};
use crate::matcher::{BuildError, Intent, Matcher, RuleMatcher, SlotType};
use crate::normalize::{normalize, parse_number};
use crate::phrases::PhraseSet;
use crate::policy::{Capability, Decision, Policy, Principal, Request};

/// The built-in command for `$ …` input; its slot `command` holds the exact text.
pub const SHELL: &str = "shell";

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
    config: Config,
    matcher: RuleMatcher,
    policy: Policy,
    commands: BTreeMap<String, CommandInfo>,
    /// Command → slot → question.
    asks: BTreeMap<String, BTreeMap<String, String>>,
    offer_shell: bool,
}

impl Engine {
    /// Builds the engine. `commands` are the commands this device can run; `offer_shell`
    /// offers unmatched input as a shell command (desktop).
    pub fn new(
        config: Config,
        phrases: &PhraseSet,
        commands: BTreeMap<String, CommandInfo>,
        offer_shell: bool,
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
        Ok(Self {
            config,
            matcher,
            policy,
            commands,
            asks,
            offer_shell,
        })
    }

    /// The config the engine was built from.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// The fully resolved action as one line, for confirmation and logs.
    pub fn readback(&self, plan: &Plan) -> String {
        if plan.command == SHELL {
            let command = plan.slots.get("command").map_or("", String::as_str);
            return format!("$ {command}");
        }
        let types = self.matcher.slots(&plan.command);
        let slots: Vec<String> = plan
            .slots
            .iter()
            .map(|(slot, value)| {
                let shown = match types.and_then(|t| t.get(slot)) {
                    Some(SlotType::Text) => format!("{value:?}"),
                    Some(SlotType::List(list)) if list == "contacts" => {
                        match self.config.contacts.get(value) {
                            Some(number) => format!("{value} ({number})"),
                            None => value.clone(),
                        }
                    }
                    _ => value.clone(),
                };
                format!("{slot} = {shown}")
            })
            .collect();
        if slots.is_empty() {
            plan.command.clone()
        } else {
            format!("{}: {}", plan.command, slots.join(", "))
        }
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
    /// Plans to schedule now, in order.
    pub run: Vec<Plan>,
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
        plan: Plan,
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
        }
    }

    /// Whether the dialog is waiting for an answer.
    pub fn is_waiting(&self) -> bool {
        !matches!(self.state, State::Idle)
    }

    /// Handles one input at time `now`.
    pub fn handle(&mut self, principal: &Principal, input: &str, now: Instant) -> Reply {
        let mut reply = Reply::default();
        let raw = input.trim();
        if raw.is_empty() {
            return reply;
        }
        if principal != &self.owner {
            reply.say.push(format!(
                "This conversation belongs to {}.",
                self.owner.source
            ));
            return reply;
        }
        if self.is_waiting() && self.deadline.is_some_and(|deadline| now > deadline) {
            self.reset();
            reply.say.push("The earlier question expired.".to_owned());
        }
        let norm = normalize(raw);
        if CANCEL.contains(&norm.as_str()) {
            let had_work = self.is_waiting() || !self.queue.is_empty();
            self.reset();
            reply.say.push(
                if had_work {
                    "Cancelled."
                } else {
                    "Nothing to cancel."
                }
                .to_owned(),
            );
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
            State::Confirming { plan, intent } => self.confirm(plan, intent, &norm, &mut reply),
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
                reply.say.push("Type a command after $.".to_owned());
            } else {
                self.decide(Plan::shell(command, true), None, reply);
            }
            return;
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
        if self.engine.offer_shell {
            let plan = Plan::shell(raw, false);
            let decision = self.engine.decision(&self.owner, &plan);
            if decision.is_some_and(|d| d != Decision::Deny) {
                reply.say.push(format!(
                    "I don't know that command. Run `{raw}` in the shell? (yes/no)"
                ));
                self.state = State::Confirming { plan, intent: None };
                return;
            }
        }
        reply
            .say
            .push("Sorry, I didn't understand that.".to_owned());
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
        if !self.engine.commands.contains_key(&intent.command) {
            reply.say.push(format!(
                "{} isn't available on this device.",
                intent.command
            ));
            return;
        }
        if let Some((slot, candidates)) = intent.ambiguous.pop_first() {
            reply.say.push(choice_question(&candidates));
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
        let plan = Plan {
            command: intent.command.clone(),
            slots: intent.slots.clone(),
            typed: false,
        };
        self.decide(plan, Some(intent), reply);
    }

    fn decide(&mut self, plan: Plan, intent: Option<Intent>, reply: &mut Reply) {
        match self.engine.decision(&self.owner, &plan) {
            None => reply
                .say
                .push(format!("{} isn't available on this device.", plan.command)),
            Some(Decision::Deny) => reply.say.push(format!(
                "{} is not allowed from {}.",
                plan.command, self.owner.source
            )),
            Some(Decision::Confirm) => {
                reply
                    .say
                    .push(format!("{}? (yes/no)", self.engine.readback(&plan)));
                self.state = State::Confirming { plan, intent };
            }
            Some(Decision::Allow) => self.submit(plan, intent, reply),
        }
    }

    fn submit(&mut self, plan: Plan, intent: Option<Intent>, reply: &mut Reply) {
        if intent.is_some() {
            self.last = intent;
        }
        reply.run.push(plan);
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
                    reply.say.push("Please say a number.".to_owned());
                    self.state = State::Asking { intent, slot };
                    return;
                }
            },
            SlotType::List(list) => {
                let candidates = self.engine.matcher.candidates(&list, &normalize(raw));
                match candidates.as_slice() {
                    [] => {
                        reply
                            .say
                            .push(format!("I don't know \"{raw}\". Try again, or say cancel."));
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
                reply.say.push(choice_question(&candidates));
                self.state = State::Choosing {
                    intent,
                    slot,
                    candidates,
                };
            }
        }
    }

    fn confirm(&mut self, plan: Plan, intent: Option<Intent>, norm: &str, reply: &mut Reply) {
        if YES.contains(&norm) {
            self.submit(plan, intent, reply);
            return self.next(reply);
        }
        if let (Some(value), Some(intent)) = (correction(norm), intent.clone()) {
            return self.correct(intent, value, reply);
        }
        if NO.contains(&norm) {
            self.queue.clear();
            reply.say.push("Okay, not doing that.".to_owned());
            return;
        }
        reply.say.push("Please answer yes or no.".to_owned());
        self.state = State::Confirming { plan, intent };
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
            reply
                .say
                .push(format!("I couldn't tell what \"{value}\" should replace."));
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

fn choice_question(candidates: &[String]) -> String {
    match candidates.split_last() {
        Some((last, rest)) if !rest.is_empty() => {
            format!("Did you mean {} or {last}?", rest.join(", "))
        }
        _ => format!("Did you mean {}?", candidates.join("")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{Capability as C, Trust};

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
"#;

    fn commands() -> BTreeMap<String, CommandInfo> {
        let info = |caps: &[Capability], owner_defined| CommandInfo {
            capabilities: caps.to_vec(),
            owner_defined,
        };
        BTreeMap::from([
            ("open_app".to_owned(), info(&[C::Apps], false)),
            ("close_app".to_owned(), info(&[C::Apps], false)),
            ("run_script".to_owned(), info(&[C::Shell], true)),
            ("search".to_owned(), info(&[C::Read], false)),
            ("send_sms".to_owned(), info(&[C::Send], false)),
            ("call".to_owned(), info(&[C::Send], false)),
            ("set_volume".to_owned(), info(&[C::Media], false)),
            (SHELL.to_owned(), info(&[C::Shell], false)),
        ])
    }

    fn engine(offer_shell: bool, commands: BTreeMap<String, CommandInfo>) -> Arc<Engine> {
        let config = Config::from_toml(CONFIG).unwrap();
        let phrases = PhraseSet::default_en().unwrap();
        Arc::new(Engine::new(config, &phrases, commands, offer_shell).unwrap())
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
            for plan in reply.run {
                out.push_str(&format!("! {}\n", engine.readback(&plan)));
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
    fn questions_expire() {
        let engine = engine(true, commands());
        let owner = Principal::local("cli");
        let mut dialog = Dialog::new(engine, owner.clone());
        let start = Instant::now();
        dialog.handle(&owner, "text mom", start);
        assert!(dialog.is_waiting());
        let later = start + ANSWER_TIMEOUT + Duration::from_secs(1);
        let reply = dialog.handle(&owner, "open firefox", later);
        assert_eq!(reply.say, ["The earlier question expired."]);
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
