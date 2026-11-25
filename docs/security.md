# Security

assist can open apps, run commands, call and text. This page explains what keeps that under
your control: who may connect, how much each source is trusted, what asks first, and what is
recorded.

## Who can connect

- **Loopback only.** The daemon listens on `127.0.0.1:7720` (`[server] bind` accepts only
  loopback addresses). Nothing on your network can reach it.
- **The CLI** sends the random token from `<data dir>/token` (readable by your user only).
  Tokens are compared in constant time, and failed attempts are slowed down.
- **The browser** signs in through `assist ui`: a one-time code in the link's `#` fragment
  is exchanged for an `HttpOnly`, `SameSite=Strict` session cookie.
- **Host and Origin** are checked on every HTTP and WebSocket request, which stops other
  websites and DNS rebinding.
- **Paired devices** connect over iroh with their own key; the computer accepts only keys it
  paired. An unknown key may only pair, with a one-time code that works once for 5 minutes.
  See [devices.md](devices.md).
- **Webhooks** (`POST /hook/<name>`) need that hook's own token (the secret
  `webhook_<name>`), are limited to one call a second, and only start a saved pipe marked
  `unattended`; the request body is never read.
- **Revoking** a client in the dashboard's Clients tab disconnects it at once.

## Trust levels

Every request carries where it came from and a trust level:

| Trust | Who |
| --- | --- |
| `local` | The CLI with the token, the dashboard, the phone app used on the phone itself |
| `owner_remote` | A paired device acting on the computer, the computer's requests on the phone, an allowed Telegram user |
| `voice` | A microphone: anyone in the room, a TV included |
| `untrusted` | Content: OCR, transcripts, messages, web pages, later LLM proposals and webhooks |
| `autopilot` | Autopilot goals, running with nobody watching |

A client can lower its own trust (`assist listen` asks for `voice`), never raise it.
Schedules and triggers are unattended: they run only saved pipes marked `unattended` and
can never confirm anything.

## The policy table

Each action declares capabilities. For each capability and trust level the table decides
`allow`, `confirm` or `deny`; the strictest of the action's capabilities wins.

| Capability | Used by | local | owner_remote | voice | untrusted | autopilot |
| --- | --- | --- | --- | --- | --- | --- |
| `read` | search, ocr, screenshot, recall | allow | allow | allow | confirm | allow |
| `apps` | open and close apps | allow | allow | allow | confirm | allow |
| `media` | volume, lock | allow | allow | allow | confirm | allow |
| `shell` | `$ cmd`, scripts | confirm (typed `$ cmd`: allow) | confirm | deny | confirm | deny |
| `fs.write` | `save`, file writes | confirm | confirm | confirm | confirm | confirm |
| `delete` | forget, deletes | confirm | confirm | confirm | confirm | confirm |
| `rename.bulk` | rename, undo | confirm | confirm | confirm | confirm | confirm |
| `send` | call, send_sms | confirm | confirm | confirm | confirm | confirm |
| `net.scan` | network tools (planned) | confirm | deny | deny | deny | deny |
| `secrets.write` | setting secrets | allow | deny | deny | deny | deny |
| `config.write` | changing settings | allow | deny | deny | deny | deny |
| `admin` | stop all, revoking clients | allow | deny | deny | deny | deny |
| `record` | microphone and screen recording | allow | confirm | confirm | deny | deny |
| `memory.write` | remember | allow | confirm | confirm | deny | deny |

- Telegram is also denied `shell` (shell commands and scripts) unless
  `[sources.telegram] shell = true`.
- `risky = true` on a command (`[commands.<name>]` or the dashboard) always asks first.
- `[policy]` in the config replaces single cells (see [config.md](config.md#policy)):
  ```toml
  [policy]
  shell = { owner_remote = "deny" }
  "fs.write" = { local = "allow" }
  ```
- `assist explain <text>` shows the decision for a sentence without running it.

## Confirmations

- A confirmation reads back exactly what will run ("Text Mom: running late?", every file of
  a rename, every stage of a pipe), and the "yes" is bound to that plan: nothing else runs on
  it.
- A question expires after 60 seconds.
- A conversation belongs to its client: another client cannot answer it, except you from the
  dashboard's Waiting tab.
- By voice, a "yes" confirms only when it matches your voiceprint (`assist enroll`);
  otherwise it must be confirmed on screen.
- A command the computer runs on the paired phone is confirmed on the computer; the phone
  does not ask again, and runs only its own phone commands.

## Data is never a command

Text from OCR, transcripts, recordings, messages, web pages and push messages is data. It can
fill a pipe or propose an action, but it never goes back into the matcher, so text on a
screen that says "delete everything" does nothing. A push wake-up carries no content at all.

## Secrets

- Stored in the OS keystore (Windows Credential Manager, Secret Service on Linux), with only
  their names and metadata in `secrets.json`.
- Write-only: you can list names and the last 4 characters, set, replace and delete; nothing
  reads a value back over the wire.
- Values are redacted from job output, logs and the audit log.
- Only local clients may change them.

## Audit log

Every job (who asked, what ran, the decision, the result) and every admin change is written
to `<data dir>/audit/YYYY-MM.jsonl`. Each record holds the hash of the one before, so an edit
or deletion breaks the chain. Secrets are redacted. View it in the dashboard's Audit tab.

## Files and processes

- Everything in the config and data directories is created readable by your user only.
- Stopping a job, or its timeout, kills its whole process tree.
- Input is bounded: input lines, messages (1 MiB frames), job output, histories and queues
  all have limits.
- GPL programs (speech-dispatcher, ffmpeg builds) run as separate processes, never linked.

## Memory and privacy

- Notes ("remember ...") stay on the device, are listed with their source, and can be
  forgotten one by one or all at once.
- The voiceprint stays in the data directory.
- Voice is processed on the device; the voice models are downloaded only after you agree.
- Network traffic: the iroh link between your devices (end-to-end encrypted, possibly through
  n0's relays), the ntfy push (empty messages), the Telegram Bot API when the bot is on (your
  messages and replies pass through Telegram), web searches you ask for and model downloads.

## Reporting a problem

Found a security issue? Open a private security advisory on the GitHub repository instead of
a public issue.
