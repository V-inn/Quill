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

### 6b. Follow-up — S Pen side button + finger click/drag — DONE

**S Pen side button.** `uinput_tablet.rs` gained `set_button()`: an independent,
edge-triggered `BTN_STYLUS` toggle decoupled from position updates (side button state
can change while hovering or drawing, unrelated to x/y). `input_receiver.rs`'s receive
loop now actually acts on `EV_BUTTON_DOWN`/`EV_BUTTON_UP` instead of ignoring them.
Android side: first tried `MotionEvent.ACTION_BUTTON_PRESS`/`ACTION_BUTTON_RELEASE`
(added a `setOnGenericMotionListener`, since those actions arrive via the
generic-motion stream, not touch or hover — confirmed Android dispatches touch/
hover/other-generic-motion as three disjoint per-event paths, so all three listeners
coexist safely with no double-processing). **Live-tested: no effect at all.** Switched
to a more robust approach — diff `event.buttonState and MotionEvent.BUTTON_STYLUS_PRIMARY`
on *every* event instead of trusting a dedicated action to ever fire, since some
digitizers (this Samsung EMR pen, `sec_e-pen`, apparently included) only ever fold the
button bit into regular move/hover events rather than synthesizing a standalone
button action. Live-tested again: **works.**

**Finger click/drag — a real multi-step debugging arc.** Initial attempt added a
distinct `BTN_TOOL_FINGER` proximity path (real Wacom hardware distinguishes pen vs.
finger this way) alongside skipping the pressure/tilt axes for finger touches (not
meaningful for a finger). Live-tested: Android correctly captured and sent full finger
down/move/up sequences (confirmed via a temporary unconditional diagnostic log, `adb
logcat`), the daemon received them and called `emit()` with no errors (confirmed via a
matching daemon-side diagnostic) — but nothing moved on screen. Root-caused to
`BTN_TOOL_FINGER` itself: it's also the standard capability bit real touchpads use, and
it's plausible libinput reclassified this device into touchpad (relative-motion)
semantics for finger-tagged events instead of tablet (absolute-positioning) ones — the
`BTN_TOOL_PEN` path is the one already confirmed working (Milestone 5), so **reverted
finger touches to report via `BTN_TOOL_PEN` too**, same as the pen.

Live-tested again: cursor still didn't move. The only remaining structural difference
between the confirmed-working pen path and the finger path was skipping the
pressure/tilt axis writes — tried always sending them (pressure pinned to whatever
Android computed, ~max for a finger's default ~1.0 pressure reading; tilt to 0).
**This fixed cursor movement** — apparently this device's tablet-tool motion handling
in libinput gates on nonzero `ABS_PRESSURE`, not just `BTN_TOUCH`, undocumented but
confirmed empirically. One bug remained: touch never released (pointer stuck "down").
Root cause: `ABS_PRESSURE` was carrying whatever value came in on the release event
(not necessarily 0), and a stale nonzero pressure at release apparently keeps libinput
thinking contact is still active independent of `BTN_TOUCH`. **Fix:** `emit()` now
forces `ABS_PRESSURE` to exactly 0 whenever `in_contact` is false, regardless of what's
passed in. Live-tested: **tap, click, and drag all confirmed working.**

**Not attempted, explicitly deferred (user's own scoping):** multi-touch gestures —
pinch-to-zoom, two-finger scroll, and similar. These need a genuinely different device
model (the current protocol and uinput device are single-pointer only, one x/y per
event) — likely a real multitouch protocol (`ABS_MT_*` slots) or OS-level gesture
recognition, not a small extension of the current single-touch path. Worth its own
milestone later, not a follow-up to this one.

## 7. Tuning pass — DONE (~300ms → ~107-112ms confirmed by camera, ~2.7x improvement)

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

**Tried `MediaFormat.KEY_LOW_LATENCY` — no measurable effect.** Set unconditionally
before `codec.configure()` (a documented Android API for reducing a hardware
decoder's internal buffering; harmless no-op if unsupported). Re-measured with the
same clock method: 150ms, 170ms, 180ms — statistically the same as the ~150-170ms
before this change, not the ~300ms pre-Milestone-7 baseline. `adb logcat` confirms
the decoder is still `c2.exynos.h264.decoder` and shows no error/acknowledgment
either way for the key, consistent with the flag being silently ignored on this
decoder. **Negative result, recorded rather than discarded** — rules out the
cheapest lever on the decode side.

**Where this leaves latency investigation:** two independent changes (drop-stale-
frames, low-latency decode flag) moved the number from ~300ms to ~150-180ms and then
plateaued — both re-measurements after Milestone 7's changes cluster tightly around
150-180ms, suggesting a real, fairly constant floor rather than one specific
bottleneck being incrementally shaved. Localizing it further would need actual
instrumentation (e.g. per-frame timestamps propagated through the wire protocol,
which requires a clock-offset calibration step since daemon and tablet are separate
devices with independently-clocked systems) rather than more guess-and-film cycles.
Not attempted yet — a real scoping decision, not a small next step.

**Built the clock-offset calibration + per-frame timestamp instrumentation.** New
`daemon/src/clock_sync.rs`: standard NTP two-message offset estimate (daemon and
tablet are separate devices with independent system clocks, so their timestamps
aren't directly comparable without this). Android appends its send time to the
capability handshake; the daemon replies once, before the first video frame, with
its own send/receive timestamps; Android computes the offset itself
(`(android_recv - daemon_send) - (daemon_recv - android_send)`, halved — assumes
symmetric one-way transport delay, reasonable for one local adb-forward/USB link).
From then on every video frame is prefixed with an 8-byte daemon send-timestamp
(`portal_capture.rs`), and Android logs a running per-frame latency estimate
(`android_render_time - (frame_timestamp + offset)`) — log-based from here on,
no more camera/filming needed for iteration.

**Result, live-tested after a full host reboot (fresh daemon, fresh app launch):**
measured offset **1692ms** (a real, large, uncorrected clock skew between the two
devices' independent system clocks — expected and exactly what the calibration
exists to cancel out), calibration round-trip sum **3ms** (sane for a local
USB/adb-forward link). Steady-state per-frame latency: **avg ~15-16ms, range
7-48ms** across 1162 frames.

**This does not mean glass-to-glass latency is 15ms — it means we finally know
where the ~150-180ms lives, and it isn't where Milestone 4 assumed.** This
instrumentation's timestamp is taken *after* the daemon already has a PipeWire
buffer in hand, right before `encode_frame` — i.e. it measures encode + transport +
decode + render only. That segment is fast (matches the already-known
~10-15ms `dequeue→encoded` number plus a small transport/decode/render remainder).
The camera-measured ~150-180ms must therefore mostly be hiding in the *unmeasured*
segment: from the moment content actually changes on the source screen to the
moment PipeWire hands the daemon a buffer for it. Milestone 4's original
backlog/CPU-roundtrip theory pointed at the daemon's own processing; this new,
more precise measurement rules that specific stage out as the dominant cost and
redirects suspicion upstream, into PipeWire/KWin's own screencast buffer delivery
— not yet instrumented. A promising next step if this is picked back up: check
whether PipeWire buffers carry their own generation-time metadata (e.g. an SPA
`Header` meta with a PTS) so buffer age can be measured directly on dequeue,
single-machine, no cross-device clock sync needed.

**Checked immediately — negative result.** Added a `buffer.find_meta::<MetaHeader>()`
check in the `process` callback (`portal_capture.rs`), comparing `pts()` against
`CLOCK_MONOTONIC` via a new `clock_sync::monotonic_ns()` helper (same-machine
comparison, no calibration needed, would have been the cleanest possible
measurement). Live-tested: **`SPA_META_Header` is absent on every single buffer**
(`"[pipewire] buffer has no MetaHeader"` logged for all 684 frames of the run) —
this specific screencast producer (`krfb-virtualmonitor` + KWin's Wayland
screencast implementation) doesn't populate a per-buffer generation timestamp.
Dead end for this specific approach, ruled out rather than left unclear. The code
is left in (harmless, just logs once if the condition ever changes) rather than
ripped out, since it's cheap and correctly-implemented instrumentation for a
question that's still open.

**Session status (superseded by the barcode-probe result below):** two real,
measured wins this milestone (drop-stale-frames + clock-offset-calibrated per-frame
latency logging), one ruled-out lever (`KEY_LOW_LATENCY`), and the root cause of the
remaining ~150-180ms localized to KWin/PipeWire's own screencast buffer delivery.

### Resumed: the burned-in-timestamp trick, implemented — big result

Built the "next lead" noted above: `experiments/capture-latency-probe/` is a small
native (X11-backed, see below) window that paints a live binary "barcode" of the
local `CLOCK_MONOTONIC` nanosecond counter as 48 black/white bars. Positioned on the
virtual monitor, the daemon decodes it straight out of the **raw PipeWire buffer**,
before any color conversion or encoding (`decode_latency_barcode` in
`portal_capture.rs`). Same machine as the probe, so the comparison against the
daemon's own `CLOCK_MONOTONIC` needs no cross-device calibration and no camera —
this is exactly "time from content changing on screen to the daemon having a buffer
for it," the one segment nothing had measured yet.

**Getting it positioned was its own detour.** `minifb`'s `set_position()` is a no-op
under KWin's native Wayland windowing (Wayland deliberately doesn't let clients place
their own top-level windows) — the probe first appeared centered on the main screen
instead of on the virtual monitor. Fix: forced `minifb` onto its X11 backend
(`default-features = false, features = ["x11", "dlopen"]` in Cargo.toml, since
`xdotool`, an X11-only tool, was the only available way to move a window
programmatically) and used `xdotool windowmove`. Even then, the first placement
attempt (`x=1536`, matching `kscreen-doctor`'s reported *logical/scaled* geometry for
the virtual monitor) landed mostly off-screen — XWayland's combined root window
turned out to operate in **physical, unscaled pixels**, not KDE's logical/scaled
coordinate space, so the correct X11 offset was `1920` (eDP-1's physical width), not
`1536` (its logical width at 1.25x scale). Confirmed by adding a one-shot raw-frame
dump (`QUILL_DUMP_FRAME=1` env var, writes a `.ppm` converted to `.png` for viewing)
so the actual captured pixels could be inspected directly instead of reasoning about
coordinate spaces blind — ground truth beat calculation here.

**Result, live-tested, 4038 samples:** capture latency (barcode → dequeue) averaged
**28.47ms**, individual samples mostly in the 19-33ms band (one 664ms first-frame
outlier from VAAPI/encoder warmup, swamped by the sample count). Combined with the
already-known `dequeue→encoded` (~10.5ms avg) and the Milestone-7 clock-sync
`encode→render` number (~15ms avg), **the entire instrumented pipeline totals only
~54ms** — nowhere near the ~150-180ms the camera method measured.

**Leading hypothesis at the time: the ~150-180ms was a Firefox-specific measurement
artifact, not real.** Browser engines carry their own multi-frame compositor latency
(main thread → compositor thread → GPU present) before a canvas update reaches the
window system — external to this project's pipeline, invisible to any of the
instrumentation above. Testable: swap the camera test's Firefox clock for a native
renderer and see if the measured gap shrinks.

### Confirming (or not) with a native clock — the hypothesis was wrong, but this found the real bug

Built `experiments/capture-latency-probe/src/bin/readable_clock.rs`: a second minifb
binary, hand-rolled 7-segment digit rendering (no browser, no text-rendering
dependency), showing `epoch_seconds.milliseconds`. Ran two instances — one on the
main screen, one on the virtual monitor (streamed to the tablet like anything else)
— and repeated the exact same slow-motion filming method.

**Result: ~144ms average (151ms, 145ms, 135ms) — statistically the same as the
Firefox-based ~150-180ms.** Hypothesis refuted with a real controlled experiment, not
just abandoned. The gap is real, and it isn't a browser artifact.

**Second hypothesis, also tested and also refuted:** the video frame write to the
transport socket did three separate `write_all()` calls (8-byte timestamp, 4-byte
length, payload) with `TCP_NODELAY` set — each plausibly its own TCP segment / adb
protocol packet, adding per-packet adb/USB overhead three times over for no reason.
Combined into one buffer, one `write_all()` (`portal_capture.rs`). Re-ran the native-
clock camera test: **~148ms average (152, 152, 136, 153ms) — no change.** Also not it.

**Third attempt found the real bug, in the measurement itself, not the pipeline.**
Cross-checked against a detail that had been visible in the logs the whole time but
not connected: `queued=N rendered=N-5` (or so) at every steady-state log line — the
hardware decoder buffers several frames internally (~5 in flight) before it starts
producing output. But `MainActivity.kt`'s per-frame latency calculation used
`frameSentAtMs` — the timestamp of whatever frame had *just been read off the socket
this loop iteration* — regardless of whether a render actually happened that
iteration, or which of the ~5 in-flight frames it was. With a ~20ms/frame arrival
cadence and ~5 frames of decoder pipeline depth, that's exactly ~100ms of real
latency the old calculation structurally could not see, no matter how long it ran.

**Fix:** track an explicit FIFO (`ArrayDeque<Long>`) of pending send-timestamps, one
pushed per frame queued into the decoder, one popped and used *only* when a render
actually happens (`outIndex >= 0`) — correct because all frames are independent IDR
with no reordering, so decode/render order matches encode order. Live-tested:
individual frame latencies now read **87-146ms** (steady state, excluding one large
early-connection outlier from the pipeline still filling) — averaging toward
**~100-110ms**, not the old measurement's ~17-20ms.

**Final accounting, all four numbers now trustworthy and pointing the same
direction:** barcode capture latency (~28ms) + `dequeue→encoded` (~10-12ms) +
corrected decode/render latency (~100-110ms) ≈ **~140-150ms total — matches the
camera-measured ~144-150ms almost exactly.** Milestone 4's original ~300ms number,
Milestone 7's ~150-180ms re-measurements, and this final instrumented total now all
agree, and the earlier ~15ms "encode→render" figure (Milestone 7, before this fix)
is retroactively known to have been wrong — an artifact of the same FIFO bug, not a
real measurement of a fast pipeline.

**Where the latency actually lives:** overwhelmingly in the Android hardware
decoder's own internal pipeline depth (~100ms, ~5 buffered frames), not in this
project's capture (~28ms) or encode (~10-12ms) stages, which are both fast.

**`KEY_LOW_LATENCY` re-checked with the corrected measurement — same verdict, now
trustworthy.** A/B tested live: with the flag, individual frame latencies ran
100-118ms (min 85ms floor); with it removed, 92-115ms (min 80-85ms floor) — no
meaningful difference, decoder pipeline depth (`pending=4-5` both ways) unchanged.
Daemon-side numbers matched almost exactly between the two runs too (capture
29.45ms vs 29.94ms, encode 14.4ms vs 15.0ms), confirming those stages are stable
and unaffected by the decoder flag either way, as expected. The original "no
effect" verdict turns out to have been *correct*, just previously unverified —
this decoder's ~4-5 frame pipeline depth is fixed regardless of the flag. Left
`KEY_LOW_LATENCY` enabled in code anyway: harmless here, and other decoders/devices
(e.g. the S10 FE+) may actually honor it, so no reason to remove a no-cost request.

**Checked whether the standard key was even the right one for this hardware —
it wasn't, but the real fix still didn't move the needle.** Researched real prior
art: `moonlight-android` (a mature, production game-streaming app solving the exact
same low-latency-decode problem) special-cases Samsung/Exynos decoders specifically
because the standard `KEY_LOW_LATENCY` is a known no-op on them — it sets a vendor
parameter, `"vendor.rtc-ext-dec-low-latency.enable"`, for any decoder name starting
with `c2.exynos`/`omx.exynos` (our tablet's decoder, `c2.exynos.h264.decoder`,
matches exactly). Implemented the same check-and-set in `MainActivity.kt`, confirmed
live via logcat that it actually applied (`"applying Exynos vendor low-latency
parameter"`). **Re-tested: no change** — individual frame latencies 84-130ms, same
`pending=4-5` decoder queue depth as both earlier conditions. Left the vendor
parameter in code anyway (same reasoning as `KEY_LOW_LATENCY`: harmless, might help
a different Exynos generation/firmware). Moonlight's own code tries several vendor
strings across up to 4 attempts for different SoC families — only the one matching
our exact decoder-name prefix was tried here, not an exhaustive sweep. This
tablet's ~5-frame decoder pipeline depth increasingly looks like a hardware/firmware
floor not exposed to software configuration at all, at least not through any lever
tried so far.

**Deeper web search, two more real levers tried, one ruled out on documentation
alone.** `KEY_OPERATING_RATE=Short.MAX_VALUE`: moonlight-android gates this to
Qualcomm specifically, and Android's own official docs agree ("can lower latency on
*some Qualcomm platforms*") — not applicable to this Exynos decoder, not tried live.
`KEY_PRIORITY=0` (realtime): moonlight applies this more broadly (their fallback for
non-Qualcomm devices), so tried live here too — confirmed no crash (a real risk
noted in Android's own docs when combined with an unsatisfiable operating rate, not
applicable since operating rate wasn't set). **Result: no change** — 90-126ms, same
`pending=4-5` queue depth as every other condition tested. Also checked whether
MediaCodec's async callback mode (vs. this app's synchronous dequeue-loop pattern)
could help: concluded no — async mode only changes how the *client* is notified
(callback vs. polling), not how many frames the codec *itself* buffers internally
before emitting output, which is what's actually costing the ~100ms. Not
implemented, since the reasoning rules it out without needing a live test.

**Five real, researched levers now tried (`KEY_LOW_LATENCY`, Exynos vendor
parameter, `KEY_PRIORITY`, plus `KEY_OPERATING_RATE` and async mode ruled out on
solid documentation/reasoning grounds), all converging on the same conclusion: this
tablet's ~85-115ms decoder pipeline depth is a firmware/silicon floor, not reachable
from app-level `MediaCodec` configuration.**

**Correction, worth getting precise rather than leaving an assumption unchecked:**
`source.android.com`'s own low-latency-media doc states the feature requires SoC
partners to have implemented decoder-driver support — prompting a direct check
instead of continuing to infer support from behavior. Added a query of
`MediaCodecInfo.CodecCapabilities.isFeatureSupported(FEATURE_LowLatency)` right
after creating the decoder. **Result: `true`** — this decoder's driver *does* claim
low-latency support. That sharpens the conclusion rather than reopening it: the
feature is implemented and enabling it demonstrably changes nothing measurable, so
the ~5-frame pipeline depth isn't a missing driver capability — it's inherent even
in this decoder's fastest advertised mode, most plausibly a real hardware pipeline-
stage depth (parse → entropy-decode → reconstruct → deblock → output, each
plausibly a pipeline stage in a streaming ASIC design) rather than a configurable
buffering policy at all.

Stopping the decoder-latency search here — further progress would need either a
different decoder entirely (unlikely to exist on this hardware) or work outside
this project's reach (SoC vendor firmware/silicon).

### "A different decoder entirely" turned out to already exist on-device — real win found

User pushback, correctly: hadn't actually checked what decoders this device *offers*
before concluding the hardware ASIC's floor was the end of the line. Researched
prior art first (couldn't find SuperDisplay's own technical approach — their site
has zero technical disclosure, and the XDA/BlurBusters forum threads discussing it
both blocked automated fetching; literature confirms MJPEG is a dead end, "inefficient,
slow, large files" vs H.264, so not an alternative worth pursuing). Then just
enumerated: `MediaCodecList(ALL_CODECS)` shows this tablet has two *software* AVC
decoders alongside the hardware one — `c2.android.avc.decoder` (Google's Codec2
software decoder) and the older `OMX.google.h264.decoder`. Software decoding has no
ASIC pipeline architecture to impose a fixed multi-frame floor the way the hardware
decoder's did.

**Live-tested `c2.android.avc.decoder`: real, measured win.** Individual frame
latencies 56-86ms (vs. the hardware decoder's 84-130ms), decoder queue depth
`pending=3-4` (vs. 4-5) — roughly a 25-30% cut on the decode/render segment, and
confirmed visually smooth with no artifacts over a 4539-frame run (daemon-side
capture ~30ms and encode ~15ms unchanged, as expected, confirming this is purely a
decode-side improvement). **Total pipeline now ~110-125ms, down from ~145ms.**

**Made the permanent default**, deliberately, with the trade-off stated plainly: a
software decoder costs meaningfully more CPU/battery/heat than the hardware ASIC
that's specifically built for this job, and that cost hasn't been measured over a
real long session (this test ran a few minutes). If sustained use turns out to be a
real battery/thermal problem, revert to `MediaCodec.createDecoderByType(...)` (picks
the hardware decoder by default) — a one-line change, not a design commitment.

### Pushed further, targeting a 2x cut on top of the software-decoder win — mostly negative results

User asked to search deeper, aiming to halve the ~110-125ms further (~55-65ms).
Researched SuperDisplay's own technical approach for any clues — no luck, their
site discloses zero technical detail, and both the XDA and BlurBusters forum
threads discussing it blocked automated fetching (403). Confirmed via literature
that MJPEG (an alternative codec) is a known dead end for this use case.

**Confirmed the software decoder's `FEATURE_LowLatency` is also `false`** (Google's
own decoder doesn't claim the feature either) — rules that lever out cleanly for
both decoder paths, not just the hardware one.

**Reasoned through, not live-tested: async `MediaCodec.Callback` mode.** The
software decoder's `pending=3-4` queue depth is *steady*, not growing, across
thousands of frames — meaning decode isn't falling behind (not compute-bound) at
the tested frame rate. That rules out async mode as a fix: it only changes how the
*client* is notified of ready buffers (callback vs. polling), not how many frames
the codec *itself* buffers internally, which is what's actually costing the time.
Not implemented, since the reasoning is conclusive without needing a live test.

**Tried PipeWire buffer-count negotiation on the capture side (~30ms segment,
previously untouched).** Requested the minimum buffer count (2) via a `ParamBuffers`
pod sent alongside the format pod at `stream.connect()` — no strongly-typed property
key exists for this in the installed `libspa` crate version, so built directly from
the raw `SPA_PARAM_BUFFERS_buffers` sys constant. Compiled clean, connected and
streamed without error (1977 frames, no negotiation failure). **Live-tested: no
change** — capture latency 29.52ms avg over 1643 samples, statistically identical to
the untouched baseline (~29-30ms). Reverted cleanly (true no-op diff) rather than
keeping unused complexity around — PipeWire's own "minimize buffering, dequeue/
requeue promptly" recommendation is already satisfied by the existing drop-stale-
frames logic, and the remaining capture latency looks like it's dominated by KWin's
own compositor scheduling cadence, not a buffer-count knob at all.

**Where this leaves the "halve it" goal:** six real, live/reasoned-through levers
tried since the software-decoder win, all negative (Exynos vendor key,
`KEY_PRIORITY`, `KEY_OPERATING_RATE` ruled out, async mode ruled out, `FEATURE_
LowLatency` confirmed false on both decoders, PipeWire buffer count). Getting from
~110-125ms to ~55-65ms would need real cuts across *multiple* pipeline stages
simultaneously, not one more single lever — and every stage tried so far
(hardware decoder floor, software decoder's own floor, capture buffer count) has
turned out to have a hard floor not reachable from this project's side. The
remaining honestly-untested territory is the transport hop itself (adb-forward vs.
raw USB/AOA, Milestone 8 below) and Android's own `SurfaceView`/`SurfaceFlinger`
presentation path — both real, but each a substantially bigger lift than anything
tried this session, not a quick lever.

### Real win found: unaccelerated CPU color conversion was costing more than the hardware encode itself

User asked directly: are we using hardware acceleration to its fullest? Answer: no
— `bgrx_to_nv12` (`color_convert.rs`) was a scalar, pure-CPU per-pixel BT.601 loop,
never touched since Milestone 2, explicitly flagged back then as "not optimized" and
never revisited. Added split timing (`dequeue→encoded` had always bundled color
convert and encode together) to find out how much it actually cost: **color-convert
averaged 10.36ms, more than the hardware VAAPI encode itself (4.24ms)** — the
supposedly-cheap CPU step was the bigger of the two.

**Rewrote color conversion to run on the GPU via VAAPI's own Video Post-Processing
(VPP) entrypoint** instead of a SIMD-accelerated CPU rewrite (the lower-risk, smaller
option) — user chose the bigger rewrite. `vaapi_encoder.rs` gained a second surface
(BGRX format, `VA_FOURCC_BGRX`/`VA_RT_FORMAT_RGB32`) and a VPP config/context
(`VAProfile_VAProfileNone` + `VAEntrypoint_VAEntrypointVideoProc`). Each frame: the
raw captured BGRX bytes are uploaded to the source surface via a straight memcpy (no
per-pixel math — that's VPP's job now), then a `VAProcPipelineParameterBuffer`
naming that surface is submitted to the VPP context, writing directly into the same
NV12 surface the H.264 encode context already uses. `color_convert.rs` deleted
entirely once confirmed working (not kept as a fallback — git history is the
fallback).

Needed `va/va_vpp.h` added to the bindgen wrapper header (the type/entrypoint/fourcc
*constants* were already generated without it, misleadingly suggesting VPP was
available, but the actual `VAProcPipelineParameterBuffer` struct definition lives in
that header specifically and wasn't generated until it was added).

**Live-tested, colors confirmed correct, full pipeline confirmed working end to end
(video + real S Pen input in GIMP) after the rewrite.** Result, clean run over 3443
frames: **upload+VPP+encode averaged 5.13ms, down from ~14.6ms (10.36 + 4.24) for
the old CPU-convert-then-encode path — a ~65% cut on this segment**, capture latency
unaffected as expected (~30ms, a separate upstream stage). Daemon-side pipeline
total now ~35ms (was ~45ms). Given decode/render still dominates the full pipeline
(~70-110ms depending on hardware vs. software decoder), this doesn't change the
*overall* glass-to-glass number by a huge margin, but it's a genuine, free win —
real hardware offload, no quality or battery trade-off unlike the software-decoder
switch, and it answers the user's question directly: no, hardware acceleration
wasn't being used to its fullest before this, and now it is for this stage.

### Asked for more research; found a bigger, compounding win nobody predicted

User asked for more improvements and to research other projects. Checked scrcpy
(its biggest documented latency fix, PR #646, was about `av_parser_parse2()`
lookahead ambiguity -- doesn't apply here, this project's protocol already gives
`MediaCodec` exact frame boundaries via length-prefixing, no parser involved), a
no-ADB WebSocket scrcpy fork (motivated by setup convenience, not latency -- WiFi
transport, likely worse than our USB adb-forward), and Parsec's engineering blog
(`<10ms` encode/decode -- real, but dedicated PC GPU ASICs, a different silicon
class from a mobile SoC decoder; not directly actionable, though their "zero-copy
GPU pipeline" principle is exactly what the VPP change above already did).

**One credible new lever surfaced: Android's `SurfaceFlinger` triple-buffering.**
Documentation confirms it trades latency for smoothness, and it sits *after* this
project's own latency measurement point (`releaseOutputBuffer()`) -- meaning the
measured ~85-115ms decode/render number could have been an understatement, with a
real, unmeasured gap hiding between "we handed the frame to the OS" and "pixel
actually on screen" (same class of blind spot the barcode probe already found and
fixed on the capture side, just never checked on the Android render side).

**Investigated by re-running the existing camera test (readable-clock method) now
that the GPU VPP change was in place, rather than building new PixelCopy
instrumentation** -- reused already-built infrastructure instead of adding more.
**Result: ~100-117ms (three fresh readings: 101ms, 117ms, 117ms, avg ~112ms) --
down from ~144-150ms before VPP.** That's a ~35-40ms improvement, far larger than
the ~9.5ms the VPP change's own direct instrumentation predicted for the
encode segment alone. Pulled fresh numbers for the other stages from the same live
session to find out why: capture latency unchanged (~30ms, confirmed separately,
ruling out a capture-side CPU-contention explanation), but **Android's own
decode/render latency dropped too -- avg 72ms (from ~85-115ms), decoder queue depth
`pending=3` (from 4-5)** -- despite the VPP change touching only the daemon side,
nothing on the Android decode path at all.

**The arithmetic now closes cleanly with no unexplained gap:** ~30ms capture +
~5.5ms encode + ~72ms decode/render ≈ 107.5ms, matching the camera-confirmed ~112ms
almost exactly. So the SurfaceFlinger hypothesis that motivated this investigation
turned out not to be needed -- there's no large hidden post-`releaseOutputBuffer`
gap. What actually happened: removing ~9ms of CPU-bound, occasionally-bursty work
from the daemon's single-threaded main loop made frame delivery to the tablet more
consistent, and that consistency let the Android decoder's own effective pipeline
depth shrink too (fewer frames in flight, less latency) -- a real, compounding,
emergent win from reducing jitter, not just the isolated segment time saved.
Recorded with appropriate honesty: the *mechanism* (jitter reduction improving
downstream buffering behavior) is a coherent, plausible explanation consistent with
every number gathered, not something independently proven via deeper profiling.

**Final numbers:** daemon-side ~35ms (was ~45ms pre-VPP), full pipeline ~107-112ms
(was ~144-150ms pre-VPP, ~300ms at the Milestone 4 baseline) -- roughly a **2.7x
improvement over where Milestone 7 started**, confirmed by camera three times.

## 8. AOA transport -- DONE (~107-112ms adb-forward -> min ~64-66ms steady-state)

User asked whether a custom USB protocol + custom decoding via adb would improve
latency further after Milestone 7 landed at ~107-112ms. Rather than a full custom
protocol, chose **Android Open Accessory (AOA) 2.0** first: talk to the tablet
directly over raw USB bulk transfers via `rusb`/libusb, bypassing adb entirely (no
adb-forward relay hop through the host's adb server and the device's adbd). Same
downstream protocol (handshake, clock-sync, length-prefixed H.264 frames, input
events) carried over a different transport -- `daemon/src/aoa.rs` implements
`Read`/`Write` for the USB bulk endpoints so the rest of the daemon (`portal_capture.rs`,
`input_receiver.rs`) is transport-agnostic via `Box<dyn Read + Send>` / `Box<dyn Write
+ Send>` trait objects, shared with the existing TCP/adb-forward path.

**AOA handshake implemented generically, not hardcoded to this tablet:** scan every
USB device for one that answers the standard `ACCESSORY_GET_PROTOCOL` vendor
request, send the six identification strings (`Quill`/`Quill Virtual Display`/...),
send `ACCESSORY_START`, then find and open the device again after it disconnects and
re-enumerates under Google's AOA VID/PID (`18d1:2d00`/`2d01`) -- matches the
project's no-hardcoding rule, any AOA-capable Android device should work here
unmodified. Android side: `accessory_filter.xml` manifest resource matches
manufacturer/model so Android routes `USB_ACCESSORY_ATTACHED` to the app, and
`UsbManager.openAccessory()` hands back a `ParcelFileDescriptor` wrapping the same
USB interface.

**Four real bugs found and fixed getting the two sides talking reliably:**

1. **Undersized-read `Overflow` on the daemon side.** `libusb`'s `read_bulk()` fails
   outright (doesn't truncate) when the caller's buffer is smaller than the incoming
   packet -- Android batches its whole 32-byte handshake into one `flush()`, but
   `input_receiver.rs`'s helpers read 4-8 bytes at a time. Fixed by wrapping the AOA
   reader in `std::io::BufReader::new(r)` before boxing it, giving `read_bulk()` an
   8KB internal buffer to absorb any single incoming packet.

2. **Connection race corrupting data.** The original clock-sync timeout (5s) was
   treated as non-fatal -- on a real timeout the daemon started streaming video with
   no clock-sync reply ever sent, and Android's first read landed mid-frame instead
   of on the handshake reply, producing garbage on both sides. Fixed by making
   `clock_sync_timeout` transport-dependent (120s for AOA, accounting for a human
   needing to react to the accessory permission dialog vs 5s for TCP) and by making
   `setup_transport()` return `None` on *any* clock-sync failure, so the daemon never
   streams without a confirmed peer.

3. **Device-reuse handshake failure.** `connect()` always attempted the full AOA
   switch handshake even when the device was already in accessory mode from a
   previous daemon run -- a device already switched won't answer
   `ACCESSORY_GET_PROTOCOL` the same way. Fixed by trying to open an
   already-enumerated `18d1:2d00`/`2d01` device first, before attempting any switch.

4. **`FileInputStream.available()` throwing on the USB accessory fd.** Confirmed via
   full stack trace that `BufferedInputStream.read()` calls `available()` as an
   optimization, and that throws `IOException("Invalid argument")` on this fd type
   (the `FIONREAD` ioctl isn't supported on it) -- matches Android's own official USB
   accessory sample code, which reads these fds unbuffered for the same reason.

**A fifth bug survived all of the above and took the longest to run down: garbage
clock-sync values, byte-for-byte reproducible across independent sessions.** After
every fix above, the handshake read correctly every time
(`2560x1498px, pressure 0..4095, tilt -90..90`), but the very next read --
`readClockOffset()`'s three `DataInputStream.readLong()` calls, 24 bytes total --
consistently came back wrong starting at the *second* field:
`offset=3788735529345...ms, round-trip sum=-7577471058690...ms`, then
`bogus frame length 1053598660`. Identical magnitude, near-identical trailing digits,
same bogus frame length, every time -- not random corruption, something deterministic.

First hypothesis: a `FileInputStream.read(buf, off, len)` offset-handling bug on this
device's USB accessory char device, corrupting `readFully`'s internal short-read
loop past the first chunk. Rewrote the reads as an offset-0-only helper
(`readExactBytes`, always reading into a fresh buffer at index 0 and copying into
place manually) -- **rebuilt, retested, got the exact same garbage value
(`1053598660`) again.** Ruled the offset theory out cleanly: whatever was wrong,
buffer offset handling wasn't it.

The real cause, worked out from the arithmetic: the first 8-byte field read
correctly every single time; only the fields after it were wrong. USB bulk transfers
are packet-oriented, not stream-oriented like a TCP socket -- confirmed this
matters going the *other* direction back in bug #1 above (undersized daemon-side
reads hard-fail with `Overflow`), but on the **Android** side the same undersized
read instead *silently truncates*: requesting only 8 bytes out of a single 24-byte
incoming packet returns those 8 bytes and **discards the rest of the packet**, rather
than queuing it for the next `read()` call the way a stream socket would. Every
`readLong()` call after the first was therefore reading from whatever arrived
*next* (the leading bytes of the first video frame), not the rest of the reply --
which explains both why it was deterministic (early frame sizes are similar between
runs) and why plain unbuffered reads (fix #4 above) weren't enough on their own: not
being unbuffered was the problem, being *undersized* was.

Fixed with a small hand-rolled `BufferedAccessoryInput` class: refills from a 1MB
internal buffer via full-size reads from the underlying `FileInputStream` (large
enough to swallow a whole incoming packet, video frames included), and serves
smaller reads out of that buffer -- effectively `BufferedInputStream` without the
`available()` call that made it unusable here. Applied uniformly to both the
adb-forward and AOA paths so `runDecodeLoop` stays transport-agnostic. Confirmed
live: clock-sync reply now reads correctly every time
(`offset=1697ms, round-trip sum=1ms` on a clean run with the daemon already waiting
before the app opened the accessory), and video renders live and continuously on the
tablet.

**Result:** with real on-screen motion to capture (idle/static content produces no
new PipeWire buffers at all -- expected portal behavior, not a bug), steady-state
decode/render latency (same FIFO-based measurement as Milestone 7) came in at
**min 64-66ms**, per-frame samples mostly 70-90ms, `pending=3` (same decoder queue
depth as the adb-forward baseline) -- down from **~107-112ms** over adb-forward.
That's a genuine **~40-45ms win**, consistent with the original hypothesis that the
adb-forward relay hop (host adb server <-> device adbd <-> app socket) was adding
real overhead beyond the H.264 encode/decode/transport work itself. Not yet
formally re-measured with the camera readable-clock method for a full
glass-to-glass number (this was decode/render latency from the Android-side FIFO
instrumentation, same segment Milestone 7 tracked) -- worth doing before calling the
AOA milestone fully closed out, but the transport itself, the handshake, and the
input path (S Pen events observed flowing back to the daemon over the same
connection during this test) are all confirmed working end to end.

### Auto-launch: plug in the tablet, nothing else to do

User asked whether AOA made it possible for the app to trigger the daemon on
connect. Not directly -- AOA requires the *host* to initiate the accessory-mode
switch (it sends `ACCESSORY_START`; the device can't put itself into accessory
mode), so nothing on the USB wire can reach the daemon before the daemon already
exists and is scanning for it. The actual fix is automating the host side instead,
which had two real blockers:

1. **The portal ScreenCast dialog.** Every daemon launch popped an interactive
   "pick your monitor" dialog -- fine for manual testing, a hard blocker for
   anything unattended. `ashpd` (the portal crate already in use) supports
   `PersistMode::ExplicitlyRevoked` plus a restore token: `open_portal()`
   (`portal_capture.rs`) now saves the token xdg-desktop-portal returns after the
   first successful pick to `~/.config/quill/portal_restore_token`, and passes it
   back in on every subsequent call. Confirmed live: first run still shows the
   picker and writes the token file; every run after that goes straight to
   `[portal] got stream: ...` with no dialog at all. Falls back to a fresh
   interactive pick if the saved token is ever rejected (revoked, virtual monitor
   recreated, etc.) rather than failing outright.

2. **Nothing was launching the daemon.** Added a udev rule
   (`daemon/packaging/99-quill-daemon.rules`) matching Samsung's USB vendor ID
   (`04e8`, generic across Samsung Android devices in normal MTP mode -- not
   hardcoded to this one tablet, matches the project's no-hardcoding rule) that
   tags `SYSTEMD_USER_WANTS+="quill-daemon.service"` -- the standard mechanism for
   reaching a logged-in user's systemd session from udev's root context, rather
   than a fragile `su`/`DISPLAY`-guessing script. The user unit
   (`daemon/packaging/quill-daemon.service`) runs the daemon in AOA mode with
   `Restart=on-failure`, capped via `StartLimitBurst` so a persistent failure
   doesn't spin-restart forever.

   First cut hardcoded the repo checkout path (`%h/Projects/Quill/daemon/target/...`)
   directly in the unit's `ExecStart` -- caught before merging: that only works on
   this machine, this exact clone location, not portable to anyone else picking up
   the project. Fixed with `daemon/packaging/install.sh`, which symlinks the built
   release binary to the checkout-independent `~/.local/bin/quill-daemon` and
   points the unit there instead -- works regardless of where the repo lives.

**Confirmed live end to end:** tablet unplugged and replugged, with nothing
manually started beforehand -- portal negotiation skipped the dialog via the saved
restore token, the daemon auto-launched via the udev rule + systemd user unit, AOA
handshake completed, and video appeared on the tablet automatically. Full plug-in
to on-screen-video chain now requires zero manual steps on the host side.

### App polish: edge-to-edge immersive fullscreen

User asked for two things: fullscreen on the tablet, and the host's taskbar
showing up in the captured video. The second turned out to be a KDE Plasma
desktop-config issue, not an app bug -- `krfb-virtualmonitor`'s virtual output
starts blank with no panel assigned to it by default (per-screen panel assignment
is a Plasma setting, not something the capture pipeline controls) -- left for the
user to fix via Plasma's panel edit mode rather than scripted, since it touches
the live desktop shell's config and a `plasmashell` reload.

Fullscreen: the app's theme was already `Theme.Black.NoTitleBar.Fullscreen`, but
that legacy theme flag only ever hid the status bar, not Android's navigation bar
-- and mixes poorly with the modern edge-to-edge APIs. Switched the manifest theme
to plain `Theme.Black.NoTitleBar` and did the rest properly in code:
`WindowCompat.setDecorFitsSystemWindows(window, false)` plus
`WindowInsetsControllerCompat.hide(WindowInsetsCompat.Type.systemBars())` with
`BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE` (both bars hidden, swipe from an edge
reveals them temporarily -- standard "immersive sticky" pattern). Re-asserted on
every `onWindowFocusChanged(true)`, since the transient reveal (or a permission
dialog stealing focus) doesn't hide itself again on its own. Confirmed live: both
system bars gone, video fills the screen edge to edge.

### No-hardcode follow-up: video resolution, and an AOA idle-timeout bug it surfaced

The Android app hardcoded `width = 1920, height = 1080` to build its decoder's
`MediaFormat` -- happened to match this project's one test setup (the daemon's
virtual monitor), but broke the project's own no-hardcoding rule and would silently
mismatch if that monitor's size ever changed. Fixed by having the daemon send an
8-byte `(width, height)` header once, right after the clock-sync reply and before
the first video frame -- written from `portal_capture.rs`'s PipeWire `param_changed`
callback, the first point the daemon actually knows the negotiated size. Android
reads it (`readVideoFormat`) before configuring the codec instead of assuming a
constant. Confirmed live: `video format: 1920x1080` read correctly off the wire,
decoder configured from it, video unaffected.

Testing this surfaced a real, unrelated bug: **S Pen and finger input stopped
working after ~90 seconds of nobody touching the screen.** Root cause: AOA's
`AoaReader::read()` uses a fixed `READ_TIMEOUT` (90s, originally sized to tolerate a
human clicking through the USB accessory permission dialog on the very first read),
but `input_receiver.rs`'s steady-state loop reuses the same reader for every
subsequent event and treats *any* read error -- including an ordinary "nothing
arrived within the timeout" -- as fatal, permanently killing the input thread with
no retry. A real TCP socket blocks indefinitely instead of erroring in the same
idle situation, which is what `input_receiver.rs` was actually written assuming.
Confirmed via `journalctl`: `[input] receiver thread exiting after 0 events`
exactly 90s after the handshake, with no touches in that window -- an entirely
normal idle stretch (just watching video, not touching yet) silently and
permanently broke input for the rest of the session. Fixed by looping on
`rusb::Error::Timeout` inside `AoaReader::read()` instead of surfacing it -- genuine
failures (device unplugged, etc.) come back as a different error variant and still
propagate normally. Confirmed live: input survives idle periods now, both pen and
finger events flowing correctly (`[input] event 600: type=1 ...`) well past the
90s mark.

### Auto-launch follow-up: surviving a USB drop/reconnect

Found live: a tablet USB cable drop (loose connection, screen off, whatever) left
the auto-launched daemon "running" but functionally dead -- the AOA handshake read
failed, `setup_transport` logged the failure and returned `None` for the transport,
and the daemon just carried on capturing forever with no client and no way to
reconnect. Since the process never exited, `systemd`'s `Restart=on-failure` (see
`packaging/quill-daemon.service`) never got a chance to fire, and the udev rule's
`SYSTEMD_USER_WANTS` on the tablet reappearing was a no-op against an
already-"active" unit. Only a manual `systemctl --user restart` recovered it.

First fix -- `setup_transport`'s two post-connection failure paths (clock-sync
reply write failure, and the clock-sync ping never arriving) now call
`std::process::exit(1)` instead of returning `None`, once past the point where
`TransportConfig::None` (the legitimate no-client debug mode) was ruled out. Rebuilt
and tested the exact live scenario: daemon waiting for the handshake, unplugged and
replugged the cable mid-wait. Confirmed the process now exits cleanly on the drop
(`AOA bulk read: No such device`) -- but the very next systemd-triggered restart
immediately failed too (`no AOA-capable USB device found`), because
`aoa::connect()`'s initial device scan was a single instant attempt with no retry,
and the tablet hadn't finished re-enumerating as MTP yet by the time the restart
fired barely a second later. Three such retries in quick succession burned through
`StartLimitBurst`, and the unit ended up permanently `failed` -- worse than the
original bug, needing a manual `systemctl --user reset-failed` to recover.

Second fix -- wrapped `connect()`'s initial device scan in a 20-second bounded
retry (500ms poll interval) instead of scanning once and failing immediately,
absorbing the few seconds a real replug takes to re-enumerate internally rather
than relying on systemd's restart cadence to paper over it. Re-tested the identical
drop/replug live: daemon exited on the drop, systemd restarted it once
(`RestartSec=2`), the retrying scan found the tablet a few seconds later, and the
full handshake/stream came back up on its own -- no crash loop, no manual
intervention, unit never entered `failed` state.

Third fix, found immediately after: the first two fixes only covered failures
*before* streaming starts. Re-testing by force-stopping and relaunching the Android
app mid-session (daemon already streaming) hit the exact same zombie state again --
the frame-write and video-format-header write failure paths still just set
`transport = None` and kept the process running forever, unable to reconnect (the
AOA USB interface claim and the input thread's read half are both already gone by
that point; a relaunched app has nothing left to talk to). Same fix, same
reasoning: both now `std::process::exit(1)` instead of degrading silently.
Confirmed live: force-stopped and reopened the app against an already-streaming
daemon, the write failure now exits the process, systemd restarts it, and the app
reconnects on its own -- no manual `systemctl restart` needed at any point in the
whole plug-in-to-video chain anymore, mid-session drops included.

## 9. (Not started) Multi-touch gestures

Pinch-to-zoom, two-finger scroll, and similar — deferred from the Milestone 6b finger
click/drag follow-up (see there for why: needs a real multitouch device model, not a
small extension of the current single-pointer protocol).

## 10. (Not started) GNOME support

Requested by the user after Milestone 8's no-sudo input fallback (`remote_desktop_input.rs`,
see the follow-up work under Milestone 8 above) landed. Two genuinely separate problems,
not one:

- **Input (already portable):** the `RemoteDesktop` portal fallback added for no-sudo
  machines uses only the standard `org.freedesktop.portal.RemoteDesktop`/`ScreenCast`
  interfaces, nothing KWin-specific, so it should work unmodified against
  `xdg-desktop-portal-gnome` (mutter's own backend, a separate implementation from KDE's).
  Confirmed hitting a real KDE-only bug testing it live on this machine (Plasma 6.3.6):
  `xdg-desktop-portal-kde`'s "MegaAuth" permission lookup fails for unsandboxed apps
  requesting `RemoteDesktop` specifically (`ScreenCast` already has this allowance,
  `RemoteDesktop` doesn't yet) -- see
  [MR !144](https://invent.kde.org/plasma/xdg-desktop-portal-kde/-/merge_requests/144),
  "Allow RemoteDesktop requests if they come from a non-sandboxed app", not yet in our
  installed version. GNOME's backend is a different codebase, unlikely to share this
  exact bug, but unconfirmed -- needs live testing on an actual GNOME machine.
- **Display (the real blocker, not yet solved):** `krfb-virtualmonitor`, which creates
  the virtual monitor in the first place, is KDE-only (part of `krfb`). GNOME/mutter has
  no equivalent simple tool -- this is the same gap already flagged in Milestone 1's
  evdi-to-portal pivot ("KWin has a native, compositor-level virtual-output mechanism...
  no stable cross-desktop equivalent exists"). Making the *display* side work on GNOME
  needs its own research pass (does mutter expose any virtual-output mechanism at all,
  stable or not; would a different approach be needed entirely) before any code gets
  written -- not started.

## 11. (Partially fixed) Reconnect robustness

User reported two related bugs live: reopening the app while the daemon was already
connected didn't reconnect, and restarting the daemon while the app was open broke it
the same way -- both needed a full manual app relaunch (and often a cable replug) to
recover, no matter which side came back first.

**Root-caused three distinct, stacked bugs, fixed the first two cleanly:**

1. **The app never retried at all.** `runAoaDecodeLoop`/`runAdbForwardDecodeLoop` ran
   once per `surfaceCreated`; any disconnect just ended the decode thread silently.
   Fixed: `surfaceCreated` now loops indefinitely, re-checking
   `alreadyAttachedAccessory()` fresh each attempt, with a 1s retry delay and a
   try/catch around the whole attempt (an uncaught exception outside `runDecodeLoop`'s
   own try/catch -- confirmed live, `openAccessory()` itself can throw -- was silently
   killing the retry loop for good, worse than the bug it was fixing).
2. **No visible feedback.** Confirmed live: even with the retry loop working, a stuck
   connection just showed a black or frozen screen with no indication anything was
   wrong or what to do about it. Added a status overlay ("Waiting for connection...",
   escalating to "...unplug and replug the USB cable" after the first failed attempt),
   hidden only once the first real frame actually renders -- the user's own suggested
   fix, simpler and more honest than chasing a fully invisible auto-reconnect.
3. **A plain blocking read has no timeout.** Confirmed live: when the daemon dies
   without the USB transport itself signaling an error (it doesn't, reliably), Android's
   `InputStream.read()` just hangs forever -- frozen on the last frame, no exception, so
   the retry loop above never even got a chance to fire. Fixed with a daemon-side
   heartbeat (a zero-length "frame" sent every ~800ms whenever no real video frame went
   out, so a legitimately idle screen -- no motion, no new PipeWire buffers at all,
   expected -- doesn't get mistaken for a dead connection) and an Android-side watchdog
   thread that force-closes the stream after 3s of total silence, unblocking the read
   with an `IOException` the existing retry machinery already handles.
4. **Two decode threads racing on the same connection.** Confirmed live: `am start` on
   an already-running single-instance activity doesn't spawn a fresh process, it just
   resumes the existing one -- re-firing `surfaceCreated()` without the old (now
   long-lived, retrying) decode thread ever having been told to stop. Fixed:
   `surfaceCreated` now interrupts and joins any previous `decodeThread` (2s bound)
   before starting a new one.
5. **Daemon write-timeout tightened 5s -> 2s** (`aoa.rs`) and **`clear_halt()` added on
   both bulk endpoints right after claiming the interface** -- confirmed live, stale
   queued USB data from a previous session was intermittently corrupting a fresh
   session's very first handshake read (garbage clock-offset, garbage capability
   handshake -- wrong pressure/tilt/coordinate ranges, `uinput` tablet creation failing
   outright on some reconnects with "Invalid argument"). This measurably shrank the
   race window but did **not** fully close it.

**Known remaining gap, not resolved this session:** stress-testing with rapid repeated
daemon restarts (an artificially aggressive test, not representative of real usage --
a user doesn't restart the daemon every few seconds) still reproduced the handshake
corruption occasionally even with `clear_halt()` in place (one run logged a garbage
event type, `type=51`, outside the valid 0-7 range). Video usually keeps working
through this (its own framing wasn't corrupted that time); input can end up silently
wrong for that session (nonsense coordinates/pressure, or `uinput` creation failing
outright) until the next reconnect cycles it out on its own. This is a genuine
USB-level race -- stale queued bulk-transfer data surviving a reconnect somewhere
`clear_halt()` doesn't reach -- not a protocol logic bug; closing it further likely
needs either a full USB device handle close+reopen (not just re-claim) on every
reconnect, or a resync marker/checksum in the handshake itself so corruption can be
*detected* and the attempt retried rather than silently accepted. Not attempted this
session -- a real scoping decision given the size of the fix already shipped, not an
oversight.

## 12. Investigated: double-cursor "ghosting" on stop-motion -- likely a hardware
## panel characteristic, not a Quill bug

User reported: after a drag/motion stops, the tablet briefly (and sometimes
persistently, in a still screenshot) shows two crisp, non-blurred cursor icons
side by side, offset by a small amount -- not a blur/smear, two distinct copies of
the same icon. Confirmed live via real tablet screenshots (not a hypothetical).

**Methodically eliminated every stage of the pipeline Quill actually controls,**
each with direct evidence, not inference:

- **Raw captured frame (KWin's own compositor output, before any encoding):**
  dumped directly via a temporary continuous-frame-dump diagnostic
  (`portal_capture.rs`'s existing one-shot `QUILL_DUMP_FRAME` debug path, briefly
  modified to dump every frame instead of just the first -- reverted after use, not
  kept). Clean, single cursor, for a real reproducing motion+stop test.
- **Our own encoded H.264 bitstream** (VAAPI + VPP color conversion, the daemon's own
  work): extracted and decoded every frame from the daemon's own `.h264` output file
  via `ffmpeg` for a real reproducing test, inspected the frames right at the stop
  transition. Clean.
- **Android's own system pointer icon:** hypothesized Android might be drawing its
  own hover-pointer overlay on top of the video (a real, separate mechanism from
  video content) -- suppressed it (`surfaceView.pointerIcon = PointerIcon.TYPE_NULL`,
  kept, harmless either way). **No change** -- also, the user confirmed the artifact
  reproduces with plain desktop mouse movement, not just S Pen hover, ruling this
  mechanism out on its own logic before the live test even confirmed it.
- **Decode/render frame pacing:** the per-frame output-draining loop
  (`runDecodeLoop` in `MainActivity.kt`) used to call `releaseOutputBuffer(index,
  render=true)` on every ready output buffer with zero pacing -- multiple real
  frames landing in a burst (exactly what happens right as motion stops) would all
  render back to back. Fixed to only render the newest buffer in a burst, discarding
  (not rendering) the rest -- same principle as the daemon's own capture-side
  drop-stale-frames logic. **Kept** (a real correctness improvement regardless), but
  **no change** to the artifact.
- **Decoder implementation:** swapped from the deliberately-chosen software decoder
  (`c2.android.avc.decoder`, Milestone 7's latency win) to the hardware one
  (`c2.exynos.h264.decoder`) as a diagnostic, reverted after. **No change** -- rules
  out a decoder-implementation-specific bug.
- **Screenshot-capture-timing artifact** (i.e. not a real bug, just an unlucky still
  captured mid-transition): ruled out -- user confirmed the artifact is visible live,
  watching the tablet with their own eyes, not only in screenshots taken to document
  it.

**Conclusion:** every stage of the pipeline this project controls -- compositor
capture, VAAPI encode, transport framing (indirectly, via the clean decoded
bitstream), MediaCodec decode (both implementations), and render-buffer pacing --
was individually verified clean or shown to make no difference. The artifact is
live-visible, decoder-independent, and downstream of everything tested. Leading
theory: LCD panel response-time/ghosting -- a physical display characteristic (this
tablet's panel EDID reports a 2020 manufacture date, plausibly not tuned for fast
motion clarity), consistent with every observation including "worse at higher
frame rates" (faster real content changes stress panel response time harder) and
"still present, just smaller, when the pipeline was artificially throttled to
~1fps" (still some transition, just less frequent). Not something Quill's software
can fix if correct -- analogous to noticing a monitor's own motion blur. The one
untested remaining variable is bit-level AOA transport corruption, considered
unlikely given the artifact's clean, correctly-shaped appearance (typical transport
corruption shows as block/macroblock artifacts, not two intact icons) -- not
pursued further this session.

## 13. Reverted attempt: automatic virtual-monitor sizing regressed FPS

User asked why the tablet's display always showed as 1920x1080 in system
settings, when it should be bigger. Tried teaching the daemon to
create/resize the `krfb-virtualmonitor` output itself from the capability
handshake's real panel resolution, before ever opening the portal, instead
of relying on the hand-picked `--resolution` the project's test setup
happened to start it with. Landed as a commit, then live-tested and found it
dropped the display from a smooth ~60fps to a laggy sub-10fps -- most likely
because forcibly tearing down and recreating the KWin output on every daemon
launch knocked the compositor out of whatever stable state it had settled
into, onto a slower path. It also didn't end up fixing the original
1920x1080-stuck complaint it was written for. Net negative, so reverted
outright (`git reset` back to the prior commit) rather than kept around and
patched -- the code is gone, not just disabled.

Confirmed via `git bisect` against real hardware (FPS is only observable by
eye on the tablet, not from daemon logs -- encode-side timings looked fine,
~6-10ms/frame, even while the actual display was laggy, so the regression is
downstream of the daemon's own encode pipeline, somewhere in the
KWin/portal/compositor path the virtual-monitor recreation touches). Live
re-test after the revert confirmed the fix: back to a smooth 60fps.

The tablet also shows as mirroring the primary display rather than
extending it -- initially suspected as a second regression from this same
commit, but the post-revert re-test still shows mirroring, so it's a
separate, pre-existing issue unrelated to virtual-monitor sizing. Not
investigated further this session.

**Worth keeping despite the revert: a real, still-unfixed bug was found
during that work.** Manually running `quill` while the systemd auto-launch
instance already owned the tablet raced `aoa::connect`'s "already in
accessory mode, reusing it" fast path against a connection with leftover
bytes still in flight -- the fresh handshake read landed on stale stream
bytes instead, producing `handshake: 67108868x369098755 px, pressure
-905969655..-2063597568, tilt 0..132096 deg`. uinput tablet creation
rejected it safely (kernel ioctl validation, `Invalid argument`), but the
reverted code had wired those same unvalidated dimensions into a
`krfb-virtualmonitor --resolution ...` shell-out, and KWin accepted the
literal garbage resolution with no bounds checking of its own --
`kscreen-doctor -j` confirmed a real output at that insane size, and the
terminal crashed (KWin itself survived; recovered with `pkill -f
krfb-virtualmonitor`). The corrupted-handshake bug itself is independent of
the reverted feature and still lives in `aoa::connect`'s connection-reuse
path today, just with nothing downstream currently acting on the garbage
values it can produce -- worth a bounds/sanity check on the handshake
read in `input_receiver.rs` regardless of whatever uses width/height next.

## 14. Real FPS root cause: hardcoded software decoder starving at native resolution

Milestone 13's revert fixed the daemon side, but re-testing against the real
`Virtual-QuillDisplay` output (not the `eDP-1` mirroring fallback) was still
laggy -- same reverted commit, same binary, only the capture source changed,
which ruled out both the daemon's encode pipeline (measured clean, ~6-10ms/frame,
`0 stale dropped`) and AOA/USB bandwidth (frame size ~28KB regardless of
source, ~14Mbps effective either way, nowhere near USB 2.0's ceiling).

Root cause was on the Android side: `MainActivity.kt` hardcoded
`MediaCodec.createByCodecName("c2.android.avc.decoder")` -- Android's
*software* AVC decoder -- rather than the device's hardware one
(`c2.exynos.h264.decoder`, confirmed present via the app's own decoder
enumeration, `hw=true`). That was a deliberate choice from Milestone 7
(commit `e96ab61`), which measured software decode as a real ~20-30ms/frame
latency win over hardware at the 1920x1080-or-smaller resolutions tested
then. That measurement doesn't hold at the tablet's real native resolution
(`2560x1600`, only reachable now that dynamic resolution + real-panel-size
capture exist) -- confirmed live via `adb logcat`: `dequeueInputBuffer`
returning -1 constantly, and comparing `queued`/`rendered` counts between
the periodic frame-stat log lines showed ~80% of incoming frames silently
dropped (e.g. 30 real frames received over the wire, only 6 successfully
queued to the decoder) as the software decoder fell behind and its input
buffer pool starved. Per-frame latency numbers were nonsensical
(hour-scale negative/overflow values) as a direct symptom of this.

Fixed by picking a hardware AVC decoder from the app's existing enumeration
loop (`isHardwareAccelerated`) instead of hardcoding the software name,
falling back to software only if no hardware decoder exists on the device
(keeps the project's no-hardcoding-to-one-vendor rule). This also
automatically enabled the existing Exynos vendor low-latency parameter
(`vendor.rtc-ext-dec-low-latency.enable`), gated on the decoder name and
already written but dead code while software was hardcoded. Live-confirmed
after the fix: zero `dequeueInputBuffer` failures, `queued`/`rendered` track
together (the small gap is the intentional render-dedup pacing from
Milestone 12, not drops), stable ~71-77ms per-frame latency, smooth on the
tablet by eye. Milestone 7's original hardware-vs-software measurement was
correct for the resolution it was measured at; it just stopped applying once
the pipeline started running at 3x the pixel count.

## 15-16. Portrait support: launch-time orientation, two failed attempts, working fix

User asked whether the tablet could be used in portrait instead of landscape.
Design settled on launch-time-only (not live mid-session rotation, matching
Milestone 13's lesson that live-reconfiguring an already-open portal/
PipeWire session is the risky part): the Android app locks
`requestedOrientation` once in `onCreate` to whatever the tablet's physical
orientation already is at that moment (manifest changed from hardcoded
`landscape` to `unspecified` to allow this), and the daemon reads the
resulting handshake dimensions to configure everything else before ever
opening the portal.

**Attempt 1 (Milestone 15), reverted: live `kscreen-doctor` rotation of an
already-created output.** Reordered `main.rs` the same way Milestone 13 did
(transport connect + handshake wait before the portal call, this time
justified since Milestone 14 already proved that reordering was never the
actual cause of the earlier FPS regression), then rotated the existing
landscape `krfb-virtualmonitor` output via `kscreen-doctor
output.NAME.rotation.left/none` to match. Looked correct at the KScreen
metadata level (`kscreen-doctor -j` reported the swapped logical size
correctly) but broke two different things live: the captured video was
black except for a cursor trail that never cleared, and tablet touch input
didn't track the rotated geometry either. Root cause never fully isolated
at the pixel level, but both symptoms are consistent with the screencast
capture and libinput's tablet-to-output mapping not actually following a
runtime rotation transform on this headless output type, even though KWin's
own bookkeeping says it rotated.

**Attempt 2, working for video: recreate the output at the native
resolution instead of rotating it.** Landscape had worked flawlessly all
session because it's always been *created* at its exact real resolution
from the start (Milestone 10's reverted-then-partially-revived idea, see
below) -- so portrait got the same treatment: `orientation::ensure()`
(`daemon/src/orientation.rs`, reusing most of Milestone 13's reverted
create/resize logic, safe to bring back now that Milestone 14 cleared it of
the FPS regression) tears down and recreates `krfb-virtualmonitor` at the
handshake's exact width x height whenever they don't already match, no-op
otherwise (same restore-token-preservation reasoning as Milestone 13). This
alone fixed the black-screen/cursor-trail/touch-mapping bugs -- confirmed
live, clean single connection, zero decoder starvation, correct touch
tracking.

**Recreate exposed a second, unrelated timing bug: the Android watchdog
raced the daemon's own setup time.** `krfb-virtualmonitor` teardown+respawn
+ portal renegotiation takes a real ~3-4s, during which no heartbeat exists
yet (heartbeats only start once PipeWire is actually streaming). The
Android app's inactivity watchdog (`MainActivity.kt`, `WATCHDOG_TIMEOUT_MS`)
was sized for the old always-fast landscape path and only tolerated 3000ms
-- it fired mid-setup, forced a reconnect while the daemon's original
connection was still legitimately mid-negotiation, and that collision
produced garbage handshake/video-format reads: the exact same connection-
reuse framing desync already flagged as unfixed in Milestone 13/14, just
newly reachable because recreate legitimately takes longer than 3 seconds.
Fixed by bumping `WATCHDOG_TIMEOUT_MS` to 15000ms, comfortably past
recreate's worst case.

**180-degree flip for this machine's cable position: KWin rotation is a
dead end for this output type, full stop.** Live-tested directly (not just
via this daemon's own automation, but manually through System Settings'
Display and Monitor panel too): setting `Virtual-QuillDisplay`'s rotation
to `inverted` or `none` changed `kscreen-doctor -j`'s reported metadata but
had *zero* effect on what actually got captured -- confirmed by testing
both settings back to back with no visible difference either time. Real
monitors get the rotation transform applied at scanout; this headless
output's screencast producer apparently reads a pre-transform buffer
regardless. Fixed at the two places that actually touch pixels instead:
`vaapi_encoder.rs` now takes a `flip_180: bool` and sets
`VAProcPipelineParameterBuffer.rotation_state = VA_ROTATION_180` in the
existing GPU color-conversion VPP pass (the struct already had the field --
`bindgen`'s generated bindings confirmed `VA_ROTATION_180` and
`rotation_state` exist, no new FFI surface needed), and
`input_receiver.rs` reflects touch/pen `x,y` (`width - x, height - y`)
before uinput injection whenever `height > width`, computed independently
of the encoder's own flip decision (same formula, same handshake dims,
different thread -- no cross-thread plumbing needed since it's a pure
function of data each side already has). Both driven by the same
`height > width` condition, kept in sync without a shared flag. Live-
confirmed working after this: correct orientation, correct touch tracking,
smooth.

## 17. GOP with P-frames (Milestone 7's deferred item 3), implemented

User asked whether LZ4 (seen referenced in a SuperDisplay-adjacent repo) could help
latency. Checked first: the wire payload is already H.264, so compressing an
already-high-entropy encoded bitstream buys ~nothing and just adds CPU -- ruled out
without writing code. That look pointed back at Milestone 7's three original tuning
items instead: item 3, real GOP with P-frames instead of all-intra, had sat
unimplemented since then (`vaapi_encoder.rs`'s top comment still said "tuning-pass
work (Milestone 7)"). Implemented it: a 2-surface ping-pong DPB, one IDR every
`GOP_SIZE` (60) frames, single-reference P slices (`max_num_ref_frames=1`,
`ip_period=1`, no B-frames) in between, referencing only the immediately preceding
frame. SPS/PPS packed headers now only get injected on IDR frames -- previously
resent every single frame, since every frame used to be an IDR.

**Bitstream correctness validated first, no live capture needed.** The daemon has
no lib target (only bin crates), so drove `VaapiEncoder` directly from a throwaway
`src/bin/gop_bitstream_test.rs` diagnostic (same `#[path = "../..."]` pattern as
`uinput_test.rs`) with synthetic moving-content BGRX frames -- avoided both the
interactive portal picker and the already-running production `quill-daemon` systemd
service entirely. **Result, 150 synthetic frames:** IDR avg 7168 bytes vs P avg 723
bytes (~10x smaller), GOP boundary landed exactly at frame 60/120 as designed,
**88.1% total bitstream reduction** vs a hypothetical all-intra encode of the same
content. `ffprobe`/`ffmpeg -f null -` confirmed clean decode, 150/150 frames, zero
errors or warnings -- the P-frames decode correctly against their reference, not
just VAAPI-accepted them. Deleted the diagnostic bin afterward (throwaway, not a
milestone artifact).

**Live test: the already-running production daemon picked up the new binary for
free.** `~/.local/bin/quill-daemon` symlinks straight to the release build; the
systemd service was already crash-looping on `[clock-sync] never received clock
ping` (waiting for the Android app to reopen the accessory), so the next normal
app-open picked up the rebuilt GOP binary with zero manual restart needed. Confirmed
live in the journal: `encoder ready: ... GOP 60`, frame sizes 380-810 bytes during
normal desktop use, well under the old ~7KB/frame all-intra floor.

**User's live by-eye impression: no difference, maybe slightly worse.** Expected
going in, not a red flag: Milestone 7 already concluded frame sizes were small
enough that GOP "mainly helps bandwidth, not latency," and this project's own
history (Milestone 7's `KEY_LOW_LATENCY`/vendor-parameter tests) shows unaided
by-eye impressions of this pipeline aren't reliable without instrumentation.

**Camera glass-to-glass re-measurement, same readable-clock method as Milestone 7**
(`experiments/capture-latency-probe/src/bin/readable_clock.rs`: one window on the
real screen, one moved onto `Virtual-QuillDisplay` via `xdotool windowmove` at
physical offset `1920,0` -- exact recipe already worked out in Milestone 7's
barcode-probe section, still correct). Three readings: .723/.589 (134ms),
.421/.287 (134ms), .488/.388 (100ms) -- **avg ~123ms**.

**Honesty check: not a clean isolate.** Milestone 7's ~144ms reference figure
(151/145/135ms) was measured over **adb-forward**; this run was over **AOA** (the
production daemon's configured transport, `aoa` arg). Milestone 8 already found AOA
saves ~40-45ms over adb-forward on the decode/render segment alone (FIFO log
instrumentation) and explicitly flagged that it was "not yet formally re-measured
with the camera readable-clock method" for a full glass-to-glass number -- so no
clean pre-GOP *AOA* camera baseline exists yet. Today's ~123ms is really the
**first AOA-era camera glass-to-glass measurement**, not a controlled GOP-only A/B:
it's confounded with the already-known, separately-attributed transport win. Can't
attribute the 144ms-to-123ms delta to GOP from this data alone -- three readings
per side is also too small a sample to lean on hard either way. A real A/B needs
the same AOA transport, same camera method, GOP code checked out vs the pre-GOP
commit (`856cffb`, current HEAD at the time this milestone's change was made) --
planned as the next test.

### The real A/B, same session -- GOP does not help glass-to-glass, and may cost a hair

Built the clean comparison right away rather than leaving it planned: `856cffb`
(pre-GOP) checked out into a throwaway `git worktree`, built release there too, both
binaries (`quill-daemon-pregop-856cffb`, `quill-daemon-gop`) copied out to
`~/quill-ab-test/` so the worktree itself could be torn down immediately. Stopped the
systemd-managed production daemon (`systemctl --user stop quill-daemon.service`,
frees the AOA USB handle -- only one process can hold it), ran each binary manually
against the real tablet with identical args (`aoa` transport, same two
`readable_clock` windows already positioned from the earlier test), filmed both back
to back in the same sitting -- same lighting, same camera, same person reading the
frames, same few minutes of wall-clock time, about as controlled as this method gets
without dedicated hardware.

**Reconnect friction, expected and already-documented, not a new bug.** Swapping
binaries hit the same stale-handshake race Milestone 13/14 already found and left
unfixed (`[input] handshake reports <garbage>x<garbage> px ... dropping this
connection`) three times in a row on the pregop->gop swap specifically. Force-closing
and reopening the Android app (not just relaunching the daemon) cleared it every
time -- consistent with the existing theory that leftover bytes from the *app's* side
of the connection, not the daemon's, are the source.

**Pre-GOP (`856cffb`), 4 readings:** .860/.726 (134ms), .877/.799 (78ms), .928/.810
(118ms), 8.045/7.928 (117ms) -- **avg ~113ms**.

**GOP, 4 readings, same method immediately after:** .364/.249 (115ms), .431/.300
(131ms), .464/.338 (126ms), .531/.400 (131ms) -- **avg ~126ms**.

**Result: GOP is not faster glass-to-glass here -- if anything, ~13ms slower.** This
is the clean isolate the previous entry's confounded ~123ms number couldn't provide:
same transport (AOA), same tablet, same camera method, same short window of time,
only the encoder binary differs. It matches the user's own live by-eye impression
from earlier in this milestone ("no difference, maybe slightly worse") rather than
the noisier confounded-transport number. 8 total readings (4 per side) is still a
small sample -- not proof of a real regression -- but it's now consistent evidence
across three independent signals (by-eye, confounded camera test, clean camera A/B)
pointing the same direction, not just one noisy number.

**Plausible mechanism, not confirmed:** every P-frame's `vaRenderPicture` submission
now carries live reference-picture-list bookkeeping (`ReferenceFrames[0]`,
`RefPicList0[0]`, `frame_num`/`poc_lsb` tracking) that the old all-intra path never
built, and the encoder now depends on the *other* ping-pong surface's reconstruction
being finished before referencing it -- both are real, if small, added per-frame
costs on the encode side that all-intra didn't pay. Consistent with, but not proof
of, the ~13ms gap. Not instrumented further this session.

**Where this leaves Milestone 17: bandwidth win confirmed (88.1% smaller
bitstream, real and validated), latency win not confirmed -- likely a small net
loss instead.** Matches Milestone 7's own original prediction almost exactly ("P-
frame GOP mainly helps bandwidth, not latency") -- the surprise here isn't that GOP
failed to help latency, it's that a controlled test was cheap enough to actually
check rather than assume. Keeping the GOP change: bandwidth matters independently
(lower USB traffic, more headroom before the pipeline itself becomes the
bottleneck), and a ~13ms cost from 4 noisy readings isn't a strong enough signal to
revert a validated bitstream win over.

## 18. Latency pass: the SPS was telling the decoder to buffer, and the capture
## path was making KWin read the GPU back to CPU every frame

User asked for every available latency lever, however expensive, checking other
open-source projects, targeting ~60ms glass-to-glass against the ~110-125ms
Milestone 17 left. Research covered project-monitorize, scrcpy, moonlight-android
/ moonlight-common-c, Sunshine, obs-studio's PipeWire capture, FFmpeg's VAAPI
hwcontext, and KWin 6.3's own screencast source.

**Two numbers in this file turned out to be stale, both found by just reading the
production journal instead of trusting the record.** `upload+VPP+encode` was
recorded as ~5.13ms; it is **12.66ms** at 2560x1600 (the 5.13ms was measured at
1920x1080, before dynamic resolution existed). And with precise timestamps the
every-30-frames log lines sit ~0.655s apart -- **~46fps delivered, not 60**, a
~21.8ms period against a 16.67ms budget, with `0 stale dropped` throughout.

### Root cause of the decoder's queue depth: no VUI in the SPS

`h264_headers.rs`'s `build_sps` wrote `vui_parameters_present_flag = 0`.
Confirmed on the real bitstream, not inferred:

```
$ ffmpeg -i /tmp/gop_test_ab.h264 -c copy -bsf:v trace_headers -f null -
  level_idc                     00101001 = 41
  vui_parameters_present_flag          0 = 0
```

With no VUI a decoder cannot know the stream never reorders, so clause E.2.1
makes it infer `max_num_reorder_frames = MaxDpbFrames` from `level_idc` and the
picture size. At level 4.1 (`MaxDpbMbs = 32768`): 2 frames at 2560x1600 (160x100
= 16000 MBs), 4 frames at 1920x1080 (120x68 = 8160 MBs). **Those are exactly the
`pending=3` and `pending=4-5` queue depths this file has been recording at those
two resolutions all along.** At 60fps that is 33-66ms of latency the decoder is
*required* to add.

That reframes Milestone 7's whole decoder investigation. `KEY_LOW_LATENCY`, the
Exynos vendor parameter, `KEY_PRIORITY`, the software-decoder switch -- every one
of them was fighting an instruction carried in the bitstream itself. Milestone 7's
conclusion, "this tablet's ~85-115ms decoder pipeline depth is a firmware/silicon
floor, not reachable from app-level MediaCodec configuration," was reasonable on
the evidence it had and is wrong.

**Fixed:** `write_low_latency_vui()` in `h264_headers.rs` emits
`bitstream_restriction_flag = 1`, `max_num_reorder_frames = 0`,
`max_dec_frame_buffering = max_num_ref_frames`. Sunshine does the same thing
host-side (`src/cbs.cpp`, `make_sps_h264`) and moonlight-android patches the same
fields client-side, with the comment "increases decoding latency".

Validated offline before touching hardware, via a throwaway
`daemon/src/bin/vui_bitstream_test.rs` driving `VaapiEncoder` on 150 synthetic
2560x1600 frames (same pattern Milestone 17 used): `trace_headers` shows
`max_num_reorder_frames = 0` / `max_dec_frame_buffering = 1`, and
`ffmpeg -f null -` decodes 150/150 with zero warnings. The exact VUI field layout
was cross-checked against x264's own `--tune zerolatency` output at the same
resolution -- identical order, identical restriction values.

**Honesty check, and it matters.** An earlier draft of this claimed moonlight
gates its patch on a device list including Exynos. It does not:
`decoderNeedsSpsBitstreamRestrictions()` prefix-matches exactly `omx.nvidia`,
`omx.qcom`, `omx.brcm` -- none of which can match `c2.exynos.h264.decoder`.
No measured before/after latency result attributable to these VUI fields on a
modern Exynos part could be found anywhere. What does hold up: AOSP's own
`C2SoftAvcDec.cpp` reads `i4_reorder_depth` back out of `ih264d` and reconfigures
`C2PortActualDelayTuning::output` live, so deriving output delay from the SPS is
the contract every Codec2 component is written against. Samsung's component is
closed and may instead report a fixed hardware-pipeline depth. **So the mechanism
is right and the framework is designed around it, but whether this tablet honors
it is unproven until `pending=N` is read live.** Expect a real chance of a
negative result.

**Also demoted on research:** `pic_order_cnt_type = 2` does not change the
Annex E default inference on its own, and states nothing the VUI doesn't state
explicitly once it is present. Not implemented.

### Capture was routing every frame through a GPU->CPU readback

KWin's `ScreenCastStream::onStreamParamChanged()` does a literal
`spa_pod_find_prop(format, nullptr, SPA_FORMAT_VIDEO_modifier)` and only sets up
its DMA-BUF path if that finds something. `portal_capture.rs`'s EnumFormat pod
never carried a modifier property, so **every frame took KWin's MemFd fallback**,
which is `grabTexture()` → a synchronous `glReadnPixels` of the whole 2560x1600
render target into CPU memory. KWin's own comment in `record()` -- "Sample it
before video rendering, readback and buffer synchronization add latency" --
places that cost inside the ~28-30ms this project measures as capture latency.
And then the daemon copied those same 16.4MB straight back onto the same GPU.

**Fixed:** two EnumFormat pods are now offered in preference order, the first
carrying a `SPA_FORMAT_VIDEO_modifier` choice with MANDATORY + DONT_FIXATE
(`build_format_pod`), the second the old shm form as a real fallback.
`vaapi_encoder.rs` gained `encode_frame_dmabuf` / `import_dmabuf`, importing the
PipeWire fd as a VA surface via `VADRMPRIMESurfaceDescriptor` +
`VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2`, cached by fd (KWin hands out a fixed
3-buffer pool and reuses the same fds). All the FFI needed was already in the
generated bindings; `wrapper.h` already included `va/va_drmcommon.h`.

Confirmed live: `[pipewire] buffer data type: DataType::DmaBuf (1 plane(s))`,
modifier `0x0` (LINEAR), three fds imported as surfaces 3/4/5, frames encoding
normally.

**The predicted win did not materialize where predicted.** The going-in
hypothesis was that the 16.4MB memcpy into a write-combined VAAPI mapping
dominated the 12.66ms. A controlled A/B -- `QUILL_FORCE_SHM=1` versus default,
same 30-second window, same on-screen motion probe, back to back -- says
otherwise: **shm 11.72ms vs DMA-BUF 10.92ms, with an identical 1350 frames
delivered either way.** The copy was ~0.8ms. That segment is dominated by GPU
work (VPP colour conversion plus H.264 encode over 4.1M pixels), not by the copy.

**And the half that should matter most is still unmeasured.** Removing KWin's
readback from the ~28-30ms capture segment was always the bigger prize, and it
could not be confirmed: the barcode probe decodes the CPU mapping that the
zero-copy path deliberately no longer has. A `QUILL_BARCODE_PROBE` escape hatch
is in (`with_mapped_dmabuf`, maps the imported surface for the diagnostic only,
off by default since mapping a GPU surface forces exactly the sync this path
exists to avoid), but the probe window currently renders clipped to ~100px of its
480px barcode on this display layout, so it decodes nothing. Confirmed by dumping
a raw frame with `QUILL_DUMP_FRAME` and looking at the pixels -- the run lengths
are clean multiples of 10, so it isn't a scaling problem, the window is just
mostly not there. Re-calibrating it is the next step; Milestone 7 called this
positioning "its own detour" and it still is.

### Encoder: two free wins on top

**Removed the `vaSyncSurface` between VPP and encode.** The encode targets the
same surface and ends with its own sync; the intermediate one only parked the CPU
between two GPU jobs that can overlap. Verified rather than assumed: the same 150
synthetic frames encode **byte-identical** (`md5` match) with and without it, and
still decode clean -- so VAAPI's ordering guarantee holds in practice on iHD.

**Pinned the encoder quality level to the driver's fastest.** Never set before.
`VAConfigAttribEncQualityRange` reports 7 on this iGPU; VAAPI's convention is 1 =
best quality, higher = faster, and rate control is CQP so picture quality is set
by QP regardless. P-frames grew from ~1250 to ~1316 bytes on synthetic content --
irrelevant next to a 240-byte steady-state frame.

**Cumulative, measured on the same probe back to back:** 12.66ms → 10.92
(DMA-BUF) → 10.65 (no VPP sync) → **9.73ms** (quality level). **-23% on the
daemon's encode segment.**

### Android: the decode loop only drained output when a packet arrived

`runDecodeLoop` read a frame off USB, queued it, then polled for output -- all on
one thread. A decoded frame could therefore only be rendered on an iteration that
*also* received a new frame, so a frame the decoder finished 2ms after queueing
waited for the next packet: up to a full inter-frame interval, unbounded while the
source screen was briefly idle.

Milestone 7 ruled out async `MediaCodec` mode on the grounds that it "only changes
how the client is notified, not how many frames the codec buffers internally."
That is true of the codec's DPB and doesn't cover this -- the polling delay is a
separate, additive cost created by the loop's shape. Research backs the
distinction: no measured A/B exists anywhere between async callbacks and a
dedicated blocking-dequeue thread (they converge structurally), but the
network-coupled third pattern -- exactly what this code did -- is the one
well-documented way to add a full inter-arrival interval of pure scheduling delay.

**Fixed:** split into a reader (socket → `queueInputBuffer`) and a dedicated
render thread doing a blocking `dequeueOutputBuffer(info, 50_000)`, both at
`THREAD_PRIORITY_URGENT_DISPLAY`, with the pending-send-time FIFO promoted to a
`ConcurrentLinkedQueue`. Milestone 12's burst-dedup is preserved verbatim on the
render side. Blocking-dequeue-on-its-own-thread rather than async callbacks
matches moonlight-android and keeps the burst logic, which needs to see a whole
burst at once, simple.

**A real bug fixed alongside it, introduced by Milestone 17 and not noticed then.**
When `dequeueInputBuffer` returned < 0 the loop logged a warning and **silently
discarded a frame it had already read off the wire**. Harmless under all-intra --
the next frame was a fresh IDR. Since the GOP landed, one discarded P frame
corrupts every frame after it until the next IDR, up to a full second of visible
garbage. Now it waits for a buffer instead of dropping.

**Wire format gained a keyframe flag.** The frame header is 13 bytes now
(`i64` send time, `u32` length, `u8` is_idr); heartbeats carry the same shape with
length 0 so the read loop has no special case before it knows the length. The
client had been passing `BUFFER_FLAG_KEY_FRAME` on every buffer, which has been a
lie on every P frame since Milestone 17. Also set `KEY_MAX_INPUT_SIZE` so the
codec never reallocates an input buffer mid-stream.

### Also: the per-frame disk write is now opt-in

`portal_capture.rs` wrote every encoded frame to `~/.local/share/quill/output.h264`
with an unbuffered `write()` syscall in the middle of the hot path, into a file
that grew without bound (~50MB after one session) and that nothing reads unless
someone is debugging the bitstream. Now behind `QUILL_DUMP_H264`, matching the
existing `QUILL_DUMP_FRAME` convention.

### Not done yet, in priority order

1. **Re-calibrate the barcode probe** so the capture segment is measurable again.
   Everything else on the capture side is guesswork until this works.
2. **Live-test A1 and A2 on the tablet.** `pending=N` in the Android stat line
   settles the VUI question in one run.
3. **Move the USB write off the capture thread** and release the PipeWire buffer
   as early as the data allows. KWin's `record()` calls `dequeueBuffer()` and, if
   the 2-4 buffer pool is exhausted, simply `return`s with **no retry scheduled**
   (the stream is `PW_STREAM_FLAG_DRIVER`; the next chance is the next damage
   event). Holding a buffer ~13ms against a 3-buffer pool is a source-grounded
   explanation for 46fps against a 60Hz ceiling.
4. **`CursorMode::Metadata` instead of `Embedded`.** `record()` ORs
   `Content::Video` in unconditionally under `Embedded`, so every cursor move
   forces a full composite of the whole output even with no window change --
   the dominant event source for a pen-driven monitor.
5. **Higher refresh virtual output.** No refresh argument exists anywhere in the
   krfb → `zkde_screencast_unstable_v1` → KWin path (`virtualOutputScreencast
   Requested` takes five arguments, none of them a rate; both virtual-output
   classes hardcode `OutputModeline(size, 60000, Preferred)`). Both advertise
   `Capability::CustomModes` though, so `kscreen-doctor addCustomMode` can
   override after the fact -- must be re-applied after every krfb launch, and
   before the daemon connects, since `screencaststream.cpp` renegotiates on size
   changes only.

### Follow-up same session: the capture segment finally measured, and three
### hypotheses killed

**The barcode probe works again, and the number in this file was stale.**
Milestone 7 recorded `xdotool windowmove <win> 1920 0` as the recipe for putting
the probe on the virtual monitor, with 1920 explained as "eDP-1's physical
width". That is no longer right and the explanation was never quite the reason.
`xrandr --listmonitors` gives it directly:

```
 0: +*eDP-1 2304/344x1296/194+0+305  eDP-1
 1: +Virtual-QuillDisplay 2560/2560x1600/1600+2304+0  Virtual-QuillDisplay
```

XWayland renders its root in units of the *largest* output scale (1.5 here), so
eDP-1's 1536x864 logical becomes 2304x1296 and the virtual output starts at
**2304**, not 1920. At 1920 the probe landed on eDP-1 and the daemon decoded
nothing -- confirmed by scanning a `QUILL_DUMP_FRAME` dump for any barcode-like
run pattern anywhere in the frame and finding none. Don't hardcode this
again: read the offset out of `xrandr --listmonitors`.

**Capture latency, the number DMA-BUF was supposed to move.** Both legs 35s,
same probe, back to back, first sample dropped as warmup (Milestone 7 saw the
same one-off, 664ms then; 1240ms now):

| | mean | median | min | max | n |
|---|---|---|---|---|---|
| shm (MemFd) | 33.16ms | 34.35ms | 20.69 | 45.54 | 52 |
| DMA-BUF | **30.59ms** | **31.08ms** | 19.57 | 47.99 | 53 |

To make that honest the timestamp is now sampled at the top of the `process`
callback rather than at the barcode read: the DMA-BUF path has to map a GPU
surface before it can read the barcode at all, and charging the zero-copy path
for its own diagnostic's sync would have faked the comparison.

**~2.6ms.** So removing KWin's `glReadnPixels` was worth real but small time,
and the ~30ms capture segment is *not* dominated by the readback -- it's
dominated by compositor scheduling. The going-in estimate for this change was
10-20ms; it delivered a quarter of that, and the honest read is that the
zero-copy work paid off mostly as ~0.8ms of encode-side copy plus this ~2.6ms,
not as the structural win it was scoped as.

**Killed: "our callback hold is what caps us at 45fps".** Added
`QUILL_NO_ENCODE`, which drops the PipeWire buffer immediately instead of
holding it across encode -- KWin's `record()` returns with no retry scheduled
when its 2-4 buffer pool is exhausted, so a long hold silently costs frames, and
that was a plausible source-grounded explanation. Measured: **45.0fps normal,
45.2fps with encoding skipped entirely.** Identical. The cap is upstream of this
daemon, so the planned writer-thread and early-buffer-release work is worth doing
for jitter but will not buy throughput. Good thing to have checked for the cost
of one env var rather than after building it.

(That diagnostic also surfaced a latent panic: `main.rs` unwrapped
`durations.iter().min()` while gating only on `frame_count`, so counting frames
without encoding any killed the summary right after printing the frame count.
Guarded.)

**Killed: raising the virtual output above 60Hz via kscreen-doctor, on this
system.** `kscreen-doctor output.Virtual-QuillDisplay.mode.2560x1600@120` returns
"Output mode 2560x1600@120 not found", and this build has no `addCustomMode`
verb at all ("Unable to parse arguments"). KWin does advertise
`Capability::CustomModes` on virtual outputs and re-derives
`m_renderLoop->setRefreshRate()` in `applyChanges()`, so the mechanism exists --
but reaching it means writing an `outputmanagement_v2` Wayland client, which is
its own project, not a tuning step.

**Where this leaves the capture segment:** ~30ms, with the readback removed and
the remaining cost sitting in KWin's own render/deliver cadence against a 60Hz
virtual output. Content on that output can only change once per 16.67ms
(`RenderLoopPrivate::scheduleRepaint()` quantises to whole vblank intervals;
`DrmVirtualOutput` drives it from a `SoftwareVsyncMonitor`), so ~30ms is roughly
two output frames. Getting it down means either a faster virtual output (blocked
above) or not needing a full frame for the common case -- which is what
`CursorMode::Metadata` would enable, since under `Embedded` KWin ORs
`Content::Video` in unconditionally and every pen or cursor move re-composites
the whole output. That is a real feature (the daemon would have to draw the
cursor from `SPA_META_Cursor` itself), not a flag flip, and it would make pen
tracking local rather than round-trip -- the most latency-sensitive thing this
project does. Flagged as the most promising remaining capture-side lever.

**Daemon-side totals for this session:** capture 33.2 -> 30.6ms, encode
12.66 -> 9.73ms. ~5.5ms off a ~46ms daemon-side budget. The decode-side change
(the SPS VUI, worth a predicted 33ms) is still the largest single unverified item
and needs the tablet.

### Live-tested on the tablet: the SPS VUI is worth 34ms, confirmed by a clean A/B

Ran it on the real Galaxy Tab S9 FE+ (SM_X610, `c2.exynos.h264.decoder`). Both
legs in the same session, same APK, same on-screen motion, ~30s each, swapping
only the daemon binary -- the only difference between them is whether
`build_sps` emits the VUI:

| | latency avg | min | max | `pending` |
|---|---|---|---|---|
| no VUI (previous behaviour) | 64ms | 36ms | 140ms | 3, steady |
| **with VUI** | **30ms** | **9ms** | 181ms | **0-2** |

**-34ms average, -27ms on the floor.** Two frames at 60fps is 33ms, which is
exactly what the DPB arithmetic predicted for level 4.1 at 160x100 macroblocks.
The mechanism is confirmed, not just the outcome.

**This settles the doubt recorded above, in the opposite direction from the
research.** moonlight's `decoderNeedsSpsBitstreamRestrictions()` does not list
Exynos, no public before/after measurement for these VUI fields on a modern
Exynos part could be found, and Samsung's Codec2 component is closed -- the
honest expectation going in was "a real chance of a negative result". This
tablet honors `max_num_reorder_frames = 0` completely. Milestone 7's conclusion
that "this tablet's ~85-115ms decoder pipeline depth is a firmware/silicon floor,
not reachable from app-level MediaCodec configuration" was wrong: it was
reachable, just not from MediaCodec -- from the bitstream.

**Attribution between the two Phase A changes.** A1 (VUI) and A2 (the
reader/render thread split) shipped together, so the A/B above was run with A2
present on both legs. That isolates A1 at -34ms. Comparing the no-VUI leg's 64ms
against the ~70-77ms this file recorded before A2 puts A2 at roughly -8 to -13ms,
though that half is a cross-session comparison and correspondingly weaker.

**Full pipeline now, summing the three instrumented segments:** ~30.6ms capture +
~8.4ms encode + ~30ms decode/render = **~69ms**, against ~112-123ms at the start
of this session. Worth a camera glass-to-glass confirmation before treating ~69ms
as the headline number -- every previous milestone that trusted summed segments
without one eventually found a gap.

### A framing bug this session introduced, caught only on the tablet

The two-EnumFormat-pod DMA-BUF negotiation makes `param_changed` fire **twice**
(modifier negotiation is inherently two-step: offer a DONT_FIXATE choice, the
producer picks and sends the fixated format back). Counting the events makes it
plain: 1 with `QUILL_FORCE_SHM`, 2 without. The handler announced the video
format on each firing, so a second 8-byte header landed in a stream the client
was already reading as length-prefixed frames.

Live symptom: handshake fine, then `clock-sync: offset=3457846859832230715ms`,
then `video format: 1174405120x18998372`, then
`MediaCodec.configure` throwing `IllegalArgumentException: Invalid size(s)` in a
1-second reconnect loop, with a green rectangle over a zoomed-in wallpaper on the
tablet. Easy to misread as the known stale-AOA desync from Milestones 13/14/17;
it wasn't, it was new and deterministic.

Fixed with a `sent_format: Option<(u32, u32)>` on `CaptureData`: the header is
announced once, a repeat of the same size is ignored, and a genuine mid-session
size change logs a warning (the client cannot reconfigure its decoder, so that
case is a reconnect either way). The encoder is likewise only rebuilt when the
geometry actually changes -- the second firing had been discarding a working
encoder and resetting its GOP state for nothing.

Worth stating plainly: this shipped in the previous commit and could not have
been caught without hardware. Capture-only runs looked perfectly healthy, because
nothing was reading the stream.


## 19. Config screen, protocol v2, and client-side cursor -- which turns out not
## to work on a KWin virtual output

User asked for a settings screen on either side, with client-side cursor as one
of the options. Built on the Android side: the daemon is headless and
systemd-launched, so a settings UI there would mean a whole toolkit for one
dialog, the tablet is where the user physically is, and the Android->daemon
handshake already exists to carry the values.

### Protocol v2, because the old framing had cost enough already

v1 was three framings sharing one connection: a fixed 32-byte handshake up, then
down an unframed 24-byte clock-sync reply, an unframed 8-byte video-format
header, and only then length-prefixed frames. Nothing on the wire said which was
which, so any disagreement about bytes consumed was permanent and silent. That
is Milestones 8, 13, 14, 17, and again in 18 when a second video-format header
slipped in. Adding cursor messages to that would have been asking for a sixth.

v2 (`daemon/src/protocol.rs`, mirrored in `MainActivity.kt`):

- Handshake carries `MAGIC` (`"QUIL"`) and a version, checked before anything is
  believed. A stale peer is now diagnosed rather than interpreted as a screen
  size and acted on -- v1's Milestone 13 bounds check exists precisely because
  garbage reached `kscreen-doctor`.
- Handshake body is length-prefixed, so fields can be appended without breaking
  an older peer; it reads what it knows and skips the rest.
- **Every** downstream byte is a typed, length-prefixed message (`MSG_VIDEO`,
  `MSG_CURSOR`, `MSG_HEARTBEAT`, `MSG_CLOCK_SYNC`, `MSG_VIDEO_FORMAT`). The
  client's read loop is uniform from the first byte; there are no pre-loop reads
  left to fall out of step with.

Verified on hardware before changing cursor behaviour: avg 30ms, `pending` 0-1,
`clock-sync offset=598ms round-trip sum=23ms`. No regression.

### Settings screen

`SettingsActivity` + `Settings` (SharedPreferences), reached by tapping the
status overlay -- the app runs edge-to-edge immersive with no system chrome to
hang a menu off. Values go up in the handshake's `config_flags`. Everything
applies at connect time, not live, and the UI says so: cursor mode decides which
kind of portal session the daemon opens, and Milestone 13's lesson was that
reconfiguring an already-open portal/PipeWire session is the part that breaks.

Each cursor mode also gets its own portal restore token file, since a token is
bound to the session it was issued for; sharing one would make every toggle look
like a rejected token and pop the picker at someone who may not be at the
keyboard.

### The cursor metadata itself: two omissions, then a wall

First omission, ours: `CursorMode::Metadata` on the portal does nothing on its
own. PipeWire only attaches a metadata region the *consumer* declared via
`SPA_PARAM_Meta`, so `find_meta::<MetaCursor>()` returned `None` on every buffer
and the probe reported the meta simply absent. Requesting it -- after format
negotiation, since buffers are allocated then -- made it appear immediately.
Same shape as the DMA-BUF gap in Milestone 18: the compositor was willing, we had
never asked. That is now three findings of one shape in two days.

With that fixed the whole path worked: position, hotspot, and an RGBA 48x48
bitmap on shape change only, forwarded as `MSG_CURSOR`, drawn by a `CursorOverlay`
view above the `SurfaceView`.

**And then the wall. KWin composites the cursor into the video anyway, so
enabling this gives you two pointers.** User reported it immediately: "the cursor
appears to be leaving a single cursor as a trail behind it from the frame
before". Exactly one stale pointer, not an accumulating trail -- which pointed at
the video containing one rather than at an overlay bug.

Confirmed from the pixels, not inferred. Recorded 900 frames in metadata mode
with `QUILL_DUMP_H264`, extracted the frames at two different logged cursor
positions, and found the pointer bitmap present at both, tracking the metadata.
(The scene was GIMP, which draws its own crosshair, so a single frame would have
been ambiguous -- two frames at two positions were not.)

Mechanism: a DRM output keeps the cursor on a hardware plane, out of the primary
framebuffer, which is what `setRenderCursor(false)` has to suppress. A virtual
output has no cursor plane, so KWin composites a software cursor straight into
the framebuffer the screencast captures. `Metadata` mode therefore supplies
cursor data *in addition to* the embedded cursor, not instead of it. Directly
analogous to Milestone 16's finding that KWin's rotation property has no effect
on what this output type exports.

**And the payoff was small regardless.** User's own comparison: the
tablet-drawn pointer ran "5 to 10 ms maximum" ahead of the content behind it.
After Milestone 18 the video is fast enough that skipping decode buys very
little.

Kept the toggle, defaulted off, and labelled it honestly in the UI -- including
the two-pointer issue and the measured 5-10ms -- rather than shipping a setting
that quietly makes things worse. Other compositors, or a real DRM output, may
behave differently, and the daemon and protocol support is now in place if so.

**Also still true:** this does nothing for pen *ink* either way. Ink is drawn by
the host application and returns through the video path regardless. For a
drawing tablet that is the number that matters, which makes Milestone 18's
34ms decoder win the one that actually helps.

### Watchdog vs. the portal picker: a measurement bug the picker made reachable

Reconnecting after a portal re-prompt produced `latency avg=585ms` while video
was visibly fine (`pending=1`, frames flowing, encode 9.1ms). Not a regression --
a broken calibration. Two calibrations had run:

```
15:28:27  offset=610ms  round-trip sum=10ms     <- clean
15:29:04  offset=52ms   round-trip sum=1119ms   <- garbage
```

The screen-picker dialog appears whenever the saved restore token is missing or
rejected, and someone has to physically walk over and click it. That took ~37s.
The Android watchdog's budget was 15s (set in Milestone 16 for the
virtual-monitor recreate case), so it fired mid-wait and forced a reconnect,
whose clock-sync then calibrated against a reply that had been sitting queued --
hence the 1119ms round trip. Every per-frame latency for the rest of that session
was off by roughly the difference between the two offsets.

The watchdog cannot distinguish "daemon is blocked on a human" from "daemon
died", and before the first frame arrives it has no basis to. Fixed by giving it
a separate startup budget (180s) that applies until real video starts, then
dropping to the existing 15s -- the steady-state case, detecting a peer that
died mid-session, is the one that needs to be tight, and it stays tight.

Clean reconnect afterwards: **avg 29ms, min 7ms, pending 0-2.**

Worth noting for its own sake: the number was wrong while the *system* was
right. Every latency figure this project records comes through this calibration,
so a bad calibration is indistinguishable from a real regression unless the
round-trip sum is checked alongside it. It is printed next to the offset for
exactly this reason -- 10ms versus 1119ms said immediately which reading to
trust.

## 20. Camera confirmation: ~55ms glass-to-glass, target met

Ran the readable-clock camera test (Milestone 7's method, unchanged): two
`readable_clock` instances, one on `eDP-1` at X11 `500,500`, one on
`Virtual-QuillDisplay` at `2304,0`-ish, filmed together in slow motion, reading
both values off the same frame.

**Four readings (main screen vs tablet):** .542/.491 (51ms), .593/.541 (52ms),
.727/.659 (68ms), .811/.760 (51ms) -- **mean 55.5ms, median 51.5ms.**

Against every previous measurement by the same method:

| | camera glass-to-glass |
|---|---|
| Milestone 4 | ~300ms |
| Milestone 7 (post-VPP) | ~144ms |
| Milestone 17 | ~113-126ms |
| **Milestone 18-20** | **~55ms** |

The ~60ms goal is met, and this is the number to quote, because it is the same
method as every historical figure -- whatever bias it carries, it carries
identically across all of them.

### Reconciling ~55ms camera against ~69ms instrumented -- both are right

The instrumented segments sum to 30.6ms capture + 8.6ms encode + 29-30ms
decode/render = ~69ms, which is *larger* than the camera number. That is not a
contradiction, and it is worth writing down so nobody "fixes" it later:

- The camera compares two **screens**. The reference clock on `eDP-1` is itself
  delayed by the laptop's own compositing and panel latency. Reading
  `X_main - X_tablet` at one photon instant yields
  `L_total - L_laptop`, not `L_total` -- the laptop's own display latency is
  subtracted out.
- The instrumented sum measures a different span: from the probe *painting* its
  buffer to `releaseOutputBuffer` on the tablet. It excludes the tablet's own
  SurfaceFlinger/panel presentation on one end.

The two reconcile at roughly 30ms of laptop-side display latency, which is
unremarkable for a 60Hz panel under KWin. So there is no unexplained gap here --
the situation Milestone 7 hit twice, where a summed figure hid real latency, does
not recur. Absolute pixel-change-to-tablet-photon is probably ~80-85ms; the
~55ms figure is the honest apples-to-apples number for tracking progress, and
~69ms is the honest figure for "what this project's own code is responsible
for".

### Where the remaining time is

Of the ~69ms this project controls: **capture is now the largest single
segment at ~30ms**, and Milestone 18 established that removing KWin's readback
only bought 2.6ms of it -- the rest is the compositor's own render/deliver
cadence against a 60Hz virtual output, roughly two output frames. Raising that
output's refresh rate is the remaining lever, and it needs an
`outputmanagement_v2` Wayland client since this build's `kscreen-doctor` has no
`addCustomMode` verb.
