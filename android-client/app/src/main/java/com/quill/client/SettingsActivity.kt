package com.quill.client

import android.graphics.drawable.ColorDrawable
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.graphics.toArgb
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

        val settings = Settings(this)

        setContent {
            // Write-through, as before: each toggle persists immediately and
            // takes effect on the reconnect that leaving this screen causes.
            // The staged-draft model that makes "Apply" mean something replaces
            // this next, in its own change.
            var clientSideCursor by remember { mutableStateOf(settings.clientSideCursor) }
            var ctrlScrollZoom by remember { mutableStateOf(settings.ctrlScrollZoom) }
            var flip180 by remember { mutableStateOf(settings.flip180) }
            var keepScreenAwake by remember { mutableStateOf(settings.keepScreenAwake) }
            var showLatencyOverlay by remember { mutableStateOf(settings.showLatencyOverlay) }

            SettingsScreen(
                SettingsScreenState(
                    clientSideCursor = clientSideCursor,
                    ctrlScrollZoom = ctrlScrollZoom,
                    flip180 = flip180,
                    keepScreenAwake = keepScreenAwake,
                    showLatencyOverlay = showLatencyOverlay,
                    onClientSideCursor = {
                        clientSideCursor = it
                        settings.clientSideCursor = it
                    },
                    onCtrlScrollZoom = {
                        ctrlScrollZoom = it
                        settings.ctrlScrollZoom = it
                    },
                    onFlip180 = {
                        flip180 = it
                        settings.flip180 = it
                    },
                    onKeepScreenAwake = {
                        keepScreenAwake = it
                        settings.keepScreenAwake = it
                    },
                    onShowLatencyOverlay = {
                        showLatencyOverlay = it
                        settings.showLatencyOverlay = it
                    },
                    onClose = { finish() },
                )
            )
        }
    }
}
