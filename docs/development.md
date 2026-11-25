# Development

How the code is organised, how to build and test it, and the rules every change follows.
[PLAN.md](../PLAN.md) is the source of truth for architecture, phases and decisions;
[CLAUDE.md](../CLAUDE.md) holds the working rules. Read PLAN.md before a non-trivial change.

## Tools

| Tool | For |
| --- | --- |
| Rust (pinned in `rust-toolchain.toml`, 1.95+) | Everything |
| [just](https://github.com/casey/just) | The recipes below |
| [cargo-nextest](https://nexte.st) | `just test` |
| [cargo-deny](https://github.com/EmbarkStudios/cargo-deny) | Licences, advisories, bans in `just lint` |
| [cargo-ndk](https://github.com/bbqsrc/cargo-ndk), Android SDK and NDK 28 | `just android` |
| [insta](https://insta.rs) (`cargo install cargo-insta`) | Reviewing snapshot changes |

On Windows the justfile runs recipes in PowerShell (`pwsh`).

## Recipes

```sh
just build       # cargo build --workspace
just test        # cargo nextest run --workspace, then doctests
just lint        # cargo fmt --check, clippy with -D warnings, cargo deny check
just shell       # cargo run -p assist-cli -- shell   (pass e.g. --config config/config.example.toml)
just run         # cargo run -p assist-cli -- serve
just android     # core for arm64 + x86_64, Kotlin bindings, Gradle debug APK
```

`just lint` and `just test` must pass before a change is done.

## Workspace

```text
crates/assist-proto    Wire protocol: the versioned JSON envelope, message kinds, rpc names
crates/assist-core     The text-only core: config, phrases and matcher, dialog, policy,
                       scheduler and jobs, facts and conditions, triggers, proactive rules,
                       autopilot, memory, secrets, audit, store
crates/assist-actions  Built-in actions, one Cargo feature each (apps, shell, scripts, search,
                       transforms, screen, record, rename, memory, ...)
crates/assist-server   The daemon: axum HTTP and WebSocket, auth, rpc, connections, paired
                       devices and the proxy actions that run phone commands on them
crates/assist-net      Device keys, pairing codes, peers, framing, the shared phone manifests
crates/assist-voice    Capture, VAD, wake word, streaming speech to text, voiceprint, TTS
crates/assist-cli      The `assist` binary: commands, shell, daemon setup, listen, autostart
crates/assist-ffi      The core for Android through UniFFI: phone actions, voice, the link
android/               The Kotlin app (Compose): UI, permissions, Android services
web/                   The dashboard: one HTML page, CSS and JS, embedded in the binary
config/                Example config, built-in phrases (config/phrases/en), matcher cases
spikes/                Experiments behind decisions, not built with the workspace
```

### How a request flows

1. Text comes in (CLI, dashboard, voice, phone, a trigger) with a `Principal` (source and
   trust).
2. The matcher (`assist-core::matcher`) turns it into an intent with slots, using the phrase
   files compiled to regexes.
3. The dialog (`assist-core::dialog`) asks for missing slots, disambiguates, handles
   follow-ups and corrections, and builds a `Plan`.
4. The policy (`assist-core::policy`) decides allow, confirm or deny from the action's
   capabilities and the principal's trust; the dialog asks for confirmation if needed.
5. The scheduler (`assist-core::scheduler`) runs the plan as a job (or a pipe of jobs) in a
   lane, streams output, audits every state change and stores the result.

The core touches nothing but its data directory. Microphones, screens, the OS and the network
belong to actions and adapters.

## Adding an action

1. Implement `assist_core::action::Action` in `assist-actions` (or in the crate of the
   platform it needs):
   - `manifest()` returns a static `Manifest`: name, capabilities, lane, pipe shape.
   - `run()` streams `Chunk`s to the output and stops promptly on cancel.
   - Blocking work (OS calls, models, files) goes to `spawn_blocking` or its own thread.
2. Put it behind a Cargo feature and register it in `builtin()`.
3. Add its phrases to a file in `config/phrases/en/` (see [phrases.md](phrases.md)).
4. Add matcher cases to `config/cases.toml` and unit tests without hardware (traits with
   fakes for OS and voice code).
5. Never put confirmation logic in the action: the capabilities, the policy table and
   `risky` decide.

Adding or removing an action must not change the core.

## Tests

- **Matcher cases:** every matcher or dialog change adds cases to `config/cases.toml`:
  ```toml
  [[case]]
  text = "text mom"
  command = "send_sms"
  slots = { contact = "mom" }
  ```
  A case without `command` must match nothing. They run against the example config and the
  built-in phrases (`crates/assist-core/tests/cases.rs`).
- **Snapshots:** parser output and dialog transcripts use `insta`
  (`crates/assist-core/src/snapshots`). Review changes with `cargo insta review`.
- **End to end:** `crates/assist-server/tests/daemon.rs` runs a real daemon on a random
  loopback port with real WebSocket, HTTP and iroh clients (pairing, routing to a phone, the
  queue for an away phone). `crates/assist-cli/tests` drives the binary.
- **Benchmarks:** `criterion`, when performance work is done; measure with `tracing` spans
  against the budgets in PLAN.md section 8.

## Code rules

The full list is in [CLAUDE.md](../CLAUDE.md). The main ones:

- Rust stable, edition 2021. `thiserror` in libraries, `anyhow` in binaries.
- No `unwrap()` or `expect()` outside tests and `main` setup.
- Never block the async runtime.
- OS-specific code in separate modules (`*_unix.rs`, `*_windows.rs`, `*_android.rs`) behind
  `#[cfg(...)]`.
- Logic in Rust; Kotlin stays thin platform glue.
- Every buffer, queue, history and input line has a bound.
- Data is never a command: OCR, transcripts, messages and network input only propose.
- Security (PLAN.md section 9): loopback only, auth on every connection, Host and Origin
  checks, audit log, confirmation binding, write-only secrets, never log secrets.
- Protocol changes are additive: keep `v` in the envelope, never remove or repurpose a field.
- Everything users see, and code comments, use plain ASCII punctuation and plain words.
- Comments say what the code is and why, in the present tense. A known limit gets one line:
  `shortcut: <limit>, <when to upgrade>`.
- `panic = "unwind"` in daemon and Android builds.

## Dependencies

- Prefer the crates listed in PLAN.md section 4; a new dependency needs a reason (size,
  maintenance, licence).
- Permissive licences only (`deny.toml`); GPL tools run only as separate processes.
- `cargo deny check` must be clean. Before a push or release, re-audit dependency updates,
  advisories and CI action pins.

## Commits and releases

- One feature per commit, small and reviewable, in
  [Conventional Commits](https://www.conventionalcommits.org) form:
  `feat(net): ...`, `fix(dialog): ...`, `docs(plan): ...`.
- A change that alters the plan updates PLAN.md (the section and the section 13 decision log)
  in the same commit.
- Versions are shared by the whole workspace (`[workspace.package] version`) and tagged
  `vX.Y.Z`.

## Adding a language

A language is a folder `config/phrases/<lang>/` with the command files and a `replies.toml`,
plus its matcher cases. Only English exists so far; the filler words used by the matcher are
English too.
