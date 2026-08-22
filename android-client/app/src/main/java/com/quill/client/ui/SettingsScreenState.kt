package com.quill.client.ui

/**
 * Everything [SettingsScreen] draws and everything it can do, in one value.
 *
 * Deliberately dumb: no `Settings` instance, no `Activity`, nothing that
 * outlives a composition. It exists so the screen can be previewed and reasoned
 * about without a tablet attached, and so that the *next* change -- which
 * replaces write-through with a staged draft -- happens in one place instead of
 * across every control.
 *
 * The `*Staged` flags are wired but always false at this point: knowing whether
 * a control differs from what the running session is using needs a snapshot
 * taken at handshake time, which lands with the draft model.
 */
data class SettingsScreenState(
    val clientSideCursor: Boolean,
    val ctrlScrollZoom: Boolean,
    val rotationDegrees: Int,
    val keepScreenAwake: Boolean,
    val showLatencyOverlay: Boolean,

    val clientSideCursorStaged: Boolean = false,
    val ctrlScrollZoomStaged: Boolean = false,
    val rotationStaged: Boolean = false,

    /** True once a desktop has actually rendered on this tablet. */
    val sessionLive: Boolean = false,

    /** `2560 × 1600`, or why there is no picture. */
    val connectionLabel: String = "Not connected",

    /** Last frame of the real desktop, for the slab. Null before the first
     * connection, or when the capture did not land in time. */
    val previewFrame: android.graphics.Bitmap? = null,

    /** Panel width / height, for drawing the slab at the true shape. */
    val panelAspect: Float = 1.6f,

    /** How far the *running* session turns the picture, which is what decides
     * how the captured pixels are already oriented. */
    val sessionRotationDegrees: Int = 0,

    val onClientSideCursor: (Boolean) -> Unit,
    val onCtrlScrollZoom: (Boolean) -> Unit,
    val onRotation: (Int) -> Unit,
    val onKeepScreenAwake: (Boolean) -> Unit,
    val onShowLatencyOverlay: (Boolean) -> Unit,

    /** Saves the staged settings, then leaves -- which is what applies them. */
    val onApply: () -> Unit,

    /** Leaves without saving. Still reconnects; see [secondaryLabel]. */
    val onDiscard: () -> Unit,
) {
    val stagedCount: Int
        get() = listOf(clientSideCursorStaged, ctrlScrollZoomStaged, rotationStaged).count { it }

    val hasStagedChanges: Boolean get() = stagedCount > 0

    /**
     * The line beside the actions. Names what is pending, or -- when nothing is
     * -- what the screen would otherwise quietly get wrong: there is no free
     * exit here. Leaving destroys the video surface either way, so both routes
     * out cost a reconnect.
     */
    val stagedSummary: String
        get() = when (stagedCount) {
            0 -> "Nothing staged. Leaving reconnects either way."
            1 -> "1 change staged"
            else -> "$stagedCount changes staged"
        }

    val primaryLabel: String
        get() = if (hasStagedChanges) "Apply and reconnect" else "Reconnect"

    /**
     * Null when there is nothing to discard -- a "Discard" next to an unchanged
     * screen is a button that does the same thing as the one beside it.
     */
    val secondaryLabel: String?
        get() = if (hasStagedChanges) "Discard and reconnect" else null
}
