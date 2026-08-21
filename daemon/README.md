# Quill daemon

The Linux half of Quill. It captures a virtual monitor, encodes it to H.264 on
the GPU, ships the frames to the tablet over USB, and turns the pen and touch
records that come back into real Linux input devices.

The tablet half lives in [`../android-client`](../android-client/README.md).

---

## What you need

- **A supported tablet** — Samsung Galaxy Tab S9 FE or newer, S10 FE or newer;
  anything with a Wacom EMR S Pen. Plus the pen, plus a USB cable.
- **KDE Plasma or GNOME.** Nothing else is supported: the virtual monitor is made
  through compositor-specific machinery, and there is no portable way to do it.
  GNOME support is written but has never been run against a real GNOME session —
  see [Project status](../README.md#project-status).
- **A GPU that can encode H.264 through VAAPI.** The encoder is hardware-only and
  there is no software fallback. Check with `vainfo` — you need a line reading
  `VAProfileH264*` with `VAEntrypointEncSlice`:

  ```sh
  vainfo | grep -E 'VAProfileH264.*EncSlice'
  ```

  Intel iGPUs and AMD cards generally have this. NVIDIA does not, through VAAPI.

---

## Build

### Dependencies

Debian / Ubuntu:

```sh
sudo apt install build-essential pkg-config curl clang libclang-dev \
                 libva-dev libpipewire-0.3-dev libusb-1.0-0-dev
```

Fedora:

```sh
sudo dnf install gcc gcc-c++ pkgconf-pkg-config clang-devel \
                 libva-devel pipewire-devel libusb1-devel
```

`clang`/`libclang` is there for `bindgen`, which generates the VAAPI bindings at
build time from `wrapper.h`.

**On KDE you also need `krfb`**, which provides `krfb-virtualmonitor` — the tool
that creates the virtual display. `sudo apt install krfb` or
`sudo dnf install krfb`. **On GNOME you need nothing extra**: mutter makes the
virtual monitor itself, through the same call that captures it.

Then Rust, if you don't have it:

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

### Compile

```sh
git clone https://github.com/V-inn/Quill.git
cd Quill/daemon
cargo build --release
```

---

## Install

`packaging/install.sh` puts the binary and a `systemd --user` unit where they
belong, then prints the two `sudo` lines you have to run yourself:

```sh
./packaging/install.sh
```

The two udev rules it names do different jobs:

| Rule | Why |
| --- | --- |
| `70-quill-uinput.rules` | Lets your user open `/dev/uinput`, so the daemon can create the virtual pen, touchpad and pointer. **This is the only thing in Quill that needs root**, and only once. |
| `99-quill-daemon.rules` | Starts the daemon automatically when the tablet is plugged in. Skip it if you would rather launch by hand. |

`install.sh` checks whether `/dev/uinput` is already accessible and stays quiet
about the first rule if it is — some systems already ship an equivalent (Steam
does). To check for yourself:

```sh
getfacl /dev/uinput | grep "$USER" && echo "already accessible"
```

**Log out and back in after installing the uinput rule.** It grants access
through a logind ACL attached to your session, so it does not apply to the
session that was already running when you installed it.

---

## Run

With the udev rule installed there is nothing to run: plug the tablet in, unlock
it, and the daemon starts. Otherwise:

```sh
quill              # AOA over USB, the normal mode
quill 7777         # adb-forward TCP instead, for development

# Capture only, with no tablet at all -- note this is the raw binary, not the
# wrapper: `quill` always passes a transport, and anything that isn't `aoa` is
# parsed as a TCP port.
quill-daemon ~/.local/share/quill/output.h264
```

**First run on KDE** shows the desktop portal's screen-picker dialog once. Pick
`Virtual-QuillDisplay`. The answer is remembered, so it never asks again — that
is what lets the daemon start unattended on a later plug-in. **On GNOME there is
no dialog at all.**

Then drag a window onto the tablet.

---

## Environment variables

Mostly diagnostics. None are needed for normal use.

| Variable | Effect |
| --- | --- |
| `QUILL_BACKEND` | `gnome` or `kde`, overriding detection. |
| `QUILL_CURSOR` | `client` or `embedded`, overriding the tablet's own setting. |
| `QUILL_GNOME_SCALE` | Sets the virtual monitor's scale on GNOME, when mutter guesses badly. |
| `QUILL_GNOME_IS_PLATFORM` | `0` makes GNOME treat the output as a shared screen, showing its screen-sharing indicator. |
| `QUILL_FORCE_NO_UINPUT` | Pretends `/dev/uinput` is unavailable, to exercise the no-root input fallback. |
| `QUILL_FORCE_SHM` | Drops the DMA-BUF offer, forcing PipeWire's shared-memory path. |
| `QUILL_NO_ENCODE` | Captures without encoding, to measure the capture path alone. |
| `QUILL_DUMP_H264` | Writes the encoded stream to the output path. |
| `QUILL_DUMP_FRAME` | Dumps the first raw frame to `/tmp/quill_frame_dump.ppm`. |
| `QUILL_BARCODE_PROBE` | Decodes the latency probe's barcode from captured frames. |
| `QUILL_USB_RESET` | Resets the USB device once before connecting, for a wedged port. |

---

## When it doesn't work

Start here — the daemon narrates every stage, tagged by subsystem
(`[desktop]`, `[transport]`, `[aoa]`, `[portal]`, `[gnome]`, `[orientation]`,
`[pipewire]`, `[vaapi]`, `[input]`):

```sh
journalctl --user -u quill-daemon -f     # if it was started by udev
quill                                     # or just run it in a terminal
```

| Symptom | Cause |
| --- | --- |
| `failed to open /dev/dri/renderD128` | Your user can't reach the GPU. Add yourself to the `render` group and log back in. If the node doesn't exist at all, there is no VAAPI-capable GPU. |
| The pen does nothing, `[input] /dev/uinput not accessible` | The uinput rule isn't installed, or you haven't logged out and back in since. On KDE the daemon falls back to position-and-click only; on GNOME it exits and tells you the fix. |
| No virtual monitor appears on KDE | `krfb` isn't installed, so `krfb-virtualmonitor` isn't there. `[orientation] failed to spawn` says so. |
| The picker dialog appears on every launch | The saved answer was revoked. Pick the monitor once more; if it keeps happening, something else is invalidating it — check that a second daemon isn't running (`[lock] another quill daemon is already running`). |
| Nothing happens on plug-in | The udev rule isn't installed, or your user session isn't active. `udevadm monitor` while plugging in shows whether the rule fires. |
| `[transport] AOA connect failed` | The tablet is locked, or the app isn't installed, or the cable is charge-only. Unlock the tablet first — Android will not route the accessory intent to a locked device. |
| The tablet shows a frozen or black screen after a replug | Stale USB data survived the reconnect. The daemon recovers on its own now; if it doesn't, `QUILL_USB_RESET=1 quill`. |
| The tablet mirrors your main display instead of extending it | Known, on KDE. Switch it to "Extend" in System Settings → Display. |

---

## Layout

| File | What it does |
| --- | --- |
| `main.rs` | Startup order, backend choice, session setup |
| `desktop.rs` | KDE vs GNOME detection |
| `aoa.rs` | USB accessory transport |
| `protocol.rs` | The wire format, mirrored by hand in `MainActivity.kt` |
| `portal_capture.rs` | Portal ScreenCast session + the PipeWire capture loop |
| `gnome_screencast.rs` | Mutter's virtual monitor, via `RecordVirtual` |
| `gnome_display.rs` | Desktop layout, via mutter's `DisplayConfig` |
| `orientation.rs` | The KDE virtual monitor, and the layout both backends share |
| `vaapi_encoder.rs` | GPU colour conversion and H.264 encode |
| `input_receiver.rs` | Decodes pen/touch records and drives the uinput devices |
| `gesture.rs` | Pinch vs scroll recognition |
| `uinput_*.rs` | The virtual tablet, touchpad and button devices |
| `remote_desktop_input.rs` | The no-root input fallback, via the RemoteDesktop portal |

The wire protocol is documented in one place, [`src/protocol.rs`](src/protocol.rs).
The constants at the bottom of `MainActivity.kt` mirror it and must change
together.
