package com.quill.client.ui

import android.graphics.Bitmap
import androidx.compose.animation.core.Animatable
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.gestures.detectDragGestures
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.text.BasicText
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.graphics.drawscope.Stroke
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
 * This replaces a switch labelled "Rotate the image 180°", which could only ever
 * describe the result in a caption. Here you turn the picture and look at it.
 *
 * **What rotates is the picture and the handle, not the body.** An earlier
 * version turned the chassis instead and left the picture upright, explaining
 * itself in terms of which end the USB cable enters. The cable framing confused
 * more than it helped; rotating the contents is both what people expect and the
 * thing actually being configured.
 *
 * The copper nub on the edge is the handle: grab it and swing it round. It
 * travels with the picture, so it doubles as the mark that says which way up
 * the display currently is -- without it, a rectangle at 0 and at 180 degrees
 * look identical and the control would appear to do nothing. It is deliberately
 * unlabelled. It marks an edge and gives you something to take hold of, and
 * needs no more explanation than that.
 *
 * **What the picture shows is the result, not the present.** The captured frame
 * is whatever is on the panel right now, already rotated if the *running*
 * session has the flip on -- the daemon applies it GPU-side, so a capture of the
 * panel is a capture of the rotated picture. So the slab rotates by the
 * *difference* between the draft and the session: matching means "this is what
 * you have", differing means "this is what you will get". Either way what you
 * are looking at is the answer.
 *
 * **The picture is the brightest thing on the screen**, and everything around it
 * stays muted graphite. This app exists to be a window onto that desktop, and
 * whoever is reading this screen is mid-drawing with their colour perception
 * already committed to their own work.
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

    // Rotation of the picture relative to the frame as captured.
    fun restingAngle(flip: Boolean) = if (flip != sessionFlip180) 180f else 0f

    val angle = remember { Animatable(restingAngle(flip180)) }
    val scope = rememberCoroutineScope()
    val reduceMotion = LocalAnimationScale.current == 0f
    val currentFlip by rememberUpdatedState(flip180)

    /**
     * Animates to whichever representation of [target] is nearest where the dial
     * currently sits, then normalises.
     *
     * Without the first part, spinning the slab a full turn and letting go sent
     * it all the way back round: the value was, say, 540 degrees, the target a
     * bare 180, and the animation dutifully unwound 360 of them. Since every
     * angle here is equivalent modulo 360, picking the nearest equivalent makes
     * it settle by the short way. Snapping to the canonical value afterwards is
     * invisible -- it differs from where the animation landed by whole turns --
     * and stops the number growing without bound.
     */
    suspend fun settleTo(target: Float) {
        val turns = ((angle.value - target) / 360f).roundToInt()
        val nearest = target + turns * 360f
        if (reduceMotion) {
            angle.snapTo(target)
        } else {
            if (abs(angle.value - nearest) > 0.5f) angle.animateTo(nearest, springSpec())
            angle.snapTo(target)
        }
    }

    // Follows the value when it changes from outside -- a Discard, or the draft
    // coming back after a process death.
    LaunchedEffect(flip180, sessionFlip180) {
        settleTo(restingAngle(flip180))
    }

    fun choose(flip: Boolean) {
        if (flip != currentFlip) onFlip180(flip)
        scope.launch { settleTo(restingAngle(flip)) }
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
                .pointerInput(sessionFlip180) {
                    detectTapGestures { choose(!currentFlip) }
                }
                .pointerInput(sessionFlip180) {
                    var previous = 0f
                    var accumulated = 0f
                    var base = 0f
                    detectDragGestures(
                        onDragStart = { offset ->
                            val c = Offset(size.width / 2f, size.height / 2f)
                            previous = angleOf(offset - c)
                            accumulated = 0f
                            base = angle.value
                        },
                        onDragEnd = { choose(isFlippedAt(angle.value, sessionFlip180)) },
                        onDragCancel = { choose(isFlippedAt(angle.value, sessionFlip180)) },
                    ) { change, _ ->
                        val c = Offset(size.width / 2f, size.height / 2f)
                        val now = angleOf(change.position - c)
                        // atan2 wraps at +/-180, so a drag across that seam
                        // reads as a 360-degree jump unless each step is
                        // unwrapped before it is accumulated. That jump was the
                        // other half of the spin-and-snap weirdness.
                        var step = now - previous
                        if (step > 180f) step -= 360f
                        if (step < -180f) step += 360f
                        accumulated += step
                        previous = now
                        scope.launch { angle.snapTo(base + accumulated) }
                    }
                }
                .semantics {
                    role = Role.Button
                    contentDescription = "Display rotation"
                    stateDescription =
                        if (flip180) "Rotated 180 degrees" else "Not rotated"
                },
        ) {
            Canvas(Modifier.fillMaxSize()) { drawSlab(image, angle.value) }
        }

        Readout(flip180 = flip180, sessionLive = sessionLive, hasPicture = image != null, staged = staged)
    }
}

/** Which stop a free-spun angle is closest to, as a flip180 value. */
private fun isFlippedAt(angleDegrees: Float, sessionFlip180: Boolean): Boolean {
    val normalised = ((angleDegrees % 360f) + 360f) % 360f
    val halfTurned = normalised in 90f..270f
    // The angle is relative to the captured frame, which is itself already
    // rotated when the running session has the flip on.
    return if (halfTurned) !sessionFlip180 else sessionFlip180
}

private fun DrawScope.drawSlab(image: ImageBitmap?, angleDegrees: Float) {
    val chassisCorner = 14.dp.toPx()
    val bezel = size.minDimension * 0.055f
    val centre = Offset(size.width / 2f, size.height / 2f)
    val screen = Rect(bezel, bezel, size.width - bezel, size.height - bezel)

    // Chassis. Raised, so the screen cut into it reads as a recess.
    drawRoundRect(
        color = QuillTokens.Raise,
        size = size,
        cornerRadius = CornerRadius(chassisCorner),
    )
    drawRoundRect(
        color = QuillTokens.Copper.copy(alpha = 0.3f),
        size = size,
        cornerRadius = CornerRadius(chassisCorner),
        style = Stroke(width = 1.dp.toPx()),
    )

    // The screen: darker than everything around it, because it is a window
    // rather than a panel sitting on top of one.
    drawRect(color = QuillTokens.Recess, topLeft = screen.topLeft, size = screen.size)

    if (image != null) {
        clipRect(screen.left, screen.top, screen.right, screen.bottom) {
            rotate(degrees = angleDegrees, pivot = centre) {
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

    // The handle. Outside the clip so it rides the bezel rather than being cut
    // off by the screen, and rotated with the picture so it is always on the
    // edge that is currently the bottom of the display.
    rotate(degrees = angleDegrees, pivot = centre) {
        val handleWidth = size.minDimension * 0.11f
        val handleHeight = 7.dp.toPx()
        drawRoundRect(
            color = QuillTokens.Copper,
            topLeft = Offset(centre.x - handleWidth / 2f, size.height - handleHeight / 2f),
            size = Size(handleWidth, handleHeight * 2f),
            cornerRadius = CornerRadius(handleHeight),
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
                append(if (flip180) "Rotated · 180°" else "Not rotated · 0°")
                if (staged) append(" · staged")
            }.uppercase(),
            style = QuillType.eyebrow.copy(
                color = if (staged) QuillTokens.CopperLit else QuillTokens.Graphite,
            ),
        )
        BasicText(
            text = when {
                hasPicture -> "Turn it until the desktop is the right way up."
                sessionLive -> "Preview unavailable. Turning it still sets the rotation."
                else -> "No picture yet. Connect the cable to see the desktop here."
            },
            style = QuillType.caption.copy(color = QuillTokens.Muted),
        )
    }
}

private fun angleOf(v: Offset): Float =
    Math.toDegrees(atan2(v.y.toDouble(), v.x.toDouble())).toFloat()

private fun <T> springSpec() = androidx.compose.animation.core.spring<T>(
    dampingRatio = 0.75f,
    stiffness = 320f,
)
