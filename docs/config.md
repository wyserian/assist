# Configuration

assist reads one TOML file, `config.toml`:

- Windows: `%APPDATA%\assist\config\config.toml`
- Linux: `~/.config/assist/config.toml`
- Phone: `config.toml` in the app's files (see [android.md](android.md#the-phones-config))
- Or any file with `--config <file>`

Every section is optional; without a file assist runs with the defaults below. Unknown
sections and keys are errors that name the problem, so a typo never passes silently. A
starting point with comments is [`config/config.example.toml`](../config/config.example.toml).

The dashboard's Settings tab edits some of these keys and keeps your comments.

Durations are a number with `s`, `m`, `h` or `d`: `30s`, `10m`, `2h`, `1d`.

## Sections

| Section | What |
| --- | --- |
| [`config_version`](#config_version) | The format version |
| [`[apps]`](#apps) | Apps you can open and close by name |
| [`[scripts]`](#scripts) | Named scripts for "run <name>" |
| [`[pipes]`](#pipes) | Saved pipes |
| [`[contacts]`](#contacts) | Names and numbers for calls and texts |
| [`[commands.<name>]`](#commandsname) | Per command: always ask, own questions |
| [`[policy]`](#policy) | Overrides of the trust and confirmation table |
| [`[lanes]`](#lanes) | How many jobs run at once |
| [`[jobs]`](#jobs) | Timeouts, output and history limits |
| [`[search]`](#search) | The search engine |
| [`[server]`](#server) | Where the daemon listens |
| [`[persona]`](#persona) | The assistant's name, how it addresses you, the wake word |
| [`[voice]`](#voice) | Voiceprint threshold, awake window |
| [`[replies]`](#replies) | Your wording for any reply |
| [`[sources.telegram]`](#sourcestelegram) | The Telegram bot |
| [`[[schedules]]`](#schedules-triggers-proactive-autopilot) | Timed saved pipes |
| [`[[triggers]]`](#schedules-triggers-proactive-autopilot) | Saved pipes on facts or folder changes |
| [`[proactive]`](#schedules-triggers-proactive-autopilot) | Messages it sends unasked |
| [`[autopilot]`](#schedules-triggers-proactive-autopilot) | Standing goals |

## config_version

```toml
config_version = 1
```

Default `1`, the only version so far.

## [apps]

```toml
[apps]
firefox  = { windows = "firefox.exe", linux = "firefox", android = "org.mozilla.firefox" }
code     = { windows = "code.cmd", linux = "code" }
notes    = "C:/Tools/notes.exe"
```

The key is the name you say ("open code"). The value is a program name or path, or a table
with one per OS (`windows`, `linux`, `android`). On Windows, the installed apps are added
automatically; entries here win on a clash. On the phone, its launcher apps are added by
label.

## [scripts]

```toml
[scripts]
backup = { windows = "C:/scripts/backup.ps1", linux = "~/scripts/backup.sh" }
build  = { run = "cargo build", lane = "background" }
deploy = "just deploy"
```

The key is the name ("run backup"). The value is a command string, or a table with `run` or
per-OS commands, and an optional `lane` (`interactive`, the default, or `background`).
The command goes to the shell as written, so quote paths with spaces. Scripts ask before
they run.

## [pipes]

```toml
[pipes]
errors = { run = "run build | filter error | save build-errors.txt", retry = 1 }
```

| Key | Default | What |
| --- | --- | --- |
| `run` | required | The pipe, as you would type it |
| `retry` | `0` | Extra tries when it fails (at most 10), 2 s apart |
| `on_error` | none | A pipe or saved pipe name to run when the last try failed |
| `if` | none | A condition over facts; skipped when false |
| `unattended` | `false` | May start from schedules, triggers and goals |

Details in [automation.md](automation.md#saved-pipes).

## [contacts]

```toml
[contacts]
mom = "+15551234567"
"Dr Rao" = "+15557654321"
```

Names for "call ..." and "text ...". On the phone, the address book is added; entries here
win on a clash. On the computer the paired phone's contact names are added too (the numbers
stay on the phone). Names match loosely, so small misspellings still find the contact, and
assist asks which one when several are close.

## [commands.<name>]

```toml
[commands.send_sms]
risky = true
ask.text = "What should the message say?"
```

| Key | Default | What |
| --- | --- | --- |
| `risky` | `false` | Always ask before running this command, from anywhere |
| `ask.<slot>` | the phrase file's question | The question for a missing slot |

`<name>` is a command name from [commands.md](commands.md) (`open_app`, `run_script`, ...).
An unknown name is an error.

## [policy]

```toml
[policy]
shell = { owner_remote = "deny" }
"fs.write" = { local = "allow" }
```

Replaces cells of the default table: per capability, per trust level, `allow`, `confirm` or
`deny`. Capability names with a dot need quotes. The table and the names are in
[security.md](security.md#the-policy-table).

## [lanes]

```toml
[lanes]
interactive = 2
background = 3
```

How many jobs (or pipes) run at once per lane. Background jobs run at low OS priority.
Defaults: `interactive = 2`, `background = 3`. Changes apply after a restart.

## [jobs]

```toml
[jobs]
timeout = "10m"
output_tail_kb = 64
retention_days = 7
log_cap_mb = 500
```

| Key | Default | What |
| --- | --- | --- |
| `timeout` | `10m` | A job running longer is stopped (`timed_out`) |
| `output_tail_kb` | `64` | Output kept for `assist log` and the dashboard |
| `retention_days` | `7` | Finished jobs and logs kept this long |
| `log_cap_mb` | `500` | Job logs kept up to this size; the oldest go first |

## [search]

```toml
[search]
url = "https://duckduckgo.com/?q={query}"
```

An `http://` or `https://` URL with `{query}` where the encoded search goes.

## [server]

```toml
[server]
bind = "127.0.0.1:7720"
```

The daemon's address. Only loopback addresses are accepted. Change the port if 7720 is
taken.

## [persona]

```toml
[persona]
name = "Bee"
address = "Prime"
wake_word = ""
```

| Key | Default | What |
| --- | --- | --- |
| `name` | `Bee` | The assistant's name |
| `address` | `Prime` | What it calls you, in `{address}` of replies; `""` for nothing |
| `wake_word` | `""` | The wake phrase. Empty means "hey <name>", "okay <name>" or "ok <name>". A longer word (`"bumblebee"`) helps when a short name wakes it by mistake |

## [voice]

```toml
[voice]
speaker_threshold = 0.35
awake = "60s"
```

| Key | Default | What |
| --- | --- | --- |
| `speaker_threshold` | `0.35` | How close (0 to 1) a voice must be to your voiceprint for a spoken "yes" to confirm. Raise it if others get through, lower it if you do not |
| `awake` | `60s` | After a request, follow-ups in this time need no wake phrase |

## [replies]

```toml
[replies]
cancelled = "Stopped, {address}."
not_understood = "Say that again?"
```

Rewords any reply. The keys and default texts are in
[`config/phrases/en/replies.toml`](../config/phrases/en/replies.toml); see
[phrases.md](phrases.md#rewording-replies).

## [sources.telegram]

```toml
[sources.telegram]
enabled = false
allowed_user_ids = []
shell = false
```

| Key | Default | What |
| --- | --- | --- |
| `enabled` | `false` | Run the Telegram bot; needs the secret `telegram_bot_token` |
| `allowed_user_ids` | `[]` | Telegram user ids allowed to use it; everyone else is ignored |
| `shell` | `false` | Allow shell commands and scripts from Telegram (they still ask first) |

Changes apply after a restart. See [telegram.md](telegram.md).

## Schedules, triggers, proactive, autopilot

These sections are described with examples in [automation.md](automation.md):

```toml
[[schedules]]
run = "errors"             # a saved pipe marked unattended
at = "07:30 mon-fri"       # or every = "1h"
missed = "skip"            # or "run"

[[triggers]]
run = "errors"
on = "node.battery < 20"   # or watch = "~/Downloads"

[proactive]
quiet_hours = "23:00-07:00"
max_per_hour = 4
sink = "speak"             # or "notify"

[[proactive.rules]]
name = "background failed"
on = "job.failed and job.lane == 'background'"
say = "{address}, {title} failed."
every = "10m"

[autopilot]
enabled = false

[[autopilot.goals]]
name = "nightly-errors"
objective = "Collect build errors every night"
run = "errors"
trigger = { at = "02:00" }
allow = ["run_script", "filter", "save"]
budget = { jobs_per_day = 20, max_runtime = "10m" }
expires = "2026-12-31"
```

## Checking a config

`assist shell` and `assist serve` load the config first and stop with the reason if
something is wrong: the file, the key and what was expected (for example an unknown field
`timout` in `[jobs]`, or a saved pipe that names an unknown command).

`assist explain <text>` shows how a sentence is understood with the current config.
