# CLI reference

`assist` is one program with subcommands. Most of them talk to the running daemon
(`assist serve`); `shell` and `install-autostart` work without it.

```text
assist [--config <file>] [--data-dir <dir>] <command> [args]
```

## Global options

| Option | Default | What |
| --- | --- | --- |
| `--config <file>` | `config.toml` in the user config directory | The config file to use |
| `--data-dir <dir>` | The user data directory | Where the token, job history, audit log and models live |
| `--help` | | Help for `assist` or for one command (`assist pair --help`) |
| `--version` | | The version |

Directories per OS are listed in [install.md](install.md#where-assist-keeps-its-files). A
daemon and the commands that talk to it must use the same `--data-dir`.

## Commands at a glance

| Command | Needs the daemon | What |
| --- | --- | --- |
| [`serve`](#serve) | | Run the daemon |
| [`shell`](#shell) | | Interactive shell with the core in-process |
| [`run <text>`](#run) | yes | Say something and follow the jobs it starts |
| [`explain <text>`](#explain) | yes | Show what a sentence would do, without doing it |
| [`jobs`](#jobs) | yes | List active and recent jobs |
| [`log <id>`](#log) | yes | Show the last output of a job |
| [`stop <id\|all>`](#stop) | yes | Stop a job, or every job |
| [`pause <id>`](#pause-and-resume) / [`resume <id>`](#pause-and-resume) | yes | Pause or resume a running job |
| [`facts`](#facts) | yes | Show device facts for conditions |
| [`memory`](#memory) | yes | List or forget remembered notes |
| [`autopilot`](#autopilot) | yes | List, switch or pause autopilot goals |
| [`secret`](#secret) | yes | List, set or delete secrets |
| [`ui`](#ui) | yes | Open the dashboard, signed in |
| [`pair`](#pair) | yes | Show a code to pair a phone |
| [`listen`](#listen) | yes | Talk to the daemon by voice |
| [`enroll`](#enroll) | | Record your voiceprint |
| [`install-autostart`](#install-autostart) | | Start the daemon at login |

`listen` and `enroll` exist only in builds with the `voice` feature (the default).

When a command needs the daemon and none runs, it says:
`No daemon running. Start it with: assist serve`.

## serve

```sh
assist serve
```

Runs the daemon in the foreground until Ctrl+C. It:

- listens on `[server] bind` (default `127.0.0.1:7720`; only loopback addresses are
  accepted),
- serves the dashboard, the CLI, `assist listen` and paired devices,
- runs schedules, triggers, proactive rules and autopilot goals,
- writes `token` and `daemon.addr` into the data directory so the other commands can find
  and authenticate to it,
- accepts paired devices over the iroh network (see [devices.md](devices.md)); if the network
  cannot start it says `Paired devices are off: ...` and serves this machine anyway.

Settings saved in the dashboard are written to `config.toml` (comments kept); `risky` applies
at once, lanes and job limits after a restart. Edits to `config.toml` by hand are read at the
next start.

## shell

```sh
assist shell
```

An interactive shell with the whole assistant in-process, no daemon needed. Type what you
want (`open firefox`, `run build`, `search rust tokio`). Words the shell handles itself:

| Input | What |
| --- | --- |
| `$ <command>` | Run in the system shell. `$ cd <dir>` changes the shell's directory |
| `jobs` | List jobs |
| `log <id>` | Show the last output of a job |
| `stop <id>` | Stop a job |
| `stop all` | Stop every job |
| `cancel` | Drop the current question |
| `help` | Show this list |
| `exit` | Stop all jobs and quit (Ctrl+C does the same) |

Job output is printed as `[#id] line`. Input lines longer than 8 KB are refused. With input
piped in (`echo "run build" | assist shell`) the shell waits for the jobs to finish.

The shell does not reach paired devices: phone commands ("call mom") need the daemon, so use
`assist run`, the dashboard or `assist listen` for them.

## run

```sh
assist run <text...>
```

Says `<text>` to the daemon, answers its questions on the terminal and follows the started
jobs to the end. Quotes are optional: the rest of the line is the text.

```sh
assist run open firefox
assist run "text mom saying running late"     # asks "Text mom: running late?"; type yes
assist run backup
assist run "ocr screen | filter error | save errors.txt"
```

The exit code is 0 when every job finished well, 1 otherwise, so it works in scripts.

## explain

```sh
assist explain <text...>
```

Shows how a sentence is understood and what the policy would decide, without running it:

```text
$ assist explain text priye saying hello
command:  send_sms
  contact = Priye
  text = hello
action:   Text Priye: hello?
decision: confirm
```

`No command matches that.` means no phrase matched; teach one in the dashboard (see
[phrases.md](phrases.md)).

## jobs

```sh
assist jobs
```

Lists active and recent jobs with id, state and title. States: `queued`, `running`, `paused`,
`done`, `failed`, `cancelled`, `timed_out`, `interrupted` (the daemon stopped while it ran).

## log

```sh
assist log <id>
```

The last output of job `<id>` (up to `[jobs] output_tail_kb`). The full output is in
`jobs/<id>.log` in the data directory.

## stop

```sh
assist stop <id>
assist stop all
```

Stops one job (and its whole process tree), or every job.

## pause and resume

```sh
assist pause <id>
assist resume <id>
```

Pauses a running job, all of its pipe and its processes, and continues it later. Pausing
suspends the processes (SIGSTOP on Linux, `NtSuspendProcess` on Windows).

## facts

```sh
assist facts
```

Prints the device facts, `name = value`, that conditions can use:

| Fact | Where | What |
| --- | --- | --- |
| `node.battery` | Laptops, phone | Battery percent |
| `node.charging` | Laptops, phone | `true` when plugged in |
| `app.focused` | Windows | File name of the app in front, e.g. `code.exe` |
| `user.idle` | Windows | Seconds since the last keyboard or mouse input |
| `screen.locked` | Phone | `true` when the screen is locked |

Facts are read every 30 seconds; a fact the device cannot tell is left out. See
[automation.md](automation.md#conditions).

## memory

```sh
assist memory                 # list remembered notes with their ids
assist memory forget <id>     # forget one
assist memory forget all      # forget everything
```

Notes come from "remember ..." (see [commands.md](commands.md#memory)).

## autopilot

```sh
assist autopilot              # list goals, state and budget used
assist autopilot on           # switch autopilot on
assist autopilot off          # switch it off
assist autopilot pause <goal>
assist autopilot resume <goal>
```

Goals are configured in `[autopilot]` (see [automation.md](automation.md#autopilot)).

## secret

```sh
assist secret list
assist secret set <name>      # reads the value from stdin
assist secret delete <name>
```

Secrets are write-only: `list` shows names and the last 4 characters, never a value. Names
use `a-z`, `0-9`, `_` and `-`.

`set` reads one line. Typed at the prompt the value is visible while you type, but it never
lands in your shell history. To keep it off the screen, pipe it in or use the dashboard's
Secrets tab:

```sh
assist secret set github_token            # then type or paste the value and press Enter
cat token.txt | assist secret set github_token
```

## ui

```sh
assist ui
assist ui --print
```

Opens the dashboard in the default browser, signed in with a one-time link. `--print` prints
the link instead (for another browser on the same machine). See [dashboard.md](dashboard.md).

## pair

```sh
assist pair
```

Shows a QR code and its text (`assist-pair:...`) for the Assist phone app. The code works
once, for 5 minutes. Only a CLI connection (with the token) can make codes. See
[devices.md](devices.md).

## listen

```sh
assist listen
assist listen --device "USB"
```

Talks to the daemon by voice and speaks its replies. The first time it asks before
downloading the voice models (about 100 MB). Say the wake phrase first ("hey Bee"); after a
request, follow-ups within `[voice] awake` (default 60 s) need no wake phrase.

| Key | What |
| --- | --- |
| Enter | Pause or resume the microphone |
| Ctrl+C | Quit |

| Option | What |
| --- | --- |
| `--device <text>` | Use the microphone whose name contains `<text>`. Default: a connected headset, else the system default |

See [voice.md](voice.md).

## enroll

```sh
assist enroll
assist enroll --device "USB"
assist enroll --forget
```

Records your voiceprint so that a spoken "yes" from you can confirm calls and texts. Without
one, risky voice requests are confirmed on screen. `--forget` deletes the voiceprint.

## install-autostart

```sh
assist install-autostart
assist install-autostart --remove
```

Starts `assist serve` at login with the same `--config` and `--data-dir`: a Startup-folder
entry with no window on Windows, a systemd user service on Linux. `--remove` deletes it.
