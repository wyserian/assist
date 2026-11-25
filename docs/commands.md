# Commands

Everything you can say or type to assist: in `assist shell`, `assist run`, the dashboard
chat, `assist listen` and the phone app. Words are not case sensitive, and small extra
words ("okay so can you ... please") are ignored when the sentence would not match
otherwise.

The phrases below are the built-in English ones from `config/phrases/en/`. In the patterns,
`(a|b)` means either word, `(...)?` means optional, and `.name.` is a slot you fill in. You
can add your own phrases and override these (see [phrases.md](phrases.md)).

"Asks first" is the default for typing on the computer. Voice and remote requests ask more
often or are refused; see [security.md](security.md#the-policy-table).

## Talking to assist

| Say | What |
| --- | --- |
| `yes`, `y`, `yeah`, `yep`, `sure`, `ok`, `okay`, `confirm`, `do it`, `go ahead` | Confirm the question |
| `no`, `n`, `nope`, `don't` | Decline |
| `cancel`, `stop`, `abort`, `never mind`, `forget it` | Drop the current question (works at any point) |
| `1`, `2`, `first`, `second` ... or the name | Pick from "Did you mean A or B?" |
| `no, I meant dad` / `I mean dad` | Correct a slot of the last request |
| `close it`, `call him` | Refer back to the last app or contact |
| `open firefox and search rust` | Several requests at once (`and`, `then`) |

A question waits 60 seconds; after that "That question expired."

When a request leaves out a slot, assist asks for it ("To whom?", "Which app?"). When a name
is close to several (`call sa` with Sam and Sarah), it asks which one.

When nothing matches, typed text that looks like a shell command is offered for the system
shell: `I don't know that command. Run "git pull" in the shell?` (only on the computer, and
only when you type it). The sentence is also kept for the dashboard's "Not understood
recently" list.

## Apps

| Command | Phrases | Examples | Asks first |
| --- | --- | --- | --- |
| `open_app` | `(open\|launch\|start) (the)? .app. (app)?`, `bring up .app.` | `open firefox`, `launch the code app`, `bring up terminal` | no |
| `close_app` | `(close\|quit\|exit) (the)? .app. (app)?` | `close firefox`, `quit the notepad app` | no |

- **Computer:** on Windows the installed apps are found automatically (the Start menu's
  app list) and joined with `[apps]` in the config, which wins on a clash. On Linux only
  `[apps]` is used. On Windows, `open` brings an app that is already open to the front, and
  `close` closes its windows; on Linux `open` always starts it and `close` is not supported
  yet.
- **Phone:** the phone's launcher apps, by their label (`open camera`).

## Search

| Command | Phrases | Example | Asks first |
| --- | --- | --- | --- |
| `search` | `(search\|google\|look up) (for)? .query.`, `search the web for .query.` | `search rust tokio` | no |

Opens `[search] url` (DuckDuckGo by default) in the browser.

## Scripts, pipes and the shell

| Command | Phrases | Example | Asks first |
| --- | --- | --- | --- |
| `run_script` | `(run\|execute) (the)? .script. (script)?`, `start (the)? .script. script` | `run backup`, `run the build script` | yes |
| `run_pipe` | `(run\|start) (the)? .pipe. (pipe)?` | `run errors` | when a stage asks |
| `shell` | `$ <command>` | `$ git status`, `$ cd ~/src` | typed on the computer: no; suggested: yes; by voice: refused |

Scripts and saved pipes are defined in `[scripts]` and `[pipes]` (see
[config.md](config.md)). `$ a | b` stays a pipe of the system shell. Pipes of assist
commands are in [automation.md](automation.md#pipes).

## Calls and texts

| Command | Phrases | Examples | Asks first |
| --- | --- | --- | --- |
| `call` | `(call\|phone\|ring\|dial) .contact.` | `call mom`, `ring dad` | yes: "Call Mom?" |
| `send_sms` | `(send (a)? (message\|text\|sms)\|text\|message) (to)? .contact. ((saying\|that) .text.)?` | `text mom saying running late`, `send a message to dad` | yes: "Text Mom: running late?" |

- Contacts come from the phone's address book, plus `[contacts]` in the config.
- Without the message, it asks "What's the message?".
- On the phone these run there. On the computer they run on the paired phone (see
  [devices.md](devices.md)); the reply starts with "On <phone>.".

## Phone and media

| Command | Phrases | Examples | Asks first |
| --- | --- | --- | --- |
| `set_volume` | `(set)? (the)? volume (to)? .level. (percent)?`, `turn (the)? volume (to)? .level. (percent)?` | `volume 30`, `set the volume to 50 percent` | no |
| `lock` | `lock (the\|my)? (phone\|screen\|device)` | `lock the phone` | no |

`lock` on the phone needs the Assist accessibility service (see [android.md](android.md)).

## Screen

| Command | Phrases | Examples | Asks first |
| --- | --- | --- | --- |
| `screenshot` | `(take\|grab\|capture) (a)? (screenshot\|screen shot\|screen grab)`, `screenshot (please)?` | `take a screenshot`, `screenshot` | no |
| `ocr` | `(read\|scan) (the)? (text on)? (the)? screen`, `what does the screen say` | `read the screen`, `ocr screen`, `ocr shot.png` | no |
| `record` | `(start)? (record\|recording) .spec.` | `record screen`, `record audio for 2 min to talk.mka`, `record screen for 30s to demo.mkv` | voice: yes |
| `transcribe` | `transcribe <file>` | `transcribe talk.mka` | no |

- `screenshot [file]` saves a PNG and prints where. On the phone it goes to the gallery,
  `Pictures/Assist`.
- `ocr [screen | image file]` prints the text it reads, with the Windows OCR on Windows. The
  phone reads its screen only (with `ocrs`). Linux desktop OCR is not available yet.
- `record screen|audio [for <length>] [to <file>]`: lengths like `30s`, `30 seconds`,
  `2 min`, `1 hour`, at most 4 hours. Without `for` it records until you stop the job. Needs
  `ffmpeg` on the computer.
- `transcribe <file>` writes the speech in an audio file as text, one line per sentence,
  with the same offline model as `assist listen`. WAV is read directly; other formats go
  through ffmpeg.
- Text from the screen or a recording is data: it is never run as a command.

## Files

| Command | Form | Example | Asks first |
| --- | --- | --- | --- |
| `rename` | `rename <find> [to] <replace> in <folder>` | `rename IMG_ to trip_ in ~/Pictures` | yes, with every change listed |
| `undo` | `undo [job]` | `undo`, `undo 12` | yes, with every change listed |

`rename` replaces text in the names of the files in one folder. The confirmation lists the
changes (the first 20, then a count) and the run refuses if the folder changed in between.
Each rename leaves an undo record; `undo` reverses the last one, `undo 12` the rename of job
12.

## Memory

| Command | Phrases | Examples | Asks first |
| --- | --- | --- | --- |
| `remember` | `remember (that)? .text.`, `make a note (that)? .text.` | `remember the spare key is in the drawer` | voice: yes |
| `recall` | `(what is\|what's\|what are) .subject.`, `do you remember .subject.`, `what do you know about .subject.`, `where did i .subject.`, `where (is\|are) .subject.` | `where is the spare key`, `what's the wifi password` | no |
| `forget` | `forget (about)? .subject.` | `forget the spare key` | yes |

Notes stay on the device (`memory.json`). List and forget them with `assist memory` or the
dashboard's Memory tab.

## Proactive messages

| Command | Phrases | Example |
| --- | --- | --- |
| `mute` | `stop telling me about (the)? .rule.`, `mute (the)? .rule. (rule\|alerts\|messages)?` | `stop telling me about background failed` |
| `unmute` | `(start)? tell me about (the)? .rule. again`, `unmute (the)? .rule. (rule\|alerts\|messages)?` | `tell me about background failed again` |

`.rule.` is the `name` of a rule in `[[proactive.rules]]` (see
[automation.md](automation.md#proactive-messages)).

## Autopilot

| Command | Phrases | Who may say it |
| --- | --- | --- |
| `stop_autopilot` | `(stop\|pause\|halt) (the)? autopilot`, `autopilot off` | anyone, from anywhere |
| `start_autopilot` | `(start\|resume) (the)? autopilot`, `autopilot on` | the owner |

## The assistant itself

| Command | Phrases | Example |
| --- | --- | --- |
| `shutdown` | `shut (yourself)? down`, `shutdown`, `turn yourself off` | `shut down` |

On the computer this stops the daemon (and `assist listen` with it).

Saying the wake phrase alone ("hey Bee") gets "Yes, Prime?"; then say the request.

## Pipe stages

These work as stages after `|` (see [automation.md](automation.md#pipes)):

| Stage | What | Example |
| --- | --- | --- |
| `head <n>` | The first `n` lines | `head 3` |
| `filter <text>` | Lines that contain the text (any case) | `filter error` |
| `grep <regex>` | Lines that match a regular expression | `grep ^E[0-9]+` |
| `count` | The number of lines | `count` |
| `sort` | Lines sorted (at most 100,000) | `sort` |
| `uniq` | Drops repeated lines next to each other | `uniq` |
| `save <path>` | Writes the lines to a file; asks first | `save errors.txt` |

## What works where

| Command | Windows | Linux | Phone | Computer, run on the phone |
| --- | --- | --- | --- | --- |
| open_app | yes | yes | yes | |
| close_app | yes | | | |
| search | yes | yes | | |
| shell, run_script, run_pipe | yes | yes | | |
| call, send_sms | | | yes | yes |
| set_volume, lock | | | yes | yes |
| screenshot, ocr | yes | screenshot needs xcap libraries | yes | |
| record | yes (ffmpeg) | yes (ffmpeg) | yes | |
| transcribe | yes | yes | | |
| rename, undo | yes | yes | | |
| pipe stages (transforms) | yes | yes | yes | |
| remember, recall, forget | yes | yes | yes | |
| mute, unmute, autopilot | yes | yes | mute, unmute | |

A command a device cannot do answers "I can't do that on this device."
