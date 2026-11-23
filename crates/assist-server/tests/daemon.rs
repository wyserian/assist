//! End-to-end: a real daemon on a random loopback port, real WebSocket and HTTP clients.

// Test-only file: helpers fail the test on any error.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use assist_core::action::{Action, ActionError, BoxFuture, Chunk, Input, JobCtx, Lane, Manifest};
use assist_core::config::Config;
use assist_core::dialog::{Engine, SHELL};
use assist_core::phrases::PhraseSet;
use assist_core::policy::Capability;
use assist_core::scheduler::{Persistence, Scheduler};
use assist_core::secrets::{SecretError, SecretStore, Secrets};
use assist_core::store::Store;
use assist_proto::rpc::{self, Explanation, JobRef, Otp, SecretEntry, SecretName, SecretSet};
use assist_proto::{
    Envelope, Hello, JobState, JobSummary, JobUpdate, Kind, Reply, Rpc, RpcResult, Utterance,
};
use assist_server::{load_or_create_token, serve, Services};
use futures_util::{SinkExt, StreamExt};
use serde::de::DeserializeOwned;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{self, Message};
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// `shell` that prints the command it was given instead of running it.
struct Echo;

static ECHO: Manifest = Manifest {
    name: SHELL,
    capabilities: &[Capability::Shell],
    owner_defined: false,
    lane: Lane::Interactive,
};

impl Action for Echo {
    fn manifest(&self) -> &Manifest {
        &ECHO
    }
    fn run(
        &self,
        ctx: JobCtx,
        _input: Input,
        out: assist_core::action::Output,
    ) -> BoxFuture<Result<(), ActionError>> {
        Box::pin(async move {
            let command = ctx.plan.slots.get("command").cloned().unwrap_or_default();
            let _ = out.send(Chunk::Line(command)).await;
            Ok(())
        })
    }
}

#[derive(Default)]
struct Memory(Mutex<HashMap<String, String>>);

impl SecretStore for Memory {
    fn set(&self, name: &str, value: &str) -> Result<(), SecretError> {
        self.0.lock().unwrap().insert(name.into(), value.into());
        Ok(())
    }
    fn get(&self, name: &str) -> Result<Option<String>, SecretError> {
        Ok(self.0.lock().unwrap().get(name).cloned())
    }
    fn delete(&self, name: &str) -> Result<(), SecretError> {
        self.0.lock().unwrap().remove(name);
        Ok(())
    }
}

struct Daemon {
    port: u16,
    token: String,
    dir: PathBuf,
    stop: Option<oneshot::Sender<()>>,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn start(name: &str) -> Daemon {
    let dir = std::env::temp_dir().join(format!("assist-daemon-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let config = Arc::new(Config::from_toml("").unwrap());
    let (store, _) = Store::open(&dir, &config.jobs).unwrap();
    let secrets = Arc::new(Secrets::open(Box::new(Memory::default()), &dir).unwrap());
    let scheduler = Scheduler::new(
        vec![Arc::new(Echo)],
        Arc::clone(&config),
        Persistence {
            store: Some(store.clone()),
            redactor: Some(secrets.redactor()),
            ..Persistence::default()
        },
    );
    let engine = Arc::new(
        Engine::new(
            config,
            &PhraseSet::default_en().unwrap(),
            scheduler.commands(),
            true,
        )
        .unwrap(),
    );
    let token = load_or_create_token(&dir).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (stop, stopped) = oneshot::channel::<()>();
    let services = Services {
        scheduler,
        engine,
        store,
        secrets: Some(secrets),
        default_cwd: std::env::temp_dir(),
        data_dir: dir.clone(),
    };
    tokio::spawn(serve(services, listener, async {
        let _ = stopped.await;
    }));
    Daemon {
        port,
        token,
        dir,
        stop: Some(stop),
    }
}

async fn connect(port: u16, headers: &[(&'static str, String)]) -> Result<Ws, tungstenite::Error> {
    let mut request = format!("ws://127.0.0.1:{port}/ws")
        .into_client_request()
        .unwrap();
    for (name, value) in headers {
        request.headers_mut().insert(*name, value.parse().unwrap());
    }
    connect_async(request).await.map(|(ws, _)| ws)
}

fn status(result: Result<Ws, tungstenite::Error>) -> u16 {
    match result {
        Err(tungstenite::Error::Http(response)) => response.status().as_u16(),
        Err(other) => panic!("unexpected error {other}"),
        Ok(_) => 101,
    }
}

async fn send(ws: &mut Ws, kind: Kind, body: impl serde::Serialize) -> Envelope {
    let envelope = Envelope::new(kind, "test", "hub", body);
    ws.send(Message::Text(envelope.encode().into()))
        .await
        .unwrap();
    envelope
}

async fn recv(ws: &mut Ws) -> Option<Envelope> {
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await
            .expect("no message within 10 s")?;
        match frame.ok()? {
            Message::Text(text) => return Some(Envelope::decode(text.as_str()).unwrap()),
            Message::Close(_) => return None,
            _ => {}
        }
    }
}

async fn call<T: DeserializeOwned>(ws: &mut Ws, method: &str, params: impl serde::Serialize) -> T {
    match call_raw(ws, method, params).await {
        RpcResult {
            result: Some(value),
            ..
        } => serde_json::from_value(value).unwrap(),
        other => panic!("{method} failed: {other:?}"),
    }
}

async fn call_raw(ws: &mut Ws, method: &str, params: impl serde::Serialize) -> RpcResult {
    let rpc = Rpc {
        method: method.to_owned(),
        params: serde_json::to_value(params).unwrap(),
    };
    let sent = send(ws, Kind::Rpc, rpc).await;
    loop {
        let message = recv(ws).await.unwrap();
        if message.re == Some(sent.id) {
            return message.body().unwrap();
        }
    }
}

fn bearer(daemon: &Daemon) -> Vec<(&'static str, String)> {
    vec![("authorization", format!("Bearer {}", daemon.token))]
}

/// A minimal HTTP/1.1 POST to `/auth`; returns the status and the session cookie, if any.
async fn sign_in(port: u16, code: &str) -> (u16, Option<String>) {
    let body = format!(r#"{{"code":"{code}"}}"#);
    let request = format!(
        "POST /auth HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    let status = response
        .split(' ')
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap();
    let cookie = response.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("set-cookie").then(|| {
            value
                .trim()
                .split(';')
                .next()
                .unwrap_or_default()
                .to_owned()
        })
    });
    (status, cookie)
}

#[tokio::test]
async fn refuses_strangers() {
    let daemon = start("strangers").await;
    let port = daemon.port;
    assert_eq!(status(connect(port, &[]).await), 401);
    let wrong = vec![("authorization", "Bearer nope".to_owned())];
    assert_eq!(status(connect(port, &wrong).await), 401);

    let mut evil_host = bearer(&daemon);
    evil_host.push(("host", format!("evil.example:{port}")));
    assert_eq!(status(connect(port, &evil_host).await), 421);

    let mut evil_origin = bearer(&daemon);
    evil_origin.push(("origin", "https://evil.example".to_owned()));
    assert_eq!(status(connect(port, &evil_origin).await), 403);
}

#[tokio::test]
async fn token_client_runs_and_inspects_jobs() {
    let daemon = start("jobs").await;
    let mut ws = connect(daemon.port, &bearer(&daemon)).await.unwrap();
    send(
        &mut ws,
        Kind::Hello,
        Hello {
            client: "test".to_owned(),
            version: "0".to_owned(),
            cwd: None,
        },
    )
    .await;
    let hello: Hello = recv(&mut ws).await.unwrap().body().unwrap();
    assert_eq!(hello.client, "assist-server");

    send(
        &mut ws,
        Kind::Utterance,
        Utterance {
            text: "$ echo hi".to_owned(),
        },
    )
    .await;
    let reply: Reply = recv(&mut ws).await.unwrap().body().unwrap();
    assert_eq!(reply.jobs.len(), 1);
    let id = reply.jobs[0].id;
    assert_eq!(reply.jobs[0].title, "$ echo hi");

    let mut output = Vec::new();
    loop {
        let message = recv(&mut ws).await.unwrap();
        assert_eq!(message.kind, Kind::JobUpdate);
        match message.body::<JobUpdate>().unwrap() {
            JobUpdate::Output { text, .. } => output.push(text),
            JobUpdate::State { state, .. } if state.is_finished() => {
                assert_eq!(state, JobState::Done);
                break;
            }
            JobUpdate::State { .. } => {}
        }
    }
    assert_eq!(output, ["echo hi"]);

    let jobs: Vec<JobSummary> = call(&mut ws, rpc::JOBS_LIST, ()).await;
    assert_eq!(jobs.iter().map(|j| j.id).collect::<Vec<_>>(), [id]);
    let tail: Vec<String> = call(&mut ws, rpc::JOBS_TAIL, JobRef { id }).await;
    assert_eq!(tail, ["echo hi"]);
    let stopped: bool = call(&mut ws, rpc::JOBS_STOP, JobRef { id }).await;
    assert!(!stopped, "already finished");
    let explained: Option<Explanation> = call(
        &mut ws,
        rpc::COMMANDS_EXPLAIN,
        rpc::Explain {
            text: "$ ls".to_owned(),
        },
    )
    .await;
    assert_eq!(explained.unwrap().decision, "allow");
    let unknown = call_raw(&mut ws, "jobs.fly", ()).await;
    assert_eq!(unknown.error.unwrap().code, "unknown_method");
}

#[tokio::test]
async fn browser_signs_in_once_with_a_code() {
    let daemon = start("browser").await;
    let mut cli = connect(daemon.port, &bearer(&daemon)).await.unwrap();
    let otp: Otp = call(&mut cli, rpc::AUTH_OTP, ()).await;

    let (status, cookie) = sign_in(daemon.port, &otp.code).await;
    assert_eq!(status, 204);
    let cookie = cookie.unwrap();
    assert!(cookie.starts_with("assist_session="));
    assert_eq!(
        sign_in(daemon.port, &otp.code).await.0,
        401,
        "codes work once"
    );

    let mut web = connect(daemon.port, &[("cookie", cookie)]).await.unwrap();
    let denied = call_raw(&mut web, rpc::AUTH_OTP, ()).await;
    assert_eq!(denied.error.unwrap().code, "denied");
}

#[tokio::test]
async fn secrets_are_write_only_over_the_wire() {
    let daemon = start("secrets").await;
    let mut ws = connect(daemon.port, &bearer(&daemon)).await.unwrap();
    let set = SecretSet {
        name: "openai".to_owned(),
        value: "sk-wire-test-12345678".to_owned(),
    };
    let _: bool = call(&mut ws, rpc::SECRETS_SET, set).await;
    let raw = call_raw(&mut ws, rpc::SECRETS_LIST, ()).await;
    let text = serde_json::to_string(&raw).unwrap();
    assert!(!text.contains("sk-wire"), "values never leave the daemon");
    let list: Vec<SecretEntry> = serde_json::from_value(raw.result.unwrap()).unwrap();
    assert_eq!(
        (list[0].name.as_str(), list[0].last4.as_str()),
        ("openai", "5678")
    );

    send(
        &mut ws,
        Kind::Utterance,
        Utterance {
            text: "$ echo sk-wire-test-12345678".to_owned(),
        },
    )
    .await;
    let reply: Reply = recv(&mut ws).await.unwrap().body().unwrap();
    assert_eq!(reply.jobs[0].title, "$ echo [secret:openai]");

    let deleted: bool = call(
        &mut ws,
        rpc::SECRETS_DELETE,
        SecretName {
            name: "openai".to_owned(),
        },
    )
    .await;
    assert!(deleted);
}

#[tokio::test]
async fn revoked_clients_are_disconnected() {
    let daemon = start("revoke").await;
    let mut admin = connect(daemon.port, &bearer(&daemon)).await.unwrap();
    let mut other = connect(daemon.port, &bearer(&daemon)).await.unwrap();
    let clients: Vec<rpc::ClientEntry> = call(&mut admin, rpc::CLIENTS_LIST, ()).await;
    assert_eq!(clients.len(), 2);
    let target = clients.iter().map(|c| c.id).max().unwrap();
    let revoked: bool = call(
        &mut admin,
        rpc::CLIENTS_REVOKE,
        rpc::ClientRef { id: target },
    )
    .await;
    assert!(revoked);
    assert!(recv(&mut other).await.is_none(), "connection closed");
}
