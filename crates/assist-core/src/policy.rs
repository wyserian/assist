//! Who may do what (PLAN.md §3.4, §3.6): a capability × trust table with config overrides.
//!
//! Actions never decide about confirmation themselves; they declare capabilities and the
//! policy decides from the table, `risky` and how the request was made.

use std::collections::BTreeMap;

use serde::Deserialize;

/// How much a request's origin is trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trust {
    /// CLI or dashboard on this machine.
    Local,
    /// A paired device over the mesh, or an allowed Telegram user.
    OwnerRemote,
    /// A microphone: anyone in the room.
    Voice,
    /// LLM proposals, MCP clients, OCR / scraped / message content, webhooks.
    Untrusted,
    /// Standing autopilot goals, running unattended.
    Autopilot,
}

/// Who a request comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    /// The adapter that produced the request, e.g. `cli`.
    pub source: String,
    /// The user, as the adapter knows them.
    pub user: String,
    /// Trust level of the adapter.
    pub trust: Trust,
}

impl Principal {
    /// The owner typing on this machine.
    pub fn local(source: &str) -> Self {
        Self {
            source: source.to_owned(),
            user: "owner".to_owned(),
            trust: Trust::Local,
        }
    }
}

/// What an action may touch. Actions declare these in their manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
pub enum Capability {
    /// Search, read files, OCR, screenshots.
    #[serde(rename = "read")]
    Read,
    /// Open or close apps.
    #[serde(rename = "apps")]
    Apps,
    /// Media keys, volume.
    #[serde(rename = "media")]
    Media,
    /// Run shell commands or scripts.
    #[serde(rename = "shell")]
    Shell,
    /// Create or change files.
    #[serde(rename = "fs.write")]
    FsWrite,
    /// Delete anything.
    #[serde(rename = "delete")]
    Delete,
    /// Rename many files at once.
    #[serde(rename = "rename.bulk")]
    RenameBulk,
    /// SMS, calls, messages, HTTP posts.
    #[serde(rename = "send")]
    Send,
    /// Network scanning tools.
    #[serde(rename = "net.scan")]
    NetScan,
    /// Add, replace or delete secrets.
    #[serde(rename = "secrets.write")]
    SecretsWrite,
    /// Change settings.
    #[serde(rename = "config.write")]
    ConfigWrite,
    /// Daemon administration (stop all, revoke clients).
    #[serde(rename = "admin")]
    Admin,
}

/// The policy's answer. Ordered from least to most strict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// Run without asking.
    Allow,
    /// Read the resolved action back and wait for "yes".
    Confirm,
    /// Refuse.
    Deny,
}

/// `[policy]` in config: per capability, per trust level, a decision that replaces the default.
///
/// ```toml
/// [policy]
/// shell = { owner_remote = "deny" }
/// "fs.write" = { local = "allow" }   # dotted names must be quoted
/// ```
pub type PolicyOverrides = BTreeMap<Capability, BTreeMap<Trust, Decision>>;

/// One request as the policy sees it.
#[derive(Debug, Clone)]
pub struct Request<'a> {
    /// Who asks.
    pub principal: &'a Principal,
    /// Capabilities the command's action declares.
    pub capabilities: &'a [Capability],
    /// `risky = true` in the command's config.
    pub risky: bool,
    /// The user typed the exact command (`$ ls`), as opposed to accepting a suggestion.
    pub typed: bool,
}

/// The capability × trust table with overrides applied.
#[derive(Debug, Clone)]
pub struct Policy {
    table: BTreeMap<(Capability, Trust), Decision>,
    /// Typed local `$ cmd` runs without asking, unless config overrides the local shell cell.
    typed_local_shell: bool,
}

use Capability as C;
use Decision::{Allow, Confirm, Deny};

/// Defaults from PLAN.md §3.6, columns: local, owner_remote, voice, untrusted, autopilot.
/// Local shell is `Confirm` here; `decide` allows a typed local `$ cmd`.
///
/// shortcut: voice "confirm on screen" and the autopilot path/recipient scopes are plain
/// `Confirm` until voice (Phase 2) and autopilot (Phase 3) exist.
const DEFAULTS: [(Capability, [Decision; 5]); 12] = [
    (C::Read, [Allow, Allow, Allow, Confirm, Allow]),
    (C::Apps, [Allow, Allow, Allow, Confirm, Allow]),
    (C::Media, [Allow, Allow, Allow, Confirm, Allow]),
    (C::Shell, [Confirm, Confirm, Deny, Confirm, Deny]),
    (C::FsWrite, [Confirm, Confirm, Confirm, Confirm, Confirm]),
    (C::Delete, [Confirm, Confirm, Confirm, Confirm, Confirm]),
    (C::RenameBulk, [Confirm, Confirm, Confirm, Confirm, Confirm]),
    (C::Send, [Confirm, Confirm, Confirm, Confirm, Confirm]),
    (C::NetScan, [Confirm, Deny, Deny, Deny, Deny]),
    (C::SecretsWrite, [Allow, Deny, Deny, Deny, Deny]),
    (C::ConfigWrite, [Allow, Deny, Deny, Deny, Deny]),
    (C::Admin, [Allow, Deny, Deny, Deny, Deny]),
];

const COLUMNS: [Trust; 5] = [
    Trust::Local,
    Trust::OwnerRemote,
    Trust::Voice,
    Trust::Untrusted,
    Trust::Autopilot,
];

impl Policy {
    /// The default table with `overrides` applied.
    pub fn new(overrides: &PolicyOverrides) -> Self {
        let mut table = BTreeMap::new();
        for (capability, row) in DEFAULTS {
            for (trust, decision) in COLUMNS.into_iter().zip(row) {
                table.insert((capability, trust), decision);
            }
        }
        for (capability, cells) in overrides {
            for (trust, decision) in cells {
                table.insert((*capability, *trust), *decision);
            }
        }
        let typed_local_shell = !overrides
            .get(&C::Shell)
            .is_some_and(|cells| cells.contains_key(&Trust::Local));
        Self {
            table,
            typed_local_shell,
        }
    }

    /// The strictest decision over all capabilities, raised to at least `Confirm` when risky.
    pub fn decide(&self, request: &Request<'_>) -> Decision {
        let trust = request.principal.trust;
        let mut decision = if request.risky { Confirm } else { Allow };
        for capability in request.capabilities {
            let mut cell = self
                .table
                .get(&(*capability, trust))
                .copied()
                .unwrap_or(Deny);
            if *capability == C::Shell
                && trust == Trust::Local
                && request.typed
                && self.typed_local_shell
            {
                cell = Allow;
            }
            decision = decision.max(cell);
        }
        decision
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decide(
        policy: &Policy,
        trust: Trust,
        caps: &[Capability],
        risky: bool,
        typed: bool,
    ) -> Decision {
        let principal = Principal {
            trust,
            ..Principal::local("test")
        };
        policy.decide(&Request {
            principal: &principal,
            capabilities: caps,
            risky,
            typed,
        })
    }

    #[test]
    fn defaults_follow_the_plan_table() {
        let p = Policy::new(&PolicyOverrides::new());
        assert_eq!(decide(&p, Trust::Local, &[C::Apps], false, false), Allow);
        assert_eq!(
            decide(&p, Trust::Untrusted, &[C::Read], false, false),
            Confirm
        );
        assert_eq!(decide(&p, Trust::Voice, &[C::Shell], false, true), Deny);
        assert_eq!(
            decide(&p, Trust::OwnerRemote, &[C::NetScan], false, false),
            Deny
        );
        assert_eq!(decide(&p, Trust::Local, &[C::Send], false, false), Confirm);
        assert_eq!(
            decide(&p, Trust::Autopilot, &[C::Admin], false, false),
            Deny
        );
    }

    #[test]
    fn typed_local_shell_runs_but_suggested_shell_asks() {
        let p = Policy::new(&PolicyOverrides::new());
        assert_eq!(decide(&p, Trust::Local, &[C::Shell], false, true), Allow);
        assert_eq!(decide(&p, Trust::Local, &[C::Shell], false, false), Confirm);
        assert_eq!(
            decide(&p, Trust::OwnerRemote, &[C::Shell], false, true),
            Confirm
        );
    }

    #[test]
    fn risky_raises_to_confirm_but_never_lowers_deny() {
        let p = Policy::new(&PolicyOverrides::new());
        assert_eq!(decide(&p, Trust::Local, &[C::Apps], true, false), Confirm);
        assert_eq!(decide(&p, Trust::Local, &[], true, false), Confirm);
        assert_eq!(decide(&p, Trust::Voice, &[C::Shell], true, false), Deny);
    }

    #[test]
    fn strictest_capability_wins() {
        let p = Policy::new(&PolicyOverrides::new());
        assert_eq!(
            decide(&p, Trust::Local, &[C::Read, C::Send], false, false),
            Confirm
        );
        assert_eq!(
            decide(&p, Trust::OwnerRemote, &[C::Read, C::Admin], false, false),
            Deny
        );
    }

    #[test]
    fn config_overrides_replace_cells() {
        let overrides: PolicyOverrides = toml::from_str(
            "shell = { owner_remote = \"deny\" }\n\"fs.write\" = { local = \"allow\" }\n",
        )
        .unwrap();
        let p = Policy::new(&overrides);
        assert_eq!(
            decide(&p, Trust::OwnerRemote, &[C::Shell], false, true),
            Deny
        );
        assert_eq!(decide(&p, Trust::Local, &[C::FsWrite], false, false), Allow);
        assert_eq!(
            decide(&p, Trust::Local, &[C::Delete], false, false),
            Confirm
        );
    }

    #[test]
    fn overriding_local_shell_also_covers_typed_commands() {
        let overrides: PolicyOverrides = toml::from_str("shell = { local = \"confirm\" }").unwrap();
        let p = Policy::new(&overrides);
        assert_eq!(decide(&p, Trust::Local, &[C::Shell], false, true), Confirm);
    }

    #[test]
    fn unknown_names_are_rejected() {
        assert!(toml::from_str::<PolicyOverrides>("shel = { local = \"allow\" }").is_err());
        assert!(toml::from_str::<PolicyOverrides>("shell = { lokal = \"allow\" }").is_err());
        assert!(toml::from_str::<PolicyOverrides>("shell = { local = \"maybe\" }").is_err());
    }
}
