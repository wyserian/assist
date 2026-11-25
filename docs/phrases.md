# Phrases and replies

assist understands sentences through phrase files: TOML files that list each command, its
slots and the ways of saying it. The built-in English ones are in
[`config/phrases/en/`](../config/phrases/en/) and are built into the program. You can add
your own, override the built-in ones, and reword every reply.

## A phrase file

```toml
[send_sms]
slots   = { contact = "list:contacts", text = "text" }
ask     = { contact = "To whom?", text = "What's the message?" }
confirm = "Text {contact}: {text}?"
reply   = "Sending."
phrases = [
    "(send (a)? (message|text|sms)|text|message) (to)? .contact. ((saying|that) .text.)?",
]
```

| Key | What |
| --- | --- |
| `[name]` | The command. It must be a command assist has (see [commands.md](commands.md)) |
| `slots` | Slot name to type, see below |
| `phrases` | The ways of saying it |
| `ask` | The question per slot when it is missing; without one assist asks "Which <slot>?" |
| `confirm` | The confirmation question, with slot values filled in |
| `reply` | What it says when the command starts |

## Phrase syntax

| Syntax | Means | Example |
| --- | --- | --- |
| `word` | That word | `call` |
| `a\|b` | Either | `call\|ring` |
| `( )` | A group | `(send a message\|text)` |
| `?` | The thing before it is optional | `(the)?` |
| `.name.` | A slot | `.contact.` |

Matching ignores case and punctuation. When several phrases match, the one with the most
literal words wins. When the sentence as said matches nothing, assist tries again without
filler words at the start and end ("okay so can you", "please").

## Slot types

| Type | Matches | Example |
| --- | --- | --- |
| `list:apps` | A name from `[apps]` and the found apps | `open .app.` |
| `list:contacts` | A name from `[contacts]` and the address book | `call .contact.` |
| `list:scripts` | A name from `[scripts]` | `run .script.` |
| `list:pipes` | A name from `[pipes]` | `run .pipe.` |
| `list:rules` | A proactive rule name | `mute .rule.` |
| `number` | A number, digits or words ("forty") | `volume .level.` |
| `text` | Any text | `search .query.` |

List slots match loosely: small misspellings still match, and when a value is close to
several names assist asks "Did you mean A or B?".

## Your own phrases

Put TOML files in the `phrases` folder next to your config:

- Windows: `%APPDATA%\assist\config\phrases\`
- Linux: `~/.config/assist/phrases/`

Every `*.toml` there is read in name order. A command in your file **replaces** the built-in
entry of the same name, so copy the built-in phrases you want to keep:

```toml
# phrases/mine.toml
[open_app]
slots = { app = "list:apps" }
reply = "Opening {app}."
phrases = [
    "(open|launch|start) (the)? .app. (app)?",
    "bring up .app.",
    "fire up .app.",
]
```

Restart the daemon (or the shell) to load hand-edited files.

## Learning from misses

Sentences assist did not understand are kept. In the dashboard's **Commands** tab, under
"Not understood recently", pick the command a sentence meant and save it. assist writes the
phrase to `phrases/user.toml` (copying the built-in entry in first, since a user entry
replaces it) and uses it at once.

To remove a learned phrase, edit `phrases/user.toml` and restart.

## Testing a sentence

```sh
assist explain fire up chrome
```

Prints the command, the slots, the readback and the policy decision, without running
anything. The Commands tab has the same test box.

## Rewording replies

Every fixed reply has a key. The defaults are in
[`config/phrases/en/replies.toml`](../config/phrases/en/replies.toml). Override any of them in
`config.toml`:

```toml
[replies]
greeting = "Hi {address}."
cancelled = "Stopped, {address}."
not_understood = "Say that again?"
```

| Placeholder | Filled with |
| --- | --- |
| `{address}` | `[persona] address` |
| `{text}` | What you said, where the reply quotes it |
| `{slot}` | The slot's name, in `missing_slot` |
| `{options}` | The choices, in `did_you_mean` |
| `{source}` | Where a request came from, in `not_allowed` and `other_owner` |
| `{action}` | The readback, in `confirm` |
| `{condition}` | The condition, in `skipped` |

Command replies (`reply` and `confirm` in phrase files) use the command's slots, such as
`{app}` or `{contact}`.

## Adding a language

Only English exists today. A language is a folder of phrase files like `config/phrases/en/`
plus its `replies.toml`; see [development.md](development.md).
