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

    /** Packed into the handshake's `config_flags` byte. */
    fun configFlags(): Int {
        var flags = 0
        if (clientSideCursor) flags = flags or CONFIG_CLIENT_SIDE_CURSOR
        if (ctrlScrollZoom) flags = flags or CONFIG_CTRL_SCROLL_ZOOM
        return flags
    }

    companion object {
        private const val KEY_CLIENT_SIDE_CURSOR = "client_side_cursor"
        private const val KEY_LATENCY_OVERLAY = "latency_overlay"
        private const val KEY_CTRL_SCROLL_ZOOM = "ctrl_scroll_zoom"

        // Mirrors protocol.rs. Kept as plain constants in both languages rather
        // than generated, since there are only a handful and a build-time
        // codegen step for them would cost more than it saves -- but they must
        // be changed together.
        const val CONFIG_CLIENT_SIDE_CURSOR = 1 shl 0
        const val CONFIG_CTRL_SCROLL_ZOOM = 1 shl 1
    }
}
