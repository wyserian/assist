//! The proactive engine (PLAN.md section 3.17): rules that speak up unasked. A rule whose
//! condition names `job.` facts is checked when a job ends ("job.failed and job.lane ==
//! 'background'"); any other rule when its fact condition turns true ("node.battery < 15").
//! Every message passes the gates in order: muted, the rule's own interval, quiet hours,
//! and the limit per hour for all rules together. A message held back is dropped, not
//! queued. The rules are pure (they get events, facts and the time); [`spawn`] runs them on a
//! device, which delivers the messages its own way.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::{SignedDuration, ZonedDateTime};

use tokio::sync::broadcast::error::RecvError;

use crate::condition::Condition;
use crate::config::{Config, ProactiveConfig, Sink};
use crate::facts::{Facts, Value};
use crate::replies::Replies;
use crate::scheduler::{JobEvent, JobInfo, Scheduler};
use crate::trigger::FactEdge;
use assist_proto::JobState;

/// A message to deliver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    /// The rule's name.
    pub rule: String,
    /// What to say.
    pub text: String,
    /// Where it goes.
    pub sink: Sink,
}

struct Rule {
    name: String,
    condition: Condition,
    say: String,
    every: Duration,
    sink: Sink,
    /// Set for fact rules; job rules are checked per job.
    edge: Option<FactEdge>,
}

/// The rules with their limits and history.
pub struct Proactive {
    rules: Vec<Rule>,
    quiet: Option<(i8, i8, i8, i8)>,
    max_per_hour: usize,
    /// When messages went out in the last hour, oldest first.
    sent: VecDeque<ZonedDateTime>,
    last: BTreeMap<String, ZonedDateTime>,
    muted: BTreeSet<String>,
}

impl Proactive {
    /// The rules in `config`; config loading has checked them.
    pub fn new(config: &ProactiveConfig) -> Self {
        let rules = config
            .rules
            .iter()
            .filter_map(|rule| {
                let condition = Condition::parse(&rule.on).ok()?;
                let job_rule = condition.names().iter().any(|n| n.starts_with("job."));
                Some(Rule {
                    name: rule.name.clone(),
                    edge: (!job_rule).then(|| FactEdge::new(condition.clone())),
                    condition,
                    say: rule.say.clone(),
                    every: rule.every,
                    sink: rule.sink.unwrap_or(config.sink),
                })
            })
            .collect();
        Self {
            rules,
            quiet: config.quiet_hours.as_deref().and_then(parse_quiet),
            max_per_hour: config.max_per_hour,
            sent: VecDeque::new(),
            last: BTreeMap::new(),
            muted: BTreeSet::new(),
        }
    }

    /// Replaces the set of muted rule names.
    pub fn set_muted(&mut self, muted: BTreeSet<String>) {
        self.muted = muted;
    }

    /// Messages for a job that just ended.
    pub fn on_job(
        &mut self,
        job: &JobInfo,
        facts: &Facts,
        replies: &Replies,
        now: &ZonedDateTime,
    ) -> Vec<Notice> {
        let fields = job_fields(job);
        let lookup = |name: &str| fields.get(name).cloned().or_else(|| facts.get(name));
        let fired: Vec<usize> = (0..self.rules.len())
            .filter(|i| self.rules[*i].edge.is_none() && self.rules[*i].condition.eval(&lookup))
            .collect();
        let mut vars: BTreeMap<String, String> = facts
            .all()
            .into_iter()
            .map(|(k, v)| (k, plain(&v)))
            .collect();
        for (key, value) in &fields {
            vars.insert(key.trim_start_matches("job.").to_owned(), plain(value));
        }
        self.deliver(&fired, &vars, replies, now)
    }

    /// Messages for fact rules that just turned true.
    pub fn on_facts(
        &mut self,
        facts: &Facts,
        replies: &Replies,
        now: &ZonedDateTime,
    ) -> Vec<Notice> {
        let fired: Vec<usize> = (0..self.rules.len())
            .filter(|i| {
                self.rules[*i]
                    .edge
                    .as_mut()
                    .is_some_and(|edge| edge.check(facts))
            })
            .collect();
        let vars: BTreeMap<String, String> = facts
            .all()
            .into_iter()
            .map(|(k, v)| (k, plain(&v)))
            .collect();
        self.deliver(&fired, &vars, replies, now)
    }

    fn deliver(
        &mut self,
        fired: &[usize],
        vars: &BTreeMap<String, String>,
        replies: &Replies,
        now: &ZonedDateTime,
    ) -> Vec<Notice> {
        let hour_ago = now.checked_sub(SignedDuration::from_hours(1)).ok();
        while self
            .sent
            .front()
            .is_some_and(|at| hour_ago.as_ref().is_some_and(|h| at < h))
        {
            self.sent.pop_front();
        }
        let vars: Vec<(&str, &str)> = vars.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let mut out = Vec::new();
        for rule in fired.iter().filter_map(|i| self.rules.get(*i)) {
            let recent = self.last.get(&rule.name).is_some_and(|at| {
                now.timestamp().duration_since(at.timestamp())
                    < SignedDuration::try_from(rule.every).unwrap_or_default()
            });
            if self.muted.contains(&rule.name)
                || recent
                || self.is_quiet(now)
                || self.sent.len() >= self.max_per_hour
            {
                continue;
            }
            self.sent.push_back(now.clone());
            self.last.insert(rule.name.clone(), now.clone());
            out.push(Notice {
                rule: rule.name.clone(),
                text: replies.fill(&rule.say, &vars),
                sink: rule.sink,
            });
        }
        out
    }

    fn is_quiet(&self, now: &ZonedDateTime) -> bool {
        let Some((from_h, from_m, to_h, to_m)) = self.quiet else {
            return false;
        };
        let at = (now.hour(), now.minute());
        let (from, to) = ((from_h, from_m), (to_h, to_m));
        if from <= to {
            from <= at && at < to
        } else {
            at >= from || at < to
        }
    }
}

/// The ended job as `job.*` values for conditions and templates.
fn job_fields(job: &JobInfo) -> BTreeMap<&'static str, Value> {
    let text = |s: &str| Value::Text(s.to_owned());
    let failed = matches!(job.state, JobState::Failed | JobState::TimedOut);
    let mut fields = BTreeMap::from([
        ("job.state", text(job.state.as_str())),
        ("job.failed", Value::Bool(failed)),
        ("job.done", Value::Bool(job.state == JobState::Done)),
        ("job.command", text(&job.command)),
        ("job.title", text(&job.title)),
        ("job.lane", text(job.lane.as_str())),
        ("job.source", text(&job.principal.source)),
        ("job.id", Value::Number(job.id as f64)),
    ]);
    if let Some(error) = &job.error {
        fields.insert("job.error", text(&error.message));
    }
    fields
}

/// A value as a message shows it: text without quotes.
fn plain(value: &Value) -> String {
    match value {
        Value::Text(t) => t.clone(),
        other => other.to_string(),
    }
}

/// The file with muted rule names, in the data directory.
pub fn muted_path(data: &Path) -> PathBuf {
    data.join("muted.json")
}

/// Muted rule names; empty when none were muted or the file is unreadable.
pub fn load_muted(data: &Path) -> BTreeSet<String> {
    std::fs::read(muted_path(data))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Saves muted rule names, user-only.
pub fn save_muted(data: &Path, muted: &BTreeSet<String>) -> std::io::Result<()> {
    let json = serde_json::to_vec(muted).map_err(std::io::Error::from)?;
    crate::private_fs::write_atomic(&muted_path(data), &json)
}

/// `"23:00-07:00"`.
pub fn parse_quiet(text: &str) -> Option<(i8, i8, i8, i8)> {
    let (from, to) = text.split_once('-')?;
    let time = |t: &str| -> Option<(i8, i8)> {
        let (h, m) = t.trim().split_once(':')?;
        let (h, m): (i8, i8) = (h.parse().ok()?, m.parse().ok()?);
        ((0..24).contains(&h) && (0..60).contains(&m)).then_some((h, m))
    };
    let (from, to) = (time(from)?, time(to)?);
    Some((from.0, from.1, to.0, to.1))
}

/// How often fact rules are checked.
const TICK: Duration = Duration::from_secs(15);

/// Runs the rules on the current tokio runtime until the scheduler closes: ended jobs and
/// facts go in, notices go to `deliver`, which says or shows them on this device. Does
/// nothing when `config` has no rules.
pub fn spawn(
    config: &Config,
    scheduler: Scheduler,
    facts: Facts,
    data: PathBuf,
    deliver: impl Fn(Notice) + Send + 'static,
) {
    if config.proactive.rules.is_empty() {
        return;
    }
    let mut engine = Proactive::new(&config.proactive);
    let replies = Replies::new(config);
    tokio::spawn(async move {
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
                deliver(notice);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::{ActionError, Lane};
    use crate::config::Config;
    use crate::policy::Principal;
    use jiff::tz::{Offset, TimeZone};

    const CONFIG: &str = "[persona]\naddress = \"Prime\"\n\n[proactive]\nquiet_hours = \"23:00-07:00\"\nmax_per_hour = 2\n\n\
        [[proactive.rules]]\nname = \"build-failed\"\non = \"job.failed and job.lane == 'background'\"\nsay = \"{address}, {command} failed: {error}\"\n\n\
        [[proactive.rules]]\nname = \"battery\"\non = \"node.battery < 15\"\nsay = \"Battery at {node.battery}%.\"\nevery = \"1m\"\n";

    fn at(time: &str) -> ZonedDateTime {
        format!("2026-11-27T{time}")
            .parse::<jiff::civil::DateTime>()
            .unwrap()
            .to_zoned_datetime(TimeZone::fixed(Offset::from_hours(5).unwrap()))
            .unwrap()
    }

    fn job(state: JobState, lane: Lane) -> JobInfo {
        JobInfo {
            id: 7,
            command: "run_script".to_owned(),
            title: "run_script: script = build".to_owned(),
            principal: Principal::local("schedule"),
            lane,
            state,
            created: 0,
            started: None,
            finished: None,
            error: Some(ActionError::exit(2)),
            group: None,
        }
    }

    fn setup() -> (Proactive, Replies, Facts) {
        let config = Config::from_toml(CONFIG).unwrap();
        (
            Proactive::new(&config.proactive),
            Replies::new(&config),
            Facts::default(),
        )
    }

    #[test]
    fn job_rules_fill_the_template_and_respect_limits() {
        let (mut p, replies, facts) = setup();
        let failed = job(JobState::Failed, Lane::Background);
        let notices = p.on_job(&failed, &facts, &replies, &at("10:00"));
        assert_eq!(notices.len(), 1);
        assert_eq!(
            notices[0].text,
            "Prime, run_script failed: exited with code 2"
        );
        // Same rule again within its interval (default 10m): held back.
        assert!(p.on_job(&failed, &facts, &replies, &at("10:05")).is_empty());
        assert_eq!(p.on_job(&failed, &facts, &replies, &at("10:20")).len(), 1);
        // The per-hour limit (2) is reached.
        assert!(p.on_job(&failed, &facts, &replies, &at("10:40")).is_empty());
        // Other lanes and successes do not match.
        let done = job(JobState::Done, Lane::Background);
        assert!(p.on_job(&done, &facts, &replies, &at("12:00")).is_empty());
    }

    #[test]
    fn fact_rules_fire_on_change_unless_quiet_or_muted() {
        let (mut p, replies, facts) = setup();
        facts.set("node.battery", Value::Number(12.0));
        let notices = p.on_facts(&facts, &replies, &at("10:00"));
        assert_eq!(notices[0].text, "Battery at 12%.");
        assert!(p.on_facts(&facts, &replies, &at("10:02")).is_empty());
        facts.set("node.battery", Value::Number(40.0));
        p.on_facts(&facts, &replies, &at("23:00"));
        facts.set("node.battery", Value::Number(10.0));
        assert!(
            p.on_facts(&facts, &replies, &at("23:30")).is_empty(),
            "quiet hours"
        );
        facts.set("node.battery", Value::Number(40.0));
        p.on_facts(&facts, &replies, &at("11:00"));
        p.set_muted(BTreeSet::from(["battery".to_owned()]));
        facts.set("node.battery", Value::Number(10.0));
        assert!(
            p.on_facts(&facts, &replies, &at("11:05")).is_empty(),
            "muted"
        );
    }

    #[test]
    fn quiet_hours_parse_and_wrap() {
        assert_eq!(parse_quiet("23:00-07:30"), Some((23, 0, 7, 30)));
        assert_eq!(parse_quiet("25:00-07:00"), None);
        assert_eq!(parse_quiet("night"), None);
    }
}
