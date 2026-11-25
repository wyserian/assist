//! One-time pairing codes (PLAN.md section 3.13). The hub makes a code with its key and a
//! fresh secret (`assist pair` shows it as a QR code); a new device connects to that key,
//! which iroh authenticates, and sends the secret. The secret works once and only for
//! [`VALID`]; a newer code replaces an older one.

use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use assist_core::secrets::constant_time_eq;
use assist_proto::Uuid;

use crate::{EndpointId, NetError};

/// How long a pairing code works.
pub const VALID: Duration = Duration::from_secs(300);

const PREFIX: &str = "assist-pair:";

/// What the new device needs: the hub's key and the secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ticket {
    /// The hub to connect to.
    pub hub: EndpointId,
    /// The one-time secret.
    pub secret: String,
}

impl Ticket {
    /// `assist-pair:<hub key>:<secret>`, the text in the QR code.
    pub fn encode(&self) -> String {
        format!("{PREFIX}{}:{}", self.hub, self.secret)
    }

    /// Reads a code as [`Ticket::encode`] writes it; spaces around it are ignored.
    pub fn parse(text: &str) -> Result<Self, NetError> {
        let bad = || NetError::Invalid("that is not an assist pairing code".to_owned());
        let rest = text.trim().strip_prefix(PREFIX).ok_or_else(bad)?;
        let (hub, secret) = rest.split_once(':').ok_or_else(bad)?;
        let secret_ok =
            (16..=64).contains(&secret.len()) && secret.bytes().all(|b| b.is_ascii_alphanumeric());
        if !secret_ok {
            return Err(bad());
        }
        Ok(Self {
            hub: hub.parse().map_err(|_| bad())?,
            secret: secret.to_owned(),
        })
    }
}

/// The hub's current pairing code, if any.
#[derive(Debug, Default)]
pub struct Pairing {
    current: Mutex<Option<(String, Instant)>>,
}

impl Pairing {
    /// A fresh code for `hub`, replacing any earlier one.
    pub fn start(&self, hub: EndpointId) -> Ticket {
        let secret = Uuid::new_v4().simple().to_string();
        *self.lock() = Some((secret.clone(), Instant::now() + VALID));
        Ticket { hub, secret }
    }

    /// Whether `secret` is the current, unexpired code; it is used up either way it matched.
    pub fn redeem(&self, secret: &str) -> bool {
        let mut current = self.lock();
        let ok = current.as_ref().is_some_and(|(expected, until)| {
            constant_time_eq(expected.as_bytes(), secret.as_bytes()) && Instant::now() < *until
        });
        if ok {
            *current = None;
        }
        ok
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<(String, Instant)>> {
        self.current.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SecretKey;

    #[test]
    fn a_code_round_trips_and_works_once() {
        let hub = SecretKey::generate().public();
        let pairing = Pairing::default();
        let ticket = pairing.start(hub);
        assert_eq!(
            Ticket::parse(&format!(" {} ", ticket.encode())).unwrap(),
            ticket
        );
        assert!(!pairing.redeem("0123456789abcdef0123456789abcdef"));
        assert!(pairing.redeem(&ticket.secret));
        assert!(!pairing.redeem(&ticket.secret));
    }

    #[test]
    fn a_newer_code_replaces_the_older_and_junk_is_refused() {
        let hub = SecretKey::generate().public();
        let pairing = Pairing::default();
        let old = pairing.start(hub);
        let new = pairing.start(hub);
        assert!(!pairing.redeem(&old.secret));
        assert!(pairing.redeem(&new.secret));
        for junk in [
            "",
            "assist-pair:",
            "assist-pair:xyz:abc",
            "http://x",
            "assist-pair:a:b:c",
        ] {
            assert!(Ticket::parse(junk).is_err(), "{junk}");
        }
    }
}
