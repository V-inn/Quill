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

    /**
     * How far the encoder turns the picture, in degrees: 0, 90, 180 or 270.
     *
     * Set from the same setting the daemon is told about in the handshake --
     * inferring it here from the aspect ratio, as this did until Milestone 24,
     * would put the pointer in the wrong corner whenever the two disagreed.
     */
    private var rotationDegrees = 0

    fun setRotation(degrees: Int) {
        if (rotationDegrees == degrees) return
        rotationDegrees = degrees
        invalidate()
    }

    private val swapsAxes: Boolean
        get() = rotationDegrees == 90 || rotationDegrees == 270

    fun setVideoSize(width: Int, height: Int) {
        videoWidth = width.coerceAtLeast(1)
        videoHeight = height.coerceAtLeast(1)
    }

    /** Called off the network thread; posts to the UI thread to redraw. */
    fun update(x: Int, y: Int, visible: Boolean, newBitmap: Bitmap?, hotX: Int, hotY: Int) {
        post {
            if (newBitmap != null) {
                bitmap = newBitmap
                hotspotX = hotX
                hotspotY = hotY
            }
            // Cursor metadata is in the *desktop's* coordinate space, while
            // the video -- and therefore this view -- is in the panel's. Those
            // coincide only at 0 degrees. At a quarter turn the desktop is the
            // transpose of the video, so its width is videoHeight and vice
            // versa, which is why the two are crossed over here.
            val deskW = if (swapsAxes) videoHeight else videoWidth
            val deskH = if (swapsAxes) videoWidth else videoHeight
            val (sx, sy) = when (rotationDegrees) {
                90 -> deskH - y to x
                180 -> deskW - x to deskH - y
                270 -> y to deskW - x
                else -> x to y
            }
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

        // The daemon rotates the whole picture GPU-side, so
        // everything the tablet shows -- including the pointer KWin composites
        // into the video itself -- arrives already turned around. This overlay
        // draws on top of that, *after* the rotation, so it has to turn its own
        // bitmap to match or it is the one thing on the panel still the old way
        // up. Turning only the position, as this did, put the pointer in the
        // right place pointing the wrong way.
        //
        // Rotating the canvas about the cursor point, rather than rotating the
        // bitmap and then working out where to put it, is what keeps the
        // hotspot right: the hotspot pixel already lands on (cursorX, cursorY),
        // and that is the centre of rotation, so it does not move. Rotating the
        // bitmap itself would have needed the hotspot moved within it too, and
        // that is the easy way to fix the direction and break the aim.
        val restore = if (rotationDegrees != 0) {
            canvas.save().also { canvas.rotate(rotationDegrees.toFloat(), cursorX, cursorY) }
        } else {
            -1
        }
        canvas.drawBitmap(
            bmp,
            cursorX - hotspotX * scaleX,
            cursorY - hotspotY * scaleY,
            paint
        )
        if (restore >= 0) canvas.restoreToCount(restore)
    }
}
