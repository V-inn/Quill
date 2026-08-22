package com.quill.client

/**
 * The settings you are *proposing*, as opposed to the ones in force.
 *
 * Only the three the daemon acts on live here. Everything else on the screen
 * takes effect immediately and has nothing to stage.
 *
 * Held in the screen rather than written straight to SharedPreferences on every
 * toggle, which is what the old screen did -- and which made any "Apply" button
 * a lie, since backing out still left the change to be picked up by the next
 * reconnect.
 */
data class SettingsDraft(
    val clientSideCursor: Boolean,
    val ctrlScrollZoom: Boolean,
    val flip180: Boolean,
) {
    /** Whether this differs from what is on disk, i.e. is there anything to save. */
    fun isDirty(settings: Settings): Boolean =
        clientSideCursor != settings.clientSideCursor ||
            ctrlScrollZoom != settings.ctrlScrollZoom ||
            flip180 != settings.flip180

    fun commit(settings: Settings) {
        settings.clientSideCursor = clientSideCursor
        settings.ctrlScrollZoom = ctrlScrollZoom
        settings.flip180 = flip180
    }

    companion object {
        fun from(settings: Settings) = SettingsDraft(
            clientSideCursor = settings.clientSideCursor,
            ctrlScrollZoom = settings.ctrlScrollZoom,
            flip180 = settings.flip180,
        )
    }
}
