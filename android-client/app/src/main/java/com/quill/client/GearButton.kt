package com.quill.client

import android.content.Context
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.graphics.Path
import android.view.MotionEvent
import android.view.View
import kotlin.math.cos
import kotlin.math.sin

/**
 * The way into settings while video is streaming.
 *
 * The status overlay is the only other entry point, and it goes `GONE` the
 * moment the first frame renders (`MainActivity.hideStatus`), so once the
 * connection is up there is nowhere left to tap. This sits above the video
 * permanently instead, dimmed to [IDLE_ALPHA] a few seconds after it is last
 * touched so it stops competing with the desktop content behind it. It is only
 * ever faded, never hidden and never made unclickable -- a gear you cannot tap
 * is the bug this exists to fix.
 *
 * **It swallows its own input.** `MainActivity`'s touch/hover/generic-motion
 * listeners live on the `SurfaceView` and do no hit-testing at all: every
 * pointer event they see, finger or pen, is forwarded to the daemon and
 * injected into the desktop. As the topmost sibling in the root `FrameLayout`
 * this view gets first crack at dispatch, and the overrides below consume all
 * three streams inside its bounds so a tap on the gear never also clicks
 * whatever is behind it on the Linux side.
 *
 * Drawn with `Canvas` rather than a vector drawable because this app ships no
 * `res/drawable` (or `res/layout`, or `res/values`) at all -- every view in it
 * is built in code.
 */
class GearButton(context: Context) : View(context) {

    private val fill = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        color = Color.WHITE
        style = Paint.Style.FILL
    }
    private val backdrop = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        color = Color.argb(140, 0, 0, 0)
        style = Paint.Style.FILL
    }
    private val gear = Path().apply { fillType = Path.FillType.EVEN_ODD }

    init {
        isClickable = true
        contentDescription = "Settings"
        alpha = ACTIVE_ALPHA
    }

    /** Full opacity now, dimming again after [FADE_DELAY_MS] of no contact. */
    fun wake() {
        animate().cancel()
        alpha = ACTIVE_ALPHA
        fadeSoon()
    }

    /** Arms the dim-down without touching the current alpha. */
    fun fadeSoon() {
        animate()
            .alpha(IDLE_ALPHA)
            .setStartDelay(FADE_DELAY_MS)
            .setDuration(FADE_DURATION_MS)
            .start()
    }

    override fun onSizeChanged(w: Int, h: Int, oldw: Int, oldh: Int) {
        super.onSizeChanged(w, h, oldw, oldh)
        buildGear(w, h)
    }

    /**
     * Alternating outer/inner radii, each segment sampled as a real arc rather
     * than a single line, so the silhouette reads as a gear and not a star.
     * The hub is punched out by a second contour under `EVEN_ODD`.
     */
    private fun buildGear(w: Int, h: Int) {
        gear.reset()
        val cx = w / 2f
        val cy = h / 2f
        // Glyph deliberately smaller than the view: the touch target stays a
        // comfortable ~48dp while the mark sitting over the desktop stays small.
        val outer = minOf(w, h) * GLYPH_FRACTION / 2f
        val inner = outer * 0.78f
        val hub = outer * 0.34f

        val segments = TEETH * 2
        val segmentAngle = (2.0 * Math.PI / segments).toFloat()
        for (segment in 0 until segments) {
            val r = if (segment % 2 == 0) outer else inner
            for (step in 0..ARC_STEPS) {
                val angle = segmentAngle * (segment + step.toFloat() / ARC_STEPS)
                val x = cx + r * cos(angle)
                val y = cy + r * sin(angle)
                if (segment == 0 && step == 0) gear.moveTo(x, y) else gear.lineTo(x, y)
            }
        }
        gear.close()
        gear.addCircle(cx, cy, hub, Path.Direction.CCW)
    }

    override fun onDraw(canvas: Canvas) {
        super.onDraw(canvas)
        // Dark disc behind the white gear: at 15% alpha over a light desktop a
        // bare white glyph disappears entirely.
        val radius = minOf(width, height) * GLYPH_FRACTION / 2f * 1.35f
        canvas.drawCircle(width / 2f, height / 2f, radius, backdrop)
        canvas.drawPath(gear, fill)
    }

    override fun onTouchEvent(event: MotionEvent): Boolean {
        if (event.actionMasked == MotionEvent.ACTION_DOWN) wake()
        // super handles the click detection (this view is clickable); returning
        // true unconditionally is what keeps ACTION_MOVE/UP of a tap that
        // started here from ever reaching the SurfaceView's forwarder.
        super.onTouchEvent(event)
        return true
    }

    override fun onHoverEvent(event: MotionEvent): Boolean {
        if (event.actionMasked == MotionEvent.ACTION_HOVER_ENTER) wake()
        return true
    }

    override fun onGenericMotionEvent(event: MotionEvent): Boolean = true

    companion object {
        /** Visible but out of the way; still tappable, by design. */
        private const val IDLE_ALPHA = 0.15f
        private const val ACTIVE_ALPHA = 0.9f
        private const val FADE_DELAY_MS = 3000L
        private const val FADE_DURATION_MS = 600L
        private const val TEETH = 8
        private const val ARC_STEPS = 4
        private const val GLYPH_FRACTION = 0.6f
    }
}
