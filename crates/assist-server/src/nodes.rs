//! Paired devices over iroh (PLAN.md section 3.13). A paired device's connection runs through
//! the same [`conn::run`] as the CLI, with remote-owner trust; an unknown key may only pair,
//! with the one-time secret from `assist pair`, and is then disconnected.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use assist_net::{Endpoint, Pairing, Peers};
use assist_proto::{Envelope, Kind, Pair, Paired, RpcResult};
use iroh::endpoint::{Connection, SendStream};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::conn::{self, Transport, HUB};
use crate::{AppState, Credential, Devices};

/// How long a new device has to send its pairing request.
const PAIR_WAIT: Duration = Duration::from_secs(30);
/// Messages read ahead from a device before the connection handles them, and queued for it.
const READ_AHEAD: usize = 16;

/// What the daemon needs to accept devices.
pub struct Nodes {
    /// This node's iroh endpoint.
    pub endpoint: Endpoint,
    /// Paired devices.
    pub peers: Mutex<Peers>,
    /// The current pairing code.
    pub pairing: Pairing,
    /// This hub's name for devices.
    pub name: String,
    /// Connected devices, for running commands on them; shared with the hub's actions.
    pub devices: Arc<Devices>,
}

/// Accepts device connections until `stop`.
pub(crate) async fn accept(state: AppState, stop: CancellationToken) {
    let Some(nodes) = state.services.nodes.as_ref() else {
        return;
    };
    let endpoint = nodes.endpoint.clone();
    loop {
        let incoming = tokio::select! {
            () = stop.cancelled() => return,
            incoming = endpoint.accept() => incoming,
        };
        let Some(incoming) = incoming else { return };
        let state = state.clone();
        tokio::spawn(async move {
            if let Ok(connection) = incoming.await {
                handle(state, connection).await;
            }
        });
    }
}

async fn handle(state: AppState, connection: Connection) {
    let Ok((send, mut recv)) = connection.accept_bi().await else {
        return;
    };
    let remote = connection.remote_id();
    let Some(nodes) = state.services.nodes.as_ref() else {
        return;
    };
    let peer = nodes
        .peers
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&remote)
        .cloned();
    match peer {
        Some(peer) => {
            let (frames, read) = mpsc::channel(READ_AHEAD);
            // QUIC reads are not cancel-safe; a task reads whole messages for the loop.
            tokio::spawn(async move {
                while let Ok(Some(text)) = assist_net::recv_frame(&mut recv).await {
                    if frames.send(text).await.is_err() {
                        break;
                    }
                }
            });
            let transport = Quic {
                read,
                send,
                connection,
            };
            let (outbox, queued) = mpsc::channel(READ_AHEAD);
            nodes
                .devices
                .connect(&peer.id, &peer.name, peer.capabilities.clone(), outbox);
            let credential = Credential::Node {
                name: peer.name,
                id: peer.id.clone(),
            };
            conn::run(state.clone(), transport, credential, Some(queued)).await;
            nodes.devices.disconnect(&peer.id);
        }
        None => {
            let first = tokio::time::timeout(PAIR_WAIT, assist_net::recv_frame(&mut recv)).await;
            let answer = match first {
                Ok(Ok(Some(text))) => pair(nodes, &remote, &text),
                _ => return,
            };
            let mut send = send;
            let _ = assist_net::send_frame(&mut send, &answer.encode()).await;
            let _ = send.finish();
            // Give the answer time to leave before the connection closes.
            let _ = tokio::time::timeout(Duration::from_secs(2), connection.closed()).await;
        }
    }
}

/// Answers an unknown device's first message: paired with a valid secret, refused otherwise.
fn pair(nodes: &Nodes, remote: &assist_net::EndpointId, text: &str) -> Envelope {
    let refuse = |message: &Envelope, why: &str| {
        message.answer(Kind::RpcResult, HUB, RpcResult::err("denied", why))
    };
    let Ok(message) = Envelope::decode(text) else {
        return Envelope::new(
            Kind::RpcResult,
            HUB,
            "device",
            RpcResult::err("denied", "pair first"),
        );
    };
    let request = match (message.kind, message.body::<Pair>()) {
        (Kind::Pair, Ok(request)) => request,
        _ => {
            return refuse(
                &message,
                "this device is not paired; run assist pair on the computer",
            )
        }
    };
    if !nodes.pairing.redeem(&request.secret) {
        return refuse(
            &message,
            "the pairing code is wrong or expired; make a new one",
        );
    }
    let added = nodes
        .peers
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .add(remote, &request.name, request.capabilities);
    match added {
        Ok(()) => {
            println!(
                "Paired {}.",
                request.name.chars().take(64).collect::<String>()
            );
            message.answer(
                Kind::Paired,
                HUB,
                Paired {
                    name: nodes.name.clone(),
                },
            )
        }
        Err(err) => refuse(&message, &err.to_string()),
    }
}

/// A paired device's connection: whole messages from the reader task, framed writes.
struct Quic {
    read: mpsc::Receiver<String>,
    send: SendStream,
    connection: Connection,
}

impl Transport for Quic {
    async fn recv(&mut self) -> Option<String> {
        self.read.recv().await
    }

    async fn send(&mut self, text: String) -> bool {
        assist_net::send_frame(&mut self.send, &text).await.is_ok()
    }

    async fn close(&mut self) {
        let _ = self.send.finish();
        self.connection.close(0u32.into(), b"bye");
    }
}
