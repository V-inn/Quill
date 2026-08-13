# Linux SuperDisplay Equivalent — Technical Design Doc

Primary target device: Samsung Galaxy Tab S9 FE+ (Wacom EMR S Pen, 4096 pressure levels, tilt, ~1cm hover), connected to a Linux host over USB. Also intended to work unmodified on the Galaxy Tab S10 FE+ (13.1", 1800×2880, 90Hz) and in principle any Samsung S Pen tablet, since nothing in this design is device-specific — see §3.5.

Goal: a real extended display (not screen mirroring) with a native, full-fidelity pen input path, at latency competitive with SuperDisplay on Windows.

## 1. Why this is buildable without new kernel drivers

Two of the four required pieces are existing, maintained, in-kernel Linux technologies:

- **evdi** (Extensible Virtual Display Interface) — the kernel module DisplayLink itself maintains and ships upstream. It registers a genuine DRM/KMS output that the Linux desktop treats as a real monitor (appears in `xrandr`/`wlr-randr`, can be arranged, scaled, rotated like any display), and hands framebuffer updates to a userspace client. GPL v2, works on kernels up to 6.15+.
- **uinput** — the standard in-kernel facility for creating virtual input devices from userspace. Used by Weylus and GfxTablet today to expose `ABS_PRESSURE`, `ABS_TILT_X/Y`, `BTN_STYLUS`, `BTN_TOOL_PEN` — picked up natively by libinput and read correctly by Krita, GIMP, Blender, etc.

So "writing custom drivers" mostly means writing **one new Android app** and **one new Linux userspace daemon**, not new kernel code.

## 2. Architecture

```
┌─────────────────────────────┐        USB (adb forward/reverse tunnel)        ┌───────────────────────────────┐
│  Linux Host                 │ <══════════════════════════════════════════>  │  Galaxy Tab S9 FE+ (Android)   │
│                              │                                                │                                │
│  evdi virtual output ──────┐│         video: H.264/H.265 (VAAPI, low-latency)│  MediaCodec hw decoder          │
│  (real DRM display)        ││ ───────────────────────────────────────────►  │  → SurfaceView render           │
│                              ││                                                │                                │
│  Capture+Encode daemon ◄────┘│                                                │  MotionEvent / InputDevice      │
│  (Rust/C++, VAAPI)          │         input: pressure/tilt/hover/buttons     │  capture (S Pen full fidelity)  │
│                              │ ◄───────────────────────────────────────────  │                                │
│  uinput virtual tablet ◄─────┤         (custom compact binary protocol)      │                                │
│  device (pressure/tilt)     │                                                │                                │
└─────────────────────────────┘                                                └───────────────────────────────┘
```

## 3. Components

### 3.1 Virtual display — evdi (existing project, reused as-is)

- Load the evdi kernel module, create one virtual output.
- Compositor (X11 or Wayland/wlroots) sees it as a normal monitor — user drags windows onto it like any second screen.
- The daemon (below) is evdi's userspace client: it receives raw framebuffer damage/updates whenever the desktop redraws.

### 3.2 Capture + encode daemon (new, Linux, Rust or C++)

Responsibilities:
- Owns the evdi connection, receives dirty-rect framebuffer updates.
- Encodes with VAAPI hardware H.264 (Intel/AMD) — target `quality-level` tuned toward speed over compression, CBR rate control, small/no B-frame GOP, matching the "tune=zerolatency" philosophy from software x264. Reference LAN benchmarks with VAAPI land around ~40ms glass-to-glass; USB-tunneled local socket should beat that since there's no network stack involved.
- Sends encoded frames over the transport (3.3).
- Hosts the uinput virtual tablet device and translates incoming input packets from the tablet into `ABS_PRESSURE`/`ABS_TILT_X/Y`/`BTN_STYLUS` events.
- Exposes a small config surface: resolution, refresh target, bitrate, orientation.

### 3.3 Transport — USB, not network

Reuse the trick Weylus already uses: `adb forward`/`adb reverse` tunnels a TCP socket over the physical USB cable — this is real USB transport, ADB is just acting as the mux, not going over Wi-Fi. Two options, in order of recommended build sequence:

1. **v1 (build this first): raw TCP over adb forward.** Minimal integration work, adb is already installed/trusted tooling, negligible overhead (single-digit ms) compared to codec latency.
2. **v2 (optional later): Android Open Accessory (AOA) mode.** Puts the Linux host in USB host role and the tablet in accessory role, communicating over raw USB bulk endpoints with no ADB/debugging dependency — "driverless" in the sense SuperDisplay is, and removes the adb-daemon hop entirely. More USB plumbing work (libusb on the host side, AccessoryManager on Android), worth doing only once v1 proves out the rest of the pipeline.

Protocol: simple length-prefixed binary frames, one stream for video (encoded NALUs + timestamp), one stream for input (event type, x, y, pressure 0–4096, tilt x/y, buttons, in-range/hover flag, timestamp). No JSON, no WebSocket framing — this is the concrete latency win over Weylus's browser-based approach.

Connection opens with a **capability handshake**, not hardcoded assumptions: the Android client reports its panel resolution, DPI, refresh rate, and pressure/tilt/hover ranges (from `Display` metrics and `InputDevice.getMotionRange()`), and the daemon configures the evdi output and pressure/tilt scaling from that. This is what makes the same daemon/client pair work across the S9 FE+, S10 FE+, and other Samsung S Pen tablets without per-device code.

### 3.4 Android client (new, native app — this is the actual "custom driver" work on the tablet side)

This is the piece that gets you real S Pen fidelity that a browser (Weylus's approach) cannot:

- `MediaCodec` hardware H.264/H.265 decoder → renders to a `SurfaceView`/`TextureView` full-screen.
- Captures `MotionEvent` directly from the Android input pipeline (not the DOM Pointer Events API), which is what exposes: 4096-level pressure, tilt X/Y, ~1cm hover/proximity (`ACTION_HOVER_MOVE`), the S Pen side button (`MotionEvent.BUTTON_STYLUS_PRIMARY`), and palm rejection via `MotionEvent.TOOL_TYPE_ERASER`/`TOOL_TYPE_STYLUS` discrimination.
- Packs events into the compact binary protocol and writes them to the USB socket.
- Optional: reintroduce Air Actions/Air Command gestures as configurable shortcuts later — not required for v1 parity.

### 3.5 Device portability (S9 FE+, S10 FE+, and beyond)

Nothing in this design is specific to one tablet model:

- The S Pen path uses standard Android stylus `MotionEvent`/`InputDevice` APIs, the same on every Samsung S Pen tablet (all use a Wacom EMR digitizer under the hood) — not a Samsung-only SDK.
- Resolution, DPI, refresh rate, and pressure/tilt ranges come from the capability handshake (§3.3), not constants in code.
- The only per-device tuning that's worth doing (not required for correctness) is capping encode bitrate/resolution sensibly for larger panels — e.g. the S10 FE+'s 13.1" 1800×2880 90Hz panel is a bigger pixel count than the S9 FE+, so the encoder's bitrate/quality-level defaults should scale with negotiated resolution rather than being fixed.

Practical implication for testing: validate the handshake and scaling logic against both tablets before treating "works on my tablet" as "device-agnostic" — don't let hardcoded constants creep in during the S9 FE+-only build phase.

## 4. Latency budget (rough, to validate empirically)

| Stage | Expected contribution |
|---|---|
| evdi framebuffer update → daemon | <1 ms (shared memory) |
| VAAPI hardware encode | ~5–15 ms |
| USB transport (adb-tunneled TCP) | ~1–5 ms |
| MediaCodec hardware decode | ~5–15 ms |
| Display composite/present | ~1 frame (≈16 ms @60Hz) |
| Input path (tablet → daemon → uinput) | ~1–5 ms |

Total video path in the ~15–40ms range is realistic and should feel comparable to SuperDisplay; getting there depends on tuning encoder settings and avoiding buffering in the Android decode/render path (single-buffered `Surface`, not a `MediaPlayer`-style pipeline with jitter buffers).

## 5. Comparison

| | SuperDisplay (Windows) | Weylus (Linux, today) | This design |
|---|---|---|---|
| Real extended display | Yes (IddCx) | No — mirrors existing screen/window | Yes (evdi) |
| Pen pressure/tilt | Yes | Yes (via uinput) | Yes (via uinput) |
| S Pen hover/side-button/full fidelity | Yes | Partial (browser Pointer Events) | Yes (native MotionEvent) |
| Transport | USB, proprietary protocol | USB via adb tunnel, WebSocket framing | USB via adb tunnel (v1) → AOA (v2), raw binary framing |
| New kernel code required | N/A | None | None (evdi + uinput both existing) |

## 6. Build plan / milestones

1. evdi bring-up: get a virtual monitor showing up in your desktop, confirm you can read raw framebuffer frames from userspace.
2. Daemon v0: evdi → VAAPI encode → dump to a file, measure encode latency.
3. Transport: adb forward socket, stream encoded frames to a throwaway Android test app, confirm decode via `MediaCodec`.
4. Android client v0: decode + render only, measure glass-to-glass latency with a stopwatch/high-fps camera test.
5. Input path: uinput virtual tablet device on Linux, static test with synthetic events in Krita/GIMP.
6. Wire Android `MotionEvent` capture → protocol → uinput injection end to end.
7. Tuning pass: encoder settings, buffer sizes, thread priorities to cut jitter.
8. (Optional v2) Swap adb transport for AOA to drop the adb dependency entirely.

## 7. Open questions / risks

- GPU dependency: VAAPI needs Intel/AMD; NVIDIA hosts would need NVENC instead — worth confirming which GPU the target host has.
- evdi is out-of-tree (DKMS); needs rebuilding on kernel upgrades, and Secure Boot setups need the module signed.
- Wayland compositor support for evdi outputs varies by compositor (works well on wlroots-based ones; GNOME/Mutter support has historically lagged behind X11's xrandr path) — worth checking against the target desktop environment before committing.
- USB cable/port matters for adb tunnel stability under sustained bulk transfer — worth testing with the actual cable being used.

## References

- evdi: https://github.com/DisplayLink/evdi
- Weylus: https://github.com/H-M-H/Weylus
- GfxTablet (uinput pressure precedent): https://github.com/rfc2822/GfxTablet
- Android AOA protocol: https://source.android.com/docs/core/interaction/accessories/aoa
- Kernel input event codes for styluses: https://github.com/linuxwacom/input-wacom/wiki/Kernel-Input-Event-Overview
