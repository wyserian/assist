# Dashboard

The dashboard is a local web page served by the daemon: a chat with the assistant and a
control panel for everything it does. It runs on this computer only.

## Opening it

```sh
assist serve     # if it is not running
assist ui
```

`assist ui` opens the default browser with a one-time sign-in link; the browser then keeps a
session. `assist ui --print` prints the link for another browser on the same computer. The
address is `http://127.0.0.1:7720` unless `[server] bind` says otherwise; opening it without
the link asks you to sign in with `assist ui`.

The **Stop all jobs** button at the top stops every job at once, from any tab.

## Tabs

### Chat

Type what you would say: `open firefox`, `run build`, `$ ls`. Replies, questions and job
output appear here. Proactive messages with `sink = "speak"` are shown here too.

### Jobs

Every active and recent job: id, state, lane, source (`cli`, `web`, `voice`, `node:<phone>`,
`autopilot:<goal>`, ...) and title. Per job: stop, pause, resume and its log. **Refresh**
reloads the list.

### Waiting

Questions that clients are waiting on, such as "Call Mom?" asked to `assist listen` or the
phone. Answer with **Yes** or **No**; your answer counts as theirs, so you can confirm a voice
request on screen.

### Memory

What the assistant remembers, with where it came from, when it was added and last used.
**Forget** single notes, or **Forget everything**. Notes stay on this computer.

### Autopilot

The goals from `[[autopilot.goals]]`: state, jobs today, expiry and objective. Switch
autopilot on or off, and pause or resume each goal. See
[automation.md](automation.md#autopilot).

### Secrets

Secret names, their last 4 characters and when they were set. Add, replace or delete a
secret. Values can be written but never read back.

### Commands

- **Explain:** type a sentence to see the command, slots, readback and policy decision, as
  `assist explain` does.
- **Not understood recently:** sentences that matched nothing, how often they were said, and
  a place to pick the command they meant and the phrase to learn (**Add phrase**), or
  **Dismiss** it. Adding writes to
  `phrases/user.toml` and applies at once (see [phrases.md](phrases.md#learning-from-misses)).

### Settings

- **Always ask before running:** the `risky` switch per command, applied at once.
- **Jobs:** lanes and job limits, applied after a restart.

Saving writes `config.toml` and keeps its comments and layout.

### Audit

The audit log: time, event, job, source and action of everything that ran or was refused.
**Refresh** reloads it. The full log is in `audit/` in the data directory.

### Nodes

Paired devices: state (connected, away, or away but can be woken by push), requests running
on them or waiting for them, what they can do, when they were paired and last seen.
**Remove** unpairs a device and drops its connection. See [devices.md](devices.md).

### Clients

Connected clients: id, kind (`cli` for token clients such as `assist run` and
`assist listen`, `web` for browser sessions, `node` for paired devices), program and since
when. **Revoke** disconnects one; a revoked browser session must sign in again.

## Safety

- The page only works from this computer. The daemon checks the Host and Origin of every
  request, and pages are served with a strict Content Security Policy.
- Output is shown as plain text, never as HTML, so job output cannot inject anything.
- Admin actions (secrets, settings, revoking, Stop all) are for local clients only.
