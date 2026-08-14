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
        // ACTION_BUTTON_PRESS/RELEASE (S Pen side button) arrive via the
        // generic-motion stream, not the touch or hover stream -- Android
        // dispatches touch/hover/other-generic-motion as three disjoint
        // paths per MotionEvent, so all three listeners coexist safely with
        // no double-processing of the same event.
        surfaceView.setOnGenericMotionListener { _, event -> handleMotionEvent(event, down = false) }
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

    // Diffed on every event rather than trusting ACTION_BUTTON_PRESS/RELEASE
    // to ever fire on their own -- some digitizers only ever expose the
    // side-button bit via buttonState on regular move/hover events and
    // never synthesize a standalone button action. UI-thread only, no
    // synchronization needed.
    private var lastStylusButtonState = false

    private fun handleMotionEvent(event: MotionEvent, down: Boolean): Boolean {
        if (output == null) return false

        val stylusButtonNow = event.buttonState and MotionEvent.BUTTON_STYLUS_PRIMARY != 0
        if (stylusButtonNow != lastStylusButtonState) {
            lastStylusButtonState = stylusButtonNow
            eventQueue.offer(
                PenEvent(
                    if (stylusButtonNow) EV_BUTTON_DOWN else EV_BUTTON_UP,
                    event.x.roundToInt(), event.y.roundToInt(), 0, 0, 0, 1
                )
            )
        }

        val type: Int = when (event.action) {
            MotionEvent.ACTION_HOVER_ENTER -> EV_HOVER_ENTER
            MotionEvent.ACTION_HOVER_MOVE -> EV_HOVER_MOVE
            MotionEvent.ACTION_HOVER_EXIT -> EV_HOVER_EXIT
            MotionEvent.ACTION_DOWN -> EV_DOWN
            MotionEvent.ACTION_MOVE -> EV_MOVE
            MotionEvent.ACTION_UP, MotionEvent.ACTION_CANCEL -> EV_UP
            else -> return down // button-only or otherwise unhandled action, already handled above
        }

        val (tiltX, tiltY) = tiltXY(event)
        val pressureRaw = (event.pressure * pressureMax).roundToInt()
        val isFinger = event.getToolType(0) == MotionEvent.TOOL_TYPE_FINGER
        val buttons = (if (stylusButtonNow) 1 else 0) or (if (isFinger) 2 else 0)

        if (isFinger) {
            // Diagnostic: confirms Android is actually delivering finger
            // touches to this listener at all (vs. e.g. S Pen palm
            // rejection suppressing them device-side before we ever see
            // them).
            Log.d(tag, "finger event: action=${event.action} at (${event.x}, ${event.y})")
        }

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
                // Standard KEY_LOW_LATENCY re-verified with the corrected
                // (FIFO-based) latency measurement (Milestone 7): genuinely
                // no effect on this decoder. Left enabled anyway -- harmless
                // here, other decoders/devices may honor it.
                format.setInteger(MediaFormat.KEY_LOW_LATENCY, 1)
                // Back to the device's default (hardware) decoder now that the
                // daemon encodes Main profile/CABAC instead of Constrained
                // Baseline/CAVLC -- the latter caused a solid-green render on
                // this tablet's hardware decoder specifically (see MILESTONES.md).
                codec = MediaCodec.createDecoderByType(MediaFormat.MIMETYPE_VIDEO_AVC)
                Log.i(tag, "using decoder: ${codec!!.name}")
                // The standard KEY_LOW_LATENCY key is a no-op on Samsung's
                // Exynos decoders -- they need a vendor-specific parameter
                // instead. Confirmed against moonlight-android (a mature
                // game-streaming app solving the exact same problem), which
                // special-cases "c2.exynos"/"omx.exynos" decoder names with
                // this exact key.
                if (codec!!.name.startsWith("c2.exynos") || codec!!.name.startsWith("omx.exynos")) {
                    Log.i(tag, "applying Exynos vendor low-latency parameter")
                    format.setInteger("vendor.rtc-ext-dec-low-latency.enable", 1)
                }
                // KEY_PRIORITY=0 (realtime) -- another moonlight-android
                // lever: asks the codec to guarantee real-time performance
                // rather than optimizing for throughput/power. They gate
                // KEY_OPERATING_RATE to Qualcomm specifically (official docs
                // agree: "some Qualcomm platforms"), so not tried here on
                // this Exynos decoder, but KEY_PRIORITY is applied more
                // broadly on their side and is worth testing on its own.
                format.setInteger(MediaFormat.KEY_PRIORITY, 0)
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
                var latencySampleCount = 0L
                // FIFO of send-timestamps for frames queued into the decoder
                // but not yet rendered -- the decoder buffers several frames
                // internally (confirmed live: `rendered` trailing `queued`
                // by ~5-6 at steady state) before it starts producing
                // output, so "the frame we just read this loop iteration" is
                // NOT the frame that actually renders on this iteration. All
                // frames are independent IDR with no reordering, so FIFO
                // order is correct.
                val pendingSentTimesMs = ArrayDeque<Long>()

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
                        pendingSentTimesMs.addLast(frameSentAtMs)
                        queuedCount++
                    } else {
                        Log.w(tag, "no input buffer available (dequeueInputBuffer=$inIndex)")
                    }

                    var latencyMs = 0L
                    var outIndex = codec!!.dequeueOutputBuffer(bufferInfo, 10_000)
                    while (true) {
                        when {
                            outIndex >= 0 -> {
                                codec!!.releaseOutputBuffer(outIndex, true)
                                renderedCount++
                                // Milestone 7 fix: match this render to the
                                // send-timestamp of the frame that actually
                                // fed it (FIFO, oldest pending), not
                                // whatever frame this loop iteration just
                                // happened to read off the socket -- the
                                // decoder buffers several frames internally,
                                // so those aren't the same frame.
                                pendingSentTimesMs.removeFirstOrNull()?.let { sentAtMs ->
                                    latencyMs = (System.currentTimeMillis() - clockOffsetMs) - sentAtMs
                                    latencySumMs += latencyMs
                                    if (latencyMs < latencyMinMs) latencyMinMs = latencyMs
                                    if (latencyMs > latencyMaxMs) latencyMaxMs = latencyMs
                                    latencySampleCount++
                                }
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

                    frameCount++
                    if (frameCount == 1L || frameCount % 30 == 0L) {
                        val avg = if (latencySampleCount > 0) latencySumMs / latencySampleCount else 0
                        Log.i(
                            tag,
                            "frame $frameCount ($length bytes): queued=$queuedCount rendered=$renderedCount, " +
                                "pending=${pendingSentTimesMs.size} latency avg=${avg}ms min=${latencyMinMs}ms max=${latencyMaxMs}ms (this frame: ${latencyMs}ms)"
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
        private const val EV_BUTTON_DOWN = 6
        private const val EV_BUTTON_UP = 7
    }
}
