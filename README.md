# Quill

Linux equivalent of SuperDisplay: use a Samsung Galaxy Tab (S9 FE+, S10 FE+, or any
Wacom-EMR S Pen tablet) as a real extended Linux display over USB, with full S Pen
fidelity (pressure, tilt, hover, side button) and low latency.

Full architecture: [`linux-tablet-display-design.md`](./linux-tablet-display-design.md).

## Why this works without new kernel drivers

- **evdi** — existing, DisplayLink-maintained kernel module. Registers a real DRM/KMS
  output the desktop treats as a normal monitor.
- **uinput** — standard in-kernel facility for virtual input devices (used by Weylus,
  GfxTablet today for pressure/tilt).

So the actual new work is one Linux daemon (capture/encode + input injection) and one
native Android app (decode + `MotionEvent` capture) — no new kernel code.

## Repo layout

- `daemon/` — Linux side, Rust. evdi capture, VAAPI encode, uinput input injection.
- `android-client/` — native Kotlin app. MediaCodec decode, MotionEvent capture.
- `experiments/` — throwaway validation code, one directory per milestone spike. Not
  part of the real daemon/client; kept so spikes don't contaminate real history.
- `MILESTONES.md` — build plan, checked off as milestones are validated.

## Status

Milestone 1 (evdi bring-up) — see `MILESTONES.md`.

## Constraint that shapes everything

No hardcoded resolution, DPI, refresh rate, or pressure/tilt ranges anywhere. The
protocol opens with a capability handshake (Android reports `Display` metrics and
`InputDevice.getMotionRange()` at connect time); the daemon configures evdi/uinput from
that. This is what lets the same daemon/client pair work unmodified across tablets —
treat any device-specific constant that creeps in as a bug.
