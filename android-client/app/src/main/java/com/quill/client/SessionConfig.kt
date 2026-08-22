package com.quill.client

/**
 * What the *running* session was actually started with.
 *
 * The settings screen needs this to tell a staged change from a settled one.
 * Every setting the daemon acts on is fixed at connect time -- it chooses its
 * capture session type before the first frame -- so a toggle flipped now and a
 * toggle that is already in force look identical in SharedPreferences and are
 * completely different facts about the machine.
 *
 * Written once per handshake, from `MainActivity.sendHandshake`, which is the
 * exact moment these values stop being a preference and start being what the
 * daemon is doing.
 *
 * `applied` is null until the first handshake of this process. That is a real
 * state, not a missing one: with nothing running, no control can differ from
 * what is running, so nothing is marked staged and the screen says so.
 */
object SessionConfig {

    @Volatile
    var applied: Snapshot? = null
        private set

    /**
     * The values, not the packed byte. The screen diffs per control, and
     * unpacking bits back into booleans in the UI layer would put a second copy
     * of the wire format somewhere it has no business being.
     */
    data class Snapshot(
        val clientSideCursor: Boolean,
        val ctrlScrollZoom: Boolean,
        val flip180: Boolean,
        val widthPx: Int,
        val heightPx: Int,
    )

    fun record(settings: Settings, widthPx: Int, heightPx: Int) {
        applied = Snapshot(
            clientSideCursor = settings.clientSideCursor,
            ctrlScrollZoom = settings.ctrlScrollZoom,
            flip180 = settings.flip180,
            widthPx = widthPx,
            heightPx = heightPx,
        )
    }

    /**
     * Deliberately *not* cleared when the connection drops.
     *
     * A reconnect is in progress for a second or two after leaving settings,
     * and during it the last known session is a far more useful baseline than
     * "nothing is running" -- which would blank every staged mark at exactly
     * the moment the user is looking at them.
     */
    fun forget() {
        applied = null
    }
}
