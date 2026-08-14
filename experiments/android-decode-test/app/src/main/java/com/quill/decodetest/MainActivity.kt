package com.quill.decodetest

import android.app.Activity
import android.content.Intent
import android.hardware.usb.UsbAccessory
import android.hardware.usb.UsbManager
import android.media.MediaCodec
import android.media.MediaCodecInfo
import android.media.MediaFormat
import android.os.Bundle
import android.util.Log
import android.view.InputDevice
import android.view.MotionEvent
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.widget.FrameLayout
import android.widget.TextView
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import java.io.BufferedOutputStream
import java.io.DataOutputStream
import java.io.FileInputStream
import java.io.FileOutputStream
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

    private var decodeThread: Thread? = null
    private var inputWriterThread: Thread? = null
    private val eventQueue = LinkedBlockingQueue<PenEvent>()

    // Visible connection-state overlay, not a silent retry: the daemon has
    // no way to detect a stale AOA session and reset itself (confirmed
    // live -- it just keeps reading/writing into a permanently desynced
    // channel after a reconnect, corrupting every retry attempt until it's
    // manually restarted too), so a fully invisible auto-reconnect isn't
    // reliable yet. Telling the user plainly what's happening and what to
    // do about it is more honest than a frozen or black screen that gives
    // no indication anything is wrong.
    private lateinit var statusText: TextView

    @Volatile
    private var output: DataOutputStream? = null

    @Volatile
    private var running = true

    // Milestone 8: set from the launching intent (or a later
    // USB_ACCESSORY_ATTACHED broadcast) when the daemon has switched the
    // tablet into AOA accessory mode -- see daemon/src/aoa.rs. Null means
    // "use the original adb-forward socket path" (Milestones 3-7).
    private var usbAccessory: UsbAccessory? = null

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        usbAccessory = accessoryFromIntent(intent) ?: alreadyAttachedAccessory()
        hideSystemBars()
        val surfaceView = SurfaceView(this)
        statusText = TextView(this).apply {
            setTextColor(android.graphics.Color.WHITE)
            textSize = 20f
            gravity = android.view.Gravity.CENTER
            setPadding(48, 48, 48, 48)
        }
        val root = FrameLayout(this).apply {
            addView(surfaceView, FrameLayout.LayoutParams(FrameLayout.LayoutParams.MATCH_PARENT, FrameLayout.LayoutParams.MATCH_PARENT))
            addView(
                statusText,
                FrameLayout.LayoutParams(FrameLayout.LayoutParams.WRAP_CONTENT, FrameLayout.LayoutParams.WRAP_CONTENT, android.view.Gravity.CENTER)
            )
        }
        setContentView(root)
        showStatus("Waiting for connection...")
        surfaceView.holder.addCallback(this)
        surfaceView.setOnTouchListener { _, event -> handleMotionEvent(event, down = true) }
        surfaceView.setOnHoverListener { _, event -> handleMotionEvent(event, down = false) }
        // ACTION_BUTTON_PRESS/RELEASE (S Pen side button) arrive via the
        // generic-motion stream, not the touch or hover stream -- Android
        // dispatches touch/hover/other-generic-motion as three disjoint
        // paths per MotionEvent, so all three listeners coexist safely with
        // no double-processing of the same event.
        surfaceView.setOnGenericMotionListener { _, event -> handleMotionEvent(event, down = false) }
        // Confirmed live via a real tablet screenshot: two distinct, crisp
        // cursor arrows side by side -- not motion blur/interpolation (that
        // would look like a smear, not two sharp icons). Android draws its
        // own system pointer icon for S Pen hover independent of whatever
        // the app renders, on top of it -- so the real desktop cursor
        // (already embedded in the video by the daemon/KWin) and Android's
        // own hover-pointer icon were both visible at once. Suppressing the
        // system one here: the video's own embedded cursor is the only one
        // that should be visible.
        surfaceView.pointerIcon = android.view.PointerIcon.getSystemIcon(this, android.view.PointerIcon.TYPE_NULL)
    }

    private fun showStatus(message: String) {
        runOnUiThread {
            statusText.text = message
            statusText.visibility = android.view.View.VISIBLE
        }
    }

    private fun hideStatus() {
        runOnUiThread { statusText.visibility = android.view.View.GONE }
    }

    /** Edge-to-edge immersive: hides both the status bar and navigation bar,
     * swipe-from-edge brings them back temporarily (standard "immersive
     * sticky" behavior) -- this is a second-screen monitor replacement, not
     * a phone app, so Android's own chrome should stay out of the way. */
    private fun hideSystemBars() {
        WindowCompat.setDecorFitsSystemWindows(window, false)
        val controller = WindowInsetsControllerCompat(window, window.decorView)
        controller.hide(WindowInsetsCompat.Type.systemBars())
        controller.systemBarsBehavior = WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
    }

    /** The system bars Android brings back on an edge-swipe (or after a
     * dialog/permission prompt steals focus) don't hide themselves again on
     * their own once the transient reveal times out -- re-assert on every
     * focus regain, the standard pattern for sticky immersive mode. */
    override fun onWindowFocusChanged(hasFocus: Boolean) {
        super.onWindowFocusChanged(hasFocus)
        if (hasFocus) hideSystemBars()
    }

    /** Fallback for a manual relaunch (tapping the app icon after the decode
     * loop already exited, e.g. daemon restarted) -- there's no fresh
     * USB_ACCESSORY_ATTACHED intent in that case since the USB device
     * itself never actually detached, so check whether one's already
     * attached and permission already granted (from the original attach
     * event) instead of silently falling back to the adb-forward path. */
    private fun alreadyAttachedAccessory(): UsbAccessory? {
        val usbManager = getSystemService(USB_SERVICE) as UsbManager
        val accessory = usbManager.accessoryList?.firstOrNull() ?: return null
        if (!usbManager.hasPermission(accessory)) {
            Log.w(tag, "accessory attached but no permission -- falling back to adb-forward")
            return null
        }
        return accessory
    }

    private fun accessoryFromIntent(intent: Intent?): UsbAccessory? {
        if (intent?.action != UsbManager.ACTION_USB_ACCESSORY_ATTACHED) return null
        @Suppress("DEPRECATION") // getParcelableExtra(String) is fine pre-Tiramisu-only warning noise here
        return intent.getParcelableExtra(UsbManager.EXTRA_ACCESSORY)
    }

    /** Retries indefinitely instead of running the decode loop once: this is
     * meant to be an always-on second-monitor appliance, not something that
     * needs a manual relaunch every time the connection drops. Two real
     * scenarios that used to require exactly that (confirmed live): the
     * daemon gets restarted (systemd, a manual `systemctl restart`, a USB
     * drop/reconnect) while the app is sitting idle with nothing to retry
     * with, or the app itself gets closed and reopened while an old daemon
     * process is still mid-write on the now-stale connection -- the two
     * sides briefly racing corrupts the fresh handshake (garbage
     * clock-offset, garbage video-format-header, immediate MediaCodec
     * crash) with no way to recover except a full manual relaunch. Retrying
     * here fixes both: whichever side comes up second just waits/retries
     * until the other side's next attempt lines up cleanly.
     *
     * Re-checks `alreadyAttachedAccessory()` fresh on every attempt rather
     * than reusing the `usbAccessory` field captured once at `onCreate` --
     * permission/attachment state is cheap to re-read and this is the
     * actual ground truth for whether AOA is currently usable, not
     * whatever was true when the activity was first created. */
    override fun surfaceCreated(holder: SurfaceHolder) {
        // surfaceCreated can fire more than once per activity lifetime
        // (surface destroyed+recreated on resume, multi-window changes,
        // `am start` bringing an already-running instance back to front
        // instead of spawning a fresh process) -- confirmed live, without
        // this the retry loop below (now long-lived, unlike the original
        // one-shot version) let two decode threads run concurrently
        // against the same AOA connection, corrupting each other exactly
        // like the stale-daemon-process race this whole retry loop was
        // built to fix. Stop any previous thread and wait for it to
        // actually exit before starting a new one -- only ever one
        // decodeThread live at a time.
        running = false
        decodeThread?.interrupt()
        decodeThread?.join(2000)
        running = true

        decodeThread = Thread {
            var attempt = 0
            while (running) {
                attempt++
                // After the first attempt, be explicit that a stuck
                // connection needs a cable replug: the daemon has no way
                // to detect and reset a stale AOA session on its own (see
                // the class doc above), so silently retrying forever with
                // no feedback would leave the user staring at a black
                // screen with no idea what to do.
                showStatus(
                    if (attempt == 1) "Waiting for connection..."
                    else "Waiting for connection... (attempt $attempt)\nIf this doesn't clear in a few seconds, unplug and replug the USB cable."
                )
                try {
                    val accessory = alreadyAttachedAccessory() ?: usbAccessory
                    if (accessory != null) {
                        runAoaDecodeLoop(holder, accessory)
                    } else {
                        runAdbForwardDecodeLoop(holder)
                    }
                } catch (e: Exception) {
                    // Belt-and-suspenders: runDecodeLoop already catches
                    // its own errors, but openAccessory() itself and
                    // alreadyAttachedAccessory() are outside that try/catch
                    // -- confirmed live, an exception there silently killed
                    // this whole retry loop with no further attempts and
                    // no log line, worse than the bug this loop exists to
                    // fix.
                    Log.e(tag, "connection attempt failed", e)
                }
                if (!running) break
                Log.i(tag, "connection ended, retrying in 1s...")
                try {
                    Thread.sleep(1000)
                } catch (e: InterruptedException) {
                    break
                }
            }
        }.also { it.start() }
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
        //
        // `resources.displayMetrics` is the legacy "app usable size" API --
        // it still excludes the system-bar-reserved region even with
        // edge-to-edge active (the bars are hidden, but that reserved
        // region isn't reflected here), which under-reports the real touch
        // surface and made pen/finger position drift increasingly off
        // target towards the bottom of the screen (the S Pen digitizer's
        // real range doesn't care about transient nav-bar visibility).
        // `maximumWindowMetrics` is the modern (API 30+) replacement that
        // always reports the display's true full size.
        val bounds = if (android.os.Build.VERSION.SDK_INT >= 30) {
            windowManager.maximumWindowMetrics.bounds
        } else {
            @Suppress("DEPRECATION")
            val p = android.graphics.Point()
            windowManager.defaultDisplay.getRealSize(p)
            android.graphics.Rect(0, 0, p.x, p.y)
        }
        val widthPx = bounds.width()
        val heightPx = bounds.height()

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
            "handshake: ${widthPx}x${heightPx}px, " +
                "pressure $pMin..$pMax, tilt -$tMaxDeg..$tMaxDeg (stylus device: ${stylusDevice?.name})"
        )

        out.writeInt(widthPx)
        out.writeInt(heightPx)
        out.writeInt(pMin)
        out.writeInt(pMax)
        out.writeInt(-tMaxDeg)
        out.writeInt(tMaxDeg)
        // Milestone 7 clock-sync ping -- see clock_sync.rs on the daemon
        // side for the two-message offset calibration this kicks off.
        out.writeLong(System.currentTimeMillis())
        out.flush()
    }

    /** Minimal replacement for `BufferedInputStream` over the USB accessory
     * fd. `BufferedInputStream` itself can't be used there -- confirmed
     * live: it calls `FileInputStream.available()` as a read optimization,
     * which throws `IOException("Invalid argument")` on this fd type (the
     * FIONREAD ioctl isn't supported on it). But going fully unbuffered is
     * *worse*: USB bulk transfers are packet-oriented, and confirmed live,
     * requesting fewer bytes than a single incoming packet holds silently
     * drops the remainder of that packet instead of queuing it for the next
     * read() call (unlike a stream socket) -- every read after the first
     * then desyncs onto whatever arrives next (observed: `readClockOffset`'s
     * first 8-byte field always came back correct, every field after it did
     * not, byte-for-byte reproducible across runs). Always refilling from a
     * buffer at least as large as the daemon's largest single write (video
     * frames can be ~100KB) avoids both problems at once. */
    private class BufferedAccessoryInput(private val underlying: java.io.InputStream, size: Int = 1 shl 20) {
        private val buf = ByteArray(size)
        private var pos = 0
        private var limit = 0

        /** Force-unblocks a read() that's currently stuck waiting for bytes
         * that will never come (e.g. the daemon process died without the
         * underlying USB transport itself signaling an error) -- closing
         * the stream out from under a blocked read is the standard way to
         * make it throw instead of hanging forever. Called only from the
         * watchdog thread, never from the thread actually doing the
         * reading. */
        fun close() {
            try {
                underlying.close()
            } catch (e: Exception) {
                // Already closed/broken -- fine, that's the whole point.
            }
        }

        private fun fill() {
            val n = underlying.read(buf, 0, buf.size)
            if (n < 0) throw java.io.EOFException("accessory stream closed")
            pos = 0
            limit = n
        }

        fun readExact(len: Int): ByteArray {
            val result = ByteArray(len)
            var got = 0
            while (got < len) {
                if (pos >= limit) fill()
                val n = minOf(limit - pos, len - got)
                System.arraycopy(buf, pos, result, got, n)
                pos += n
                got += n
            }
            return result
        }

        fun readLong(): Long {
            val b = readExact(8)
            var v = 0L
            for (i in 0 until 8) v = (v shl 8) or (b[i].toLong() and 0xFF)
            return v
        }

        fun readInt(): Int {
            val b = readExact(4)
            var v = 0
            for (i in 0 until 4) v = (v shl 8) or (b[i].toInt() and 0xFF)
            return v
        }
    }

    /**
     * Reads the daemon's clock-sync reply (sent once, before any video
     * frame) and computes the android-clock-minus-daemon-clock offset via
     * the standard NTP two-message estimate -- see clock_sync.rs for the
     * derivation. Assumes symmetric one-way transport delay, reasonable for
     * a single local adb-forward/USB link.
     */
    private fun readClockOffset(input: BufferedAccessoryInput): Long {
        val daemonSendMs = input.readLong()
        val androidSendEchoMs = input.readLong()
        val daemonRecvMs = input.readLong()
        val androidRecvMs = System.currentTimeMillis()
        val offset = ((androidRecvMs - daemonSendMs) - (daemonRecvMs - androidSendEchoMs)) / 2
        val roundTripSum = (daemonRecvMs - androidSendEchoMs) + (androidRecvMs - daemonSendMs)
        Log.i(tag, "clock-sync: offset=${offset}ms (android-daemon), round-trip sum=${roundTripSum}ms")
        return offset
    }

    /** Reads the video resolution the daemon actually negotiated for its
     * virtual monitor (sent once, right after the clock-sync reply and
     * before the first video frame -- see `portal_capture.rs`'s
     * `param_changed` handler). Not a fixed constant: the host's virtual
     * monitor size isn't under this app's control and shouldn't be assumed. */
    private fun readVideoFormat(input: BufferedAccessoryInput): Pair<Int, Int> {
        val videoWidth = input.readInt()
        val videoHeight = input.readInt()
        Log.i(tag, "video format: ${videoWidth}x${videoHeight}")
        return videoWidth to videoHeight
    }

    /** Original transport (Milestones 3-7): listens on a TCP port reached
     * via `adb forward tcp:PORT tcp:PORT`. */
    private fun runAdbForwardDecodeLoop(holder: SurfaceHolder) {
        Log.i(tag, "Listening on port $port, waiting for daemon (adb forward)...")
        try {
            ServerSocket(port).use { server ->
                server.reuseAddress = true
                val socket: Socket = server.accept()
                Log.i(tag, "daemon connected from ${socket.remoteSocketAddress}")
                val input = BufferedAccessoryInput(socket.getInputStream())
                val out = DataOutputStream(BufferedOutputStream(socket.getOutputStream()))
                runDecodeLoop(holder, input, out)
            }
        } catch (e: Exception) {
            Log.e(tag, "adb-forward decode loop error", e)
        }
    }

    /** Milestone 8: talks directly to the daemon over raw USB via the
     * Android Open Accessory framework, no adb involved at all -- the
     * daemon (daemon/src/aoa.rs) already switched the device into
     * accessory mode before this activity was even launched (that's what
     * the USB_ACCESSORY_ATTACHED intent means). */
    private fun runAoaDecodeLoop(holder: SurfaceHolder, accessory: UsbAccessory) {
        Log.i(tag, "Opening USB accessory: ${accessory.manufacturer}/${accessory.model}")
        val usbManager = getSystemService(USB_SERVICE) as UsbManager
        val pfd = usbManager.openAccessory(accessory)
        if (pfd == null) {
            Log.e(tag, "openAccessory returned null -- permission not granted?")
            return
        }
        pfd.use {
            val fd = it.fileDescriptor
            val input = BufferedAccessoryInput(FileInputStream(fd))
            val out = DataOutputStream(BufferedOutputStream(FileOutputStream(fd)))
            try {
                runDecodeLoop(holder, input, out)
            } catch (e: Exception) {
                Log.e(tag, "AOA decode loop error", e)
            }
        }
    }

    /** Shared protocol logic -- handshake, clock-sync, video decode/render,
     * input writer thread -- identical regardless of which transport
     * carried the bytes in (`input`/`out` are already open and connected). */
    // Confirmed live: a plain blocking InputStream.read() has no timeout at
    // all, and when the daemon process dies without the underlying USB
    // transport itself signaling an error (it doesn't, reliably), the read
    // just hangs forever -- frozen on the last frame, no exception, so the
    // retry loop in surfaceCreated never even gets a chance to fire (it's
    // gated on runDecodeLoop actually returning). This watchdog force-closes
    // the stream after too long without any data, which does unblock a
    // pending read with an IOException the existing retry/status-overlay
    // machinery already handles correctly. Needs the daemon's periodic
    // heartbeat (portal_capture.rs's run_capture, ~800ms interval) to avoid
    // false-positiving during a legitimately idle screen (no motion = no
    // new video frames at all, expected -- see MILESTONES.md) -- the
    // timeout here just needs to be comfortably longer than that interval.
    private val WATCHDOG_TIMEOUT_MS = 3000L

    private fun runDecodeLoop(holder: SurfaceHolder, input: BufferedAccessoryInput, out: DataOutputStream) {
        var codec: MediaCodec? = null
        val lastDataAtMs = java.util.concurrent.atomic.AtomicLong(System.currentTimeMillis())
        val watchdogRunning = java.util.concurrent.atomic.AtomicBoolean(true)
        val watchdogThread = Thread {
            while (watchdogRunning.get()) {
                if (System.currentTimeMillis() - lastDataAtMs.get() > WATCHDOG_TIMEOUT_MS) {
                    Log.w(tag, "no data for ${WATCHDOG_TIMEOUT_MS}ms, forcing reconnect")
                    input.close()
                    return@Thread
                }
                try {
                    Thread.sleep(500)
                } catch (e: InterruptedException) {
                    return@Thread
                }
            }
        }.also { it.start() }
        try {
            run {
                output = out
                sendHandshake(out)
                val clockOffsetMs = readClockOffset(input)
                val (width, height) = readVideoFormat(input)
                lastDataAtMs.set(System.currentTimeMillis())
                inputWriterThread = Thread { runInputWriterLoop(out) }.also { it.start() }

                // Diagnostic: enumerate every AVC decoder this device offers,
                // hardware and software, so we know what alternatives even
                // exist before assuming the default hardware one is the
                // only option.
                for (info in android.media.MediaCodecList(android.media.MediaCodecList.ALL_CODECS).codecInfos) {
                    if (!info.isEncoder && info.supportedTypes.contains(MediaFormat.MIMETYPE_VIDEO_AVC)) {
                        Log.i(tag, "available AVC decoder: ${info.name} hw=${info.isHardwareAccelerated} sw=${info.isSoftwareOnly}")
                    }
                }

                val format = MediaFormat.createVideoFormat(MediaFormat.MIMETYPE_VIDEO_AVC, width, height)
                // Standard KEY_LOW_LATENCY re-verified with the corrected
                // (FIFO-based) latency measurement (Milestone 7): genuinely
                // no effect on this decoder. Left enabled anyway -- harmless
                // here, other decoders/devices may honor it.
                format.setInteger(MediaFormat.KEY_LOW_LATENCY, 1)
                // Deliberately the software decoder, not the device default
                // hardware one (c2.exynos.h264.decoder). The hardware ASIC's
                // internal pipeline depth turned out to be a fixed ~85-115ms
                // floor no MediaCodec configuration could touch (Milestone
                // 7); live-tested this software decoder at ~56-86ms with no
                // visible quality loss -- a real, measured ~20-30ms win.
                // Trade-off accepted deliberately: software decode costs far
                // more CPU/battery/heat than the hardware ASIC, unverified
                // over long sessions -- worth watching if this becomes a
                // real-world battery/thermal problem later.
                codec = MediaCodec.createByCodecName("c2.android.avc.decoder")
                Log.i(tag, "using decoder: ${codec!!.name}")
                // Definitive answer, not inference from behavior: per
                // source.android.com's own low-latency-media doc, SoC
                // partners must implement decoder-driver support for this
                // feature -- if they haven't, the flag is simply ignored.
                // Ask the OS directly whether this decoder's driver claims
                // support at all.
                val lowLatencySupported = try {
                    codec!!.codecInfo
                        .getCapabilitiesForType(MediaFormat.MIMETYPE_VIDEO_AVC)
                        .isFeatureSupported(MediaCodecInfo.CodecCapabilities.FEATURE_LowLatency)
                } catch (e: Exception) {
                    Log.w(tag, "failed to query FEATURE_LowLatency support", e)
                    null
                }
                Log.i(tag, "decoder FEATURE_LowLatency supported: $lowLatencySupported")
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
                    if (length > 16 * 1024 * 1024) {
                        Log.w(tag, "bogus frame length $length, stopping")
                        break
                    }
                    lastDataAtMs.set(System.currentTimeMillis())
                    if (length == 0) {
                        // Heartbeat (portal_capture.rs's run_capture, ~800ms
                        // interval) -- no payload, just proof the daemon is
                        // still alive during a legitimately idle screen
                        // (no motion = no new video frames at all,
                        // expected). Nothing to decode, just keep looping.
                        continue
                    }
                    val frameBytes = input.readExact(length)

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
                    // Confirmed live via a real tablet screenshot: two
                    // crisp, non-blurred cursor icons visible at once --
                    // not a compositor/encode artifact (both the daemon's
                    // raw captured frame and its encoded H.264 output were
                    // independently verified clean for the same motion).
                    // Root cause: this loop used to call
                    // releaseOutputBuffer(index, render=true) on every
                    // single output buffer it found ready, with zero
                    // pacing. Right as motion stops, several real frames
                    // (queued during the stop) land in a tight burst --
                    // rendering all of them back to back, faster than the
                    // panel/eye can cleanly separate them, showed as two
                    // overlapping valid frames ("ghosting" that got worse
                    // the faster frames arrived -- confirmed live, it
                    // shrank when the pipeline was artificially throttled).
                    // Same fix as the daemon's own capture-side drop-stale
                    // logic: only *render* the newest buffer in a burst,
                    // discard (don't render) the rest.
                    var outIndex = codec!!.dequeueOutputBuffer(bufferInfo, 10_000)
                    var pendingRenderIndex = -1
                    var lastSentAtMs: Long? = null
                    while (true) {
                        when {
                            outIndex >= 0 -> {
                                if (pendingRenderIndex >= 0) {
                                    // An older buffer from this same burst --
                                    // release without rendering it.
                                    codec!!.releaseOutputBuffer(pendingRenderIndex, false)
                                }
                                pendingRenderIndex = outIndex
                                // One decoded buffer == one queued input
                                // frame, rendered or not -- pop the FIFO
                                // here (every buffer, not just the one that
                                // ends up rendered) or it desyncs from
                                // queuedCount over time.
                                lastSentAtMs = pendingSentTimesMs.removeFirstOrNull() ?: lastSentAtMs
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
                    if (pendingRenderIndex >= 0) {
                        codec!!.releaseOutputBuffer(pendingRenderIndex, true)
                        renderedCount++
                        if (renderedCount == 1L) hideStatus()
                        // Milestone 7 fix: match this render to the
                        // send-timestamp of the frame that actually fed it
                        // (FIFO, oldest pending), not whatever frame this
                        // loop iteration just happened to read off the
                        // socket -- the decoder buffers several frames
                        // internally, so those aren't the same frame.
                        lastSentAtMs?.let { sentAtMs ->
                            latencyMs = (System.currentTimeMillis() - clockOffsetMs) - sentAtMs
                            latencySumMs += latencyMs
                            if (latencyMs < latencyMinMs) latencyMinMs = latencyMs
                            if (latencyMs > latencyMaxMs) latencyMaxMs = latencyMs
                            latencySampleCount++
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
            watchdogRunning.set(false)
            watchdogThread.interrupt()
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
