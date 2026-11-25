# assist

An offline-first personal assistant you control by typing or by voice. It runs on your
computer (Windows and Linux) and on your Android phone, keeps everything on your own devices,
and asks before it does anything risky.

```text
> open firefox
Opening firefox.
> text mom saying running late
Text mom: running late?
> yes
Sending.
> run build | filter error | save build-errors.txt
```

Version 0.6.0. Pre-release: the desktop and the phone app work every day; the multi-device
features are being built (see [Status](#status)).

## Contents

- [Features](#features)
- [Quick start: computer](#quick-start-computer)
- [Quick start: phone](#quick-start-phone)
- [Examples](#examples)
- [Documentation](#documentation)
- [How it works](#how-it-works)
- [Security](#security)
- [Status](#status)
- [Building from source](#building-from-source)
- [Repository layout](#repository-layout)
- [License](#license)

## Features

- **Commands in plain words.** "open firefox", "close code", "search rust tokio", "run
  backup", "set volume to 40", "remember the wifi password is on the fridge". Phrases are
  editable TOML, and sentences it did not understand can be learned from the dashboard.
- **Voice, offline.** Wake word ("hey Bee"), speech to text and spoken replies on the device,
  with no cloud service. A recorded voiceprint lets a spoken "yes" confirm calls and texts.
- **Jobs.** Everything runs as a job you can list, pause, resume, stop and read the log of.
  Jobs run in parallel lanes with timeouts. STOP ALL stops everything.
- **Pipes.** Chain commands and filters: `ocr screen | filter error | save errors.txt`.
  Saved pipes, retries, error handlers and conditions over device facts.
- **Automation.** Schedules ("07:30 mon-fri"), folder watches, fact triggers ("battery below
  20"), proactive messages with quiet hours, and opt-in autopilot goals with budgets.
- **Desktop.** Open, close and switch apps (found automatically on Windows), scripts, the system shell
  (`$ ls`), screenshots, screen OCR (Windows), screen and audio recording, transcription,
  bulk rename with undo, web search.
- **Phone app.** Calls, texts by contact name, volume, lock, open apps, screenshots, screen
  OCR, recording, battery rules, push-to-talk and a hands-free wake word.
- **Computer and phone together.** Pair the phone with a QR code. Say "call mom" on the
  computer and the phone makes the call. A phone that is away is woken by push.
- **Telegram.** Talk to assist from anywhere through your own Telegram bot; shell stays off
  unless you allow it.
- **Dashboard.** A local web page with chat, jobs, pending confirmations, memory, autopilot,
  secrets, commands, settings, audit log, paired devices and connected clients.

## Quick start: computer

You need [Rust](https://rustup.rs) (the version in `rust-toolchain.toml` installs itself).

```sh
git clone https://github.com/wyserian/assist.git
cd assist
cargo install --path crates/assist-cli
```

Try it without a daemon:

```sh
assist shell
```

Or run the daemon and talk to it:

```sh
assist serve                # keep this running
assist run open firefox     # in another terminal
assist ui                   # the dashboard, signed in
assist listen               # voice; asks before downloading the models
```

To start the daemon at login: `assist install-autostart`.

Configuration lives in `config.toml` in your config directory (`%APPDATA%\assist\config` on
Windows, `~/.config/assist` on Linux). Start from
[`config/config.example.toml`](config/config.example.toml). Details:
[docs/install.md](docs/install.md) and [docs/config.md](docs/config.md).

## Quick start: phone

1. Build and install the app (see [docs/android.md](docs/android.md)):
   ```sh
   just android
   adb install -r android/app/build/outputs/apk/debug/app-debug.apk
   ```
2. Open Assist and allow contacts. Type or tap the mic and say what you want.
3. To link it with the computer, run `assist pair` there and scan the code in the app with
   **Link to computer**.
4. Optional: install the **ntfy** app so the computer can wake the phone while Assist is
   closed.

## Examples

| You say or type | What happens |
| --- | --- |
| `open firefox` / `close firefox` | Opens the app, or brings it to the front if it is open; closes it |
| `search rust tokio` | Opens a web search |
| `run backup` | Runs a script from `[scripts]`, after asking |
| `$ git status` | Runs in the system shell |
| `call mom` / `text mom saying hi` | Calls or texts, after asking; from the computer it runs on the paired phone |
| `set volume to 30` | Sets the volume |
| `take a screenshot` / `read the screen` | Saves a screenshot / reads the text on screen |
| `record screen for 30s to demo.mkv` | Records the screen (needs ffmpeg on the computer) |
| `rename IMG_ to trip_ in ~/Pictures` | Previews every change, asks, renames; `undo` reverses it |
| `remember the spare key is in the drawer` | Stores a note; `where is the spare key` finds it |
| `ocr screen \| filter error \| count` | A pipe of three stages |
| `assist stop all` | Stops every job (also the STOP ALL button, or `stop all` in the shell) |

The full list is in [docs/commands.md](docs/commands.md).

## Documentation

| Page | Covers |
| --- | --- |
| [docs/README.md](docs/README.md) | Index of the documentation |
| [Install](docs/install.md) | Requirements, installing, autostart, file locations, updating |
| [CLI](docs/cli.md) | Every `assist` command and option, and the interactive shell |
| [Commands](docs/commands.md) | Everything you can say or type, with examples |
| [Pipes and automation](docs/automation.md) | Pipes, saved pipes, schedules, triggers, facts, proactive rules, autopilot |
| [Configuration](docs/config.md) | Every `config.toml` section and key |
| [Phrases and replies](docs/phrases.md) | Phrase syntax, your own phrases, learning from misses, rewording replies |
| [Voice](docs/voice.md) | Listening, wake word, voiceprint, models |
| [Dashboard](docs/dashboard.md) | The web control panel, tab by tab |
| [Android app](docs/android.md) | Building, installing, permissions and using the phone app |
| [Devices](docs/devices.md) | Pairing the phone, running phone commands from the computer, push wake-ups |
| [Telegram](docs/telegram.md) | Talking to assist from Telegram |
| [Security](docs/security.md) | Trust levels, confirmations, policy, secrets, audit log, network |
| [Development](docs/development.md) | Workspace, build recipes, tests, code rules, contributing |

[PLAN.md](PLAN.md) is the design document: architecture, roadmap and decision log.

## How it works

```text
 text or voice                    proposals only
      |                                 ^
      v                                 |
  matcher (phrases) -> dialog (slots, confirm) -> policy -> scheduler -> actions
                                                                |
                                                     jobs, audit log, replies
```

- A Rust core (`assist-core`) turns text into a plan, asks for what is missing, checks the
  policy and runs the plan as jobs. It touches nothing but its own data directory.
- Actions (apps, shell, screen, phone, ...) plug in behind Cargo features.
- The daemon (`assist serve`) serves the CLI, the dashboard and paired devices on
  `127.0.0.1:7720`.
- The Android app is a thin Kotlin shell around the same core (through UniFFI).
- Output of OCR, transcription, messages or the network is data, never a command: it can
  propose an action but not run one.

## Security

- The daemon listens on loopback only. Every connection needs the token in the data
  directory or a one-time sign-in link; browsers are checked for Host and Origin.
- Calls, texts, file writes, shell suggestions and anything marked `risky` are confirmed
  first. Voice and remote requests get less trust than typing on the computer.
- Secrets are write-only: they can be set and deleted, never read back, and are redacted from
  logs.
- Every action is recorded in a hash-chained audit log.
- Paired devices are known by their key; an unknown key can only pair, with a one-time code.

More in [docs/security.md](docs/security.md).

## Status

| Phase | What | State |
| --- | --- | --- |
| 0, 1a, 1b | Core, CLI, daemon, dashboard | Done |
| 2 | Desktop voice | Done |
| 3 | Desktop features, pipes, automation | Done |
| 4 | Android app | Done (v0.6.0) |
| 5 | Multiple devices | In progress: pairing, phone commands from the computer, push wake done; Nodes tab, Telegram, MQTT and webhooks to come |
| 6 to 9 | Messaging, plugins and optional AI, dev tooling, hardware | Planned |

## Building from source

Install [just](https://github.com/casey/just) and
[cargo-nextest](https://nexte.st), then:

```sh
just build      # cargo build --workspace
just test       # all tests
just lint       # fmt, clippy (warnings are errors), cargo deny
just shell      # the interactive shell from source
just run        # the daemon from source
just android    # the Android APK (needs the NDK, see docs/android.md)
```

See [docs/development.md](docs/development.md).

## Repository layout

```text
crates/
  assist-core      the core: config, matcher, dialog, policy, scheduler, facts, memory
  assist-actions   built-in actions, each behind a Cargo feature
  assist-proto     the wire protocol (versioned JSON envelopes)
  assist-server    the daemon: WebSocket, HTTP, auth, rpc, paired devices
  assist-cli       the `assist` program
  assist-voice     microphone, speech to text, wake word, voiceprint
  assist-net       device keys, pairing, the iroh network
  assist-ffi       the core for Android (UniFFI)
android/           the Android app (Kotlin, Compose)
web/               the dashboard (plain HTML, CSS, JS)
config/            example config, built-in phrases, matcher test cases
docs/              documentation
spikes/            throwaway experiments behind decisions
```

## License

MIT OR Apache-2.0, at your option (see `Cargo.toml`). GPL tools such as Piper are only ever
run as separate programs, never linked.
