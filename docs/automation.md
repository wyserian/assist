# Pipes and automation

How to chain commands, save them, run them on a schedule or when something changes, have
assist speak up on its own, and give it standing goals.

- [Pipes](#pipes)
- [Saved pipes](#saved-pipes)
- [Conditions](#conditions)
- [Schedules](#schedules)
- [Triggers](#triggers)
- [Webhooks](#webhooks)
- [Proactive messages](#proactive-messages)
- [Autopilot](#autopilot)

## Pipes

Join commands with `|`. Each stage reads the lines the stage before it wrote:

```text
run build | filter error | save build-errors.txt
ocr screen | grep ^Error | count
$ git log --oneline | head 5
```

- A stage is either a transform (`head`, `filter`, `grep`, `count`, `sort`, `uniq`, `save`;
  see [commands.md](commands.md#pipe-stages)) written `<name> <arg>`, or any command that
  needs no question (`run build`, `ocr screen`).
- `$ a | b` is one system-shell command with its own pipe; `$` must come first.
- Stages are checked before anything runs: a stage that cannot take input, or one after a
  stage with no output, is refused with a reason.
- One confirmation covers the whole pipe. The readback lists every stage that may run.
- The pipe is one group of jobs that takes one lane slot. Stopping any stage stops the pipe;
  a failed stage cancels the later ones; a stage that stops reading (`head 3`) ends the
  earlier ones.
- Only the last stage's output is logged.

## Saved pipes

Give a pipe a name in `config.toml` and run it with "run <name>":

```toml
[pipes]
errors  = { run = "run build | filter error | save build-errors.txt", retry = 1 }
nightly = { run = "run backup", on_error = "errors", if = "node.charging", unattended = true }
```

| Key | Default | What |
| --- | --- | --- |
| `run` | required | The pipe |
| `retry` | `0` | Run it again this many times if it fails, 2 seconds apart |
| `on_error` | none | A pipe, or another saved pipe's name, to run when the last try failed |
| `if` | none | A [condition](#conditions); when false the pipe is skipped with "Not running ...: ... is not true." |
| `unattended` | `false` | Allowed to start from a schedule, trigger or goal, with nobody there |

Retries and `on_error` run in the same lane slot. A saved pipe that cannot work (an unknown
command, a bad stage) makes config loading fail with the reason; a stage this device lacks
does not.

An unattended pipe may not need a confirmation: nobody is there to give one. If a stage would
ask (a `save`, a script), the run is refused.

## Conditions

Conditions are used by `if` on saved pipes, `on` of triggers, proactive rules and goals.

```text
node.battery < 20 and not node.charging
app.focused == 'code.exe' or user.idle > 600
(node.battery <= 15) or screen.locked
```

- Operators: `==`, `!=`, `<`, `<=`, `>`, `>=`; combine with `and`, `or`, `not` and
  parentheses.
- Values: fact names, numbers, `'text'` in single quotes, `true`, `false`. A fact alone
  means "is true".
- A fact that is not set makes its comparison false.
- Facts on this device: `assist facts` (see [cli.md](cli.md#facts)).

For proactive rules, a job that ends also sets `job.` facts: `job.failed` (failed or timed
out), `job.done`, `job.state`, `job.command`, `job.title`, `job.lane`, `job.source`,
`job.id` and, when it failed, `job.error`.

## Schedules

Run a saved pipe (marked `unattended`) at a time or interval:

```toml
[[schedules]]
run = "errors"
at = "07:30 mon-fri"

[[schedules]]
run = "nightly"
every = "1h"
missed = "run"
```

| Key | What |
| --- | --- |
| `run` | The saved pipe |
| `at` | A local time `HH:MM`, optionally with weekdays: `mon-fri`, `sat,sun`, `mon,wed,fri` |
| `every` | An interval: `90s`, `15m`, `1h`, `1d` (at least 1 minute) |
| `missed` | `skip` (default): wait for the next time. `run`: run once after the computer wakes up if a time was missed |

Give exactly one of `at` or `every`. A run more than 2 minutes late counts as missed.

## Triggers

Run a saved pipe (marked `unattended`) when a fact condition turns true, or when a folder
changes:

```toml
[[triggers]]
run = "errors"
on = "node.battery < 20"

[[triggers]]
run = "sort-downloads"
watch = "~/Downloads"
```

| Key | What |
| --- | --- |
| `run` | The saved pipe |
| `on` | A condition; fires when it turns true, not again while it stays true |
| `watch` | A folder; fires after changes settle for 2 seconds (a copy of many files fires once). Changes in the 10 seconds after it fired are ignored, so a pipe writing into the folder does not loop |

Give exactly one of `on` or `watch`.

## Webhooks

Let another program on this computer start a saved pipe with an HTTP call, for example a
backup tool when it finishes, a home automation server, or a script:

```toml
[pipes]
sort-downloads = { run = "run sort_downloads", unattended = true }

[webhooks]
downloads = { run = "sort-downloads" }
```

Give the hook a token (any long random text), stored as the secret `webhook_<name>`:

```sh
assist secret set webhook_downloads
```

Then call it:

```sh
curl -X POST -H "Authorization: Bearer <token>" http://127.0.0.1:7720/hook/downloads
```

| Answer | Means |
| --- | --- |
| `202 {"run": "...", "jobs": [12]}` | Started; the job ids |
| `200 {"skipped": "..."}` | The pipe's `if` does not hold |
| `401` | Wrong or missing token (the daemon prints a hint when no token is set) |
| `404` | No hook with that name |
| `409 {"error": "..."}` | The pipe may not run unattended, with the reason |
| `429` | Called again within a second |

Rules:

- A hook is a trigger like a schedule: the caller decides **when** the pipe runs, never
  **what**. The request body is not read, so nothing in it can become a command.
- The pipe must be marked `unattended` and run without any confirmation. The job's source is
  `webhook:<name>`, so it shows in Jobs and Audit.
- Hook names use `a-z`, `0-9`, `_` and `-` (at most 40 characters).
- Each hook runs at most once a second; this also slows down guessing tokens.
- The daemon listens on `127.0.0.1` only, so only programs on this computer can call hooks.
  The Host header must be `127.0.0.1:<port>` (or `localhost:<port>`).

## Proactive messages

Rules that make assist speak up unasked:

```toml
[proactive]
quiet_hours = "23:00-07:00"
max_per_hour = 4
sink = "speak"

[[proactive.rules]]
name = "background failed"
on = "job.failed and job.lane == 'background'"
say = "{address}, {title} failed."

[[proactive.rules]]
name = "battery low"
on = "node.battery < 15 and not node.charging"
say = "Battery at {node.battery} percent."
every = "30m"
sink = "notify"
```

| Key | Default | What |
| --- | --- | --- |
| `quiet_hours` | none | No messages in this window (it may cross midnight) |
| `max_per_hour` | `4` | Limit for all rules together |
| `sink` | `speak` | `speak`: `assist listen` says it and the dashboard shows it. `notify`: a desktop notification |
| `rules[].name` | required | The name you mute it by |
| `rules[].on` | required | A condition. With `job.` facts it is checked when a job ends; otherwise when the condition turns true |
| `rules[].say` | required | The message. Filled in: `{address}`, fact names (`{node.battery}`) and, for job rules, the job fields without `job.` (`{title}`, `{command}`, `{error}`, `{state}`, `{lane}`, `{id}`) |
| `rules[].every` | `10m` | At most one message from this rule per interval |
| `rules[].sink` | `[proactive] sink` | Where this rule's messages go |

A message passes the gates in this order: muted, the rule's interval, quiet hours, the hourly
limit. A message held back is dropped, not queued.

Mute a rule by saying "stop telling me about battery low" (or typing `mute battery low`);
"tell me about battery low again" undoes it. The phone runs its own rules with its own facts.

## Autopilot

Standing goals that run a saved pipe on a trigger with nobody watching, within limits you
set. Off by default.

```toml
[autopilot]
enabled = true

[[autopilot.goals]]
name = "nightly-errors"
objective = "Collect build errors every night"
run = "errors"
trigger = { at = "02:00" }
allow = ["run_script", "filter", "save"]
budget = { jobs_per_day = 20, max_runtime = "10m" }
expires = "2026-12-31"
```

| Key | Default | What |
| --- | --- | --- |
| `enabled` | `false` | Autopilot as a whole |
| `goals[].name` | required | The goal's name |
| `goals[].objective` | `""` | What it is for, shown in the dashboard |
| `goals[].run` | required | The saved pipe it runs |
| `goals[].trigger` | required | One of `at`, `every` (with `missed`), `on`, `watch`, as for schedules and triggers |
| `goals[].allow` | required | The commands it may use; every stage of the pipe must be in it |
| `goals[].budget.jobs_per_day` | `50` | Jobs per local day |
| `goals[].budget.max_runtime` | `5m` | How long one run may take |
| `goals[].expires` | required | `YYYY-MM-DD`; the goal stops after this day |

Rules:

- Goals run with the `autopilot` trust level, as source `autopilot:<goal>`, so every step is
  audited and STOP ALL stops it.
- Every stage must be in `allow` and allowed without a confirmation. Named scripts count as
  allowed when `run_script` is in `allow`; raw shell never is. Otherwise the goal pauses and
  you get a notice.
- Config loading refuses a goal that allows a command needing `delete`, `send`, `net.scan`,
  `secrets.write`, `config.write`, `admin`, `record` or `memory.write`.
- A run past `max_runtime` is stopped and pauses the goal; going past `jobs_per_day` pauses
  it.
- Controls: `assist autopilot [on|off|pause <goal>|resume <goal>]`, the dashboard's Autopilot
  tab, "stop autopilot" (anyone may say it), "start autopilot" (local only).
