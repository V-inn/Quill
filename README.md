# Quill

Linux equivalent of SuperDisplay: use a Samsung Galaxy Tab as an extended desktop
display over USB, with S Pen input (pressure, tilt, hover, side button) working like a
normal drawing tablet.

Supported hardware: Samsung Galaxy Tab S9 FE and newer, S10 FE and newer — any tablet
with a Wacom EMR S Pen.

## Demo

<table>
<tr>
<td width="50%">

![A window being dragged from the desktop onto the tablet, which acts as an extended display](./assets/extended-display.gif)

Dragging a window from the desktop onto the tablet as an extended display.

</td>
<td width="50%">

![Drawing in GIMP with the S Pen, with pressure and tilt working live on the tablet](./assets/s-pen-gimp.gif)

Drawing in GIMP with the S Pen — pressure and tilt working live.

</td>
</tr>
</table>

## What it needs to work

- A supported Samsung Galaxy Tab with its S Pen
- A Linux desktop running KDE Plasma, or GNOME (GNOME support is new and not yet
  tested on real hardware — see [Project status](#project-status))
- A USB cable between the two

No new kernel drivers or firmware required. The virtual display and screen capture go
through standard desktop mechanisms (PipeWire plus the `ScreenCast` portal on KDE, or
GNOME's own screen-cast interface — the same plumbing tools like OBS and Sunshine
use); the pen input side uses the standard Linux virtual-input facility (uinput), the
same one other Linux tablet-input tools use for pressure and tilt.

Quill needs root exactly once, to install a udev rule granting your user access to
`/dev/uinput` — the device that lets it create the virtual pen and touchpad. Many
systems already have an equivalent rule from another package; `install.sh` checks and
tells you if you don't need it. Nothing else in Quill runs as root, and it never asks
for a password again.

## Getting started

Three steps: build the daemon on your computer, install the app on the tablet,
plug them together.

### 1. The daemon

You need a GPU that can encode H.264 through VAAPI — the encoder is hardware-only.
Check with `vainfo | grep -E 'VAProfileH264.*EncSlice'`; if that prints nothing,
Quill will not run. Intel and AMD graphics generally work, NVIDIA does not.

```sh
# Debian / Ubuntu
sudo apt install build-essential pkg-config curl clang libclang-dev \
                 libva-dev libpipewire-0.3-dev libusb-1.0-0-dev

# ...plus krfb on KDE, for the virtual monitor. GNOME needs nothing extra.
sudo apt install krfb

# Rust, if you don't have it
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

```sh
git clone https://github.com/V-inn/Quill.git
cd Quill/daemon
cargo build --release
./packaging/install.sh
```

`install.sh` prints two `sudo` lines to run — one udev rule that lets Quill create
the virtual pen (this is the only step that needs root, and only once), and one
that starts the daemon when you plug the tablet in. **Log out and back in
afterwards**, so the pen permission applies to your session.

Full detail, every option, and a troubleshooting table:
[`daemon/README.md`](./daemon/README.md).

### 2. The tablet

```sh
cd Quill/android-client
echo "sdk.dir=$HOME/Android/Sdk" > local.properties
./gradlew assembleDebug
adb install -r app/build/outputs/apk/debug/app-debug.apk
```

More in [`android-client/README.md`](./android-client/README.md).

### 3. Plug it in

Connect the cable and unlock the tablet — Android will not hand the connection to
an app while the screen is locked. The app opens on its own.

On KDE, the first run shows your desktop's screen-picker dialog once: choose
`Virtual-QuillDisplay`. The answer is remembered, so it never asks again. On GNOME
there is no dialog.

Then drag a window onto the tablet.

## Features

- Extended display over USB, connecting directly to the tablet (no `adb` relay
  involved) — plug the tablet in and it starts on its own, no manual steps
- Full S Pen input: pressure, tilt, hover, and a remappable side button
- Multi-touch gestures — two-finger scroll, pinch-to-zoom, tap-to-click — handled the
  same way a laptop touchpad's gestures are
- Rotate the desktop 0, 90, 180 or 270 degrees — a quarter turn asks for a
  portrait desktop that then fills the landscape panel exactly
- Run the desktop smaller than the panel, so everything on it is bigger
- A settings screen on the tablet: rotation, desktop size, picture quality, frame
  rate, pointer mode, gestures, S Pen side button, and a latency readout
- Keeps the tablet awake while a desktop is on it, and lets it sleep when not
- Recovers on its own from a dropped cable, an app restart, or a daemon restart
- ~55ms glass-to-glass latency (camera-measured)

## Project status

Core path works end to end on KDE Plasma: display, S Pen, multi-touch, auto-launch,
and auto-reconnect are all in place and the latency target has been met.

**GNOME support is written but untested.** GNOME's compositor turns out to have its
own virtual-monitor interface, which is a better fit than KDE's — it creates the
display and streams it in one step, with no screen-picker dialog to click through.
That path is implemented and the daemon picks it automatically on a GNOME session,
but the only machine this project is developed on runs Plasma, so nobody has yet run
it against a real GNOME desktop. If you try it, the daemon logs every step of the
setup; bug reports with that output are exactly what it needs. On GNOME the S Pen
requires the `/dev/uinput` rule above — there's no reduced-capability fallback there
yet the way there is on KDE.

One other known gap, on KDE: the tablet often comes up mirroring the main display
rather than extending it, and has to be switched over by hand in display settings.
Whether GNOME behaves the same way is one of the things a first real run there would
answer. See [`MILESTONES.md`](./MILESTONES.md) for the full history and what's still
open.

## Repo layout

- `assets/` — GIFs used in this README.
- `daemon/` — the Linux-side program (written in Rust) that talks to the tablet:
  captures the screen, sends video to it, and turns pen input back into something
  Linux understands. [Build and run instructions](./daemon/README.md).
- `android-client/` — the app that runs on the tablet: displays the video feed and
  reports pen/touch input back to the daemon.
- `experiments/` — throwaway spikes used to validate ideas before they become real
  code; not part of the shipped daemon or client.
- `MILESTONES.md` — the build plan and progress tracker.
- `linux-tablet-display-design.md` — the full technical design, for readers who want
  the details of how the pieces fit together.

## License

MIT — see [`LICENSE`](./LICENSE).
