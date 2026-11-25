//! The network between assist nodes (PLAN.md section 3.13), over iroh: QUIC between device
//! keys, direct when it can be and through n0's relays when not (they see only encrypted
//! traffic). This crate holds a node's key, its paired devices, one-time pairing codes and
//! the message framing; the hub's server and the phone's core decide what the messages do.

use std::path::Path;

use iroh::endpoint::{presets, RecvStream, SendStream};
pub use iroh::{Endpoint, EndpointId, SecretKey};

mod pairing;
mod peers;

pub use pairing::{Pairing, Ticket};
pub use peers::{Hub, Peer, Peers};

/// The protocol on an iroh connection: assist envelopes, one framed message each.
pub const ALPN: &[u8] = b"assist/1";

/// The node's key in the data directory.
const KEY_FILE: &str = "node.key";

/// Why the network could not do something.
#[derive(Debug, thiserror::Error)]
pub enum NetError {
    /// A file in the data directory.
    #[error("{0}")]
    Io(#[from] std::io::Error),
    /// The endpoint could not start (no network stack, ports taken).
    #[error("cannot start the network: {0}")]
    Bind(String),
    /// A peer could not be reached or the connection broke.
    #[error("{0}")]
    Connect(String),
    /// A pairing code or a stored file is malformed.
    #[error("{0}")]
    Invalid(String),
}

/// This node's key, created owner-only in `data` the first time.
pub fn load_or_create_key(data: &Path) -> Result<SecretKey, NetError> {
    let path = data.join(KEY_FILE);
    if let Ok(text) = std::fs::read_to_string(&path) {
        if let Ok(key) = text.trim().parse::<SecretKey>() {
            return Ok(key);
        }
    }
    let key = SecretKey::generate();
    assist_core::private_fs::create_dir(data)?;
    let hex: String = key.to_bytes().iter().map(|b| format!("{b:02x}")).collect();
    assist_core::private_fs::write_atomic(&path, hex.as_bytes())?;
    Ok(key)
}

/// An endpoint with this node's key that accepts and makes assist connections. Needs the
/// network; with none it still starts and connects once one is there.
pub async fn endpoint(data: &Path) -> Result<Endpoint, NetError> {
    let key = load_or_create_key(data)?;
    Endpoint::builder(presets::N0)
        .secret_key(key)
        .alpns(vec![ALPN.to_vec()])
        .bind()
        .await
        .map_err(|err| NetError::Bind(err.to_string()))
}

/// Sends one message: its length (4 bytes, big endian), then its text.
pub async fn send_frame(send: &mut SendStream, text: &str) -> Result<(), NetError> {
    let len = u32::try_from(text.len())
        .ok()
        .filter(|len| *len as usize <= assist_proto::MAX_FRAME)
        .ok_or_else(|| NetError::Invalid("message too large".to_owned()))?;
    send.write_all(&len.to_be_bytes())
        .await
        .map_err(|err| NetError::Connect(err.to_string()))?;
    send.write_all(text.as_bytes())
        .await
        .map_err(|err| NetError::Connect(err.to_string()))
}

/// Receives one message; `None` when the stream ended. A frame over
/// [`assist_proto::MAX_FRAME`] or not UTF-8 is an error, read no further.
pub async fn recv_frame(recv: &mut RecvStream) -> Result<Option<String>, NetError> {
    let mut len = [0u8; 4];
    if recv.read_exact(&mut len).await.is_err() {
        return Ok(None);
    }
    let len = u32::from_be_bytes(len) as usize;
    if len > assist_proto::MAX_FRAME {
        return Err(NetError::Invalid("message too large".to_owned()));
    }
    let mut buf = vec![0u8; len];
    recv.read_exact(&mut buf)
        .await
        .map_err(|err| NetError::Connect(err.to_string()))?;
    String::from_utf8(buf)
        .map(Some)
        .map_err(|_| NetError::Invalid("message is not UTF-8".to_owned()))
}

/// This computer's name for the other side, e.g. `LAPTOP-7`; bounded.
pub fn host_name() -> String {
    let name = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "assist".to_owned());
    name.chars().take(peers::MAX_NAME).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_is_created_once_and_kept() {
        let dir = std::env::temp_dir().join(format!("assist-net-key-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let first = load_or_create_key(&dir).unwrap();
        let again = load_or_create_key(&dir).unwrap();
        assert_eq!(first.public(), again.public());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
