package com.quill.client

import android.graphics.drawable.ColorDrawable
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.BackHandler
import androidx.activity.compose.setContent
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.graphics.toArgb
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import com.quill.client.ui.QuillTokens
import com.quill.client.ui.SettingsScreen
import com.quill.client.ui.SettingsScreenState

/**
 * Settings, reached from the gear while streaming, or from the status overlay
 * before the first frame (the app runs edge-to-edge immersive as a monitor
 * replacement, so the video surface itself is the only affordance).
 *
 * Built in Compose. That is not a departure from this app's "everything in
 * code, no layout resources" habit -- it is the strongest form of it. There is
 * still no `res/layout`, and the only resource directory that exists is
 * `res/font`, which holds the three faces this screen is set in.
 *
 * `material3` is deliberately absent: this design replaces every default it
 * would have supplied, so it would only have been fought at every control. See
 * `ui/Controls.kt` for the switch, press feedback and focus ring built in its
 * place.
 *
 * **Leaving here reconnects, whatever you pressed.** Opening this activity
 * destroys the video surface, so returning restarts the decode loop with a
 * fresh handshake -- which is exactly what makes a changed setting take effect,
 * since the daemon fixes its capture session before the first frame. The action
 * bar says so rather than pretending otherwise.
 */
class SettingsActivity : ComponentActivity() {

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        // The activity theme is Theme.Black.NoTitleBar, whose window background
        // is black -- which would flash before the first Compose frame paints
        // over it in graphite.
        window.setBackgroundDrawable(ColorDrawable(QuillTokens.Slate.toArgb()))
        hideSystemBars()

        val settings = Settings(this)

        setContent {
            // Session settings are *staged*: held here and written only on
            // Apply. The old screen wrote every toggle through on the spot,
            // which made any Apply button a lie -- backing out still left the
            // change for the next reconnect to pick up.
            //
            // rememberSaveable so a rotation or a process death does not
            // silently drop what you were half-way through choosing.
            var clientSideCursor by rememberSaveable { mutableStateOf(settings.clientSideCursor) }
            var ctrlScrollZoom by rememberSaveable { mutableStateOf(settings.ctrlScrollZoom) }
            var flip180 by rememberSaveable { mutableStateOf(settings.flip180) }

            // Tablet settings are not staged -- there is nothing to stage
            // against, since they take effect the moment you leave. They keep
            // writing through, and can never show a staged mark.
            var keepScreenAwake by rememberSaveable { mutableStateOf(settings.keepScreenAwake) }
            var showLatencyOverlay by rememberSaveable { mutableStateOf(settings.showLatencyOverlay) }

            val draft = SettingsDraft(clientSideCursor, ctrlScrollZoom, flip180)
            val session = SessionConfig.applied

            val apply = {
                draft.commit(settings)
                finish()
            }
            val discard = { finish() }

            // The system back button means "leave", and leaving without
            // applying is discarding -- so it does exactly what the Discard
            // button does rather than some third thing.
            BackHandler(onBack = discard)

            SettingsScreen(
                SettingsScreenState(
                    clientSideCursor = clientSideCursor,
                    ctrlScrollZoom = ctrlScrollZoom,
                    flip180 = flip180,
                    keepScreenAwake = keepScreenAwake,
                    showLatencyOverlay = showLatencyOverlay,

                    // "Differs from what is running", not "differs from what is
                    // saved". With no session yet, nothing can be out of step
                    // with one, so nothing is marked.
                    clientSideCursorStaged = session != null && clientSideCursor != session.clientSideCursor,
                    ctrlScrollZoomStaged = session != null && ctrlScrollZoom != session.ctrlScrollZoom,
                    flip180Staged = session != null && flip180 != session.flip180,

                    sessionLive = session != null,
                    previewFrame = FramePreview.peek(),
                    panelAspect = FramePreview.panelAspect(),
                    sessionFlip180 = session?.flip180 ?: flip180,
                    connectionLabel = session
                        ?.let { "${it.widthPx} × ${it.heightPx}" }
                        ?: "Not connected",

                    onClientSideCursor = { clientSideCursor = it },
                    onCtrlScrollZoom = { ctrlScrollZoom = it },
                    onFlip180 = { flip180 = it },
                    onKeepScreenAwake = {
                        keepScreenAwake = it
                        settings.keepScreenAwake = it
                    },
                    onShowLatencyOverlay = {
                        showLatencyOverlay = it
                        settings.showLatencyOverlay = it
                    },
                    onApply = apply,
                    onDiscard = discard,
                )
            )
        }
    }

    override fun onDestroy() {
        super.onDestroy()
        // The captured frame exists for this screen and nothing else.
        FramePreview.clear()
    }

    /** Same edge-to-edge immersive treatment `MainActivity` uses.
     *
     * Without it this screen came up with Samsung's status bar and navigation
     * dock over it -- jarring next to a main activity that has no system chrome
     * at all, and the nav bar sat on top of the action row. `SettingsScreen`
     * still pads for the bars, so a transient reveal never covers anything. */
    private fun hideSystemBars() {
        WindowCompat.setDecorFitsSystemWindows(window, false)
        val controller = WindowInsetsControllerCompat(window, window.decorView)
        controller.hide(WindowInsetsCompat.Type.systemBars())
        controller.systemBarsBehavior =
            WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
    }

    /** The bars Android brings back on an edge swipe don't hide again on their
     * own once the transient reveal times out -- re-assert on focus regain, the
     * standard sticky-immersive pattern (and what `MainActivity` does). */
    override fun onWindowFocusChanged(hasFocus: Boolean) {
        super.onWindowFocusChanged(hasFocus)
        if (hasFocus) hideSystemBars()
    }
}
