//! A WebSocket client of the local daemon, authenticated with the user-only token file.

use std::collections::VecDeque;
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use assist_proto::{
    Envelope, Hello, JobUpdate, Kind, Notice, Reply, Rpc, RpcResult, Subscribe, Utterance, Uuid,
};
use futures_util::{SinkExt, StreamExt};
use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
use tokio_tungstenite::tungstenite::{self, Message};
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

use crate::daemon::addr_file;

/// Messages kept while waiting for an answer; older ones are dropped.
const MAX_PENDING: usize = 10_000;

/// A connection to the daemon.
pub struct Client {
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
    /// `host:port` of the daemon.
    addr: String,
    /// Messages that arrived while waiting for something else.
    pending: VecDeque<Envelope>,
}

impl Client {
    /// Connects to the daemon of the data directory `data` and says hello; `voice` asks for
    /// voice trust (utterances from a microphone).
    pub async fn connect(data: &Path, voice: bool) -> Result<Self> {
        let addr = std::fs::read_to_string(addr_file(data))
            .map_err(|_| anyhow!("No daemon running. Start it with: assist serve"))?;
        let addr = addr.trim();
        let token = std::fs::read_to_string(data.join("token"))
            .context("cannot read the daemon token in the data directory")?;
        let mut request = format!("ws://{addr}/ws").into_client_request()?;
        request
            .headers_mut()
            .insert(AUTHORIZATION, format!("Bearer {}", token.trim()).parse()?);
        let ws = match connect_async(request).await {
            Ok((ws, _)) => ws,
            Err(tungstenite::Error::Http(response)) => bail!(
                "the daemon at {addr} refused this client ({})",
                response.status()
            ),
            Err(err) => bail!("cannot reach the daemon at {addr}: {err}"),
        };
        let mut client = Self {
            ws,
            addr: addr.to_owned(),
            pending: VecDeque::new(),
        };
        let cwd = std::env::current_dir()
            .ok()
            .map(|dir| dir.display().to_string());
        let hello = Hello {
            client: "assist-cli".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            cwd,
            voice,
            contacts: Vec::new(),
        };
        let sent = client.send(Kind::Hello, hello).await?;
        client.answer(sent).await?;
        Ok(client)
    }

    /// The daemon's `host:port`.
    pub fn addr(&self) -> &str {
        &self.addr
    }

    /// Sends text to this connection's conversation.
    pub async fn say(&mut self, text: &str) -> Result<Reply> {
        self.say_as(text, false).await
    }

    /// Says something; `speaker_verified` when the voice matched the owner's voiceprint.
    pub async fn say_as(&mut self, text: &str, speaker_verified: bool) -> Result<Reply> {
        let utterance = Utterance {
            text: text.to_owned(),
            speaker_verified,
        };
        let sent = self.send(Kind::Utterance, utterance).await?;
        Ok(self.answer(sent).await?.body()?)
    }

    /// Calls an rpc method.
    pub async fn call<T: DeserializeOwned>(
        &mut self,
        method: &str,
        params: impl Serialize,
    ) -> Result<T> {
        let rpc = Rpc {
            method: method.to_owned(),
            params: serde_json::to_value(params)?,
        };
        let sent = self.send(Kind::Rpc, rpc).await?;
        decode(method, self.answer(sent).await?.body()?)
    }

    /// Asks for `topics` (`jobs`, `notices`) as they happen.
    pub async fn subscribe(&mut self, topics: &[&str]) -> Result<()> {
        let topics = topics.iter().map(|t| (*t).to_owned()).collect();
        self.send(Kind::Subscribe, Subscribe { topics }).await?;
        Ok(())
    }

    /// The next proactive message; other messages that arrive meanwhile are skipped.
    /// Cancel-safe, so it can wait in a `select!`.
    pub async fn notice(&mut self) -> Result<Notice> {
        loop {
            let message = match self.pending.pop_front() {
                Some(message) => message,
                None => self.next().await?,
            };
            if message.kind == Kind::Notice {
                return Ok(message.body()?);
            }
        }
    }

    /// The next job update.
    pub async fn update(&mut self) -> Result<JobUpdate> {
        loop {
            let message = match self.pending.pop_front() {
                Some(message) => message,
                None => self.next().await?,
            };
            if message.kind == Kind::JobUpdate {
                return Ok(message.body()?);
            }
        }
    }

    async fn send(&mut self, kind: Kind, body: impl Serialize) -> Result<Uuid> {
        let envelope = Envelope::new(kind, "cli", "hub", body);
        self.ws
            .send(Message::Text(envelope.encode().into()))
            .await
            .context("lost the connection to the daemon")?;
        Ok(envelope.id)
    }

    /// Waits for the answer to message `to`, keeping other messages for later.
    async fn answer(&mut self, to: Uuid) -> Result<Envelope> {
        loop {
            let message = self.next().await?;
            if message.re == Some(to) {
                return Ok(message);
            }
            if self.pending.len() == MAX_PENDING {
                self.pending.pop_front();
            }
            self.pending.push_back(message);
        }
    }

    async fn next(&mut self) -> Result<Envelope> {
        loop {
            match self.ws.next().await {
                Some(Ok(Message::Text(text))) => return Ok(Envelope::decode(text.as_str())?),
                Some(Ok(Message::Close(_))) | None => bail!("the daemon closed the connection"),
                Some(Ok(_)) => {}
                Some(Err(err)) => return Err(err).context("lost the connection to the daemon"),
            }
        }
    }
}

/// The value of an rpc answer. A `null` result arrives as no result at all, so a method that
/// may answer "nothing" (an `Option`) gets `None`; anything else needs a value.
fn decode<T: DeserializeOwned>(method: &str, answer: RpcResult) -> Result<T> {
    if let Some(error) = answer.error {
        bail!("{}", error.message);
    }
    let value = answer.result.unwrap_or(serde_json::Value::Null);
    serde_json::from_value(value).map_err(|err| anyhow!("bad answer to {method}: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_null_answer_is_none_and_a_missing_value_an_error() {
        let empty = || RpcResult {
            result: None,
            error: None,
        };
        let none: Option<String> = decode("commands.explain", empty()).unwrap();
        assert_eq!(none, None);
        assert!(decode::<String>("commands.explain", empty()).is_err());
    }
}
