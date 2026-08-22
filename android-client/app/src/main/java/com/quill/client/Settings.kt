package com.quill.client

import android.content.Context
import android.content.SharedPreferences

/**
 * User-selectable options, persisted on the tablet and sent up to the daemon in
 * the capability handshake (see `protocol.rs`).
 *
 * The tablet is where these live rather than the daemon because the daemon is
 * headless and systemd-launched -- a settings UI there would mean pulling in a
 * whole toolkit for one dialog -- and because the handshake already flows in
 * this direction, so no new transport is needed to carry them.
 *
 * Everything here applies at *connect* time, not live. That is deliberate:
 * cursor mode in particular decides which kind of portal session the daemon
 * opens, and Milestone 13's lesson was that reconfiguring an already-open
 * portal/PipeWire session is the part that breaks. The UI says so.
 */
class Settings(context: Context) {
    private val prefs: SharedPreferences =
        context.getSharedPreferences("quill_settings", Context.MODE_PRIVATE)

    /**
     * When true, the daemon asks the portal for cursor *metadata* and the app
     * draws the pointer itself.
     *
     * Worth understanding before enabling: it makes the pointer track at local
     * latency instead of a full pipeline round trip, and stops every pointer
     * move from re-compositing the whole virtual output. But the pointer will
     * then visibly lead the content under it -- drag a window and the pointer
     * arrives first, the window follows. It also does nothing for pen *ink*,
     * which is drawn by the host application and comes back through the video
     * path either way.
     */
    var clientSideCursor: Boolean
        get() = prefs.getBoolean(KEY_CLIENT_SIDE_CURSOR, false)
        set(value) = prefs.edit().putBoolean(KEY_CLIENT_SIDE_CURSOR, value).apply()

    /** Draws the running per-frame latency over the video. */
    var showLatencyOverlay: Boolean
        get() = prefs.getBoolean(KEY_LATENCY_OVERLAY, false)
        set(value) = prefs.edit().putBoolean(KEY_LATENCY_OVERLAY, value).apply()

    /**
     * Keeps the panel lit while a desktop is actually on screen.
     *
     * Defaults to **on**: this app is a monitor, and a monitor that goes dark
     * because you stopped drawing for a moment is the surprising behaviour, not
     * the other way round. An idle desktop produces no video frames at all --
     * that is what `MSG_HEARTBEAT` exists for -- so "nothing has arrived
     * recently" is exactly the case this has to survive. Which is why
     * `MainActivity` arms it on the first rendered frame and disarms it when the
     * connection drops, rather than tracking frame arrival.
     *
     * Tablet-local, like [showLatencyOverlay]: it changes nothing about what the
     * daemon captures, so it is deliberately **not** in [configFlags], unlike
     * the three booleans around it. It also takes effect on return from
     * settings rather than at the next connect.
     */
    var keepScreenAwake: Boolean
        get() = prefs.getBoolean(KEY_KEEP_SCREEN_AWAKE, true)
        set(value) = prefs.edit().putBoolean(KEY_KEEP_SCREEN_AWAKE, value).apply()

    /**
     * Sends pinch as Ctrl+scroll instead of as a real pinch gesture.
     *
     * Off, the daemon hands both fingers to a virtual touchpad and libinput
     * turns them into a proper pinch -- which is delivered over the Wayland
     * gesture protocol, so only applications that speak it (Firefox, GTK, Qt6)
     * zoom, and anything running on XWayland ignores it entirely. On, the
     * daemon recognizes the pinch itself and synthesizes Ctrl+scroll, which
     * nearly every application honours, but zooms in fixed steps rather than
     * smoothly.
     */
    var ctrlScrollZoom: Boolean
        get() = prefs.getBoolean(KEY_CTRL_SCROLL_ZOOM, false)
        set(value) = prefs.edit().putBoolean(KEY_CTRL_SCROLL_ZOOM, value).apply()

    /**
     * How far the streamed image is turned, in degrees: 0, 90, 180 or 270. The
     * touch and pen mapping turns with it, so they keep lining up.
     *
     * Which way up the desktop belongs depends on which end of the device the
     * USB cable enters, and only the person holding it knows that. Until
     * Milestone 24 the daemon inferred it from the aspect ratio -- portrait
     * meant flipped -- which was a fact about one tablet held one way, and would
     * have flipped every phone unconditionally, since phones are portrait by
     * default.
     *
     * The quarter turns arrived later and are the more interesting case: at 90
     * or 270 the handshake asks for a monitor whose dimensions are *swapped*, so
     * a landscape tablet drives a portrait desktop that then fills the panel
     * exactly. See [rotationSwapsAxes] and `MainActivity.sendHandshake`.
     */
    var rotationDegrees: Int
        get() {
            val stored = prefs.getInt(KEY_ROTATION_DEGREES, -1)
            if (stored in VALID_ROTATIONS) return stored
            // Migration: before the quarter turns there was only a boolean.
            // Read it through once rather than writing, so an older build
            // installed over this one still finds what it expects.
            return if (prefs.getBoolean(KEY_FLIP_180, false)) 180 else 0
        }
        set(value) {
            val normalised = if (value in VALID_ROTATIONS) value else 0
            prefs.edit()
                .putInt(KEY_ROTATION_DEGREES, normalised)
                // Kept in step so a downgrade to a build that only knows the
                // boolean still behaves sensibly at 0 and 180.
                .putBoolean(KEY_FLIP_180, normalised == 180)
                .apply()
        }

    /** True at the quarter turns, where the monitor is the panel transposed. */
    val rotationSwapsAxes: Boolean
        get() = rotationDegrees == 90 || rotationDegrees == 270

    /**
     * Which screen edge the settings gear is parked against, as a
     * [GearEdge] ordinal.
     *
     * Stored as an edge plus a fraction along it rather than as absolute
     * pixels, so the position survives a physical rotation, split-screen, and a
     * different device -- all of which would put a saved pixel coordinate
     * somewhere meaningless or off-screen entirely.
     */
    var gearEdge: Int
        get() = prefs.getInt(KEY_GEAR_EDGE, GearEdge.RIGHT.ordinal)
        set(value) = prefs.edit().putInt(KEY_GEAR_EDGE, value).apply()

    /** How far along [gearEdge] the gear sits, 0..1. */
    var gearFraction: Float
        get() = prefs.getFloat(KEY_GEAR_FRACTION, DEFAULT_GEAR_FRACTION)
        set(value) = prefs.edit().putFloat(KEY_GEAR_FRACTION, value.coerceIn(0f, 1f)).apply()

    /**
     * What the S Pen's side button does while held.
     *
     * One of [SIDE_BUTTON_RIGHT], [SIDE_BUTTON_MIDDLE], [SIDE_BUTTON_ERASER] or
     * [SIDE_BUTTON_NONE].
     *
     * Tablet-local, and deliberately *not* in [configFlags]: it rides each
     * button event instead, so changing it takes effect on the next press
     * rather than at the next connect. "None" needs no representation on the
     * wire at all -- the event simply is not sent.
     */
    var sideButtonAction: Int
        get() = prefs.getInt(KEY_SIDE_BUTTON, SIDE_BUTTON_RIGHT)
        set(value) = prefs.edit()
            .putInt(KEY_SIDE_BUTTON, if (value in VALID_SIDE_BUTTONS) value else SIDE_BUTTON_RIGHT)
            .apply()

    /**
     * How large a desktop to ask for, as a percentage of the panel's own
     * resolution: 100, 75 or 60.
     *
     * At 100 the virtual monitor is the panel, pixel for pixel, which on an
     * 11-inch screen is a very dense desktop -- lots of room, small text. Below
     * that the desktop is smaller than the panel and gets scaled up on the way
     * in, so everything is bigger, there is less to encode, and less goes over
     * the cable.
     *
     * The aspect never changes, so nothing is ever letterboxed: both axes take
     * the same factor. That is also what keeps the input mapping to a single
     * multiply -- see `MainActivity.send`.
     */
    var workspaceScalePercent: Int
        get() {
            val stored = prefs.getInt(KEY_WORKSPACE_SCALE, 100)
            return if (stored in VALID_WORKSPACE_SCALES) stored else 100
        }
        set(value) = prefs.edit()
            .putInt(KEY_WORKSPACE_SCALE, if (value in VALID_WORKSPACE_SCALES) value else 100)
            .apply()

    /**
     * Halve the stream to 30fps.
     *
     * Roughly halves what the daemon encodes and what crosses the cable. Fine
     * for reading, not for drawing, which is why it is a choice rather than a
     * default.
     */
    var cap30Fps: Boolean
        get() = prefs.getBoolean(KEY_CAP_30_FPS, false)
        set(value) = prefs.edit().putBoolean(KEY_CAP_30_FPS, value).apply()

    /**
     * How hard the daemon's encoder works: [QUALITY_BALANCED],
     * [QUALITY_SHARPER] or [QUALITY_LIGHTER].
     *
     * Balanced is zero on the wire and is exactly what every version before
     * this setting used, so an older daemon is unaffected by a newer client.
     */
    var quality: Int
        get() {
            val stored = prefs.getInt(KEY_QUALITY, QUALITY_BALANCED)
            return if (stored in VALID_QUALITIES) stored else QUALITY_BALANCED
        }
        set(value) = prefs.edit()
            .putInt(KEY_QUALITY, if (value in VALID_QUALITIES) value else QUALITY_BALANCED)
            .apply()

    /** Packed into the handshake's `config_flags` byte. */
    fun configFlags(): Int {
        var flags = 0
        if (clientSideCursor) flags = flags or CONFIG_CLIENT_SIDE_CURSOR
        if (ctrlScrollZoom) flags = flags or CONFIG_CTRL_SCROLL_ZOOM
        // Two bits, arranged so that bit 2 still means exactly 180 degrees and
        // the two orientations that predate the quarter turns are bit-for-bit
        // what they always were. See protocol.rs, which this mirrors.
        when (rotationDegrees) {
            90 -> flags = flags or CONFIG_ROTATE_90
            180 -> flags = flags or CONFIG_FLIP_180
            270 -> flags = flags or CONFIG_ROTATE_90 or CONFIG_FLIP_180
        }
        if (cap30Fps) flags = flags or CONFIG_FPS_30
        flags = flags or (quality shl CONFIG_QUALITY_SHIFT)
        return flags
    }

    companion object {
        private const val KEY_CLIENT_SIDE_CURSOR = "client_side_cursor"
        private const val KEY_LATENCY_OVERLAY = "latency_overlay"
        private const val KEY_KEEP_SCREEN_AWAKE = "keep_screen_awake"
        private const val KEY_GEAR_EDGE = "gear_edge"
        private const val KEY_GEAR_FRACTION = "gear_fraction"

        /** Near the top of the right edge -- roughly where the gear has always
         * been, so nothing moves for someone who never drags it. */
        private const val DEFAULT_GEAR_FRACTION = 0.06f
        private const val KEY_CTRL_SCROLL_ZOOM = "ctrl_scroll_zoom"
        private const val KEY_FLIP_180 = "flip_180"
        private const val KEY_ROTATION_DEGREES = "rotation_deg"
        private const val KEY_SIDE_BUTTON = "side_button_action"
        private const val KEY_WORKSPACE_SCALE = "workspace_scale_percent"
        private const val KEY_CAP_30_FPS = "cap_30_fps"
        private const val KEY_QUALITY = "encoder_quality"
        private val VALID_ROTATIONS = setOf(0, 90, 180, 270)

        // Mirrors protocol.rs. Kept as plain constants in both languages rather
        // than generated, since there are only a handful and a build-time
        // codegen step for them would cost more than it saves -- but they must
        // be changed together.
        const val CONFIG_CLIENT_SIDE_CURSOR = 1 shl 0
        const val CONFIG_CTRL_SCROLL_ZOOM = 1 shl 1
        const val CONFIG_FLIP_180 = 1 shl 2
        const val CONFIG_ROTATE_90 = 1 shl 3
        const val CONFIG_FPS_30 = 1 shl 4
        const val CONFIG_QUALITY_SHIFT = 5

        // Mirrors protocol.rs's Quality. Balanced is zero, which is what every
        // version before this setting sent.
        const val QUALITY_BALANCED = 0
        const val QUALITY_SHARPER = 1
        const val QUALITY_LIGHTER = 2

        // Bits 2-3 of a button event's `buttons` byte, mirroring
        // uinput_tablet.rs's ButtonAction. Right click is zero, which is what
        // every version before this mapping existed sent.
        const val SIDE_BUTTON_RIGHT = 0
        const val SIDE_BUTTON_MIDDLE = 1
        const val SIDE_BUTTON_ERASER = 2

        /** Not a wire value: the event is simply not sent. */
        const val SIDE_BUTTON_NONE = 3

        val QUALITIES = listOf(QUALITY_BALANCED, QUALITY_SHARPER, QUALITY_LIGHTER)
        private val VALID_QUALITIES = QUALITIES.toSet()

        val WORKSPACE_SCALES = listOf(100, 75, 60)
        private val VALID_WORKSPACE_SCALES = WORKSPACE_SCALES.toSet()

        private val VALID_SIDE_BUTTONS =
            setOf(SIDE_BUTTON_RIGHT, SIDE_BUTTON_MIDDLE, SIDE_BUTTON_ERASER, SIDE_BUTTON_NONE)
    }
}
