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
    val rotationDegrees: Int,
    val workspaceScalePercent: Int,
    val cap30Fps: Boolean,
    val quality: Int,
) {
    /** Whether this differs from what is on disk, i.e. is there anything to save. */
    fun isDirty(settings: Settings): Boolean =
        clientSideCursor != settings.clientSideCursor ||
            ctrlScrollZoom != settings.ctrlScrollZoom ||
            rotationDegrees != settings.rotationDegrees ||
            workspaceScalePercent != settings.workspaceScalePercent ||
            cap30Fps != settings.cap30Fps ||
            quality != settings.quality

    fun commit(settings: Settings) {
        settings.clientSideCursor = clientSideCursor
        settings.ctrlScrollZoom = ctrlScrollZoom
        settings.rotationDegrees = rotationDegrees
        settings.workspaceScalePercent = workspaceScalePercent
        settings.cap30Fps = cap30Fps
        settings.quality = quality
    }

    /**
     * Which controls differ from the session that is actually running.
     *
     * "Differs from what is running", not "differs from what is saved" -- the
     * two are different facts and only the first one is worth a mark on screen.
     * With no session yet ([session] null) nothing can be out of step with one,
     * so nothing is marked: that is a real state, not a missing one.
     *
     * Lives here rather than inline in the settings screen so it can be tested.
     * It is the arithmetic behind every staged mark and the "N changes staged"
     * line, and it was previously only ever checked by a person reading a
     * screenshot of a footer.
     */
    fun stagedAgainst(session: SessionConfig.Snapshot?): Staged {
        if (session == null) return Staged()
        return Staged(
            clientSideCursor = clientSideCursor != session.clientSideCursor,
            ctrlScrollZoom = ctrlScrollZoom != session.ctrlScrollZoom,
            rotation = rotationDegrees != session.rotationDegrees,
            workspace = workspaceScalePercent != session.workspaceScalePercent,
            cap30Fps = cap30Fps != session.cap30Fps,
            quality = quality != session.quality,
        )
    }

    /** One flag per stageable control; all false when nothing is running. */
    data class Staged(
        val clientSideCursor: Boolean = false,
        val ctrlScrollZoom: Boolean = false,
        val rotation: Boolean = false,
        val workspace: Boolean = false,
        val cap30Fps: Boolean = false,
        val quality: Boolean = false,
    ) {
        val count: Int
            get() = listOf(
                clientSideCursor, ctrlScrollZoom, rotation, workspace, cap30Fps, quality,
            ).count { it }

        val any: Boolean get() = count > 0
    }

    companion object {
        fun from(settings: Settings) = SettingsDraft(
            clientSideCursor = settings.clientSideCursor,
            ctrlScrollZoom = settings.ctrlScrollZoom,
            rotationDegrees = settings.rotationDegrees,
            workspaceScalePercent = settings.workspaceScalePercent,
            cap30Fps = settings.cap30Fps,
            quality = settings.quality,
        )
    }
}
