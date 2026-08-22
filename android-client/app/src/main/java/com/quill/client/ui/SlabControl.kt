package com.quill.client.ui

import android.graphics.Bitmap
import androidx.compose.animation.core.Animatable
import androidx.compose.foundation.gestures.detectDragGestures
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.text.BasicText
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.graphics.drawscope.clipRect
import androidx.compose.ui.graphics.drawscope.rotate
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.dp
import kotlin.math.abs
import kotlin.math.atan2
import kotlin.math.roundToInt
import kotlinx.coroutines.launch

/**
 * The tablet, drawn to scale, with the desktop actually on it.
 *
 * This replaces a switch labelled "Rotate the image 180°". That switch could
 * only ever describe the thing in a caption, because the setting exists for a
 * reason no label can show: which end of the tablet the USB cable enters --
 * "which the person holding the device knows and the aspect ratio does not"
 * (Milestone 24). A picture of the slab with its cable can show it.
 *
 * **What rotates.** The chassis and its cable stub turn as one rigid body; the
 * desktop inside stays upright. So you turn it until the stub points at where
 * your cable physically is, and what you are looking at is the answer: *with the
 * cable there, this is what you will see, the right way up.* Nothing here needs
 * to know which physical edge the port is on, which is the one fact only the
 * person holding it has.
 *
 * **The picture is the brightest thing on the screen**, and everything around it
 * stays muted graphite. That is deliberate: this app exists to be a window onto
 * that desktop, and the person reading this screen is mid-drawing with their
 * colour perception already committed to their own work.
 *
 * The captured frame arrives *already* rotated when the running session has the
 * flip on -- the daemon applies it GPU-side, so a capture of the panel is a
 * capture of the rotated picture. [sessionFlip180] is what un-rotates it back to
 * an upright desktop; see `CursorOverlay` for the same correction applied to the
 * pointer, and note that these two are the only places that need to know it.
 */
@Composable
fun SlabControl(
    frame: Bitmap?,
    panelAspect: Float,
    flip180: Boolean,
    sessionFlip180: Boolean,
    sessionLive: Boolean,
    onFlip180: (Boolean) -> Unit,
    staged: Boolean,
    modifier: Modifier = Modifier,
) {
    val image: ImageBitmap? = remember(frame) {
        frame?.takeIf { !it.isRecycled }?.asImageBitmap()
    }

    // Chassis angle. 0 = cable at the bottom, 180 = cable at the top.
    val angle = remember { Animatable(if (flip180) 180f else 0f) }
    val scope = rememberCoroutineScope()
    val reduceMotion = LocalAnimationScale.current == 0f
    val currentFlip by rememberUpdatedState(flip180)

    // Follows the value when it changes from outside -- a Discard, or the draft
    // being restored after a process death.
    LaunchedEffect(flip180) {
        val target = if (flip180) 180f else 0f
        if (abs(angle.value - target) > 0.5f) {
            if (reduceMotion) angle.snapTo(target) else angle.animateTo(target, springSpec())
        }
    }

    val settle: (Float) -> Unit = { raw ->
        // Nearest half turn wins.
        val flipped = ((raw % 360f) + 360f) % 360f in 90f..270f
        scope.launch {
            val target = if (flipped) 180f else 0f
            if (reduceMotion) angle.snapTo(target) else angle.animateTo(target, springSpec())
        }
        if (flipped != currentFlip) onFlip180(flipped)
    }

    Column(
        modifier = modifier,
        verticalArrangement = Arrangement.spacedBy(QuillTokens.SpaceMd),
    ) {
        Box(
            Modifier
                .fillMaxWidth()
                .aspectRatio(panelAspect.coerceIn(0.4f, 2.5f))
                // Tap is the primary interaction and the only one TalkBack can
                // drive: a rotational drag is not operable without sight.
                .pointerInput(Unit) {
                    detectTapGestures {
                        val next = !currentFlip
                        scope.launch {
                            val target = if (next) 180f else 0f
                            if (reduceMotion) angle.snapTo(target) else angle.animateTo(target, springSpec())
                        }
                        onFlip180(next)
                    }
                }
                .pointerInput(Unit) {
                    var start = 0f
                    var base = 0f
                    detectDragGestures(
                        onDragStart = { offset ->
                            val c = Offset(size.width / 2f, size.height / 2f)
                            start = angleOf(offset - c)
                            base = angle.value
                        },
                        onDragEnd = { settle(angle.value) },
                        onDragCancel = { settle(angle.value) },
                    ) { change, _ ->
                        val c = Offset(size.width / 2f, size.height / 2f)
                        val now = angleOf(change.position - c)
                        scope.launch { angle.snapTo(base + (now - start)) }
                    }
                }
                .semantics {
                    role = Role.Button
                    contentDescription = "Tablet orientation"
                    stateDescription =
                        if (flip180) "Cable at the top of the tablet"
                        else "Cable at the bottom of the tablet"
                },
        ) {
            SlabCanvas(image, angle.value, sessionFlip180)
        }

        Readout(
            flip180 = flip180,
            sessionLive = sessionLive,
            hasPicture = image != null,
            staged = staged,
        )
    }
}

@Composable
private fun SlabCanvas(image: ImageBitmap?, angleDegrees: Float, sessionFlip180: Boolean) {
    // The Box around this already carries the panel's aspect ratio.
    androidx.compose.foundation.Canvas(Modifier.fillMaxSize()) {
        drawSlab(image, angleDegrees, sessionFlip180)
    }
}

private fun DrawScope.drawSlab(image: ImageBitmap?, angleDegrees: Float, sessionFlip180: Boolean) {
    val chassisCorner = 14.dp.toPx()
    val bezel = size.minDimension * 0.055f
    val centre = Offset(size.width / 2f, size.height / 2f)
    val screen = Rect(
        left = bezel,
        top = bezel,
        right = size.width - bezel,
        bottom = size.height - bezel,
    )

    rotate(degrees = angleDegrees, pivot = centre) {
        // Chassis. Raised, so the screen cut into it reads as a recess.
        drawRoundRect(
            color = QuillTokens.Raise,
            size = size,
            cornerRadius = androidx.compose.ui.geometry.CornerRadius(chassisCorner),
        )
        drawRoundRect(
            color = QuillTokens.Copper.copy(alpha = 0.3f),
            size = size,
            cornerRadius = androidx.compose.ui.geometry.CornerRadius(chassisCorner),
            style = androidx.compose.ui.graphics.drawscope.Stroke(width = 1.dp.toPx()),
        )

        // The screen itself: darker than everything around it, because it is a
        // window rather than a panel sitting on top of one.
        drawRect(color = QuillTokens.Recess, topLeft = screen.topLeft, size = screen.size)

        if (image != null) {
            clipRect(screen.left, screen.top, screen.right, screen.bottom) {
                // Counter-rotate by the chassis angle so the desktop stays
                // upright while the body turns, and by another half turn when
                // the running session's flip means the captured pixels are
                // already upside down.
                val correction = -angleDegrees + if (sessionFlip180) 180f else 0f
                rotate(degrees = correction, pivot = centre) {
                    drawImage(
                        image = image,
                        srcOffset = IntOffset.Zero,
                        srcSize = IntSize(image.width, image.height),
                        dstOffset = IntOffset(screen.left.roundToInt(), screen.top.roundToInt()),
                        dstSize = IntSize(screen.width.roundToInt(), screen.height.roundToInt()),
                    )
                }
            }
        }

        // The cable. The whole reason this control exists, so it is drawn as a
        // real stub straddling the edge rather than an icon near it.
        val stubWidth = size.minDimension * 0.11f
        val stubHeight = 7.dp.toPx()
        val stubTop = size.height - stubHeight / 2f
        drawRoundRect(
            color = QuillTokens.Copper,
            topLeft = Offset(centre.x - stubWidth / 2f, stubTop),
            size = Size(stubWidth, stubHeight * 2f),
            cornerRadius = androidx.compose.ui.geometry.CornerRadius(stubHeight),
        )
    }
}

@Composable
private fun Readout(
    flip180: Boolean,
    sessionLive: Boolean,
    hasPicture: Boolean,
    staged: Boolean,
) {
    Column(verticalArrangement = Arrangement.spacedBy(QuillTokens.SpaceXs)) {
        // Staged is carried by the same copper the switch rows use, rather than
        // by a sentence underneath. The slab is already showing the proposed
        // arrangement; it only has to say that it is a proposal.
        BasicText(
            text = buildString {
                append(if (flip180) "Cable at top · 180°" else "Cable at bottom · 0°")
                if (staged) append(" · staged")
            }.uppercase(),
            style = QuillType.eyebrow.copy(
                color = if (staged) QuillTokens.CopperLit else QuillTokens.Graphite,
            ),
        )
        BasicText(
            text = when {
                hasPicture -> "Turn it until the cable matches yours. The desktop stays upright."
                sessionLive -> "Preview unavailable. The diagram still works."
                else -> "No picture yet. Connect the cable to see the desktop here."
            },
            style = QuillType.caption.copy(color = QuillTokens.Muted),
        )
    }
}

private fun angleOf(v: Offset): Float =
    Math.toDegrees(atan2(v.y.toDouble(), v.x.toDouble())).toFloat()

private fun <T> springSpec() = androidx.compose.animation.core.spring<T>(
    dampingRatio = 0.7f,
    stiffness = 300f,
)
