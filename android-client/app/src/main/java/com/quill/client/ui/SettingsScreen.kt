package com.quill.client.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.text.BasicText
import androidx.compose.foundation.verticalScroll
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.unit.dp

/**
 * The settings screen.
 *
 * **Two panes above [QuillTokens.TwoPaneMinWidth], one column below.** This app
 * is landscape-locked on a 2560x1600 panel, so a phone-style single scrolling
 * column would be the wrong shape entirely -- a 40-character measure down the
 * middle of a very wide screen. The breakpoint reads the window's own width
 * rather than `Configuration.orientation`, so split-screen, freeform and an
 * actual phone all fall out of it.
 *
 * **Sections are grouped by when they take effect, not by topic.** Every
 * setting the daemon acts on is deferred: it picks its capture session type
 * before the first frame, so nothing can change mid-session. The old screen
 * carried that as a grey paragraph at the top while its switches snapped on and
 * implied "done". Splitting "this session" from "this tablet" makes the
 * distinction structural, and means the staged mark has exactly one place it
 * can appear.
 */
@Composable
fun SettingsScreen(state: SettingsScreenState) {
    QuillTheme(sessionLive = state.sessionLive) {
        BoxWithConstraints(
            Modifier
                .fillMaxSize()
                .background(QuillTokens.Slate)
        ) {
            val twoPane = maxWidth >= QuillTokens.TwoPaneMinWidth
            // Edge-to-edge immersive (see SettingsActivity.hideSystemBars), so
            // nothing lays itself out under the notch or under a system bar an
            // edge swipe has temporarily brought back.
            Column(
                Modifier
                    .fillMaxSize()
                    .windowInsetsPadding(WindowInsets.safeDrawing)
            ) {
                Header(state, Modifier.padding(QuillTokens.SpaceLg))
                Rule()
                Box(Modifier.weight(1f)) {
                    if (twoPane) TwoPane(state) else SinglePane(state)
                }
                Rule()
                ActionBar(state, Modifier.padding(QuillTokens.SpaceLg))
            }
        }
    }
}

@Composable
private fun Header(state: SettingsScreenState, modifier: Modifier = Modifier) {
    Row(
        modifier.fillMaxWidth(),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Eyebrow("Quill · Settings", Modifier.weight(1f))
        ConnectionChip(state)
    }
}

/**
 * Says whether there is a desktop on the panel right now, and at what size.
 *
 * Not decoration: it is the baseline the staged marks are measured against. If
 * nothing has ever connected this process, no control can be "different from
 * what is running", and the chip is what explains why nothing is marked.
 */
@Composable
private fun ConnectionChip(state: SettingsScreenState) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(QuillTokens.SpaceSm),
    ) {
        Box(
            Modifier
                .size(7.dp)
                .clip(CircleShape)
                .background(if (state.sessionLive) QuillTokens.Copper else QuillTokens.Muted)
        )
        BasicText(
            text = state.connectionLabel,
            style = QuillType.data.copy(
                color = if (state.sessionLive) QuillTokens.Chalk else QuillTokens.Muted,
            ),
        )
    }
}

@Composable
private fun TwoPane(state: SettingsScreenState) {
    Row(Modifier.fillMaxSize()) {
        Column(
            Modifier
                .weight(0.4f)
                .widthIn(min = 320.dp)
                .fillMaxHeight()
                .verticalScroll(rememberScrollState())
                .padding(QuillTokens.SpaceLg),
            verticalArrangement = Arrangement.spacedBy(QuillTokens.SpaceMd),
        ) {
            DisplaySection(state)
        }
        Box(
            Modifier
                .padding(vertical = QuillTokens.SpaceLg)
                .width(1.dp)
                .fillMaxHeight()
                .background(QuillTokens.Rule)
        )
        Column(
            Modifier
                .weight(0.6f)
                .fillMaxHeight()
                .verticalScroll(rememberScrollState())
                .padding(QuillTokens.SpaceLg),
            verticalArrangement = Arrangement.spacedBy(QuillTokens.SpaceXl),
        ) {
            SessionSection(state)
            TabletSection(state)
            Box(Modifier.height(QuillTokens.SpaceLg))
        }
    }
}

@Composable
private fun SinglePane(state: SettingsScreenState) {
    Column(
        Modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(QuillTokens.SpaceLg),
        verticalArrangement = Arrangement.spacedBy(QuillTokens.SpaceXl),
    ) {
        DisplaySection(state)
        SessionSection(state)
        TabletSection(state)
        Box(Modifier.height(QuillTokens.SpaceLg))
    }
}

/**
 * The display half. For now this is the 180-degree flip as a switch, exactly as
 * before -- the slab that replaces it lands in its own change, once the frame
 * capture it draws inside is proven on hardware.
 */
@Composable
private fun DisplaySection(state: SettingsScreenState) {
    Section(
        eyebrow = "Display",
        note = "Takes effect the next time the tablet connects.",
    ) {
        SwitchRow(
            label = "Rotate the image 180°",
            checked = state.flip180,
            onCheckedChange = state.onFlip180,
            staged = state.flip180Staged,
            note = "Which way up the desktop belongs depends on which end of the " +
                "tablet the cable enters, and only you can see that. Touch and pen " +
                "coordinates rotate with the image, so they keep lining up either way.",
        )
    }
}

@Composable
private fun SessionSection(state: SettingsScreenState) {
    Section(
        eyebrow = "This session",
        note = "Takes effect the next time the tablet connects.",
    ) {
        SwitchRow(
            label = "Draw the pointer on the tablet",
            checked = state.clientSideCursor,
            onCheckedChange = state.onClientSideCursor,
            staged = state.clientSideCursorStaged,
            note = "Lower latency: the pointer moves with your hand instead of " +
                "waiting for a frame. Pen ink is unaffected either way — it is drawn " +
                "by the application on the desktop and comes back with the video.",
        )
        if (state.clientSideCursor) {
            Note(
                "On KDE you will see two pointers. KWin composites its own into the " +
                    "video and cannot be told not to.",
                Modifier.padding(horizontal = QuillTokens.SpaceMd),
            )
        }
        SwitchRow(
            label = "Zoom with Ctrl+scroll",
            checked = state.ctrlScrollZoom,
            onCheckedChange = state.onCtrlScrollZoom,
            staged = state.ctrlScrollZoomStaged,
            note = if (state.ctrlScrollZoom) {
                "On: the daemon recognises the pinch and sends Ctrl+scroll, which " +
                    "nearly every application honours — but zoom steps instead of gliding."
            } else {
                "Off: sends a real pinch. Firefox and GTK applications zoom from it; " +
                    "Krita and anything else on XWayland ignores it entirely."
            },
        )
        Note(
            "Two-finger scroll, swipes and two-finger tap are recognised by Linux " +
                "itself, from a virtual touchpad the daemon creates. Configure them in " +
                "System Settings → Touchpad, like any real trackpad. One finger points " +
                "where you touch, and holding one finger still is a right click.",
            Modifier.padding(horizontal = QuillTokens.SpaceMd),
        )
    }
}

@Composable
private fun TabletSection(state: SettingsScreenState) {
    Section(
        eyebrow = "This tablet",
        note = "Takes effect right away.",
    ) {
        SwitchRow(
            label = "Keep the screen awake",
            checked = state.keepScreenAwake,
            onCheckedChange = state.onKeepScreenAwake,
            note = "While a desktop is showing. The tablet still sleeps when nothing " +
                "is connected.",
        )
        SwitchRow(
            label = "Show latency overlay",
            checked = state.showLatencyOverlay,
            onCheckedChange = state.onShowLatencyOverlay,
            note = "Draws the running per-frame latency over the video.",
        )
    }
}

@Composable
private fun Section(
    eyebrow: String,
    note: String,
    content: @Composable () -> Unit,
) {
    Column(verticalArrangement = Arrangement.spacedBy(QuillTokens.SpaceSm)) {
        Eyebrow(eyebrow)
        SectionNote(note, Modifier.padding(bottom = QuillTokens.SpaceSm))
        content()
    }
}

/**
 * One action, whose label names what actually happens.
 *
 * Both exits reconnect, because leaving this screen destroys the video surface
 * either way -- so "Close" would be a quieter lie than the switches this
 * redesign set out to fix. Say it in both labels instead.
 */
@Composable
private fun ActionBar(state: SettingsScreenState, modifier: Modifier = Modifier) {
    Row(
        modifier.fillMaxWidth(),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(QuillTokens.SpaceMd),
    ) {
        BasicText(
            text = state.stagedSummary,
            modifier = Modifier.weight(1f),
            style = QuillType.data.copy(
                color = if (state.stagedCount > 0) QuillTokens.CopperLit else QuillTokens.Muted,
            ),
        )
        PrimaryButton(
            text = if (state.stagedCount > 0) "Apply and reconnect" else "Close",
            onClick = state.onClose,
        )
    }
}
