package com.quill.client

import android.animation.ValueAnimator
import android.content.Context
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.graphics.Path
import android.graphics.Rect
import android.graphics.RectF
import android.os.Build
import android.view.MotionEvent
import android.view.View
import android.view.ViewConfiguration
import kotlin.math.abs
import kotlin.math.cos
import kotlin.math.max
import kotlin.math.min
import kotlin.math.sin

/**
 * The way into settings while video is streaming.
 *
 * The status overlay is the only other entry point, and it goes `GONE` the
 * moment the first frame renders (`MainActivity.hideStatus`), so once the
 * connection is up there is nowhere left to tap. This sits above the video
 * permanently instead.
 *
 * **It swallows its own input.** `MainActivity`'s touch/hover/generic-motion
 * listeners live on the `SurfaceView` and do no hit-testing at all: every
 * pointer event they see, finger or pen, is forwarded to the daemon and
 * injected into the desktop. As the topmost sibling in the root `FrameLayout`
 * this view gets first crack at dispatch, and all three overrides below return
 * `true` unconditionally, so a tap *or a drag* on the gear never also does
 * something on the Linux side.
 *
 * **This view detects its own taps.** It used to delegate to
 * `super.onTouchEvent` and rely on `isClickable`, but a drag that happens to
 * end near where it began looks exactly like a click to that detector. Tap and
 * drag are now told apart by [ViewConfiguration.getScaledTouchSlop] and the
 * click is fired explicitly with [performClick] -- which is also what TalkBack
 * activates, so `isClickable` stays and accessibility is unaffected. A hovering
 * S Pen wakes the gear but cannot drag it: dragging needs a real `ACTION_DOWN`.
 *
 * **Position.** Drag it anywhere; on release it flies to the nearest edge and
 * stays there across sessions. Movement is by [setTranslationX]/[setTranslationY]
 * rather than by layout params, so a drag costs no layout pass over a live video
 * surface.
 *
 * The insets it snaps to are not cosmetic. The old fixed `TOP|END` position put
 * this view's touch target inside the region Android watches for the
 * status-bar pull-down, and tapping the gear opened the notification shade
 * instead of settings -- reproduced twice on a Tab S9 FE+. Sides keep
 * [SIDE_INSET_DP] and are additionally protected by
 * `setSystemGestureExclusionRects`, which claims priority over the back
 * gesture. Top and bottom get double the inset and nothing else: **gesture
 * exclusion does not apply to the immersive system-bar reveal**, which is
 * handled by a system window above the app, so distance is the only lever
 * there.
 *
 * **Idle behaviour.** A few seconds after it is last touched the glyph retracts
 * into a thin sliver against its edge and dims. The *view* never changes size --
 * only the drawing does -- so the touch target stays a full 48dp however small
 * the mark looks. A gear you cannot tap is the bug this view exists to fix.
 *
 * Drawn with `Canvas` rather than a vector drawable because this app ships no
 * `res/drawable` at all.
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
    private val sliverPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        color = Color.WHITE
        style = Paint.Style.FILL
    }
    private val gear = Path().apply { fillType = Path.FillType.EVEN_ODD }
    private val sliverRect = RectF()

    private val settings = Settings(context)
    private val touchSlop = ViewConfiguration.get(context).scaledTouchSlop

    /** 0 = full gear, 1 = sliver. Animated, never toggled. */
    private var retract = 0f
        set(value) {
            field = value
            invalidate()
        }

    private var retractAnimator: ValueAnimator? = null

    private var edge = GearEdge.fromOrdinal(settings.gearEdge)
    private var fraction = settings.gearFraction

    private var dragging = false
    private var downRawX = 0f
    private var downRawY = 0f
    private var downTranslationX = 0f
    private var downTranslationY = 0f

    init {
        isClickable = true
        contentDescription = "Settings"
        alpha = ACTIVE_ALPHA
    }

    /** Full opacity and full glyph now, retracting again after [FADE_DELAY_MS]. */
    fun wake() {
        animate().cancel()
        alpha = ACTIVE_ALPHA
        animateRetract(to = 0f, delayMs = 0L, durationMs = EXPAND_DURATION_MS)
        fadeSoon()
    }

    /** Arms the dim-and-retract without touching the current state. */
    fun fadeSoon() {
        animate()
            .alpha(IDLE_ALPHA)
            .setStartDelay(FADE_DELAY_MS)
            .setDuration(FADE_DURATION_MS)
            .start()
        animateRetract(to = 1f, delayMs = FADE_DELAY_MS, durationMs = FADE_DURATION_MS)
    }

    private fun animateRetract(to: Float, delayMs: Long, durationMs: Long) {
        retractAnimator?.cancel()
        if (retract == to) return
        // ValueAnimator consults ANIMATOR_DURATION_SCALE itself and collapses
        // to instant when the user has asked for no animation, so reduced
        // motion needs nothing extra here. (Compose does not; see QuillTheme.)
        retractAnimator = ValueAnimator.ofFloat(retract, to).apply {
            startDelay = delayMs
            duration = durationMs
            addUpdateListener { retract = it.animatedValue as Float }
            start()
        }
    }

    override fun onLayout(changed: Boolean, left: Int, top: Int, right: Int, bottom: Int) {
        super.onLayout(changed, left, top, right, bottom)
        moveTo(edge, fraction, animated = false)
    }

    override fun onSizeChanged(w: Int, h: Int, oldw: Int, oldh: Int) {
        super.onSizeChanged(w, h, oldw, oldh)
        buildGear(w, h)
    }

    // ---- position -------------------------------------------------------

    private fun parentWidth() = (parent as? View)?.width ?: 0
    private fun parentHeight() = (parent as? View)?.height ?: 0

    private fun dp(value: Float) = value * resources.displayMetrics.density

    /**
     * Where the view's top-left corner sits for a given edge and fraction.
     *
     * The fraction runs along the edge between the corner-avoidance margins, so
     * 0 and 1 are still comfortably clear of the corners -- the gear should
     * never end up wedged where two system gesture regions meet.
     */
    private fun positionFor(edge: GearEdge, fraction: Float): Pair<Float, Float> {
        val pw = parentWidth()
        val ph = parentHeight()
        if (pw == 0 || ph == 0) return 0f to 0f

        val side = dp(SIDE_INSET_DP)
        val ends = dp(END_INSET_DP)
        val corner = dp(CORNER_MARGIN_DP)
        val f = fraction.coerceIn(0f, 1f)

        return when (edge) {
            GearEdge.LEFT -> side to lerp(corner, ph - height - corner, f)
            GearEdge.RIGHT -> (pw - width - side) to lerp(corner, ph - height - corner, f)
            GearEdge.TOP -> lerp(corner, pw - width - corner, f) to ends
            GearEdge.BOTTOM -> lerp(corner, pw - width - corner, f) to (ph - height - ends)
        }
    }

    private fun lerp(from: Float, to: Float, t: Float) = from + (to - from) * t

    private fun moveTo(edge: GearEdge, fraction: Float, animated: Boolean) {
        val (x, y) = positionFor(edge, fraction)
        if (animated) {
            animate().translationX(x).translationY(y)
                .setStartDelay(0)
                .setDuration(SNAP_DURATION_MS)
                .withEndAction { updateGestureExclusion() }
                .start()
        } else {
            translationX = x
            translationY = y
            updateGestureExclusion()
        }
    }

    /**
     * Picks the edge whose distance from the view's centre is smallest, and
     * where along it the gear ended up, then parks there and remembers it.
     */
    private fun snapToNearestEdge() {
        val pw = parentWidth().toFloat()
        val ph = parentHeight().toFloat()
        if (pw == 0f || ph == 0f) return

        val cx = translationX + width / 2f
        val cy = translationY + height / 2f

        val distances = mapOf(
            GearEdge.LEFT to cx,
            GearEdge.RIGHT to (pw - cx),
            GearEdge.TOP to cy,
            GearEdge.BOTTOM to (ph - cy),
        )
        val nearest = distances.minByOrNull { it.value }?.key ?: GearEdge.RIGHT

        val along = when (nearest) {
            GearEdge.LEFT, GearEdge.RIGHT -> (cy - height / 2f) / max(1f, ph - height)
            GearEdge.TOP, GearEdge.BOTTOM -> (cx - width / 2f) / max(1f, pw - width)
        }.coerceIn(0f, 1f)

        edge = nearest
        fraction = along
        settings.gearEdge = nearest.ordinal
        settings.gearFraction = along
        moveTo(nearest, along, animated = true)
    }

    /**
     * Claims the back gesture over the gear's own rectangle.
     *
     * Only helps on the left and right edges. The immersive system-bar reveal
     * on the top and bottom edges is a system window above the app and cannot
     * be claimed at all, which is why those edges snap to a bigger inset.
     */
    private fun updateGestureExclusion() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) return
        systemGestureExclusionRects = listOf(Rect(0, 0, width, height))
    }

    // ---- drawing --------------------------------------------------------

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
        // comfortable 48dp while the mark sitting over the desktop stays small.
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
        val cx = width / 2f
        val cy = height / 2f

        // Cross-fade rather than switch: at any point in between, both marks
        // are drawn at partial alpha and the result reads as one shrinking.
        if (retract < 1f) {
            val gearAlpha = 1f - retract
            // Dark disc behind the white gear: at low alpha over a light
            // desktop a bare white glyph disappears entirely.
            val radius = min(width, height) * GLYPH_FRACTION / 2f * 1.35f
            backdrop.alpha = (140 * gearAlpha).toInt()
            fill.alpha = (255 * gearAlpha).toInt()
            canvas.drawCircle(cx, cy, radius, backdrop)
            canvas.drawPath(gear, fill)
        }

        if (retract > 0f) {
            sliverPaint.alpha = (255 * retract).toInt()
            val thickness = dp(SLIVER_THICKNESS_DP)
            val length = dp(SLIVER_LENGTH_DP)
            // The mark slides from the middle of the touch target out to the
            // edge-facing side of it as the gear retracts, so what is left
            // reads as a tab attached to the screen edge rather than a dash
            // floating half a target's width away from it. The *view* does not
            // move -- it stays a full 48dp target wherever the mark ends up.
            val slide = (width / 2f - thickness / 2f) * retract
            when (edge) {
                GearEdge.LEFT -> sliverRect.set(
                    cx - thickness / 2f - slide, cy - length / 2f,
                    cx + thickness / 2f - slide, cy + length / 2f,
                )
                GearEdge.RIGHT -> sliverRect.set(
                    cx - thickness / 2f + slide, cy - length / 2f,
                    cx + thickness / 2f + slide, cy + length / 2f,
                )
                GearEdge.TOP -> sliverRect.set(
                    cx - length / 2f, cy - thickness / 2f - slide,
                    cx + length / 2f, cy + thickness / 2f - slide,
                )
                GearEdge.BOTTOM -> sliverRect.set(
                    cx - length / 2f, cy - thickness / 2f + slide,
                    cx + length / 2f, cy + thickness / 2f + slide,
                )
            }
            canvas.drawRoundRect(sliverRect, thickness / 2f, thickness / 2f, sliverPaint)
        }
    }

    // ---- input ----------------------------------------------------------

    override fun onTouchEvent(event: MotionEvent): Boolean {
        when (event.actionMasked) {
            MotionEvent.ACTION_DOWN -> {
                wake()
                dragging = false
                downRawX = event.rawX
                downRawY = event.rawY
                downTranslationX = translationX
                downTranslationY = translationY
            }

            MotionEvent.ACTION_MOVE -> {
                val dx = event.rawX - downRawX
                val dy = event.rawY - downRawY
                if (!dragging && (abs(dx) > touchSlop || abs(dy) > touchSlop)) dragging = true
                if (dragging) {
                    val pw = parentWidth()
                    val ph = parentHeight()
                    translationX = (downTranslationX + dx).coerceIn(0f, max(0f, (pw - width).toFloat()))
                    translationY = (downTranslationY + dy).coerceIn(0f, max(0f, (ph - height).toFloat()))
                }
            }

            MotionEvent.ACTION_UP -> {
                if (dragging) snapToNearestEdge() else performClick()
                dragging = false
                fadeSoon()
            }

            MotionEvent.ACTION_CANCEL -> {
                if (dragging) snapToNearestEdge()
                dragging = false
                fadeSoon()
            }
        }
        // Unconditional, for every action: this is what keeps a tap or a drag
        // that started here from also reaching the SurfaceView's forwarder and
        // being injected into the Linux desktop.
        return true
    }

    override fun onHoverEvent(event: MotionEvent): Boolean {
        if (event.actionMasked == MotionEvent.ACTION_HOVER_ENTER) wake()
        return true
    }

    override fun onGenericMotionEvent(event: MotionEvent): Boolean = true

    companion object {
        /** Visible but out of the way; still tappable, by design. A sliver can
         * afford more opacity than a whole gear could. */
        private const val IDLE_ALPHA = 0.35f
        private const val ACTIVE_ALPHA = 0.9f
        private const val FADE_DELAY_MS = 3000L
        private const val FADE_DURATION_MS = 600L
        private const val EXPAND_DURATION_MS = 160L
        private const val SNAP_DURATION_MS = 180L
        private const val TEETH = 8
        private const val ARC_STEPS = 4
        private const val GLYPH_FRACTION = 0.6f

        /** Small, because `setSystemGestureExclusionRects` is what actually
         * claims the back gesture over this view -- the inset is only there so
         * the touch target is not literally flush with the bezel. */
        private const val SIDE_INSET_DP = 6f

        /** Double, because nothing can claim the system-bar reveal swipe. */
        private const val END_INSET_DP = 24f

        /** Keeps the gear out of the corners, where gesture regions meet. */
        private const val CORNER_MARGIN_DP = 64f

        private const val SLIVER_THICKNESS_DP = 5f
        private const val SLIVER_LENGTH_DP = 30f
    }
}
