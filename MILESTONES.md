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
  frames flowing) is exactly when it hung.
- **2026-08-13, tightened client + Wayland retry:** rewrote `update_ready_handler` to
  call `evdi_request_update()` immediately after `grab_pixels`, before any logging, and
  throttled prints to ~1/sec (was: log every frame, request-update last). Rebuilt,
  retried under `kwin_wayland` with the SSH escape hatch confirmed live first. **Same
  `flip_done timed out` stall recurred anyway** — this rules out "test client too slow
  to service updates" as the (sole) cause, at least under Wayland. This time, VT-switch
  (Ctrl+Alt+F3, which didn't respond on the first Wayland incident but did this time)
  successfully recovered the session with **no reboot needed** — `kwin_wayland` kept
  the same PID throughout, never crashed. dmesg showed the same three-strikes pattern
  (`CRTC`/`CONNECTOR`/`PLANE` commit-wait timeouts, ~10s apart) before the client
  disconnected.
- **Final verdict for this machine:** the evdi + KWin/Wayland atomic-commit stall is a
  genuine compositor-level gap (matches design doc §7's flagged risk), not something
  fixed by a faster userspace client. X11 is the only session where this daemon/evdi
  combination self-recovers cleanly and has successfully driven a real extended
  display (3840x1080, `DVI-I-1-1` active). **Decision: target Plasma X11 for all
  further live evdi work on this machine; Wayland is parked, not pursued further** —
  chasing the KWin-internals root cause is out of scope for this project.

- **2026-08-13, `KWIN_DRM_NO_AMS=1` retry:** created
  `~/.config/plasma-workspace/env/evdi-legacy-kms.sh` forcing legacy (non-atomic) KMS
  in KWin. Stall recurred anyway (`flip_done timed out` at t≈9290s, ~10s after
  connect) — self-recovered without reboot this time, screen came back on its own.
  Revised theory: this env var changes which ioctl *KWin* uses, but the timeout comes
  from *inside* evdi.ko's own vblank-completion simulation, which still uses atomic
  helpers internally regardless — wrong lever. Positive side finding: our own client's
  log finally showed `mode_changed` firing correctly (1920x1080@60), confirming
  negotiation itself is solid.
- **2026-08-13, `KWIN_DRM_NO_DIRECT_SCANOUT=1` added:** same stall recurred a third
  time, same ~10s-after-connect pattern, this time needing a manual `pkill` (no
  self-recovery, no reboot). Three-for-three: neither KWin env var lever fixes it.
- **2026-08-13, root cause reframed via evdi's own changelog:** checked
  `github.com/DisplayLink/evdi` releases between our installed 1.14.8 (Debian package,
  Dec 2024) and current (1.15.0, Jul 2026). Directly relevant fixes we don't have:
  **v1.14.11 — "Fix for black screens on Intel Xe" / "Fix for corruptions on Intel
  Xe"**; v1.14.12 — "Fix artifacts on Intel Meteor Lake and newer... integrated
  graphics" / "Fix 'Failed to map scanout buffer' error"; v1.14.13 — same scanout fix
  specifically for "Intel Core Ultra 7". Our GPU is Intel Iris Xe (TigerLake) and our
  symptom is literally a black screen — this is very likely the actual fix, already
  upstream, just not in Debian's packaged `evdi-dkms` (5+ releases behind). Not a KWin
  config problem after all.
- **2026-08-13, built evdi v1.14.16 from source, replaced the apt-installed 1.14.8
  DKMS module and matching `libevdi` (source: `experiments/evdi-bringup/vendor/evdi`,
  scripts: `vendor/upgrade-evdi.sh` and `vendor/swap-evdi-watcher.sh` for the
  module-swap-after-logout dance — old module's refcount never reliably hit 0 without
  a full reboot). Confirmed running (`LibEvdi version (1.14.16)` / `Evdi version
  (1.14.16)` in the test client's own log). Retried under Wayland: **same
  `flip_done timed out` / `[CRTC:36:crtc-0] commit wait timed out` at ~10s
  post-connect, identical to every prior attempt.** The v1.14.11 "black screens on
  Intel Xe" fix addressed a different manifestation, not this one.
- **Final verdict (four reproductions, four different mitigations, all identical
  failure):** no env vars → hard freeze/reboot; `KWIN_DRM_NO_AMS=1` → same stall,
  self-recovered; `+ KWIN_DRM_NO_DIRECT_SCANOUT=1` → same stall, needed manual kill;
  evdi upgraded to 1.14.16 → same stall again. This is an unfixed upstream evdi bug in
  its vblank/flip-completion simulation specifically under `kwin_wayland` real
  compositing on this Intel Iris Xe (TigerLake) GPU — not a KWin config issue, not an
  outdated-package issue, not a client-loop-speed issue. **Wayland is closed out as
  not viable on this machine for this project.** X11 remains the working path (real
  extended display achieved, self-recovers cleanly, `mode_changed`/negotiation both
  confirmed correct). Filing an upstream evdi bug report is a reasonable future step
  but out of scope for this project right now — daemon development proceeds targeting
  Plasma X11.
- **2026-08-13, X11 verdict revised — evdi abandoned entirely.** The real Milestone 2
  daemon (not the trivial test client) caused a **hard freeze requiring reboot under
  X11 too** — first time X11 needed a reboot rather than self-recovering. Leading
  theory: the daemon's per-frame work (BGRX→NV12 conversion + full VAAPI encode
  setup/render/teardown) runs synchronously inside the evdi `update_ready_handler`,
  competing with X11/KWin for the same physical GPU heavily enough to stall the KMS
  commit path harder than the trivial C client ever did. No forensic data survived
  (tmpfs wiped on reboot). Given evdi has now caused genuine freezes under *both*
  session types, at the user's request we researched a Wayland-native alternative
  instead of chasing this further.
- **2026-08-13, `krfb-virtualmonitor` + portal/PipeWire capture — verified as the
  path forward.** KWin has a native, compositor-level virtual-output mechanism
  (`zkde-screencast-unstable-v1`, a KDE-internal Wayland protocol — no stable
  cross-desktop equivalent exists; confirmed the standard
  `org.freedesktop.portal.RemoteDesktop` has no virtual-monitor support at all) that
  never touches DRM/KMS, so the atomic-commit stall class of bug structurally can't
  happen. `krfb-virtualmonitor --resolution 1920x1080 --name QuillTest` created a
  real KWin output (`Virtual-QuillTest`, enabled, connected, correct mode, positioned
  beside the real panel) instantly, no crash. Verified it's genuinely capturable
  through the *standard* portal too: a `spectacle -f` fullscreen screenshot captured
  both the real panel and the virtual monitor's desktop content side by side. Clean
  teardown on `pkill krfb-virtualmonitor` (output disappeared, no leftover state).
  **Decision: pivot away from evdi entirely.** New architecture: `krfb-virtualmonitor`
  (or a from-scratch minimal client of the same protocol, later) creates the virtual
  output; capture moves from `evdi_grab_pixels()` to the standard `ScreenCast` portal
  + PipeWire (mature Rust support via `ashpd` + `pipewire-rs`, same mechanism
  OBS/Sunshine use). VAAPI encoder (`daemon/src/vaapi_encoder.rs`) and H.264 header
  packing (`daemon/src/h264_headers.rs`) are unaffected and carry forward unchanged —
  only the capture-source code (`daemon/src/evdi_capture.rs`) is being replaced.
  `experiments/evdi-bringup/` and the `daemon/src/ffi.rs`/`build.rs` evdi bindings are
  kept as-is for the historical record but are no longer part of the live daemon path.
- **2026-08-13, portal/PipeWire capture implemented and validated end-to-end —
  Milestone 2 done.** New module `daemon/src/portal_capture.rs`: negotiates a
  `ScreenCast` session via `ashpd` (`SourceType::Monitor`, `CursorMode::Embedded`),
  triggers KDE's native screen-picker dialog, opens the PipeWire remote, and connects
  a `pipewire` stream to the selected node requesting `BGRx` (matches evdi's old XR24
  byte order exactly, so `color_convert::bgrx_to_nv12` needed zero changes). Required
  bumping off Debian's packaged rustc 1.85 to a `rustup`-installed 1.97 (ashpd's zbus
  dependency chain needs 1.87+) and installing `libpipewire-0.3-dev`.
  **Live-tested against a real `krfb-virtualmonitor` output under Wayland: 1200+
  frames captured, encoded, and written continuously with zero crashes and zero
  freezes** — real live desktop content confirmed by decoding and viewing a frame
  partway through the capture (showed an actual open browser window). No `pkexec`/root
  needed anywhere in this path (unlike evdi) — portal access and VAAPI both work as
  the normal user. Latency (dequeue-buffer → encoded-bytes-ready), sampled every 30th
  frame over the run: avg 40.85ms, min 35.98ms, max 51.30ms. Isolated VAAPI encode
  alone was ~3-5ms/frame (measured earlier in Milestone 2's first attempt) — the gap
  is almost certainly the unoptimized scalar `bgrx_to_nv12` CPU conversion (a full
  1920x1080 pixel loop, no SIMD). Known v0 optimization opportunity, correctly
  deferred to Milestone 7 (tuning pass) rather than fixed now.
  **Milestone 2 core goal (evdi → VAAPI encode → dump to file, measure latency) is
  met**, on the new capture architecture.
- **2026-08-13, fixed: SIGINT summary not printing.** Root cause was pipewire's own
  `Loop::add_signal_local` signal source: registered without error, but its callback
  never actually fired — `SIGINT` hit the OS default disposition (immediate terminate)
  every time, confirmed via `timeout --signal=INT` deterministically killing the
  process before any post-registration diagnostic ever printed. Tried forcing a
  single-threaded tokio runtime first (`flavor = "current_thread"`, on the theory that
  a multi-threaded runtime could let the signal land on a worker thread that never
  blocked it) — didn't fix it alone, so not the whole story. Replaced it with the
  same plain `libc::signal` + atomic flag + manual `loop_.iterate()` polling pattern
  already used successfully in the old evdi-based daemon (`daemon/src/main.rs`:
  `set_up_sigint_handler`/`sigint_received`; `portal_capture::run_capture` polls it
  every 100ms instead of blocking on `mainloop.run()`). Verified fixed: SIGINT now
  stops the loop, stats compute correctly, and the full summary prints every time.

## 2. Daemon v0 — DONE (capture architecture changed from evdi to portal/PipeWire)

Capture → VAAPI encode → dump to a file, measure encode latency. Originally scoped as
"evdi → VAAPI encode"; evdi was dropped mid-milestone after causing freezes under both
session types — see findings above. Final: `ScreenCast` portal + PipeWire → VAAPI
encode → file, 1200+ frames validated live, avg 40.85ms/frame.

## 3. Transport — DONE

`adb forward` socket, stream encoded frames to a throwaway Android test app, confirm
decode via `MediaCodec`.

**Environment setup:** Debian's packaged Android SDK (API 28) and Gradle (4.4.1) are
years too old for current Kotlin/AGP tooling. Installed `openjdk-21-jdk-headless` via
apt; downloaded Google's official cmdline-tools and used `sdkmanager` to fetch
`platform-tools` (37.0.1), `platforms;android-34`, `build-tools;34.0.0`, all under
`~/Android/Sdk` (home partition — root only had 5.6GB free, home has 50GB). Downloaded
the official Gradle 8.9 binary distribution (paired with AGP 8.5.2, a known-stable
combo) to generate a proper wrapper — apt's Gradle 4.4.1 can't run modern AGP at all.

**Daemon side:** `run_capture` now takes an optional `transport_port`; when set, it
connects out to `127.0.0.1:<port>` (reached via `adb forward tcp:<port> tcp:<port>`,
matching the design doc's transport direction — device listens, host connects) and
writes each frame as a 4-byte big-endian length prefix + the frame bytes, alongside
the existing file output.

**Android side:** `experiments/android-decode-test/` — minimal Kotlin/Gradle app,
`SurfaceView` + `MediaCodec`. Listens on a fixed port (throwaway-scope hardcoded
1920x1080/port 7777 — the real capability handshake with no hardcoded resolution is
Milestone 4+ work, not this transport-validation step), reads length-prefixed frames,
feeds each directly to the decoder with `BUFFER_FLAG_KEY_FRAME` (matches our all-intra
encoding). Built clean on the first real attempt.

**Live end-to-end result:** `adb forward tcp:7777 tcp:7777`, daemon connects, streams
to the Tab S9 FE+. 458 frames captured → encoded → transported → decoded → rendered
in one continuous run, clean shutdown on both sides (`stream ended: null` on Android,
full summary on the daemon), no crashes anywhere in the chain.

**Found and diagnosed a real bug along the way:** using the default (hardware)
decoder via `MediaCodec.createDecoderByType`, the tablet's screen rendered solid
green instead of the actual desktop content — but `queued`/`rendered` counters
confirmed the decoder *was* successfully decoding and rendering every frame; this
was a color-interpretation bug, not a decode failure. Cross-checked by forcing the
AOSP software decoder (`MediaCodec.createByCodecName("c2.android.avc.decoder")`):
**colors rendered correctly**, isolating this to a hardware-decoder-specific quirk on
this tablet's chip, not a bug in our encoding pipeline or app code (also independently
corroborated: the exact same bytes, decoded via `ffmpeg`/libavcodec on the desktop
earlier in Milestone 2, already looked correct). Our H.264 stream carries no VUI color
metadata (`vui_parameters_present_flag=0`); Android's logged output format shows it
guessing `color-standard=1` (BT.709) with no signal to go on either way. **Open item
for Milestone 4:** figure out why the hardware decoder specifically mishandles this
and fix it (likely needs explicit VUI color signaling in our hand-built SPS, or a
different level/profile setting this chip's decoder is stricter about) — hardware
decode is required for real latency numbers, software decode doesn't represent
achievable performance.

**Also expected, not a bug:** streaming felt laggy with visible motion
duplication/ghosting when the mouse stopped moving. Consistent with known v0
limitations stacked together — every frame is a full independent I-frame (~26-45KB
each, no P-frame savings), software-decoded, over the full USB adb-tunnel path.
Tuning (real GOP structure, buffer sizing) is explicitly Milestone 7's job.

## 4. Android client v0 — DONE

Decode + render only, measure glass-to-glass latency with a stopwatch/high-fps camera
test.

**Fixed the Milestone 3 hardware-decoder color bug.** Hypothesis: Constrained
Baseline profile forces CAVLC entropy coding, a much less-used/tested code path on
most decoder silicon than CABAC (used by Main/High profile) — plausible cause of a
hardware-specific decode bug that both `ffmpeg` and Android's software decoder didn't
hit. Switched the daemon to H.264 Main profile + CABAC:
`daemon/src/vaapi_encoder.rs` (`VAProfile_VAProfileH264Main`,
`entropy_coding_mode_flag=1`) and `daemon/src/h264_headers.rs` (`profile_idc: 77`,
matching `entropy_coding_mode_flag` bit in the hand-built PPS — this has to stay in
sync with the VAAPI-side picture-parameter setting, or the PPS metadata would lie
about what's actually in the bitstream). Validated locally first (`ffprobe` now
reports `profile=Main`, decodes clean, frame extracted and visually correct) before
touching the tablet again.

**Live-tested on the real hardware decoder (`c2.exynos.h264.decoder`, confirmed via
Android's own `codec.name` log)**: colors render correctly. 447 frames captured,
encoded, transported, decoded, and rendered in one continuous run, clean shutdown
both sides. Bonus: CABAC compresses better than CAVLC, so frames also got smaller
(~18.7KB avg vs ~26KB before, same content).

**Remaining for this milestone:** measure actual glass-to-glass latency. The design
doc calls for a stopwatch/high-fps camera test — inherently a physical, hands-on
measurement (filming both the source screen and the tablet simultaneously and
comparing timestamps frame-by-frame), needs the user's participation, not something
that can be done from the terminal.

**2026-08-13, glass-to-glass latency measured — Milestone 4 done.** Method: a
millisecond-precision clock (`requestAnimationFrame`-driven, see the approach in this
session) shown in two windows, one on the real screen, one dragged onto the virtual
monitor (so it's captured/streamed/decoded like any other content). Both read the same
system clock, so filming both screens together in slow-motion and reading the two
displayed values in a single video frame gives the latency directly. Two
measurements: 53.580 vs 53.262 (**318ms**), 52.447 vs 52.145 (**302ms**). User also
observed the tablet's effective refresh rate looked noticeably slower/choppier than
the source's.

**~300ms is far higher than our own "dequeue→encoded" instrumentation ever showed**
(avg ~40ms throughout this same run, 5124 frames, no growth trend). Root cause: that
metric only times from calling `dequeue_buffer()` to finishing encode — it excludes
any time a frame already spent sitting in PipeWire's internal buffer queue *before* we
got to it. The clock's source content changes about as fast as the display refreshes
(~60Hz), but our own pipeline can only process one frame every ~40ms (~25fps) purely
for capture+encode, before transport and decode are even counted — since nothing in
the current pipeline checks buffer timestamps or drops stale/backlogged frames, we're
structurally slower than the rate content changes, so a growing backlog of
increasingly-stale buffers accumulates and we always end up encoding/sending an old
one. This is a coherent, root-caused explanation, not a mystery: buffer staleness
invisible to our own internal timer, not (only) an inherently-slow individual stage.

**Confirms the known Milestone-2 optimization flag is the right next lever, not just
theoretical:** the pipeline currently does a lot of avoidable CPU↔GPU round-tripping
— `StreamFlags::MAP_BUFFERS` explicitly requests CPU-mapped memory from PipeWire
(not zero-copy DMA-BUF), then `bgrx_to_nv12` runs a scalar CPU loop, then that result
gets copied back up to a GPU surface for VAAPI. A real hardware-accelerated pipeline
(SuperDisplay-equivalent, and what PipeWire/VAAPI both actually support) would import
a DMA-BUF handle directly into a VAAPI surface with zero CPU copies and let the GPU do
color conversion too, keeping every stage's ~40ms cost down and directly shrinking the
backlog that causes the 300ms glass-to-glass gap. **Concrete direction for Milestone
7:** (1) negotiate/import DMA-BUF from PipeWire instead of `MAP_BUFFERS`, (2) always
grab the newest available buffer and explicitly drop stale queued ones instead of
processing in strict FIFO order, (3) a real GOP with P-frames instead of all-intra, to
cut per-frame cost further.

## 5. Input path — DONE

uinput virtual tablet device on Linux, static test with synthetic events in Krita/GIMP.

**Implementation:** `daemon/src/uinput_tablet.rs` (`UinputTablet`, reusable module for
Milestone 6) using the `input-linux` crate rather than hand-rolled ioctls — its API is
clean and well-typed, and `/dev/uinput` already had a working per-user ACL grant from
earlier in the session, no root needed. Declares `BTN_TOOL_PEN`/`BTN_TOUCH`/
`BTN_STYLUS` and `ABS_X`/`ABS_Y`/`ABS_PRESSURE`/`ABS_TILT_X`/`ABS_TILT_Y`. Key/button
events are edge-triggered (only sent on an actual state change, matching real
hardware) rather than resent every frame. Test harness: `daemon/src/bin/uinput_test`,
a separate throwaway binary that creates the device and injects a synthetic diagonal
stroke with pressure ramping 0→max→0, GIMP open with the Paintbrush tool and
pressure-sensitive dynamics enabled.

**Debugging journey — device was invisible on screen despite being 100% correct at
the kernel level.** First attempts: cursor never appeared to move, no stroke drawn in
GIMP, in either native-Wayland GIMP or GIMP forced through XWayland (same underlying
Wayland session either way, so not a true independent test — user firmly ruled out
switching to a full X11 session again, given it broke before too, so this was worked
through entirely on Wayland).

Root-caused methodically:
1. `udevadm info` confirmed `ID_INPUT_TABLET=1` — udev classification was never the
   problem.
2. `evtest` (needed `/dev/input/eventN` access — fixed by adding the user to the
   `input` group via `usermod -aG input`, then using `sg input -c '...'` to run
   commands with that group active without a full re-login; far less friction than
   repeated `pkexec` prompts) confirmed, at the raw kernel level, that our events were
   flawless: smooth `ABS_X`/`ABS_Y`/`ABS_PRESSURE`/`ABS_TILT_X` changes, correct
   `SYN_REPORT` framing, correct `BTN_TOUCH` 1→0 transition at stroke end. The kernel
   and evdev layer saw exactly what they should — ruling out our event-emission code
   as the cause.
3. Web research: even Weylus (this project's own cited precedent) documents Wayland
   input support as "experimental" with acknowledged gaps; GfxTablet needed a
   dedicated "Wayland-compatible fork". Confirms this is a known rough edge in the
   ecosystem generally, not something obviously wrong in our approach.
4. Found the actual fix by comparing against `LinusCDE/rmTabletDriver` (a real,
   community-tested uinput tablet driver for the reMarkable): it explicitly sets a
   non-zero `resolution` field (units/mm) on `ABS_X`/`ABS_Y`, while our code had
   `resolution: 0` ("no calibration data") on every axis. Changed `ABS_X`/`ABS_Y`
   resolution to `100` (arbitrary but plausible units/mm, matching that reference) —
   **this fixed it immediately.** Likely explanation: libinput needs real resolution
   data to compute a valid tablet-to-screen coordinate mapping; without it, the
   device was correctly classified as a tablet but effectively unmappable, so no
   visible pointer motion was ever produced despite perfect kernel-level events.

**Confirmed working end to end:** cursor moves, `BTN_TOUCH` registers as a genuine
click/drag system-wide (first observed indirectly — a full corner-to-corner stroke
grabbed and dragged whatever window was frontmost), and a stroke deliberately
centered on a smaller coordinate range (40%-60% of the full range, landing inside
GIMP's canvas instead of sweeping through surrounding panels) produced a real,
visibly pressure-tapered brush stroke in GIMP — thin → thick → thin, confirmed
directly by the user. Milestone 5's exit criterion is met.

## 6. End-to-end input — DONE

Wire Android `MotionEvent` capture → protocol → uinput injection end to end.

**Implementation.** Android side: `MainActivity.kt` registers `setOnTouchListener` (finger/
pen contact) and `setOnHoverListener` (S Pen hover, proximity without contact) on the
`SurfaceView`. `tiltXY()` converts Android's single tilt-from-vertical `AXIS_TILT` +
`orientation` into Wacom-style perpendicular `tiltX`/`tiltY` via trig, matching the
daemon's `ABS_TILT_X`/`ABS_TILT_Y` axes. `sendHandshake()` sends the real capability
handshake before any input records — actual `Display` metrics and the stylus device's
`InputDevice.getMotionRange()` for pressure and tilt, not per-device constants, per the
design doc's hard constraint. Daemon side: `input_receiver.rs` runs on its own thread,
reading a cloned half of the same TCP socket video is written on (input flows
Android→daemon, video flows daemon→Android, independent directions of one connection —
no separate port). It reads the handshake once, creates the `uinput` tablet from the
*real* reported ranges (`TabletRanges`, reusing Milestone 5's `UinputTablet`), then loops
injecting events via `tablet.emit()`.

**Bug hit and fixed: `android.os.NetworkOnMainThreadException`.** First live test failed
silently (`"failed to send input event: null"` — an unhelpfully stripped log call).
Fixed the logging to print the full exception (`Log.w(tag, "...", e)`), rebuilt, and got
the real exception: touch/hover listener callbacks run on Android's UI thread, and
writing directly to the socket from inside them (as the first version did) violates
Android's ban on network I/O on the main thread. **Fix:** `handleMotionEvent` now only
does a cheap, non-blocking `eventQueue.offer(PenEvent(...))` (a `LinkedBlockingQueue`); a
dedicated `inputWriterThread` blocks on `eventQueue.take()` and performs the actual
`DataOutputStream` writes. Thread is started right after the handshake is sent and
interrupted in the decode loop's `finally` block on teardown.

**Bug hit and fixed: tablet took over the main screen depending on where the desktop
cursor started.** Live-tested end to end (daemon log showed real events — hover, moves,
pressure varying 1044–2319 — reaching uinput), but drawing on the tablet sometimes
dragged the cursor around the *laptop's built-in screen* instead of the virtual monitor.
Root cause: `libinput list-devices` showed the virtual tablet with an unrestricted
`Calibration: identity matrix` / `Area rectangle: (0,0)-(1,1)` — i.e. its normalized
0..1 input range was mapped across KWin's *entire combined desktop geometry*
(`kscreen-doctor -o` confirmed two outputs side by side: `eDP-1` at `0,0 1536x864` and
`Virtual-QuillTest` at `1536,0 1920x1080`), not just the virtual monitor. **Fix:** KDE's
own `kcm_tablet` panel (System Settings → Graphics Tablet) lists the device (it shows up
there because `input_linux`'s uinput device declares itself as a proper tablet — pressure,
tilt, `BTN_TOOL_PEN`) and offers a per-device screen-mapping dropdown; setting it to
`Virtual-QuillTest` and applying fixed it immediately, confirmed live by starting the
desktop cursor on the main screen and drawing — stroke landed correctly on the virtual
monitor regardless. No code change needed. Because the uinput device uses a fixed,
hardcoded `InputId` (`bustype: BUS_VIRTUAL, vendor: 0x1209, product: 0x0001` — see
`uinput_tablet.rs`), this mapping is a **one-time setting that persists across daemon
restarts** on a given machine (KDE keys it by device identity), not a per-session
workaround. Should be noted in setup docs as a one-time step on a new machine.

**Confirmed working end to end:** real device metrics in the handshake
(`2560x1498 px, pressure 0..4095, tilt -90..90 deg`, matching the actual Tab S9 FE+ — no
hardcoding), real varying pressure values reaching the daemon, and a real S Pen stroke
drawn on the tablet appearing correctly in GIMP on the virtual monitor.

**Known gap, not blocking:** S Pen side button (`BTN_STYLUS`) is not wired up.
`input_receiver.rs` defines `EV_BUTTON_DOWN`/`EV_BUTTON_UP` (protocol event types 6/7)
but the receive loop doesn't act on them yet — declared as dead code, flagged as a
follow-up rather than part of this milestone's exit criteria.

## 7. Tuning pass — IN PROGRESS

Encoder settings, buffer sizes, thread priorities to cut jitter. Three concrete items
were recorded as Milestone 4's findings: (1) import DMA-BUF from PipeWire instead of
`MAP_BUFFERS`+CPU color convert, (2) drop stale queued buffers instead of strict FIFO,
(3) real GOP with P-frames instead of all-intra.

**Item 2, drop-stale-frames — implemented.** `portal_capture.rs`'s `process` callback
now drains all currently-available PipeWire buffers and only encodes the newest one;
older ones are dropped unprocessed (auto-requeued to PipeWire via `Buffer`'s `Drop`
impl — confirmed by reading `pipewire-0.10.0/src/buffer.rs` before relying on it).
`CaptureStats` gained a `dropped_stale` counter, logged periodically and in the final
summary.

**Live re-measurement of glass-to-glass latency, same method as Milestone 4** (two
`requestAnimationFrame`-driven clock windows, one on the main screen, one dragged onto
the virtual monitor, both filmed together in slow-motion): **~150-170ms** (readings:
170ms, 160ms, 150ms) versus Milestone 4's ~300-320ms — roughly halved. User also
reported the tablet's effective framerate looked noticeably smoother than before.

**Important honesty check, not just declaring victory:** the same run's own
`dropped_stale` counter read **0** across 7452 frames — the drop-stale logic never
actually triggered. `dequeue→encoded` averaged 14.7ms/frame (well under the 16.6ms
60Hz budget), so this specific run was never backlogged on the capture/encode side
to begin with. That means **the ~150ms improvement can't be cleanly attributed to
this fix** from this run's evidence alone — it may be genuine (Milestone 4's baseline
run could plausibly have hit backlog under different conditions) or it may be
run-to-run variance (thermal/driver-cache state, background load, etc.) unrelated to
the code change. Recorded honestly rather than overclaiming causation.

**Re-scoping items 1 and 3 given this data:** with capture+encode already at ~14.7ms
average and zero observed backlog, the CPU↔GPU round-trip that DMA-BUF import (item 1)
would eliminate is only a fraction of that already-small number — its realistic upside
on the *glass-to-glass* number looks smaller than Milestone 4's analysis assumed,
though it would still cut CPU load. Frame sizes are already small (30-80KB), so P-frame
GOP (item 3) mainly helps bandwidth, not latency. The remaining ~150ms most likely
lives *outside* the daemon's own capture/encode stage — PipeWire's own internal
buffering before a frame reaches us, the adb/USB transport hop, or (a well-known
source of multi-frame latency on Android) `MediaCodec`/`SurfaceView`'s own buffer
queueing on the decode+render side. None of that is instrumented yet.

## 8. (Optional v2) AOA transport

Swap adb transport for Android Open Accessory mode, drop the adb dependency entirely.
