# CLAUDE.md

Assist — personal automation & voice assistant. Rust core shared by desktop (Linux + Windows), Android
(Kotlin app, core via UniFFI) and later Raspberry Pi / ESP32.

**Read `PLAN.md` before any non-trivial change.** It is the source of truth for architecture,
stack, phases and security rules. Work on the current phase only.

## Hard rules

- **Text-only core.** `assist-core` never touches mic, speaker, screen, OS or network directly; its
  only I/O is its own data directory. Adapters and actions do the rest.
- **Logic in Rust, platform glue in Kotlin/OS modules.** Android code stays thin.
- **Never block the async runtime.** Audio, STT, OCR, TTS, embeddings, LLM run on dedicated
  threads or `spawn_blocking`.
- **Every built-in action lives behind a Cargo feature** and implements the `Action` trait with
  a manifest declaring its capabilities. Adding/removing an action must not change the core.
- **Risk is config.** Never hard-code confirmation logic in an action; use the policy table,
  `risky` in config and the policy layer.
- **Data is never a command.** Output of OCR, scraping, LLM, messages, webhooks is never fed back
  into the matcher; it can only propose actions.
- **Security (PLAN.md §9) is mandatory:** bind to localhost/mesh only, auth on every connection,
  Host + Origin check, audit log, confirmation binding, secrets write-only. Never log secrets.
- **Protocol changes are additive.** Keep `v` in the envelope; never remove or repurpose fields.
- **Dependencies:** prefer the crates listed in PLAN.md §4. Ask before adding a dependency that
  is not listed, and say why (size, maintenance, license). No GPL code linked into any binary;
  GPL tools (e.g. Piper, espeak-ng) only as external processes.
- Create a crate only when its phase starts. Never name a crate `core` or `std`.

## Code style

- Rust stable, edition 2021+. `thiserror` in libraries, `anyhow` in binaries.
- No `unwrap()` / `expect()` outside tests and `main` setup.
- `cargo fmt` and `cargo clippy --all-targets --all-features -- -D warnings` must pass.
- OS-specific code via `#[cfg(target_os = "...")]` in separate modules (`*_linux.rs`,
  `*_windows.rs`, `*_android.rs`).
- `panic = "unwind"` (never `abort`) in daemon and Android builds.
- Small modules, clear names, doc comments on public items. No dead code "for later".

## Testing

- Every matcher/dialog change adds cases to `config/cases.toml`:
  ```toml
  [[case]]
  text = "text mom"
  command = "send_sms"
  slots = { contact = "mom" }
  ```
- Core logic is unit-tested without hardware. Voice/OS code gets thin integration tests or is
  behind traits with fakes.
- Use `insta` snapshots for parser output and dialog transcripts.
- Benchmarks (`criterion`) for matcher and pipes when performance work is done.

## Commands

```sh
just build        # cargo build --workspace
just test         # cargo nextest run --workspace
just lint         # fmt --check + clippy -D warnings + cargo deny check
just run          # cargo run -p assist-cli -- serve
just android      # cargo ndk build + gradle assembleDebug (Phase 4+)
```

(Create these recipes in Phase 0 if the `justfile` does not exist yet. On Windows the justfile
sets `windows-shell := ["pwsh", "-NoLogo", "-Command"]`.)

## Workflow

1. Start each task by restating which PLAN.md phase and item it belongs to.
2. For anything spanning several files, propose a short plan first, then implement.
3. Keep changes small and reviewable; one feature per commit.
4. Run `just lint` and `just test` before saying a task is done.
5. When a decision changes the plan, update PLAN.md (relevant section + §13 decision log) in
   the same change.
6. Measure performance on real numbers (`tracing` spans) against PLAN.md §8 budgets; don't
   guess.
