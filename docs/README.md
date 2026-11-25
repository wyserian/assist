# assist documentation

The user and developer guide for assist 0.7.0. The project overview is in the
[README](../README.md); the design and its reasons are in [PLAN.md](../PLAN.md).

## Using assist

| Page | Read it to |
| --- | --- |
| [Install](install.md) | Install assist on the computer, start it at login, find its files, update it |
| [CLI](cli.md) | Look up any `assist` command, its options and the interactive shell |
| [Commands](commands.md) | See everything you can say or type, with examples and what asks first |
| [Pipes and automation](automation.md) | Chain commands, save pipes, schedule them, react to facts, set up proactive rules and autopilot |
| [Voice](voice.md) | Talk to the computer, set the wake word, record your voiceprint |
| [Dashboard](dashboard.md) | Use the web control panel |
| [Android app](android.md) | Build, install and use the phone app |
| [Devices](devices.md) | Pair the phone, run phone commands from the computer, wake the phone by push |
| [Telegram](telegram.md) | Talk to assist from Telegram |

## Setting it up your way

| Page | Read it to |
| --- | --- |
| [Configuration](config.md) | Every `config.toml` section and key, with defaults |
| [Phrases and replies](phrases.md) | Teach new phrases, override built-in ones, reword replies |
| [Security](security.md) | Understand trust levels, confirmations, the policy table, secrets and the audit log |

## Working on assist

| Page | Read it to |
| --- | --- |
| [Development](development.md) | Build, test and change the code; rules every change follows |

## Words used in these pages

- **Command:** something assist can do, such as `open_app` or `send_sms`. A **phrase** is a
  way of saying it ("open firefox").
- **Slot:** a part of a command that changes, such as the app in "open firefox".
- **Job:** one running command. Jobs have ids (`#12`), a state, a log and a lane.
- **Pipe:** commands joined with `|`, each stage reading the lines of the one before.
- **Fact:** a piece of device state, such as `node.battery`, used in conditions.
- **Daemon:** `assist serve`, the background program the CLI, dashboard and phone talk to.
- **Hub:** the computer that paired devices connect to.
- **Trust level:** how much assist trusts where a request came from: `local`,
  `owner_remote`, `voice`, `untrusted` or `autopilot`.
