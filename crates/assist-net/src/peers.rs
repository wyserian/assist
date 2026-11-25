//! Paired devices, kept in the data directory: the hub's `peers.json` lists the devices it
//! accepts, a device's `hub.json` names the hub it talks to. Only these keys get in.

use std::path::{Path, PathBuf};

use assist_core::private_fs;
use serde::{Deserialize, Serialize};

use crate::{EndpointId, NetError};

/// Most paired devices; pairing more first needs one removed.
pub const MAX_PEERS: usize = 32;
/// Longest device name kept.
pub const MAX_NAME: usize = 64;

const PEERS_FILE: &str = "peers.json";
const HUB_FILE: &str = "hub.json";

/// A paired device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Peer {
    /// Its key.
    pub id: String,
    /// Its name, e.g. `Pixel 5`.
    pub name: String,
    /// Commands it runs for the hub.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// When it was paired, Unix ms.
    pub added: u64,
    /// Its push address, for waking it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push: Option<String>,
}

/// The hub's paired devices.
#[derive(Debug)]
pub struct Peers {
    path: PathBuf,
    list: Vec<Peer>,
}

impl Peers {
    /// The list in `data`; empty when there is none yet.
    pub fn load(data: &Path) -> Result<Self, NetError> {
        let path = data.join(PEERS_FILE);
        let list = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|err| NetError::Invalid(format!("{}: {err}", path.display())))?,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(err) => return Err(err.into()),
        };
        Ok(Self { path, list })
    }

    /// The paired device with key `id`.
    pub fn get(&self, id: &EndpointId) -> Option<&Peer> {
        let id = id.to_string();
        self.list.iter().find(|peer| peer.id == id)
    }

    /// Every paired device.
    pub fn all(&self) -> &[Peer] {
        &self.list
    }

    /// Pairs `id` as `name` (pairing again renames it) and saves the list.
    pub fn add(
        &mut self,
        id: &EndpointId,
        name: &str,
        capabilities: Vec<String>,
    ) -> Result<(), NetError> {
        let id = id.to_string();
        self.list.retain(|peer| peer.id != id);
        if self.list.len() >= MAX_PEERS {
            return Err(NetError::Invalid(format!(
                "{MAX_PEERS} devices are paired; remove one first"
            )));
        }
        self.list.push(Peer {
            id,
            name: bounded(name),
            capabilities: capabilities
                .into_iter()
                .take(64)
                .map(|c| bounded(&c))
                .collect(),
            added: assist_proto::now_ms(),
            push: None,
        });
        self.save()
    }

    /// Sets device `id`'s push address and saves the list when it changed.
    pub fn set_push(&mut self, id: &str, push: &str) -> Result<(), NetError> {
        match self.list.iter_mut().find(|peer| peer.id == id) {
            Some(peer) if peer.push.as_deref() != Some(push) => {
                peer.push = Some(push.to_owned());
                self.save()
            }
            _ => Ok(()),
        }
    }

    fn save(&self) -> Result<(), NetError> {
        if let Some(dir) = self.path.parent() {
            private_fs::create_dir(dir)?;
        }
        let json = serde_json::to_vec_pretty(&self.list)
            .map_err(|err| NetError::Invalid(err.to_string()))?;
        Ok(private_fs::write_atomic(&self.path, &json)?)
    }
}

/// The hub a device is paired with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hub {
    /// Its key.
    pub id: String,
    /// Its name, e.g. the computer's.
    pub name: String,
}

impl Hub {
    /// The hub `data` is paired with, if any.
    pub fn load(data: &Path) -> Option<Self> {
        let bytes = std::fs::read(data.join(HUB_FILE)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Remembers this hub in `data`.
    pub fn save(&self, data: &Path) -> Result<(), NetError> {
        private_fs::create_dir(data)?;
        let json =
            serde_json::to_vec_pretty(self).map_err(|err| NetError::Invalid(err.to_string()))?;
        Ok(private_fs::write_atomic(&data.join(HUB_FILE), &json)?)
    }

    /// Its key.
    pub fn key(&self) -> Result<EndpointId, NetError> {
        self.id
            .parse()
            .map_err(|_| NetError::Invalid("the stored hub key is malformed".to_owned()))
    }
}

/// `text` trimmed and cut to [`MAX_NAME`] characters.
fn bounded(text: &str) -> String {
    text.trim().chars().take(MAX_NAME).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SecretKey;

    #[test]
    fn peers_are_saved_renamed_and_bounded() {
        let dir = std::env::temp_dir().join(format!("assist-net-peers-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let phone = SecretKey::generate().public();
        let mut peers = Peers::load(&dir).unwrap();
        peers
            .add(&phone, "Pixel", vec!["send_sms".to_owned()])
            .unwrap();
        peers.add(&phone, &"x".repeat(200), Vec::new()).unwrap();
        peers
            .set_push(&phone.to_string(), "https://ntfy.sh/up1")
            .unwrap();
        let again = Peers::load(&dir).unwrap();
        assert_eq!(
            again.get(&phone).unwrap().push.as_deref(),
            Some("https://ntfy.sh/up1")
        );
        assert_eq!(again.all().len(), 1);
        assert_eq!(again.get(&phone).unwrap().name.len(), MAX_NAME);
        assert!(again.get(&SecretKey::generate().public()).is_none());

        let hub = Hub {
            id: phone.to_string(),
            name: "laptop".to_owned(),
        };
        hub.save(&dir).unwrap();
        assert_eq!(Hub::load(&dir).unwrap().key().unwrap(), phone);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
