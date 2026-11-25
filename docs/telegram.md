# Telegram

Talk to assist on your computer from Telegram, from anywhere: "run backup", "text mom saying
on my way" (sent by the paired phone), "what's the wifi password". Replies, questions and
job output come back in the chat.

The computer only reaches out to Telegram (it polls the Bot API); nothing on your network is
opened.

## Set it up

1. **Make a bot.** In Telegram, open **@BotFather**, send `/newbot` and follow it. It gives
   you a token like `123456789:AAF...`. Keep it private.
2. **Store the token** on the computer as a secret (it is never shown again, and never
   logged):
   ```sh
   assist secret set telegram_bot_token
   ```
3. **Turn the bot on** in `config.toml`:
   ```toml
   [sources.telegram]
   enabled = true
   allowed_user_ids = []
   ```
4. **Find your user id.** Restart `assist serve`, then send your bot any message. The
   daemon prints:
   ```text
   Telegram: ignored a message from user 123456789; add the id to [sources.telegram]
   allowed_user_ids to let them in.
   ```
   Put that number in the list and restart again:
   ```toml
   allowed_user_ids = [123456789]
   ```
5. Send `/start`, then any request.

## Using it

- Write what you would type on the computer: `open firefox`, `run backup`, `search rust
  tokio`, `call mom`.
- Questions come with **Yes** and **No** buttons; typing `yes` or `no` works too.
- Job output arrives as messages, gathered a little so a busy job does not flood the chat.
  Very long output is cut.
- Phone commands ("text mom ...") run on the paired phone, which is woken if needed (see
  [devices.md](devices.md)).
- Each request appears in the dashboard (Jobs, Waiting, Audit, Clients) with the source
  `telegram:<your id>`.

## What Telegram may do

Messages from allowed users get `owner_remote` trust, like a paired phone, with one change:

- **Shell commands and scripts are refused** (`$ ...`, `run backup`), so a stolen Telegram
  account cannot run commands on your computer. To allow them (they still ask first):
  ```toml
  [sources.telegram]
  shell = true
  ```
- Calls, texts, file writes, recording and "remember" ask first.
- Settings, secrets and admin actions are not reachable from Telegram.
- Messages from anyone not in `allowed_user_ids`, and anything in groups, are ignored.

Change any of this with `[policy]` (see [security.md](security.md#the-policy-table)).

## Reference

```toml
[sources.telegram]
enabled = false          # run the bot
allowed_user_ids = []    # Telegram user ids allowed to use it
shell = false            # allow shell commands and scripts from Telegram (asked first)
```

| Secret | What |
| --- | --- |
| `telegram_bot_token` | The token from BotFather |

## Troubleshooting

| The daemon says | Do |
| --- | --- |
| `Telegram is off for now: set the bot token with: assist secret set telegram_bot_token` | Store the token; the daemon picks it up within 30 seconds |
| `Telegram refused the bot token (Unauthorized)` | The token is wrong or was revoked in BotFather; set it again |
| `Telegram is on but [sources.telegram] allowed_user_ids is empty` | Add your user id (step 4) |
| Nothing, and the bot does not answer | Check `enabled = true`, restart `assist serve`, and check the computer is online |

To stop using it, set `enabled = false` (or delete the secret) and restart the daemon. In
BotFather, `/revoke` makes the old token useless.
