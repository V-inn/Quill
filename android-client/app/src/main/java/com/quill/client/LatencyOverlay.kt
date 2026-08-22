package com.quill.client

import android.content.Context
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.graphics.RectF
import android.os.Handler
import android.os.Looper
import android.view.View
import androidx.core.content.res.ResourcesCompat

/**
 * The per-frame latency, drawn over the video.
 *
 * `Settings.showLatencyOverlay` existed for four milestones and did nothing:
 * the only code that read it was the switch that set it. The numbers were
 * already being computed in the render thread and going to `Log.i` every 30
 * frames. On a project whose headline figure is ~55ms glass-to-glass and whose
 * MILESTONES are half latency archaeology, an on-screen readout is worth its
 * forty lines.
 *
 * **This view must not swallow input, which is the exact opposite of the
 * contract two files away.** `GearButton` overrides all three pointer streams
 * and returns `true` so its taps never reach the `SurfaceView`'s forwarder and
 * get injected into the Linux desktop. Copying that here -- the natural instinct
 * after reading it -- would put a dead rectangle over the desktop that silently
 * ate every touch inside it. A non-clickable `View` with no overrides returns
 * `false`, so `FrameLayout.dispatchTouchEvent` walks straight past it to the
 * surface underneath. Leave it that way.
 *
 * **It drives its own redraws.** The render thread writes five primitives and
 * nothing else; it never calls `invalidate` or `postInvalidate`, which would
 * allocate a `Message` per video frame and put a composition pass in front of a
 * surface whose whole design is about not having one (Milestone 18 spent itself
 * removing exactly that class of cost). Instead this ticks at [REFRESH_MS] while
 * visible and reads whatever the last write left behind.
 */
class LatencyOverlay(context: Context) : View(context) {

    // Written by the render thread, read by the UI thread. Primitives, so no
    // boxing and no allocation on the hot path.
    @Volatile private var lastMs = 0L
    @Volatile private var avgMs = 0L
    @Volatile private var minMs = 0L
    @Volatile private var maxMs = 0L
    @Volatile private var pending = 0
    @Volatile private var haveSamples = false

    /**
     * The clock-sync round-trip this connection calibrated against.
     *
     * Every number above is derived from an offset between two machines' clocks,
     * and that offset is only as good as the exchange that measured it. A
     * healthy link settles in 10-25ms; a reply that sat queued while the daemon
     * waited on the portal's picker dialog has produced 1119ms, and
     * `MainActivity`'s own watchdog comment records what that cost: "every
     * reported per-frame latency was off by ~550ms for the rest of the
     * session".
     *
     * So the readout says when it does not trust itself. A confidently wrong
     * "696 ms" over a stream that is actually running at 30ms is worse than no
     * overlay at all -- it is the exact reading Milestone 19 warns cannot be
     * told from a real regression without this number beside it.
     */
    @Volatile private var syncRoundTripMs = 0L

    private val text = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        // Mirrors QuillTokens.Chalk. Spelled out rather than imported: this view
        // lives in MainActivity's tree, which deliberately loads no Compose.
        color = Color.rgb(0xE8, 0xE6, 0xE1)
        typeface = ResourcesCompat.getFont(context, R.font.ibm_plex_mono_regular)
        textSize = 13f * resources.displayMetrics.scaledDensity
    }
    private val backdrop = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        color = Color.argb(150, 0x2A, 0x2F, 0x35)
    }
    /** Mirrors QuillTokens.CopperLit. */
    private val warnColor = Color.rgb(0xE8, 0xA8, 0x5E)
    private val okColor = Color.rgb(0xE8, 0xE6, 0xE1)
    private val box = RectF()
    private val builder = StringBuilder(48)

    private val ticker = Handler(Looper.getMainLooper())
    private val tick = object : Runnable {
        override fun run() {
            if (visibility != VISIBLE) return
            invalidate()
            ticker.postDelayed(this, REFRESH_MS)
        }
    }

    init {
        // See the class doc: no overrides, nothing clickable, nothing focusable.
        // Touches have to fall through to the video surface underneath.
        isClickable = false
        isFocusable = false
    }

    /**
     * Called from the render thread once per rendered frame. Five primitive
     * writes and no allocation; guard it with a cheap visibility check at the
     * call site so the disabled path costs one branch.
     */
    fun submit(last: Long, avg: Long, min: Long, max: Long, pendingFrames: Int) {
        lastMs = last
        avgMs = avg
        minMs = min
        maxMs = max
        pending = pendingFrames
        haveSamples = true
    }

    fun reset() {
        haveSamples = false
    }

    /** Called once per connection, when the clock-sync exchange completes. */
    fun setClockSync(roundTripMs: Long) {
        syncRoundTripMs = roundTripMs
    }

    override fun onVisibilityChanged(changedView: View, visibility: Int) {
        super.onVisibilityChanged(changedView, visibility)
        ticker.removeCallbacks(tick)
        if (visibility == VISIBLE && isAttachedToWindow) ticker.post(tick)
    }

    override fun onAttachedToWindow() {
        super.onAttachedToWindow()
        if (visibility == VISIBLE) ticker.post(tick)
    }

    override fun onDetachedFromWindow() {
        super.onDetachedFromWindow()
        ticker.removeCallbacks(tick)
    }

    override fun onDraw(canvas: Canvas) {
        super.onDraw(canvas)
        if (!haveSamples) return

        val trusted = syncRoundTripMs in 0..MAX_TRUSTED_SYNC_MS
        text.color = if (trusted) okColor else warnColor

        builder.setLength(0)
        builder.append(lastMs).append(" ms  ·  avg ").append(avgMs)
            .append("  min ").append(minMs).append("  max ").append(maxMs)
            .append("  ·  pending ").append(pending)
        if (!trusted) {
            builder.append("  ·  clock sync ").append(syncRoundTripMs)
                .append(" ms, reading unreliable")
        }

        val pad = 8f * resources.displayMetrics.density
        val inset = 12f * resources.displayMetrics.density
        val width = text.measureText(builder, 0, builder.length)
        val metrics = text.fontMetrics
        val lineHeight = metrics.descent - metrics.ascent

        box.set(inset, inset, inset + width + pad * 2, inset + lineHeight + pad * 2)
        val radius = 6f * resources.displayMetrics.density
        canvas.drawRoundRect(box, radius, radius, backdrop)
        canvas.drawText(
            builder,
            0,
            builder.length,
            box.left + pad,
            box.top + pad - metrics.ascent,
            text,
        )
    }

    companion object {
        /** 4Hz. Fast enough to read as live, slow enough to be free. */
        private const val REFRESH_MS = 250L

        /** A healthy local USB link syncs in 10-25ms. Anything past this and
         * the offset was measured against a delayed reply, so every latency
         * derived from it is shifted by roughly half the excess. */
        private const val MAX_TRUSTED_SYNC_MS = 100L
    }
}
