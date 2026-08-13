# Milestones

Each milestone is validated (by the user) before starting the next.

## 1. evdi bring-up — MOSTLY DONE (one item deferred to Milestone 2)

Get evdi building and loaded as a DKMS module, create a virtual output, confirm it
shows up as a real display in the desktop environment, and confirm raw framebuffer
frames can be read from userspace.

- [x] `dkms status` shows evdi installed for the running kernel; `lsmod` shows it loaded
- [x] connects; mode negotiated with nonzero dimensions — confirmed kernel-side via
      `dmesg`: `Notifying mode changed: 1920x1080@60; bpp 32` (not yet confirmed via
      our own `mode_changed_handler` log — client was killed before that print flushed)
- [x] evdi output visible and enableable as an extended display in KDE under X11 —
      `xrandr --output DVI-I-1-1 --auto --right-of eDP-1` extended the desktop to
      3840x1080 cleanly. Wayland (kwin_wayland) is NOT usable for this — hard freeze,
      required reboot.
- [ ] `update_ready_handler` fires continuously once active/composited — **blocked**:
      sustained real compositing triggers a `flip_done timed out` DRM stall with our
      single-threaded test client (see finding below). Real daemon needs a proper
      continuous servicing loop; deferred to Milestone 2 rather than retried here with
      more live-freeze risk.
- [x] `adb devices -l` shows the tablet in `device` (authorized) state over USB —
      confirmed: `RX2Y200XQ3R device usb:3-4 product:gts9fepwifixx model:SM_X610`
      (Galaxy Tab S9 FE+ Wi-Fi). `adb forward tcp:9873 tcp:9873` also confirmed working.
- [x] Findings recorded below: session type, KWin quirks/crashes seen, exact
      evdi/kernel/adb versions

**Findings:**

- **2026-08-13, KWin/Wayland session (Plasma 6.3.6, kwin_wayland 6.3.6):** loading
  `evdi` (`initial_device_count=1`) and connecting via `evdi_test_client` worked fine —
  `card1` appeared, KWin opened it, `mode_changed` looked to be about to fire. Shortly
  after connect, the screen went black and the machine was fully unresponsive for
  ~20s; had to hard power-cycle (confirmed via `journalctl --list-boots`: previous boot
  ended abruptly with no clean shutdown, immediately followed by a new boot). Kernel
  log from the crashed boot wasn't captured (no elevated journalctl access at the
  time) — root cause unconfirmed, but this matches the exact risk flagged in
  `linux-tablet-display-design.md` §7 (KWin+evdi Wayland compatibility is unresolved
  upstream). **Do not run evdi test clients under the Wayland session again without a
  recovery escape hatch in place** (SysRq, a free VT, or SSH-from-another-device) and
  all other work saved/closed first.
- Next attempt should target the `plasmax11.desktop` (Plasma X11) SDDM session per the
  plan's fallback path, with the above precautions.
- **2026-08-13, Plasma X11 session (kwin_x11), first connect:** loaded evdi, connected,
  KDE popped its native display-arrangement dialog (mirror/extend/etc) — good sign, no
  freeze. User closed the dialog without choosing an option, so KWin never issued a
  modeset; `mode_changed_handler` never fired. Clean SIGINT shutdown afterward, no
  crash.
- **2026-08-13, Plasma X11 session, second connect:** reconnected, this time enabled
  the output manually via `xrandr --output DVI-I-1-1 --auto --right-of eDP-1` instead
  of the dialog. Succeeded — virtual screen extended to 3840x1080, `DVI-I-1-1` active
  at 1920x1080. Then, on disconnect/cleanup (exact trigger unconfirmed — either the
  xrandr disable or the client process ending), the machine froze/black-screened
  again — this time briefly, and it **self-recovered** without a reboot (confirmed via
  `journalctl --list-boots`: same boot session throughout, no new boot entry).
- **2026-08-13, root cause confirmed via `sudo dmesg` (captured over SSH during the
  freeze, per §"escape hatch"):**
  ```
  evdi evdi.0: [drm] *ERROR* flip_done timed out
  evdi evdi.0: [drm] *ERROR* [PLANE:32:plane-0] commit wait timed out
  evdi: [I] (card1) VT switch detected
  evdi: [I] (card1) Notifying display power state: off
  ```
  Not a GPU/driver crash — a DRM atomic-commit stall. KWin issued a real pageflip to
  the evdi output and blocked waiting for `flip_done`; evdi's virtual plane didn't
  signal completion in time. Atomic commits can bundle all active CRTCs/planes
  together, so a stuck evdi plane stalled the real eDP panel's pageflip too — that's
  the black screen, not a system crash. Kernel's own recovery (forced VT switch +
  display power-off) is what self-recovered under X11; under Wayland it apparently
  didn't recover and required a hard reboot (same underlying stall, worse recovery
  path — consistent with design doc §7's flagged KWin/Wayland fragility).
  Corroborating evidence: the *first* X11 attempt (dialog closed, KWin never issued a
  modeset) had zero freeze. The *second* attempt (`xrandr --auto`, real composited
  frames flowing) is exactly when it hung. Working theory: `evdi_test_client`'s
  event loop (single-threaded `poll()`, 1s timeout) isn't servicing
  `evdi_request_update()`/`evdi_grab_pixels()` fast enough to keep up with KWin's
  ~60Hz pageflip pacing — a throwaway-test-client limitation, not an evdi/hardware
  incompatibility. The real Milestone 2 daemon has an actual continuous capture/encode
  loop and should not hit this. **Still keep the SSH escape hatch ready for any future
  live evdi test** until the real daemon's loop is proven to keep up.

## 2. Daemon v0

evdi → VAAPI encode → dump to a file, measure encode latency.

## 3. Transport

`adb forward` socket, stream encoded frames to a throwaway Android test app, confirm
decode via `MediaCodec`.

## 4. Android client v0

Decode + render only, measure glass-to-glass latency with a stopwatch/high-fps camera
test.

## 5. Input path

uinput virtual tablet device on Linux, static test with synthetic events in Krita/GIMP.

## 6. End-to-end input

Wire Android `MotionEvent` capture → protocol → uinput injection end to end.

## 7. Tuning pass

Encoder settings, buffer sizes, thread priorities to cut jitter.

## 8. (Optional v2) AOA transport

Swap adb transport for Android Open Accessory mode, drop the adb dependency entirely.
