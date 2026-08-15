# Quill Android client

The tablet half of Quill: decodes the H.264 stream the daemon sends over USB and
draws it fullscreen, and sends S Pen / touch input back the other way over the
same connection.

Was `experiments/android-decode-test/` until Milestone 21 — it started as a
throwaway decode spike in Milestone 3 and quietly became the real client. See
MILESTONES.md for anything referring to the old path.

## Build and install

Needs an Android SDK. Point `local.properties` at it (or set `ANDROID_HOME`):

```sh
echo "sdk.dir=$HOME/Android/Sdk" > local.properties
./gradlew assembleDebug
adb install -r app/build/outputs/apk/debug/app-debug.apk
```

Nothing else has to be launched by hand: the daemon switches the tablet into
USB accessory mode, and Android routes the resulting `USB_ACCESSORY_ATTACHED`
intent straight to this app (`app/src/main/res/xml/accessory_filter.xml` has to
keep matching the identification strings `daemon/src/aoa.rs` sends).

## Layout

| File | What it does |
|---|---|
| `MainActivity.kt` | Transport, protocol, decode loop, S Pen capture |
| `Settings.kt` | Persisted options, packed into the handshake's `config_flags` |
| `SettingsActivity.kt` | The settings screen — reached by tapping the status overlay |
| `CursorOverlay.kt` | Draws the desktop pointer, for client-side cursor mode |

The wire protocol both sides speak is documented in one place,
[`daemon/src/protocol.rs`](../daemon/src/protocol.rs). The constants at the
bottom of `MainActivity.kt` mirror it and must be changed together.

## Things that will bite you

- **The decode loop is split across two threads on purpose.** A reader feeds the
  decoder, a separate thread blocks on `dequeueOutputBuffer` and renders.
  Merging them back costs a full frame interval of latency — that was Milestone
  18.
- **Never drop a frame.** Since the encoder gained a real GOP, discarding one
  P-frame corrupts everything until the next IDR, up to a second later.
- **`BufferedAccessoryInput` exists for a reason.** USB accessory fds can't use
  `BufferedInputStream` (`available()` throws) and can't be read unbuffered
  either (an undersized read silently discards the rest of the packet). See its
  doc comment.
