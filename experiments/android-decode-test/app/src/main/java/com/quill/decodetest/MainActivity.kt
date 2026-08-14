package com.quill.decodetest

import android.app.Activity
import android.media.MediaCodec
import android.media.MediaFormat
import android.os.Bundle
import android.util.Log
import android.view.InputDevice
import android.view.MotionEvent
import android.view.SurfaceHolder
import android.view.SurfaceView
import java.io.BufferedInputStream
import java.io.BufferedOutputStream
import java.io.DataInputStream
import java.io.DataOutputStream
import java.net.ServerSocket
import java.net.Socket
import java.util.concurrent.LinkedBlockingQueue
import kotlin.math.cos
import kotlin.math.roundToInt
import kotlin.math.sin

/** One pen/touch sample, queued from the UI thread and written to the
 * socket on a dedicated background thread -- touch/hover callbacks run on
 * the UI thread, and Android forbids network I/O there
 * (NetworkOnMainThreadException). */
private data class PenEvent(
    val type: Int,
    val x: Int,
    val y: Int,
    val pressure: Int,
    val tiltX: Int,
    val tiltY: Int,
    val buttons: Int,
)

/**
 * Milestone 6: real S Pen `MotionEvent` capture -> protocol -> uinput,
 * layered onto the Milestone 3 decode test app. Listens on a TCP port
 * (reached via `adb forward tcp:PORT tcp:PORT`), reads length-prefixed
 * H.264 frames on the read side (video, daemon -> device), and writes a
 * capability handshake + a stream of input event records on the write
 * side (S Pen -> daemon) -- independent directions of the same socket.
 */
class MainActivity : Activity(), SurfaceHolder.Callback {
    private val tag = "QuillDecodeTest"
    private val port = 7777
    private val width = 1920
    private val height = 1080

    private var decodeThread: Thread? = null
    private var inputWriterThread: Thread? = null
    private val eventQueue = LinkedBlockingQueue<PenEvent>()

    @Volatile
    private var output: DataOutputStream? = null

    @Volatile
    private var running = true

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val surfaceView = SurfaceView(this)
        setContentView(surfaceView)
        surfaceView.holder.addCallback(this)
        surfaceView.setOnTouchListener { _, event -> handleMotionEvent(event, down = true) }
        surfaceView.setOnHoverListener { _, event -> handleMotionEvent(event, down = false) }
    }

    override fun surfaceCreated(holder: SurfaceHolder) {
        decodeThread = Thread { runDecodeLoop(holder) }.also { it.start() }
    }

    override fun surfaceChanged(holder: SurfaceHolder, format: Int, w: Int, h: Int) {}

    override fun surfaceDestroyed(holder: SurfaceHolder) {
        running = false
        decodeThread?.interrupt()
    }

    override fun onDestroy() {
        super.onDestroy()
        running = false
    }

    /**
     * Converts Android's single tilt-from-vertical angle + orientation into
     * Wacom-style perpendicular tilt_x/tilt_y (degrees), matching what the
     * daemon's uinput ABS_TILT_X/Y axes expect.
     */
    private fun tiltXY(event: MotionEvent): Pair<Int, Int> {
        val tilt = event.getAxisValue(MotionEvent.AXIS_TILT) // radians from vertical
        val orientation = event.orientation // radians
        val tiltDeg = Math.toDegrees(tilt.toDouble())
        val tiltX = (tiltDeg * sin(orientation.toDouble())).roundToInt()
        val tiltY = (tiltDeg * cos(orientation.toDouble())).roundToInt()
        return tiltX to tiltY
    }

    private fun handleMotionEvent(event: MotionEvent, down: Boolean): Boolean {
        if (output == null) return false
        val (tiltX, tiltY) = tiltXY(event)
        val pressureRaw = (event.pressure * pressureMax).roundToInt()

        val type: Int = when (event.action) {
            MotionEvent.ACTION_HOVER_ENTER -> EV_HOVER_ENTER
            MotionEvent.ACTION_HOVER_MOVE -> EV_HOVER_MOVE
            MotionEvent.ACTION_HOVER_EXIT -> EV_HOVER_EXIT
            MotionEvent.ACTION_DOWN -> EV_DOWN
            MotionEvent.ACTION_MOVE -> EV_MOVE
            MotionEvent.ACTION_UP, MotionEvent.ACTION_CANCEL -> EV_UP
            else -> return down // unhandled action, let the view keep default handling
        }

        val buttons = if (event.buttonState and MotionEvent.BUTTON_STYLUS_PRIMARY != 0) 1 else 0

        // Cheap, non-blocking enqueue on the UI thread; the actual socket
        // write happens on inputWriterThread.
        eventQueue.offer(
            PenEvent(type, event.x.roundToInt(), event.y.roundToInt(), pressureRaw, tiltX, tiltY, buttons)
        )
        return true
    }

    /** Drains eventQueue on a dedicated thread, blocking-writes each event to
     * the socket -- the actual I/O that used to run (illegally) on the UI
     * thread inside handleMotionEvent. */
    private fun runInputWriterLoop(out: DataOutputStream) {
        while (running) {
            val ev = try {
                eventQueue.take()
            } catch (e: InterruptedException) {
                break
            }
            try {
                out.writeByte(ev.type)
                out.writeInt(ev.x)
                out.writeInt(ev.y)
                out.writeInt(ev.pressure)
                out.writeInt(ev.tiltX)
                out.writeInt(ev.tiltY)
                out.writeByte(ev.buttons)
                out.flush()
            } catch (e: Exception) {
                Log.w(tag, "input writer stopping, socket write failed", e)
                break
            }
        }
    }

    @Volatile
    private var pressureMax = 4095 // overwritten from the real stylus MotionRange once known

    private fun sendHandshake(out: DataOutputStream) {
        // Real Display metrics + InputDevice.getMotionRange() -- the design
        // doc's capability handshake, not hardcoded per-device constants.
        val metrics = resources.displayMetrics
        val stylusDevice = InputDevice.getDeviceIds()
            .map { InputDevice.getDevice(it) }
            .firstOrNull { d -> d != null && d.sources and InputDevice.SOURCE_STYLUS == InputDevice.SOURCE_STYLUS }

        val pressureRange = stylusDevice?.getMotionRange(MotionEvent.AXIS_PRESSURE)
        val tiltRange = stylusDevice?.getMotionRange(MotionEvent.AXIS_TILT)

        val pMin = 0
        val pMax = ((pressureRange?.max ?: 1.0f) * 4095).roundToInt().coerceAtLeast(1)
        pressureMax = pMax
        val tMaxDeg = Math.toDegrees((tiltRange?.max ?: (Math.PI / 4)).toDouble()).roundToInt()

        Log.i(
            tag,
            "handshake: ${metrics.widthPixels}x${metrics.heightPixels}px, " +
                "pressure $pMin..$pMax, tilt -$tMaxDeg..$tMaxDeg (stylus device: ${stylusDevice?.name})"
        )

        out.writeInt(metrics.widthPixels)
        out.writeInt(metrics.heightPixels)
        out.writeInt(pMin)
        out.writeInt(pMax)
        out.writeInt(-tMaxDeg)
        out.writeInt(tMaxDeg)
        // Milestone 7 clock-sync ping -- see clock_sync.rs on the daemon
        // side for the two-message offset calibration this kicks off.
        out.writeLong(System.currentTimeMillis())
        out.flush()
    }

    /**
     * Reads the daemon's clock-sync reply (sent once, before any video
     * frame) and computes the android-clock-minus-daemon-clock offset via
     * the standard NTP two-message estimate -- see clock_sync.rs for the
     * derivation. Assumes symmetric one-way transport delay, reasonable for
     * a single local adb-forward/USB link.
     */
    private fun readClockOffset(input: DataInputStream): Long {
        val daemonSendMs = input.readLong()
        val androidSendEchoMs = input.readLong()
        val daemonRecvMs = input.readLong()
        val androidRecvMs = System.currentTimeMillis()
        val offset = ((androidRecvMs - daemonSendMs) - (daemonRecvMs - androidSendEchoMs)) / 2
        val roundTripSum = (daemonRecvMs - androidSendEchoMs) + (androidRecvMs - daemonSendMs)
        Log.i(tag, "clock-sync: offset=${offset}ms (android-daemon), round-trip sum=${roundTripSum}ms")
        return offset
    }

    private fun runDecodeLoop(holder: SurfaceHolder) {
        Log.i(tag, "Listening on port $port, waiting for daemon (adb forward)...")
        var codec: MediaCodec? = null
        try {
            ServerSocket(port).use { server ->
                server.reuseAddress = true
                val socket: Socket = server.accept()
                Log.i(tag, "daemon connected from ${socket.remoteSocketAddress}")
                val input = DataInputStream(BufferedInputStream(socket.getInputStream()))
                val out = DataOutputStream(BufferedOutputStream(socket.getOutputStream()))
                output = out
                sendHandshake(out)
                val clockOffsetMs = readClockOffset(input)
                inputWriterThread = Thread { runInputWriterLoop(out) }.also { it.start() }

                val format = MediaFormat.createVideoFormat(MediaFormat.MIMETYPE_VIDEO_AVC, width, height)
                // Milestone 7: ask the decoder to minimize its own internal
                // buffering (some hardware decoders queue several frames
                // deep by default). Harmlessly ignored on decoders/OS
                // versions that don't support it.
                format.setInteger(MediaFormat.KEY_LOW_LATENCY, 1)
                // Back to the device's default (hardware) decoder now that the
                // daemon encodes Main profile/CABAC instead of Constrained
                // Baseline/CAVLC -- the latter caused a solid-green render on
                // this tablet's hardware decoder specifically (see MILESTONES.md).
                codec = MediaCodec.createDecoderByType(MediaFormat.MIMETYPE_VIDEO_AVC)
                Log.i(tag, "using decoder: ${codec!!.name}")
                codec!!.configure(format, holder.surface, null, 0)
                codec!!.start()

                val bufferInfo = MediaCodec.BufferInfo()
                var frameCount = 0L
                var queuedCount = 0L
                var renderedCount = 0L
                var presentationTimeUs = 0L
                var latencySumMs = 0L
                var latencyMinMs = Long.MAX_VALUE
                var latencyMaxMs = Long.MIN_VALUE

                while (running) {
                    val frameSentAtMs = try {
                        input.readLong()
                    } catch (e: Exception) {
                        Log.i(tag, "stream ended: ${e.message}")
                        break
                    }
                    val length = input.readInt()
                    if (length <= 0 || length > 16 * 1024 * 1024) {
                        Log.w(tag, "bogus frame length $length, stopping")
                        break
                    }
                    val frameBytes = ByteArray(length)
                    input.readFully(frameBytes)

                    val inIndex = codec!!.dequeueInputBuffer(10_000)
                    if (inIndex >= 0) {
                        val inputBuffer = codec!!.getInputBuffer(inIndex)!!
                        inputBuffer.clear()
                        inputBuffer.put(frameBytes)
                        codec!!.queueInputBuffer(
                            inIndex, 0, frameBytes.size,
                            presentationTimeUs, MediaCodec.BUFFER_FLAG_KEY_FRAME
                        )
                        presentationTimeUs += 16_666 // ~60fps spacing, cosmetic only for v0
                        queuedCount++
                    } else {
                        Log.w(tag, "no input buffer available (dequeueInputBuffer=$inIndex)")
                    }

                    var outIndex = codec!!.dequeueOutputBuffer(bufferInfo, 10_000)
                    while (true) {
                        when {
                            outIndex >= 0 -> {
                                codec!!.releaseOutputBuffer(outIndex, true)
                                renderedCount++
                                outIndex = codec!!.dequeueOutputBuffer(bufferInfo, 0)
                            }
                            outIndex == MediaCodec.INFO_OUTPUT_FORMAT_CHANGED -> {
                                Log.i(tag, "output format changed: ${codec!!.outputFormat}")
                                outIndex = codec!!.dequeueOutputBuffer(bufferInfo, 0)
                            }
                            outIndex == MediaCodec.INFO_TRY_AGAIN_LATER -> break
                            else -> {
                                Log.w(tag, "dequeueOutputBuffer returned $outIndex")
                                break
                            }
                        }
                    }

                    // Milestone 7: per-frame glass-to-glass latency estimate,
                    // converting the daemon's send timestamp into
                    // android-clock terms via the calibrated offset -- see
                    // clock_sync.rs. Approximate (doesn't isolate decode vs
                    // transport vs render), but log-based instead of
                    // camera-based, so it's cheap to sample continuously.
                    val latencyMs = (System.currentTimeMillis() - clockOffsetMs) - frameSentAtMs
                    latencySumMs += latencyMs
                    if (latencyMs < latencyMinMs) latencyMinMs = latencyMs
                    if (latencyMs > latencyMaxMs) latencyMaxMs = latencyMs

                    frameCount++
                    if (frameCount == 1L || frameCount % 30 == 0L) {
                        Log.i(
                            tag,
                            "frame $frameCount ($length bytes): queued=$queuedCount rendered=$renderedCount, " +
                                "latency avg=${latencySumMs / frameCount}ms min=${latencyMinMs}ms max=${latencyMaxMs}ms (this frame: ${latencyMs}ms)"
                        )
                    }
                }
            }
        } catch (e: Exception) {
            Log.e(tag, "decode loop error", e)
        } finally {
            codec?.stop()
            codec?.release()
            output = null
            inputWriterThread?.interrupt()
            Log.i(tag, "decode loop stopped")
        }
    }

    companion object {
        private const val EV_HOVER_ENTER = 0
        private const val EV_HOVER_MOVE = 1
        private const val EV_HOVER_EXIT = 2
        private const val EV_DOWN = 3
        private const val EV_MOVE = 4
        private const val EV_UP = 5
    }
}
