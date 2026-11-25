# Devices: computer and phone together

Pair the phone with the computer once. After that the computer can run phone commands on
the phone ("call mom" said to the computer), and the phone can send its requests to the
computer.

## How devices connect

- Devices talk over [iroh](https://iroh.computer): direct QUIC connections, encrypted end to
  end, across Wi-Fi and mobile data, without a VPN or port forwarding. When no direct path
  exists, traffic goes through n0's free public relays, which see only encrypted data.
- Each device has its own key (`node.key` in its data directory). The key is its identity:
  the computer accepts only keys it paired, and the phone only talks to the computer key it
  paired with.
- The computer is the **hub**: it keeps the list of paired devices (`peers.json`); the phone
  keeps its hub (`hub.json`).
- Nothing here opens a port on your network. The daemon's own WebSocket stays on loopback.

## Pairing

1. On the computer, with `assist serve` running:
   ```sh
   assist pair
   ```
   It shows a QR code and the same code as text:
   ```text
   In the Assist app: Link to computer, then scan this code or paste:
   assist-pair:<computer key>:<secret>
   ```
2. On the phone: **Link to computer**, **Scan** (or paste the text), **Link**.
3. The computer prints `Paired <phone name>.`; the phone shows `Linked to <computer>.`.

The code holds the computer's key and a one-time secret. It works once, for 5 minutes; a new
`assist pair` replaces the old code. Pairing again from the same phone just renames it. Up
to 32 devices can be paired.

Only a token client (the CLI on the computer) can make pairing codes, so a paired phone
cannot pair more devices.

## Phone commands from the computer

The computer offers the phone's commands it cannot do itself: `call`, `send_sms`,
`set_volume`, `lock` (and others the phone announces). When the phone connects it also sends
its contact **names** (not numbers), so the computer understands "text mom".

```text
$ assist run text mom saying running late
Text Mom: running late?
> yes
On CPH2569.
```

1. You confirm on the computer. The request is checked against the computer's policy with
   your trust level there.
2. The computer sends the confirmed plan to a connected phone that announced the command.
3. The phone runs it without asking again (you already confirmed), shows "The computer
   asked: Text Mom: running late" in its chat, and reports the result.
4. The job on the computer shows "On <phone>." and the phone's output.

The phone runs only its own phone commands for the computer; anything else is refused.

Use `assist run`, `assist listen` or the dashboard for this: `assist shell` runs without the
daemon and cannot reach the phone.

## When the phone is away

If no phone that can run the command is connected, the request waits up to **10 minutes**:

- With push set up (below), the job says `Waking <phone>.` and the computer wakes the phone.
- Without it, the job says `Waiting for <phone>; open Assist on it.`
- As soon as the phone connects, the request goes to it.
- After 10 minutes the job fails with `<phone> did not connect within 10 minutes; open Assist
  on it`.

While the Assist app is open it reconnects every 15 seconds, so opening the app is enough.

## Push wake-ups

To reach the phone while Assist is closed, the phone uses
[UnifiedPush](https://unifiedpush.org) with the **ntfy** app as its distributor and ntfy.sh's
public server.

1. Install **ntfy** on the phone (F-Droid or Google Play). No account is needed.
2. Open Assist once after linking. It registers with ntfy and sends its push address to the
   computer on the next connection; the computer keeps it in `peers.json`.
3. When a request waits for the phone, the computer posts an empty message to that address
   (at most once a minute, with the system `curl`, HTTPS only).
4. ntfy wakes Assist, which connects to the computer and gets the request over the secure
   link. It stays up for about 2.5 minutes with a "Running a request from the computer."
   notification.

What the push carries: nothing. It only makes the phone connect; the request itself comes
over the authenticated iroh connection. Someone who learned the push address could make the
phone connect to your computer, and nothing more.

Limits:

- Texts work with the app closed. Calls may not: Android can block an app in the background
  from opening the dialer.
- Battery savers can delay pushes. If wake-ups are slow, let ntfy (and Assist) run
  unrestricted in the phone's battery settings.

## Talking to the computer from the phone

With **Send to <computer>** on, what you type or say with **Talk** goes to the computer's
assistant as its owner, with `owner_remote` trust: for example `run backup` or
`open firefox` on the computer. Replies and job output show in the phone's chat. Switch it
off to talk to the phone itself. Wake-word requests always stay on the phone.

## Seeing your devices

```sh
assist nodes
```

```text
CPH2569              connected, can be woken
    does: call, send_sms, set_volume, lock, open_app, screenshot, ocr, record
    key:  9f2c...
```

The dashboard's **Nodes** tab shows the same: each device's state (connected, away, or away
but can be woken), requests running on it or waiting for it, what it can do, when it was
paired and when it was last seen.

## Removing a device

```sh
assist nodes remove CPH2569      # by name or key; asks first
```

Or **Remove** in the Nodes tab. The device is unpaired at once and its connection dropped;
requests waiting for it fail. To use it again it must pair again with `assist pair`. On the
phone, clear the app's storage (or reinstall it) to forget the computer as well.
