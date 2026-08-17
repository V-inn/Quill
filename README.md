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
- A Linux desktop running KDE Plasma (GNOME not supported yet)
- A USB cable between the two

No new kernel drivers or firmware required. The virtual display and screen capture go
through standard desktop mechanisms (the `ScreenCast` portal and PipeWire — the same
plumbing tools like OBS and Sunshine use); the pen input side uses the standard Linux
virtual-input facility (uinput), the same one other Linux tablet-input tools use for
pressure and tilt.

## Features

- Extended display over USB, connecting directly to the tablet (no `adb` relay
  involved) — plug the tablet in and it starts on its own, no manual steps
- Full S Pen input: pressure, tilt, hover, and the side button
- Multi-touch gestures — two-finger scroll, pinch-to-zoom, tap-to-click — handled the
  same way a laptop touchpad's gestures are
- Portrait and landscape, with a display-flip setting for cable orientation
- A settings screen on the tablet (display orientation, cursor mode, gesture mode)
- Recovers on its own from a dropped cable, an app restart, or a daemon restart
- ~55ms glass-to-glass latency (camera-measured)

## Project status

Core path works end to end on KDE Plasma: display, S Pen, multi-touch, auto-launch,
and auto-reconnect are all in place and the latency target has been met. Two known
gaps: GNOME isn't supported yet (KDE's virtual-display tooling doesn't have a GNOME
equivalent), and the tablet currently shows as mirroring the main display rather than
extending it. See [`MILESTONES.md`](./MILESTONES.md) for the full history and what's
still open.

## Repo layout

- `assets/` — GIFs used in this README.
- `daemon/` — the Linux-side program (written in Rust) that talks to the tablet:
  captures the screen, sends video to it, and turns pen input back into something
  Linux understands.
- `android-client/` — the app that runs on the tablet: displays the video feed and
  reports pen/touch input back to the daemon.
- `experiments/` — throwaway spikes used to validate ideas before they become real
  code; not part of the shipped daemon or client.
- `MILESTONES.md` — the build plan and progress tracker.
- `linux-tablet-display-design.md` — the full technical design, for readers who want
  the details of how the pieces fit together.

## License

MIT — see [`LICENSE`](./LICENSE).
