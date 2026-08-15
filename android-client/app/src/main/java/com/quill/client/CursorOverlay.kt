package com.quill.client

import android.content.Context
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Paint
import android.view.View

/**
 * Draws the desktop's pointer on top of the video, for client-side cursor mode.
 *
 * The whole point is that this is driven by cursor messages rather than by
 * decoded frames, so the pointer moves at roughly transport + one Android vsync
 * instead of inheriting the full capture/encode/decode pipeline. It therefore
 * deliberately invalidates itself on position changes, independent of whatever
 * the video surface underneath is doing.
 *
 * Shapes are cached by the daemon's rule: a bitmap arrives only when the shape
 * actually changed, and until the next one does, the last bitmap keeps being
 * drawn at each new position.
 */
class CursorOverlay(context: Context) : View(context) {

    private var bitmap: Bitmap? = null
    private var hotspotX = 0
    private var hotspotY = 0
    private var cursorX = 0f
    private var cursorY = 0f
    private var visible = false
    private val paint = Paint(Paint.ANTI_ALIAS_FLAG or Paint.FILTER_BITMAP_FLAG)

    /** Source video dimensions, so cursor coordinates can be mapped to view space. */
    private var videoWidth = 1
    private var videoHeight = 1

    /** 180-degree rotation, matching the encoder's `flip_180` for portrait. */
    private var flip180 = false

    fun setVideoSize(width: Int, height: Int) {
        videoWidth = width.coerceAtLeast(1)
        videoHeight = height.coerceAtLeast(1)
        // Mirrors vaapi_encoder.rs and input_receiver.rs: portrait on this
        // hardware is flipped GPU-side, and the cursor has to follow the same
        // rule or it lands in the wrong corner.
        flip180 = height > width
    }

    /** Called off the network thread; posts to the UI thread to redraw. */
    fun update(x: Int, y: Int, visible: Boolean, newBitmap: Bitmap?, hotX: Int, hotY: Int) {
        post {
            if (newBitmap != null) {
                bitmap = newBitmap
                hotspotX = hotX
                hotspotY = hotY
            }
            val sx = if (flip180) videoWidth - x else x
            val sy = if (flip180) videoHeight - y else y
            // Video is letterboxed/stretched into this view by the SurfaceView
            // underneath; scale cursor coordinates the same way so it lines up
            // with what is actually drawn.
            cursorX = sx.toFloat() * width / videoWidth
            cursorY = sy.toFloat() * height / videoHeight
            this.visible = visible
            invalidate()
        }
    }

    fun clear() {
        post {
            visible = false
            invalidate()
        }
    }

    override fun onDraw(canvas: Canvas) {
        super.onDraw(canvas)
        if (!visible) return
        val bmp = bitmap ?: return
        val scaleX = width.toFloat() / videoWidth
        val scaleY = height.toFloat() / videoHeight
        canvas.drawBitmap(
            bmp,
            cursorX - hotspotX * scaleX,
            cursorY - hotspotY * scaleY,
            paint
        )
    }
}
