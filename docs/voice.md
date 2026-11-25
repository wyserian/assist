# Voice

assist listens, understands and answers on the device. No audio leaves it. This page covers
the computer; the phone is in [android.md](android.md#voice).

## Listening

1. Start the daemon: `assist serve`.
2. In another terminal:
   ```sh
   assist listen
   ```
3. The first time, it lists the voice models it needs and asks before downloading them.
4. When it prints `Listening on <mic>. Say "hey Bee" first`, say the wake phrase, then the
   request: "hey Bee, open firefox".

```text
Listening on Headset Microphone. Say "hey bee" first; Enter pauses, Ctrl+C stops.
> open firefox  [voice 0.52]
Opening firefox.
```

| Key | What |
| --- | --- |
| Enter | Pause or resume the microphone |
| Ctrl+C | Stop listening |

`--device <text>` picks the microphone whose name contains the text. By default a connected
headset is used, else the system default.

## Wake phrase and awake window

- The wake phrase is "hey <name>", "okay <name>" or "ok <name>", with the name from
  `[persona] name` (default "Bee").
- Set `[persona] wake_word = "bumblebee"` for one longer phrase instead, if the short name
  wakes it by mistake.
- The wake phrase alone gets "Yes, Prime?"; then say the request.
- After an exchange, requests within `[voice] awake` (default 60 seconds) need no wake
  phrase. After that it waits for the wake phrase again.
- Saying the wake phrase while it speaks stops the reply (barge-in).

## Trust and confirmations by voice

Anyone in the room can speak, so voice requests get the `voice` trust level:

- The system shell is refused by voice.
- Calls, texts, file writes, recording and "remember" are confirmed.
- With a voiceprint, a spoken "yes" confirms only when the voice is yours. Without one, or
  when the voice does not match, it says "Sorry, I don't recognise your voice. Confirm on
  screen." and you confirm in the dashboard or the terminal.

The printed `[voice 0.52]` is how close the voice was to your voiceprint.

## Voiceprint

```sh
assist enroll              # record it: read the sentences it shows
assist enroll --forget     # delete it
```

The voiceprint is stored in the data directory (`voiceprint.txt`), readable by your user
only, and never sent anywhere. Tune `[voice] speaker_threshold` (default `0.35`): raise it if
other voices get through, lower it if yours is refused.

## Models

Downloaded once, with your consent, into `<data dir>/models`:

| Model | Size | Used for |
| --- | --- | --- |
| Silero VAD | 1 MB | Telling speech from silence |
| Kroko streaming zipformer (English) | 55 MB | Speech to text |
| Zipformer keyword spotter (gigaspeech) | 18 MB | The wake phrase |
| 3D-Speaker CAM++ | 30 MB | Your voiceprint |

They come from the sherpa-onnx releases on GitHub over HTTPS. Delete the folder to remove
them; `assist listen` asks again next time.

## Spoken replies

- Windows: the built-in Windows voice.
- Linux: speech-dispatcher (`spd-say`), run as a separate program. Install
  `speech-dispatcher` if replies are silent.

## Transcribing files

`transcribe <file>` turns the speech in an audio file into text with the same model (see
[commands.md](commands.md#screen)). The text is data: it is never run as a command.

## Tips

- A headset beats a laptop microphone by a lot.
- Speak the request after a short pause following the wake phrase.
- Words it often mishears can be taught as phrases (see [phrases.md](phrases.md)).
