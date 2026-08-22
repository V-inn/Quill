package com.quill.client

import kotlin.math.roundToInt

/**
 * The arithmetic that turns a panel, a rotation and a scale into the monitor
 * the daemon is asked for -- and the one comparison that decides whether a
 * changed surface is worth renegotiating.
 *
 * This lives outside [MainActivity] for one reason: it is the logic that broke.
 * A rotation setting is defined relative to the panel, so re-deriving the panel
 * mid-session made the two compound and put the desktop on its side. That was
 * found by a person looking at a tablet, which is an expensive way to find it,
 * and it could not be reproduced on the hardware afterwards -- on this device
 * the window will not turn while the app is in front.
 *
 * None of it needs Android. Keeping it as plain functions over Ints means the
 * cases hardware cannot reach are still covered, in a test that runs in a
 * second rather than in a session with five cable replugs.
 */
object PanelGeometry {

    /** A monitor request, in pixels: what the handshake asks the daemon for. */
    data class MonitorSize(val widthPx: Int, val heightPx: Int)

    /**
     * True at the quarter turns, where the monitor is the panel transposed.
     *
     * 180 does not swap: it is the same rectangle the other way up, which the
     * daemon handles by turning the picture rather than by being asked for a
     * different shape.
     */
    fun swapsAxes(rotationDegrees: Int): Boolean =
        rotationDegrees == 90 || rotationDegrees == 270

    /**
     * What to ask the daemon for, given the panel and the user's settings.
     *
     * At 90 or 270 the dimensions are swapped, so a landscape tablet drives a
     * portrait desktop that the encoder then turns to fill the panel exactly.
     * Asking for the panel's own shape instead would letterbox a rotated image
     * into it and waste most of the screen.
     *
     * The scale applies to both axes equally, so the aspect never changes and
     * nothing is ever letterboxed -- which is also what keeps the input mapping
     * a single multiply. Scaling happens before the swap; since both axes take
     * the same factor the two orders agree, and doing it first keeps the
     * rounding in one place.
     */
    fun monitorSize(
        panelWidthPx: Int,
        panelHeightPx: Int,
        rotationDegrees: Int,
        workspaceScalePercent: Int,
    ): MonitorSize {
        val scale = workspaceScalePercent / 100f
        val scaledWidth = (panelWidthPx * scale).roundToInt()
        val scaledHeight = (panelHeightPx * scale).roundToInt()
        return if (swapsAxes(rotationDegrees)) {
            MonitorSize(scaledHeight, scaledWidth)
        } else {
            MonitorSize(scaledWidth, scaledHeight)
        }
    }

    /**
     * Whether a surface of [surfaceWidthPx] x [surfaceHeightPx] is still the
     * same panel the session negotiated against -- either as it was, or turned
     * a quarter.
     *
     * Compared as an unordered pair, because a panel turned a quarter is the
     * same two numbers the other way round. That case must *not* renegotiate:
     * the rotation setting is relative to the panel, so re-deriving it after a
     * turn applies the swap to an already-swapped panel and the desktop arrives
     * sideways.
     *
     * False means the window no longer owns the whole display -- split-screen
     * or freeform -- which is a different situation entirely.
     */
    fun isSamePanel(
        surfaceWidthPx: Int,
        surfaceHeightPx: Int,
        panelWidthPx: Int,
        panelHeightPx: Int,
    ): Boolean =
        (surfaceWidthPx == panelWidthPx && surfaceHeightPx == panelHeightPx) ||
            (surfaceWidthPx == panelHeightPx && surfaceHeightPx == panelWidthPx)

    /**
     * The rotation the next handshake should actually ask for.
     *
     * Normally the user's setting. The exception is a daemon that has already
     * demonstrated it does not understand that particular quarter turn, in
     * which case this session stops asking for it -- without touching what the
     * user saved. [rejectedRotation] is a fact about this connection, not a
     * preference, and it is deliberately matched by value: picking a *different*
     * rotation is a fresh request and gets tried.
     */
    fun effectiveRotation(settingDegrees: Int, rejectedRotation: Int?): Int =
        if (rejectedRotation != null && settingDegrees == rejectedRotation) 0 else settingDegrees
}
