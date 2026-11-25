# Install

How to install assist on a computer, keep it running, and where it keeps its files. For the
phone, see [android.md](android.md).

## Requirements

| What | Needed for | Notes |
| --- | --- | --- |
| Windows 10/11 or Linux (X11) | Everything | Wayland is not supported yet |
| [Rust](https://rustup.rs) | Building | `rust-toolchain.toml` pins the version (1.95+); rustup installs it |
| A C toolchain | Building | Visual Studio Build Tools on Windows, `build-essential` on Linux |
| `curl` | Voice model downloads, waking the phone | Included in Windows 10+ and most Linux systems |
| `ffmpeg` | `record`, and `transcribe` of non-WAV files | Optional |
| speech-dispatcher (`spd-say`) | Spoken replies on Linux | Optional; Windows uses its built-in voice |
| A microphone | `assist listen`, `assist enroll` | Optional |

## Install from source

```sh
git clone https://github.com/wyserian/assist.git
cd assist
cargo install --path crates/assist-cli
```

This puts `assist` in `~/.cargo/bin` (on Windows `%USERPROFILE%\.cargo\bin`), which rustup
adds to your PATH. Check it:

```sh
assist --version
```

Without voice (smaller and faster to build, no microphone code):

```sh
cargo install --path crates/assist-cli --no-default-features
```

## First run

1. Copy the example config and edit it:
   ```sh
   # Windows (PowerShell)
   mkdir $env:APPDATA\assist\config
   copy config\config.example.toml $env:APPDATA\assist\config\config.toml
   # Linux
   mkdir -p ~/.config/assist && cp config/config.example.toml ~/.config/assist/config.toml
   ```
   Without a config file assist still runs with defaults. Unknown keys are errors, so a typo
   is reported instead of ignored. See [config.md](config.md).
2. Try the interactive shell (no daemon needed):
   ```sh
   assist shell
   ```
   Type `help`, then for example `open firefox`.
3. Start the daemon, which the dashboard, `assist run`, `assist listen` and the phone need:
   ```sh
   assist serve
   ```
4. In another terminal:
   ```sh
   assist run search rust tokio
   assist ui
   ```

## Start at login

```sh
assist install-autostart
```

- **Windows:** a shortcut in the Startup folder that starts `assist serve` with no window.
- **Linux:** a systemd user service.

The entry uses the same `--config` and `--data-dir` you passed, if any. Remove it with:

```sh
assist install-autostart --remove
```

## Where assist keeps its files

| What | Windows | Linux |
| --- | --- | --- |
| Config (`config.toml`) | `%APPDATA%\assist\config\` | `~/.config/assist/` |
| Your phrases (`phrases/*.toml`, `phrases/user.toml`) | `%APPDATA%\assist\config\phrases\` | `~/.config/assist/phrases/` |
| Data directory | `%APPDATA%\assist\data\` | `~/.local/share/assist/` |

Override them for any command with `--config <file>` and `--data-dir <dir>`.

The data directory holds (all readable by your user only):

| File | What |
| --- | --- |
| `token` | The daemon's access token; the CLI reads it to connect |
| `daemon.addr` | The address the running daemon listens on |
| `assist.db` | Job history and output tails (SQLite) |
| `jobs/<id>.log` | Full job output, kept 7 days or 500 MB |
| `audit/YYYY-MM.jsonl` | The hash-chained audit log |
| `memory.json` | What you asked it to remember |
| `muted.json` | Muted proactive rules |
| `autopilot.json` | Autopilot state |
| `secrets.json` | Secret names and metadata (never the values) |
| `models/` | Voice models, after you agree to download them |
| `voiceprint.txt` | Your voiceprint, after `assist enroll` |
| `node.key` | This device's network key, for pairing |
| `peers.json` | Paired devices |
| `undo/<job>.json` | Undo records of bulk renames |

Secret values are not in the data directory: they are in the OS keystore (Windows Credential
Manager, or the Secret Service on Linux).

## Update

```sh
cd assist
git pull
cargo install --path crates/assist-cli
```

Then restart the daemon: stop it with Ctrl+C (or `assist run shutdown`, see
[commands.md](commands.md)) and run `assist serve` again. Your config, phrases and data are
kept.

## Uninstall

```sh
assist install-autostart --remove
cargo uninstall assist-cli
```

Then delete the config and data directories above if you want your data gone too.
