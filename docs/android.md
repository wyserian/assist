# Android app

The Assist app runs the same core as the computer, on the phone, offline. It handles calls,
texts, volume, locking, apps, screenshots, screen text and recording, by typing or voice, and
can link to the computer.

Requirements: Android 8.0 (API 26) or later; screenshots need Android 11. Built and tested on
arm64 phones; an x86_64 build is included for the emulator.

## Build

You need, besides Rust and [just](https://github.com/casey/just):

| Tool | Version | Notes |
| --- | --- | --- |
| Android SDK | compileSdk 36 | Android Studio installs it |
| Android NDK | 28.2.13676358 | Set `ANDROID_NDK_HOME` to it |
| JDK | 17+ | Android Studio's bundled JBR works |
| [cargo-ndk](https://github.com/bbqsrc/cargo-ndk) | latest | `cargo install cargo-ndk` |
| Rust targets | | `rustup target add aarch64-linux-android x86_64-linux-android` |

Then:

```sh
just android
```

This builds the core for arm64 and x86_64 (fetching the sherpa-onnx Android libraries once,
checked against a pinned SHA-256), generates the Kotlin bindings and runs Gradle. The APK is
`android/app/build/outputs/apk/debug/app-debug.apk`.

On Windows (PowerShell), point the build at the tools first, for example:

```powershell
$env:ANDROID_NDK_HOME = "C:\Android\ndk\28.2.13676358"
$env:JAVA_HOME = "C:\Program Files\Android\Android Studio\jbr"
just android
```

## Install

With USB debugging on and the phone connected:

```sh
adb install -r android/app/build/outputs/apk/debug/app-debug.apk
```

`-r` keeps the app's data (pairing, models, notes) when updating.

## First start

1. Open **Assist**.
2. Allow **contacts**, so "call mom" and "text dad" find people by name.
3. Type a request in **Say something** and tap **Send**, or tap **Talk** and say it.

Other permissions are asked the first time a request needs them: phone (calls), SMS,
microphone, notifications, camera (only while scanning a pairing code) and screen capture
(each time you record the screen).

## Using it

| Control | What |
| --- | --- |
| **Say something** + **Send** | A typed request |
| **Talk** | Push-to-talk: one spoken request; **Stop** ends it early |
| **Listen for the wake word** | Hands-free: listens for "hey Bee" in the background, with a notification while it does |
| **Yes** / **No** | Appear when a request heard by the wake word needs a confirmation |
| **Link to computer** | Pair with the computer (see below) |
| **Send to <computer>** | When on, typed and push-to-talk requests go to the computer instead of this phone |

Assist can also be the phone's digital assistant app: choose it in Android settings under
Default apps, Digital assistant app. A long press on Home (or the assistant gesture) then
opens it listening.

### What it does on the phone

| Say | Notes |
| --- | --- |
| `call mom`, `text dad saying on my way` | Asks first, then calls or sends the SMS |
| `open camera`, `open whatsapp` | Any launcher app, by its label |
| `set volume to 40` | Media volume, in percent |
| `lock the phone` | Needs the Assist accessibility service |
| `take a screenshot` | Saved to the gallery in `Pictures/Assist`; needs the accessibility service and Android 11 |
| `read the screen` | Reads the text on the screen (screen only, not image files) |
| `record screen for 30s`, `record audio for 2 min` | Asks Android for screen capture each time |
| `remember ...`, `where is ...`, `forget ...` | Notes stay on the phone |
| Pipes, e.g. `read the screen \| filter total` | The text transforms work on the phone too |

Full list: [commands.md](commands.md).

### The accessibility service

`lock` and `screenshot` use the **Assist** accessibility service. The first time, the app
opens Accessibility settings: turn on Assist there, then ask again. The service subscribes to
no events and reads no window content; it only locks and takes screenshots when you ask.

### Voice

Voice works offline. The first time you use push-to-talk, the wake word or screen reading,
the app shows the models it needs with their size and asks before downloading them. Spoken
replies use the phone's text-to-speech voice.

- Push-to-talk requests count as typed (you are holding the phone); the reply is spoken.
- Wake-word requests get `voice` trust: calls and texts then need **Yes** on screen.

### Proactive messages on the phone

The phone has its own facts: `node.battery`, `node.charging` and `screen.locked`. Rules in
its config (see below) can use them, such as a low battery warning. Messages are spoken, or
shown as notifications.

## The phone's config

The app reads an optional `config.toml` (same format as the computer's, at most 64 KB) from
its private files. With a debug build you can put one there:

```sh
adb push config.toml /data/local/tmp/assist-config.toml
adb shell run-as dev.assist.app cp /data/local/tmp/assist-config.toml files/config.toml
adb shell rm /data/local/tmp/assist-config.toml
```

Then close and reopen the app. Useful sections: `[persona]`, `[voice]`, `[contacts]`,
`[commands.<name>]`, `[replies]`, `[proactive]`. See [config.md](config.md).

## Linking to the computer

1. On the computer: `assist serve`, then in another terminal `assist pair`.
2. In the app: **Link to computer**, then **Scan** the code (or paste its text, which starts
   with `assist-pair:`) and tap **Link**.
3. The app says "Linked to <computer>". **Send to <computer>** is switched on.

While the app runs it keeps a connection to the computer, so "call mom" said to the computer
runs on the phone. Each such request shows in the chat as "The computer asked: ...".

To let the computer reach the phone while Assist is closed, install the **ntfy** app (from
F-Droid or Google Play). Assist registers with it after linking; if it is missing the app says
"Install the ntfy app so the computer can reach this phone while Assist is closed."

Details and limits: [devices.md](devices.md).

## Troubleshooting

| Problem | Try |
| --- | --- |
| "Allow contacts so I can call and text people by name." | Allow contacts in the app's Android settings |
| A contact is not found | Check the name in the address book; `assist explain call <name>` on the computer shows the match |
| "Turn on Assist in Accessibility settings, then ask again." | Settings, Accessibility, Assist, on |
| It did not hear me | Check the microphone permission; speak once the button shows **Stop** |
| "The computer is not reachable; is assist serve running?" | Start `assist serve`; check that both devices are online |
| "Not linked: the pairing code is wrong or expired; make a new one" | Codes work once, for 5 minutes: run `assist pair` again |
