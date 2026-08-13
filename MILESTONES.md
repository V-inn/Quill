# Milestones

Each milestone is validated (by the user) before starting the next.

## 1. evdi bring-up — IN PROGRESS

Get evdi building and loaded as a DKMS module, create a virtual output, confirm it
shows up as a real display in the desktop environment, and confirm raw framebuffer
frames can be read from userspace.

- [ ] `dkms status` shows evdi installed for the running kernel; `lsmod` shows it loaded
- [ ] `experiments/evdi-bringup/evdi_test_client` connects; `mode_changed_handler` fires
      with nonzero dimensions
- [ ] evdi output visible and enableable as an extended display in KDE (record which
      session type worked: Wayland or X11 fallback)
- [ ] `update_ready_handler` fires continuously once the output is active/composited
- [ ] `adb devices -l` shows the tablet in `device` (authorized) state over USB
- [ ] Findings recorded below: session type, KWin quirks/crashes seen, exact
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
- **Working theory:** this is not Wayland-specific. Both the hard Wayland freeze and
  the milder X11 freeze happened around evdi modeset/connect-state transitions on this
  machine's Intel TigerLake Iris Xe GPU, which has *both* `i915` and `xe` kernel
  drivers available (`i915` currently bound, per `lspci -k`). Whether the dual-driver
  availability is related is unconfirmed — worth checking `i915`-vs-`xe` driver
  binding and searching for known evdi/i915 TigerLake modeset issues before further
  live testing. **Treat every future evdi connect/disconnect on this machine as
  freeze-risk** regardless of session type, keep the SSH escape hatch ready, and save
  work first.

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
