package com.quill.client

import android.graphics.Bitmap
import android.os.Handler
import android.os.HandlerThread
import android.util.Log
import android.view.PixelCopy
import android.view.SurfaceView
import java.util.concurrent.atomic.AtomicBoolean

/**
 * A still of the last desktop frame, handed from [MainActivity] to
 * [SettingsActivity] so the settings screen can show what is actually on the
 * tablet rather than an abstraction of it.
 *
 * **Why a process-global rather than an Intent extra.** Even at the small size
 * captured here the bitmap is hundreds of kilobytes, and a Binder transaction
 * caps out around a megabyte -- putting it in an Intent would throw
 * `TransactionTooLargeException`, intermittently, depending on what else was in
 * flight. Both activities are in the same process, so a plain object is both
 * correct and free.
 *
 * **Ownership.** [offer] recycles whatever it replaces, so at most one bitmap is
 * alive at a time regardless of how often settings is opened. Readers get the
 * bitmap without taking ownership and must not recycle it; [clear] is called
 * from `SettingsActivity.onDestroy` and again from `MainActivity.onResume`, the
 * second covering the case where settings was killed without `onDestroy`
 * running.
 */
object FramePreview {

    private const val TAG = "Quill"

    /**
     * Destination size for the copy. `PixelCopy` scales the source into
     * whatever the destination bitmap is, so this never allocates the 16.4 MB a
     * full 2560x1600 ARGB_8888 capture would cost -- it is a thumbnail inside a
     * diagram, not a screenshot.
     */
    private const val WIDTH = 512
    private const val HEIGHT = 320

    /**
     * How long the gear is willing to wait for the copy before opening settings
     * without a picture.
     *
     * Non-negotiable: the gear's contract is that tapping it opens settings.
     * A GPU readback that stalls must cost a preview, never the tap.
     */
    const val TIMEOUT_MS = 150L

    @Volatile
    private var frame: Bitmap? = null

    /** Panel geometry from the last handshake, for the preview's aspect. */
    @Volatile
    var panelWidthPx: Int = 0
        private set

    @Volatile
    var panelHeightPx: Int = 0
        private set

    private var thread: HandlerThread? = null
    private var handler: Handler? = null

    fun setPanelSize(width: Int, height: Int) {
        panelWidthPx = width
        panelHeightPx = height
    }

    /** Panel width / height, or a sane landscape default before any handshake. */
    fun panelAspect(): Float {
        val w = panelWidthPx
        val h = panelHeightPx
        return if (w > 0 && h > 0) w.toFloat() / h.toFloat() else 1.6f
    }

    /** The last captured frame, or null. The caller does **not** own it. */
    fun peek(): Bitmap? = frame?.takeIf { !it.isRecycled }

    private fun offer(bitmap: Bitmap?) {
        // Null the field before recycling, so a concurrent peek() can never be
        // handed a bitmap that is about to become invalid.
        val previous = frame
        frame = bitmap
        previous?.recycle()
    }

    fun clear() = offer(null)

    /**
     * Grabs the current contents of [surfaceView], then runs [then] on the main
     * thread -- once, whether the copy succeeded, failed, or took too long.
     *
     * The copy has to happen *before* the settings activity starts, because
     * starting it destroys the surface this reads from.
     */
    fun captureThen(surfaceView: SurfaceView, mainHandler: Handler, then: () -> Unit) {
        val done = AtomicBoolean(false)
        val finish = {
            if (done.compareAndSet(false, true)) mainHandler.post(then)
        }

        val surface = surfaceView.holder.surface
        if (surface == null || !surface.isValid) {
            Log.i(TAG, "[preview] no valid surface to capture; opening settings without one")
            finish()
            return
        }

        // Whichever lands first wins. A slow readback costs the picture, not
        // the tap.
        mainHandler.postDelayed({
            if (!done.get()) Log.i(TAG, "[preview] capture did not finish within ${TIMEOUT_MS}ms")
            finish()
        }, TIMEOUT_MS)

        val startNs = System.nanoTime()
        val destination = try {
            Bitmap.createBitmap(WIDTH, HEIGHT, Bitmap.Config.ARGB_8888)
        } catch (e: OutOfMemoryError) {
            Log.w(TAG, "[preview] could not allocate the destination bitmap: $e")
            finish()
            return
        }

        try {
            PixelCopy.request(surfaceView, destination, { result ->
                val elapsedMs = (System.nanoTime() - startNs) / 1_000_000.0
                if (result == PixelCopy.SUCCESS) {
                    Log.i(
                        TAG,
                        "[preview] captured ${destination.width}x${destination.height} " +
                            "from a ${surfaceView.width}x${surfaceView.height} surface " +
                            "in %.1fms".format(elapsedMs)
                    )
                    offer(destination)
                } else {
                    // Codes are PixelCopy.ERROR_* -- logged raw so a failure on
                    // some other GPU is diagnosable from a bug report.
                    Log.w(TAG, "[preview] PixelCopy failed with result=$result after %.1fms".format(elapsedMs))
                    destination.recycle()
                }
                finish()
            }, copyHandler())
        } catch (e: IllegalArgumentException) {
            // Thrown when the surface has no size yet.
            Log.w(TAG, "[preview] PixelCopy rejected the request: $e")
            destination.recycle()
            finish()
        }
    }

    /** PixelCopy needs a handler that is not the one it will call back on. */
    private fun copyHandler(): Handler {
        handler?.let { return it }
        val t = HandlerThread("quill-preview").apply { start() }
        thread = t
        return Handler(t.looper).also { handler = it }
    }
}
