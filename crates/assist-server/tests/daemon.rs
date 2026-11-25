//! End-to-end: a real daemon on a random loopback port, real WebSocket and HTTP clients.

// Test-only file: helpers fail the test on any error.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use assist_core::action::{
    Action, ActionError, BoxFuture, Chunk, Input, JobCtx, Lane, Manifest, PipeShape,
};
use assist_core::audit::Audit;
use assist_core::config::Config;
use assist_core::dialog::{Device, Engine, LiveEngine, SHELL};
use assist_core::facts::Facts;
use assist_core::phrases::PhraseSet;
use assist_core::policy::Capability;
use assist_core::scheduler::{Persistence, Scheduler};
use assist_core::secrets::{SecretError, SecretStore, Secrets};
use assist_core::store::Store;
use assist_proto::rpc::{
    self, AuditEntry, Explanation, JobRef, Otp, SecretEntry, SecretName, SecretSet,
};
use assist_proto::{
    Envelope, Hello, JobState, JobSummary, JobUpdate, Kind, Notice, Reply, Rpc, RpcResult,
    Subscribe, Utterance,
};
use assist_server::{load_or_create_token, serve, Nodes, Services};
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
    pipe: PipeShape::SOURCE,
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
    /// The hub's iroh address, for device tests.
    hub: iroh::EndpointAddr,
    dir: PathBuf,
    stop: Option<oneshot::Sender<()>>,
    notices: tokio::sync::broadcast::Sender<Notice>,
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
    let audit = Audit::open(&dir.join("audit")).unwrap();
    let scheduler = Scheduler::new(
        vec![Arc::new(Echo)],
        Arc::clone(&config),
        Persistence {
            audit: Some(audit.clone()),
            store: Some(store.clone()),
            redactor: Some(secrets.redactor()),
            data_dir: None,
            processes: None,
        },
    );
    let engine = Arc::new(LiveEngine::new(Arc::new(
        Engine::new(
            config,
            &PhraseSet::default_en().unwrap(),
            Device {
                commands: scheduler.commands(),
                offer_shell: true,
                facts: Facts::default(),
                preview: None,
            },
        )
        .unwrap(),
    )));
    let token = load_or_create_token(&dir).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (stop, stopped) = oneshot::channel::<()>();
    let notices = tokio::sync::broadcast::channel(16).0;
    let endpoint = local_endpoint().await;
    let hub = endpoint.addr();
    let nodes = Nodes {
        endpoint,
        peers: Mutex::new(assist_net::Peers::load(&dir).unwrap()),
        pairing: assist_net::Pairing::default(),
        name: "test hub".to_owned(),
    };
    let services = Services {
        facts: Facts::default(),
        notices: notices.clone(),
        scheduler,
        engine,
        store,
        audit: Some(audit),
        secrets: Some(secrets),
        default_cwd: std::env::temp_dir(),
        data_dir: dir.clone(),
        user_phrases: Some(dir.join("phrases").join("user.toml")),
        config_file: Some(dir.join("config.toml")),
        nodes: Some(nodes),
    };
    tokio::spawn(serve(services, listener, async {
        let _ = stopped.await;
    }));
    Daemon {
        port,
        token,
        hub,
        dir,
        stop: Some(stop),
        notices,
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
async fn notices_reach_only_subscribed_clients() {
    let daemon = start("notices").await;
    let hello = Hello {
        client: "test".to_owned(),
        version: "0".to_owned(),
        cwd: None,
        voice: false,
    };
    let mut listening = connect(daemon.port, &bearer(&daemon)).await.unwrap();
    let mut other = connect(daemon.port, &bearer(&daemon)).await.unwrap();
    for ws in [&mut listening, &mut other] {
        send(ws, Kind::Hello, hello.clone()).await;
        recv(ws).await.unwrap();
    }
    let topics = Subscribe {
        topics: vec!["notices".to_owned()],
    };
    send(&mut listening, Kind::Subscribe, topics).await;
    // A round trip, so the subscription is in place before the notice goes out.
    send(&mut listening, Kind::Hello, hello.clone()).await;
    recv(&mut listening).await.unwrap();

    let notice = Notice {
        rule: "build-failed".to_owned(),
        text: "Your build failed.".to_owned(),
    };
    daemon.notices.send(notice.clone()).unwrap();
    let got = recv(&mut listening).await.unwrap();
    assert_eq!(got.kind, Kind::Notice);
    assert_eq!(got.body::<Notice>().unwrap(), notice);
    // The other client gets its own next answer, not the notice.
    send(&mut other, Kind::Hello, hello).await;
    assert_eq!(recv(&mut other).await.unwrap().kind, Kind::Hello);
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
            voice: false,
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
            speaker_verified: false,
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

    // Audit records are written off the runtime; give them a moment.
    let mut events = Vec::new();
    for _ in 0..100 {
        let entries: Vec<AuditEntry> =
            call(&mut ws, rpc::AUDIT_TAIL, rpc::AuditPage { limit: Some(10) }).await;
        events = entries.into_iter().map(|e| e.event).collect();
        if events.len() >= 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(events, ["submitted", "running", "done"]);
}

#[tokio::test]
async fn voice_client_gets_voice_trust() {
    let daemon = start("voice").await;
    let mut ws = connect(daemon.port, &bearer(&daemon)).await.unwrap();
    let hello = Hello {
        client: "test".to_owned(),
        version: "0".to_owned(),
        cwd: None,
        voice: true,
    };
    send(&mut ws, Kind::Hello, hello).await;
    recv(&mut ws).await.unwrap();

    // Shell is denied for voice, and nothing local-only is allowed.
    let utterance = Utterance {
        text: "$ echo hi".to_owned(),
        speaker_verified: false,
    };
    send(&mut ws, Kind::Utterance, utterance).await;
    let reply: Reply = recv(&mut ws).await.unwrap().body().unwrap();
    assert!(reply.jobs.is_empty(), "{reply:?}");
    let name = SecretName {
        name: "x".to_owned(),
    };
    let denied = call_raw(&mut ws, rpc::SECRETS_DELETE, name).await;
    assert_eq!(denied.error.unwrap().code, "denied");
}

/// A minimal HTTP/1.1 GET; returns the whole response.
async fn get(port: u16, path: &str) -> String {
    let request =
        format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    response
}

#[tokio::test]
async fn serves_the_page_with_strict_headers() {
    let daemon = start("page").await;
    let page = get(daemon.port, "/").await;
    let lower = page.to_ascii_lowercase();
    assert!(page.starts_with("HTTP/1.1 200"), "{page}");
    assert!(lower.contains("content-type: text/html"), "{page}");
    assert!(
        lower.contains("content-security-policy: default-src 'self'"),
        "{page}"
    );
    assert!(lower.contains("x-content-type-options: nosniff"), "{page}");
    assert!(page.contains("<title>assist</title>"));
    let script = get(daemon.port, "/app.js").await.to_ascii_lowercase();
    assert!(script.contains("content-type: text/javascript"));
    assert!(get(daemon.port, "/nope").await.starts_with("HTTP/1.1 404"));
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
            speaker_verified: false,
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
async fn another_client_answers_a_waiting_question() {
    let daemon = start("waiting").await;
    let mut asker = connect(daemon.port, &bearer(&daemon)).await.unwrap();
    let mut dashboard = connect(daemon.port, &bearer(&daemon)).await.unwrap();

    let text = Utterance {
        text: "make me a sandwich".to_owned(),
        speaker_verified: false,
    };
    send(&mut asker, Kind::Utterance, text).await;
    let reply: Reply = recv(&mut asker).await.unwrap().body().unwrap();
    assert!(reply.waiting);

    let waiting: Vec<rpc::Waiting> = call(&mut dashboard, rpc::DIALOGS_PENDING, ()).await;
    assert_eq!(waiting.len(), 1);
    assert!(waiting[0]
        .question
        .contains("Run \"make me a sandwich\" in the shell?"));

    let answer = rpc::DialogAnswer {
        client: waiting[0].client,
        text: "yes".to_owned(),
    };
    let answered: Reply = call(&mut dashboard, rpc::DIALOGS_ANSWER, answer.clone()).await;
    assert_eq!(answered.jobs.len(), 1);
    assert!(!answered.waiting);

    // The asker gets the same reply and follows the job.
    let pushed: Reply = recv(&mut asker).await.unwrap().body().unwrap();
    assert_eq!(pushed, answered);
    let update: JobUpdate = loop {
        let message = recv(&mut asker).await.unwrap();
        if let Ok(JobUpdate::Output { id, stream, text }) = message.body() {
            break JobUpdate::Output { id, stream, text };
        }
    };
    assert!(matches!(update, JobUpdate::Output { ref text, .. } if text == "make me a sandwich"));

    let none: Vec<rpc::Waiting> = call(&mut dashboard, rpc::DIALOGS_PENDING, ()).await;
    assert!(none.is_empty());
    let again = call_raw(&mut dashboard, rpc::DIALOGS_ANSWER, answer).await;
    assert_eq!(again.error.unwrap().code, "not_found");
}

#[tokio::test]
async fn unmatched_sentences_become_phrases() {
    let daemon = start("misses").await;
    let mut ws = connect(daemon.port, &bearer(&daemon)).await.unwrap();
    for _ in 0..2 {
        let text = Utterance {
            text: "find me rust tokio".to_owned(),
            speaker_verified: false,
        };
        send(&mut ws, Kind::Utterance, text).await;
        let reply: Reply = recv(&mut ws).await.unwrap().body().unwrap();
        assert!(reply.waiting, "unmatched input gets the shell offer");
        send(
            &mut ws,
            Kind::Utterance,
            Utterance {
                text: "no".to_owned(),
                speaker_verified: false,
            },
        )
        .await;
        recv(&mut ws).await.unwrap();
    }

    // Misses are written off the runtime; give them a moment.
    let mut misses: Vec<rpc::Miss> = Vec::new();
    for _ in 0..100 {
        misses = call(&mut ws, rpc::MISSES_LIST, ()).await;
        if misses.first().is_some_and(|m| m.count == 2) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(misses[0].text, "find me rust tokio");
    assert_eq!(misses[0].count, 2);

    let commands: Vec<rpc::CommandEntry> = call(&mut ws, rpc::COMMANDS_LIST, ()).await;
    let search = commands.iter().find(|c| c.name == "search").unwrap();
    assert_eq!(search.slots["query"], "text");
    assert!(!search.available, "the test daemon only runs shell");

    let bad = rpc::PhraseAdd {
        command: "search".to_owned(),
        phrase: "find me .nope.".to_owned(),
        miss: None,
    };
    let refused = call_raw(&mut ws, rpc::PHRASES_ADD, bad).await;
    assert_eq!(
        refused.error.unwrap().code,
        "bad_request",
        "undeclared slot"
    );

    let add = rpc::PhraseAdd {
        command: "search".to_owned(),
        phrase: "find me .query.".to_owned(),
        miss: Some(misses[0].text.clone()),
    };
    let added: bool = call(&mut ws, rpc::PHRASES_ADD, add).await;
    assert!(added);
    let file = daemon.dir.join("phrases").join("user.toml");
    assert!(std::fs::read_to_string(file)
        .unwrap()
        .contains("find me .query."));

    let explained: Option<Explanation> = call(
        &mut ws,
        rpc::COMMANDS_EXPLAIN,
        rpc::Explain {
            text: "find me rust tokio".to_owned(),
        },
    )
    .await;
    let explained = explained.expect("the new phrase matches at once");
    assert_eq!(explained.slots["query"], "rust tokio");

    let mut left = 1;
    for _ in 0..100 {
        let misses: Vec<rpc::Miss> = call(&mut ws, rpc::MISSES_LIST, ()).await;
        left = misses.len();
        if left == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(left, 0, "the miss is forgotten once it is a phrase");
}

#[tokio::test]
async fn settings_are_saved_and_risky_applies_at_once() {
    let daemon = start("settings").await;
    let mut ws = connect(daemon.port, &bearer(&daemon)).await.unwrap();
    let settings: rpc::Settings = call(&mut ws, rpc::SETTINGS_GET, ()).await;
    assert_eq!(settings.lanes_interactive, 2);
    assert!(!settings.restart_pending);
    assert!(settings
        .commands
        .iter()
        .any(|c| c.name == "shell" && !c.risky));

    let set = |key: &str, value: serde_json::Value| rpc::SettingChange {
        key: key.to_owned(),
        value,
    };
    let result: rpc::SettingResult = call(
        &mut ws,
        rpc::SETTINGS_SET,
        set("commands.shell.risky", serde_json::json!(true)),
    )
    .await;
    assert!(!result.restart_needed);

    // Risky applies to the very next request.
    let text = Utterance {
        text: "$ echo hi".to_owned(),
        speaker_verified: false,
    };
    send(&mut ws, Kind::Utterance, text).await;
    let reply: Reply = recv(&mut ws).await.unwrap().body().unwrap();
    assert!(reply.waiting, "typed shell now asks first: {reply:?}");
    assert!(reply.jobs.is_empty());

    let result: rpc::SettingResult = call(
        &mut ws,
        rpc::SETTINGS_SET,
        set("lanes.interactive", serde_json::json!(4)),
    )
    .await;
    assert!(result.restart_needed);
    let settings: rpc::Settings = call(&mut ws, rpc::SETTINGS_GET, ()).await;
    assert_eq!(settings.lanes_interactive, 4);
    assert!(settings.restart_pending);

    for (key, value) in [
        ("commands.sned_sms.risky", serde_json::json!(true)),
        ("server.bind", serde_json::json!("0.0.0.0:1")),
        ("lanes.interactive", serde_json::json!(0)),
        ("lanes.interactive", serde_json::json!([1])),
    ] {
        let refused = call_raw(&mut ws, rpc::SETTINGS_SET, set(key, value)).await;
        assert_eq!(refused.error.unwrap().code, "bad_request", "{key}");
    }
    let saved = std::fs::read_to_string(daemon.dir.join("config.toml")).unwrap();
    assert!(
        saved.contains("interactive = 4") && saved.contains("risky = true"),
        "{saved}"
    );
    assert!(
        !saved.contains("sned_sms"),
        "refused changes are not written"
    );
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

/// An iroh endpoint for tests: no relays, reachable on this machine only.
async fn local_endpoint() -> iroh::Endpoint {
    iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .alpns(vec![assist_net::ALPN.to_vec()])
        .bind()
        .await
        .unwrap()
}

/// Connects `device` to the hub, sends `first` and returns the hub's first answer.
async fn device_says(
    device: &iroh::Endpoint,
    hub: &iroh::EndpointAddr,
    first: Envelope,
) -> (
    iroh::endpoint::SendStream,
    iroh::endpoint::RecvStream,
    Envelope,
) {
    let connection = device.connect(hub.clone(), assist_net::ALPN).await.unwrap();
    let (mut send, mut recv) = connection.open_bi().await.unwrap();
    assist_net::send_frame(&mut send, &first.encode())
        .await
        .unwrap();
    let answer = tokio::time::timeout(Duration::from_secs(10), assist_net::recv_frame(&mut recv))
        .await
        .expect("no answer within 10 s")
        .unwrap()
        .expect("the hub closed the stream");
    // The connection stays open while its streams live.
    std::mem::forget(connection);
    (send, recv, Envelope::decode(&answer).unwrap())
}

fn pair_request(secret: &str) -> Envelope {
    let pair = assist_proto::Pair {
        secret: secret.to_owned(),
        name: "Test phone".to_owned(),
        capabilities: vec!["send_sms".to_owned()],
    };
    Envelope::new(Kind::Pair, "phone", "hub", pair)
}

#[tokio::test]
async fn a_device_pairs_once_and_then_talks_as_the_remote_owner() {
    let daemon = start("pair").await;
    let device = local_endpoint().await;

    // A stranger without a code is refused.
    let (_, _, refused) =
        device_says(&device, &daemon.hub, pair_request("0123456789abcdef0123")).await;
    assert_eq!(refused.kind, Kind::RpcResult);
    let refused: RpcResult = refused.body().unwrap();
    assert_eq!(refused.error.unwrap().code, "denied");

    // The CLI makes a code; the device pairs with it once.
    let mut cli = connect(daemon.port, &bearer(&daemon)).await.unwrap();
    let code: String = call(&mut cli, rpc::PAIR_START, ()).await;
    let ticket = assist_net::Ticket::parse(&code).unwrap();
    assert_eq!(ticket.hub, daemon.hub.id);
    let (_, _, paired) = device_says(&device, &daemon.hub, pair_request(&ticket.secret)).await;
    assert_eq!(paired.kind, Kind::Paired, "{paired:?}");
    let hub: assist_proto::Paired = paired.body().unwrap();
    assert_eq!(hub.name, "test hub");

    // Now it talks like a client, with remote-owner trust: a typed shell command asks first.
    let utterance = Utterance {
        text: "$ echo hi".to_owned(),
        speaker_verified: false,
    };
    let (_, _, reply) = device_says(
        &device,
        &daemon.hub,
        Envelope::new(Kind::Utterance, "phone", "hub", utterance),
    )
    .await;
    assert_eq!(reply.kind, Kind::Reply, "{reply:?}");
    let reply: Reply = reply.body().unwrap();
    assert!(reply.jobs.is_empty() && reply.waiting, "{reply:?}");

    // The used code is gone, and only the CLI may make codes.
    let other = local_endpoint().await;
    let (_, _, again) = device_says(&other, &daemon.hub, pair_request(&ticket.secret)).await;
    assert_eq!(again.kind, Kind::RpcResult);
}
