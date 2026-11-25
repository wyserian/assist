//! The Telegram bot (PLAN.md section 3.13): the owner talks to the hub from Telegram. The
//! daemon polls the Bot API (outbound only, nothing listens), and each allowed user gets a
//! connection of their own through the same [`conn::run`] as the CLI, with remote-owner
//! trust as `telegram:<user id>`. Shell commands and scripts are denied to it unless
//! `[sources.telegram] shell = true`. The bot token is the secret `telegram_bot_token`; it
//! is never logged (request errors drop their URL, which holds it).

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use assist_core::config::TelegramConfig;
use assist_proto::{Envelope, Hello, JobUpdate, Kind, Reply, Utterance};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::conn::{self, Transport};
use crate::{AppState, Credential};

/// The secret that holds the bot token.
pub const TOKEN_SECRET: &str = "telegram_bot_token";
const API: &str = "https://api.telegram.org";
/// How long one `getUpdates` waits for a message.
const POLL_S: u64 = 50;
/// Longest Telegram message; longer output is cut.
const MAX_MESSAGE: usize = 4000;
/// Longest request text taken from a message.
const MAX_INPUT: usize = 4096;
/// How long job output is collected into one message.
const BATCH: Duration = Duration::from_millis(800);
/// Messages queued per chat each way.
const QUEUE: usize = 32;
/// How often a missing or refused token is looked for again.
const TOKEN_CHECK: Duration = Duration::from_secs(30);
/// Most strangers remembered, so each is reported once.
const MAX_STRANGERS: usize = 64;

/// Runs the bot until `stop`, if `[sources.telegram]` enables it.
pub(crate) async fn run(state: AppState, stop: CancellationToken) {
    let config = state
        .services
        .engine
        .get()
        .config()
        .sources
        .telegram
        .clone();
    if !config.enabled {
        return;
    }
    if config.allowed_user_ids.is_empty() {
        println!(
            "Telegram is on but [sources.telegram] allowed_user_ids is empty; nobody can use it."
        );
    }
    let mut told = false;
    loop {
        let bot = match token(&state).await.and_then(|token| Bot::new(&token)) {
            Ok(bot) => bot,
            Err(why) => {
                if !told {
                    println!("Telegram is off for now: {why}");
                    told = true;
                }
                tokio::select! {
                    () = stop.cancelled() => return,
                    () = tokio::time::sleep(TOKEN_CHECK) => continue,
                }
            }
        };
        told = false;
        println!("Telegram bot is on.");
        if let Err(why) = poll(&state, &bot, &config, &stop).await {
            println!("Telegram stopped: {why}");
            tokio::select! {
                () = stop.cancelled() => return,
                () = tokio::time::sleep(TOKEN_CHECK) => {}
            }
        } else {
            return;
        }
    }
}

async fn token(state: &AppState) -> Result<String, String> {
    let secrets = state
        .services
        .secrets
        .clone()
        .ok_or_else(|| "no keystore on this machine".to_owned())?;
    let token = tokio::task::spawn_blocking(move || secrets.get(TOKEN_SECRET))
        .await
        .map_err(|err| err.to_string())?
        .map_err(|err| err.to_string())?;
    token.ok_or_else(|| format!("set the bot token with: assist secret set {TOKEN_SECRET}"))
}

/// Polls for messages until `stop`; an error ends it (a refused token, say).
async fn poll(
    state: &AppState,
    bot: &Bot,
    config: &TelegramConfig,
    stop: &CancellationToken,
) -> Result<(), String> {
    let mut offset = 0_i64;
    let mut chats: HashMap<i64, mpsc::Sender<String>> = HashMap::new();
    let mut strangers = HashSet::new();
    let mut backoff = Duration::from_secs(1);
    loop {
        let updates = tokio::select! {
            () = stop.cancelled() => return Ok(()),
            updates = bot.updates(offset) => updates,
        };
        let updates = match updates {
            Ok(updates) => {
                backoff = Duration::from_secs(1);
                updates
            }
            Err(Failure::Refused(why)) => return Err(why),
            Err(Failure::Network(_)) => {
                // Offline for a while: try again, slower each time.
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(60));
                continue;
            }
        };
        for update in updates {
            offset = offset.max(update.update_id + 1);
            let Some((user, chat, text)) = update.request(bot).await else {
                continue;
            };
            if !config.allowed_user_ids.contains(&user) {
                if strangers.len() < MAX_STRANGERS && strangers.insert(user) {
                    println!(
                        "Telegram: ignored a message from user {user}; add the id to \
                         [sources.telegram] allowed_user_ids to let them in."
                    );
                }
                continue;
            }
            if text.trim() == "/start" {
                bot.say(
                    chat,
                    "Hello. Tell me what to do, as you would on the computer.",
                    false,
                )
                .await;
                continue;
            }
            let utterance = Utterance {
                text: text.chars().take(MAX_INPUT).collect(),
                speaker_verified: false,
            };
            let message = Envelope::new(Kind::Utterance, "telegram", "hub", utterance).encode();
            let inbox = chats
                .entry(chat)
                .or_insert_with(|| open(state, bot, user, chat));
            if inbox.try_send(message.clone()).is_err() {
                // The connection ended (revoked) or is stuck: start a new one.
                let fresh = open(state, bot, user, chat);
                let _ = fresh.try_send(message);
                chats.insert(chat, fresh);
            }
        }
    }
}

/// Starts the connection for one chat; returns where its requests go.
fn open(state: &AppState, bot: &Bot, user: i64, chat: i64) -> mpsc::Sender<String> {
    let (inbox, read) = mpsc::channel(QUEUE);
    let (out, lines) = mpsc::channel(QUEUE);
    let hello = Hello {
        client: "telegram".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        cwd: None,
        voice: false,
        contacts: Vec::new(),
        push: None,
    };
    let _ = inbox.try_send(Envelope::new(Kind::Hello, "telegram", "hub", hello).encode());
    tokio::spawn(speak(bot.clone(), chat, lines));
    let transport = Chat { read, out };
    tokio::spawn(conn::run(
        state.clone(),
        transport,
        Credential::Telegram { user },
        None,
    ));
    inbox
}

/// One chat as a connection: requests in, replies and job output out.
struct Chat {
    read: mpsc::Receiver<String>,
    out: mpsc::Sender<Said>,
}

/// A message for the chat.
struct Said {
    text: String,
    /// A question: show Yes and No buttons.
    question: bool,
}

impl Transport for Chat {
    async fn recv(&mut self) -> Option<String> {
        self.read.recv().await
    }

    async fn send(&mut self, text: String) -> bool {
        match said(&text) {
            Some(said) => self.out.send(said).await.is_ok(),
            None => !self.out.is_closed(),
        }
    }

    async fn close(&mut self) {}
}

/// What the chat shows for one message from the hub, if anything.
fn said(frame: &str) -> Option<Said> {
    let message = Envelope::decode(frame).ok()?;
    match message.kind {
        Kind::Reply => {
            let reply: Reply = message.body().ok()?;
            let text = reply.say.join("\n");
            (!text.is_empty()).then_some(Said {
                text,
                question: reply.waiting,
            })
        }
        Kind::JobUpdate => match message.body::<JobUpdate>().ok()? {
            JobUpdate::Output { text, .. } => Some(Said {
                text,
                question: false,
            }),
            JobUpdate::State {
                error: Some(error), ..
            } => Some(Said {
                text: format!("Failed: {error}"),
                question: false,
            }),
            JobUpdate::State { .. } => None,
        },
        _ => None,
    }
}

/// Sends what the chat's connection says, gathering job output into fewer messages.
async fn speak(bot: Bot, chat: i64, mut lines: mpsc::Receiver<Said>) {
    while let Some(first) = lines.recv().await {
        if first.question {
            bot.say(chat, &first.text, true).await;
            continue;
        }
        let mut text = first.text;
        let deadline = tokio::time::Instant::now() + BATCH;
        let mut question = None;
        while let Ok(Some(next)) = tokio::time::timeout_at(deadline, lines.recv()).await {
            if next.question {
                question = Some(next);
                break;
            }
            text.push('\n');
            text.push_str(&next.text);
        }
        bot.say(chat, &text, false).await;
        if let Some(question) = question {
            bot.say(chat, &question.text, true).await;
        }
    }
}

/// Why a Bot API call failed.
enum Failure {
    /// Telegram refused the bot (a wrong or revoked token).
    Refused(String),
    /// Telegram could not be reached, or answered with an error worth retrying.
    Network(String),
}

/// The Bot API with the token. Never printed: the token is in `base`.
#[derive(Clone)]
struct Bot {
    http: reqwest::Client,
    base: String,
}

impl Bot {
    fn new(token: &str) -> Result<Self, String> {
        let token = token.trim();
        let valid = token.len() <= 128
            && token.contains(':')
            && token
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b':' || b == b'_' || b == b'-');
        if !valid {
            return Err(format!(
                "the bot token does not look like one from BotFather; set it again with: \
                 assist secret set {TOKEN_SECRET}"
            ));
        }
        // reqwest uses the process-wide TLS provider; ring is the one iroh brings.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(POLL_S + 15))
            .build()
            .map_err(|err| err.without_url().to_string())?;
        Ok(Self {
            http,
            base: format!("{API}/bot{token}"),
        })
    }

    /// Calls `method` with `body`; the answer's `result`.
    async fn call(&self, method: &str, body: Value) -> Result<Value, Failure> {
        let response = self
            .http
            .post(format!("{}/{method}", self.base))
            .json(&body)
            .send()
            .await
            .map_err(|err| Failure::Network(err.without_url().to_string()))?;
        let status = response.status();
        let answer: Answer = response
            .json()
            .await
            .map_err(|err| Failure::Network(err.without_url().to_string()))?;
        match answer {
            Answer {
                ok: true,
                result: Some(result),
                ..
            } => Ok(result),
            Answer { description, .. } => {
                let why = description.unwrap_or_else(|| status.to_string());
                if status == reqwest::StatusCode::UNAUTHORIZED
                    || status == reqwest::StatusCode::NOT_FOUND
                {
                    Err(Failure::Refused(format!(
                        "Telegram refused the bot token ({why}); set it again with: \
                         assist secret set {TOKEN_SECRET}"
                    )))
                } else {
                    Err(Failure::Network(why))
                }
            }
        }
    }

    /// The next updates after `offset`, waiting up to [`POLL_S`] for one.
    async fn updates(&self, offset: i64) -> Result<Vec<Update>, Failure> {
        let body = json!({
            "offset": offset,
            "timeout": POLL_S,
            "allowed_updates": ["message", "callback_query"],
        });
        let result = self.call("getUpdates", body).await?;
        serde_json::from_value(result).map_err(|err| Failure::Network(err.to_string()))
    }

    /// Sends `text` to `chat`, with Yes and No buttons for a question. Failures are dropped:
    /// the reply is lost, the request is not.
    async fn say(&self, chat: i64, text: &str, question: bool) {
        let mut body = json!({ "chat_id": chat, "text": cut(text) });
        if question {
            body["reply_markup"] = json!({ "inline_keyboard": [[
                { "text": "Yes", "callback_data": "yes" },
                { "text": "No", "callback_data": "no" },
            ]] });
        }
        if let Err(Failure::Network(why) | Failure::Refused(why)) =
            self.call("sendMessage", body).await
        {
            println!("Telegram: a reply was not sent: {why}");
        }
    }
}

/// `text` cut to [`MAX_MESSAGE`] characters.
fn cut(text: &str) -> String {
    if text.chars().count() <= MAX_MESSAGE {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(MAX_MESSAGE).collect();
    cut.push_str("\n(cut)");
    cut
}

/// A Bot API answer.
#[derive(Deserialize)]
struct Answer {
    ok: bool,
    result: Option<Value>,
    description: Option<String>,
}

/// One update from `getUpdates`; other kinds are ignored.
#[derive(Deserialize)]
struct Update {
    update_id: i64,
    message: Option<Message>,
    callback_query: Option<Callback>,
}

#[derive(Deserialize)]
struct Message {
    chat: ChatInfo,
    from: Option<User>,
    text: Option<String>,
}

#[derive(Deserialize)]
struct ChatInfo {
    id: i64,
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Deserialize)]
struct User {
    id: i64,
}

/// A tap on a Yes or No button.
#[derive(Deserialize)]
struct Callback {
    id: String,
    from: User,
    data: Option<String>,
    message: Option<Message>,
}

impl Update {
    /// Who asked, in which private chat, and the text; `None` for anything else (groups,
    /// pictures, edits). A button tap is answered so Telegram stops its spinner.
    async fn request(self, bot: &Bot) -> Option<(i64, i64, String)> {
        if let Some(tap) = self.callback_query {
            let _ = bot
                .call(
                    "answerCallbackQuery",
                    json!({ "callback_query_id": tap.id }),
                )
                .await;
            let chat = tap.message.filter(|m| m.chat.kind == "private")?.chat.id;
            return Some((tap.from.id, chat, tap.data?));
        }
        let message = self.message.filter(|m| m.chat.kind == "private")?;
        Some((message.from?.id, message.chat.id, message.text?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn updates_parse_and_only_private_text_counts() {
        let updates: Vec<Update> = serde_json::from_str(
            r#"[
              {"update_id": 7, "message": {"message_id": 1, "date": 0,
                "chat": {"id": 42, "type": "private"}, "from": {"id": 42, "is_bot": false},
                "text": "open firefox"}},
              {"update_id": 8, "message": {"message_id": 2, "date": 0,
                "chat": {"id": -5, "type": "group"}, "from": {"id": 42}, "text": "hi"}},
              {"update_id": 9, "edited_message": {"text": "x"}}
            ]"#,
        )
        .unwrap();
        assert_eq!(updates.len(), 3);
        let first = updates[0].message.as_ref().unwrap();
        assert_eq!(
            (first.chat.id, first.text.as_deref()),
            (42, Some("open firefox"))
        );
        assert_eq!(updates[1].message.as_ref().unwrap().chat.kind, "group");
        assert!(updates[2].message.is_none() && updates[2].callback_query.is_none());
    }

    #[test]
    fn replies_and_output_become_chat_messages() {
        let reply = Reply {
            say: vec!["Call Mom?".to_owned()],
            jobs: Vec::new(),
            waiting: true,
        };
        let said_reply =
            said(&Envelope::new(Kind::Reply, "hub", "telegram", reply).encode()).unwrap();
        assert_eq!(
            (said_reply.text.as_str(), said_reply.question),
            ("Call Mom?", true)
        );

        let failed = JobUpdate::State {
            id: 3,
            state: assist_proto::JobState::Failed,
            error: Some("no phone".to_owned()),
        };
        let said_failed =
            said(&Envelope::new(Kind::JobUpdate, "hub", "telegram", failed).encode()).unwrap();
        assert_eq!(said_failed.text, "Failed: no phone");
        let hello = Envelope::new(Kind::Hello, "hub", "telegram", serde_json::json!({}));
        assert!(said(&hello.encode()).is_none());
    }

    #[test]
    fn long_text_is_cut_and_bad_tokens_are_refused() {
        assert_eq!(cut("short"), "short");
        assert!(cut(&"x".repeat(MAX_MESSAGE + 10)).ends_with("(cut)"));
        assert!(Bot::new("not a token").is_err());
        assert!(Bot::new("123:abc/../x").is_err());
    }
}
