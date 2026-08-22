package com.quill.client

import android.app.Activity
import android.content.Intent
import android.content.pm.ActivityInfo
import android.content.res.Configuration
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
import android.view.WindowManager
import android.widget.FrameLayout
import android.widget.TextView
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import java.io.BufferedOutputStream
import java.io.DataOutputStream
import java.io.FileInputStream
import java.io.FileOutputStream
import java.net.InetAddress
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
    private val tag = "Quill"
    private val port = 7777

    /** Exactly one daemon ever connects, and it's accepted immediately -- there
     * is nothing for a queue to hold. */
    private val backlog = 1

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

    /** Only used in client-side cursor mode; otherwise stays empty and hidden. */
    private lateinit var surfaceView: SurfaceView

    private lateinit var cursorOverlay: CursorOverlay

    private lateinit var latencyOverlay: LatencyOverlay

    /** The one settings entry point that survives streaming -- see [GearButton]. */
    private lateinit var gearButton: GearButton

    @Volatile
    private var output: DataOutputStream? = null

    @Volatile
    private var running = true

    /** True between the first rendered frame and the next connection drop --
     * i.e. exactly while a desktop is actually on the panel. Drives
     * [setScreenAwake]; see [applyLocalSettings]. */
    @Volatile
    private var rendering = false

    /** What the last handshake asked the daemon for, so [checkVideoFormat] can
     * tell whether the daemon understood the rotation it was sent. */
    @Volatile
    private var requestedMonitorWidth = 0
    @Volatile
    private var requestedMonitorHeight = 0

    // Milestone 8: set from the launching intent (or a later
    // USB_ACCESSORY_ATTACHED broadcast) when the daemon has switched the
    // tablet into AOA accessory mode -- see daemon/src/aoa.rs. Null means
    // "use the original adb-forward socket path" (Milestones 3-7).
    private var usbAccessory: UsbAccessory? = null

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        // Launch-time orientation choice, not live-switchable (MILESTONES.md
        // Milestone 15): lock to whichever way the tablet is physically
        // held right now, before any connection/handshake activity starts
        // below. The manifest's `configChanges` already tells Android not
        // to recreate this activity on a later physical rotation, so this
        // lock holds for the whole session -- rotating the tablet after
        // this point does nothing, by design.
        requestedOrientation = if (resources.configuration.orientation == Configuration.ORIENTATION_PORTRAIT) {
            ActivityInfo.SCREEN_ORIENTATION_PORTRAIT
        } else {
            ActivityInfo.SCREEN_ORIENTATION_LANDSCAPE
        }
        usbAccessory = accessoryFromIntent(intent) ?: alreadyAttachedAccessory()
        hideSystemBars()
        surfaceView = SurfaceView(this)
        statusText = TextView(this).apply {
            setTextColor(android.graphics.Color.WHITE)
            textSize = 20f
            gravity = android.view.Gravity.CENTER
            setPadding(48, 48, 48, 48)
        }
        // Its flip is seeded by `applyLocalSettings` below, not here: returning
        // from settings recreates the *surface*, not this activity, so an
        // onCreate-only read went stale the moment anyone changed the setting.
        cursorOverlay = CursorOverlay(this)
        latencyOverlay = LatencyOverlay(this)
        gearButton = GearButton(this)
        val gearSize = (GEAR_SIZE_DP * resources.displayMetrics.density).toInt()
        val root = FrameLayout(this).apply {
            addView(surfaceView, FrameLayout.LayoutParams(FrameLayout.LayoutParams.MATCH_PARENT, FrameLayout.LayoutParams.MATCH_PARENT))
            // Above the video, below the status text.
            addView(cursorOverlay, FrameLayout.LayoutParams(FrameLayout.LayoutParams.MATCH_PARENT, FrameLayout.LayoutParams.MATCH_PARENT))
            // Above the pointer so it stays readable, below the status text so
            // a "replug the cable" message is never hidden by a diagnostic, and
            // below the gear so nothing can shadow its touch target. Unlike the
            // gear it must not consume input -- see LatencyOverlay's class doc.
            addView(latencyOverlay, FrameLayout.LayoutParams(FrameLayout.LayoutParams.MATCH_PARENT, FrameLayout.LayoutParams.MATCH_PARENT))
            addView(
                statusText,
                FrameLayout.LayoutParams(FrameLayout.LayoutParams.WRAP_CONTENT, FrameLayout.LayoutParams.WRAP_CONTENT, android.view.Gravity.CENTER)
            )
            // Topmost, so it gets first crack at touch/hover dispatch and can
            // consume its own taps before the SurfaceView's forwarder (which
            // hit-tests nothing) turns them into desktop clicks.
            //
            // Laid out at the origin with no margins on purpose: GearButton
            // owns its own position now (it is draggable and remembers where it
            // was parked), and drives it entirely through translation so a drag
            // costs no layout pass over the live video surface. The insets it
            // keeps from each edge live there too -- see its class doc for why
            // the top and bottom ones are bigger.
            addView(
                gearButton,
                FrameLayout.LayoutParams(gearSize, gearSize, android.view.Gravity.TOP or android.view.Gravity.START)
            )
        }
        setContentView(root)
        // The app runs edge-to-edge immersive with no system chrome, so the
        // status overlay doubles as the way into settings -- there is nowhere
        // else to hang a menu. It only exists while disconnected, though
        // (`hideStatus` on the first rendered frame), which is why the gear
        // above it stays put for the streaming case.
        statusText.setOnClickListener { openSettings() }
        gearButton.setOnClickListener { openSettings() }
        gearButton.fadeSoon()
        showStatus("Waiting for connection...\n(tap here for settings)")
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
        applyLocalSettings()
    }

    /** Re-reads the tablet-local settings every time this screen comes back.
     *
     * Opening settings destroys the *surface*, not this activity, so anything
     * seeded once in [onCreate] kept its old value for the life of the process
     * -- which is what made a changed 180-degree flip reach the daemon (it
     * rides the next handshake) but not the tablet-drawn pointer. Reading here
     * covers the settings round trip and an activity recreation alike, and
     * needs no result plumbing: SharedPreferences is already the single source
     * of truth for both sides. */
    private fun applyLocalSettings() {
        val settings = Settings(this)
        cursorOverlay.setRotation(settings.rotationDegrees)
        showLatency = settings.showLatencyOverlay
        sideButtonAction = settings.sideButtonAction
        latencyOverlay.visibility =
            if (showLatency) android.view.View.VISIBLE else android.view.View.GONE
        setScreenAwake(rendering)
    }

    /** Mirrors `Settings.showLatencyOverlay` so the render thread can skip the
     * per-frame handoff with a single field read rather than touching
     * SharedPreferences or the view. */
    @Volatile
    private var showLatency = false

    override fun onResume() {
        super.onResume()
        applyLocalSettings()
        // Belt and braces with SettingsActivity.onDestroy: whichever runs,
        // the captured frame does not outlive the screen that displayed it.
        FramePreview.clear()
    }

    /** Keeps the panel lit while, and only while, a desktop is on it.
     *
     * `FLAG_KEEP_SCREEN_ON` rather than a `PowerManager.WakeLock` because it
     * needs no permission, and on the window rather than on the SurfaceView
     * because that surface is destroyed and recreated on every trip through
     * settings -- one lifecycle to reason about instead of two.
     *
     * Deliberately not driven by frame arrival. An idle desktop sends no
     * frames at all (that is what MSG_HEARTBEAT exists for), and "you paused to
     * think" is precisely the idle case this exists to survive. It is armed on
     * the first rendered frame and disarmed when the connection drops, which is
     * what [showStatus]/[hideStatus] already mean.
     *
     * The flag only acts while the window is visible and focused, so the
     * settings round trip needs no handling of its own: backgrounding drops it,
     * and coming back re-arms through [onResume] and the reconnect. */
    private fun setScreenAwake(on: Boolean) {
        if (on && Settings(this).keepScreenAwake) {
            window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        } else {
            window.clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        }
    }

    /** Settings apply at connect time, so leaving here and coming back is what
     * makes a changed toggle take effect -- the surface is destroyed and the
     * decode loop reconnects on return.
     *
     * Which is also why the frame grab happens here rather than over there:
     * by the time `SettingsActivity` is running, the surface it would read is
     * already gone. `captureThen` runs the callback exactly once, on whichever
     * of the copy or its timeout lands first, so a stalled readback costs the
     * preview and never the tap. */
    private fun openSettings() {
        FramePreview.captureThen(surfaceView, window.decorView.handler) {
            startActivity(Intent(this, SettingsActivity::class.java))
        }
    }

    private fun showStatus(message: String) {
        runOnUiThread {
            statusText.text = message
            statusText.visibility = android.view.View.VISIBLE
            rendering = false
            setScreenAwake(false)
        }
    }

    private fun hideStatus() {
        runOnUiThread {
            statusText.visibility = android.view.View.GONE
            rendering = true
            setScreenAwake(true)
        }
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
        if (hasFocus) {
            hideSystemBars()
            // Coming back from settings (or any dialog): show the gear at full
            // opacity briefly so it's obvious where it lives, then dim again.
            if (::gearButton.isInitialized) gearButton.wake()
        }
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
            // This thread does the USB read and feeds the decoder; anything
            // that delays it delays every frame behind it. Same priority
            // moonlight and project-monitorize give their equivalent threads.
            android.os.Process.setThreadPriority(android.os.Process.THREAD_PRIORITY_URGENT_DISPLAY)
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

    /** Mirrors `Settings.sideButtonAction`, so the pen path reads a field
     * rather than SharedPreferences on every event. Refreshed by
     * [applyLocalSettings]. */
    @Volatile
    private var sideButtonAction = Settings.SIDE_BUTTON_RIGHT

    /** Panel pixels to monitor pixels. 1.0 unless the desktop is smaller than
     * the panel; see [send]. Fixed for the life of a connection, since the
     * monitor it refers to is created at connect time. */
    @Volatile
    private var workspaceScale = 1f

    // --- Multi-touch state (Milestone 9). UI thread only, like the field above.

    /** Android pointer id -> multitouch slot, stable for the life of a contact. */
    private val slotOfPointer = HashMap<Int, Int>()

    /** True from the moment a second finger lands until the last one lifts.
     * Once a gesture starts, the remaining fingers keep going to the touchpad
     * even as the count drops back to one -- resuming absolute dragging
     * mid-gesture would jump the cursor to whichever finger happened to
     * survive. */
    private var gestureActive = false

    private val longPressHandler = android.os.Handler(android.os.Looper.getMainLooper())
    private var longPressPending: Runnable? = null
    private var longPressFired = false
    private var longPressAnchor = 0f to 0f

    /**
     * The single choke point every coordinate passes through, which is why the
     * workspace scale is applied here rather than at each of the ten call
     * sites.
     *
     * The daemon's axes are declared against the *monitor*, not the panel, so a
     * desktop smaller than the panel needs the touch position brought into that
     * space. Because the aspect never changes, both axes take the same factor
     * -- including at a quarter turn, where the monitor is the panel
     * transposed and scaled, and the two cancel out to the same single
     * multiply.
     */
    private fun send(type: Int, x: Int, y: Int, pressure: Int = 0, tiltX: Int = 0, tiltY: Int = 0, buttons: Int = 0) {
        // Cheap, non-blocking enqueue on the UI thread; the actual socket
        // write happens on inputWriterThread.
        val scale = workspaceScale
        val sx = if (scale == 1f) x else (x * scale).roundToInt()
        val sy = if (scale == 1f) y else (y * scale).roundToInt()
        eventQueue.offer(PenEvent(type, sx, sy, pressure, tiltX, tiltY, buttons))
    }

    private fun handleMotionEvent(event: MotionEvent, down: Boolean): Boolean {
        if (output == null) return false

        val stylusButtonNow = event.buttonState and MotionEvent.BUTTON_STYLUS_PRIMARY != 0
        if (stylusButtonNow != lastStylusButtonState) {
            lastStylusButtonState = stylusButtonNow
            // The chosen action rides bits 2-3 of the event rather than the
            // handshake, so changing the mapping takes effect on the next press
            // instead of at the next connect. "None" has no wire value: the
            // event is just not sent, which an older daemon also handles
            // correctly by never hearing about it.
            if (sideButtonAction != Settings.SIDE_BUTTON_NONE) {
                send(
                    if (stylusButtonNow) EV_BUTTON_DOWN else EV_BUTTON_UP,
                    event.x.roundToInt(), event.y.roundToInt(),
                    buttons = 1 or (sideButtonAction shl 2),
                )
            }
        }

        // `actionMasked`, not `action`: for the secondary-pointer actions the
        // raw value carries the pointer index in its high byte, so switching on
        // it made ACTION_POINTER_DOWN/UP unmatchable -- which is why every
        // finger past the first was silently dropped before Milestone 9.
        val action = event.actionMasked
        val isFinger = event.getToolType(event.actionIndex) == MotionEvent.TOOL_TYPE_FINGER
        if (isFinger && down) {
            return handleFingerEvent(event, action)
        }

        val type: Int = when (action) {
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
        val buttons = (if (stylusButtonNow) 1 else 0) or (if (isFinger) 2 else 0)
        send(type, event.x.roundToInt(), event.y.roundToInt(), pressureRaw, tiltX, tiltY, buttons)
        return true
    }

    /**
     * The finger half of the input path.
     *
     * One finger keeps the pre-Milestone-9 behaviour exactly: it drives the
     * tablet device absolutely, so a touch lands where you touch. Two or more
     * become real multitouch slots on their own device, which is what lets
     * libinput recognize scroll, pinch and swipe rather than us
     * (`daemon/src/uinput_touchpad.rs`).
     */
    private fun handleFingerEvent(event: MotionEvent, action: Int): Boolean {
        when (action) {
            MotionEvent.ACTION_DOWN -> {
                slotOfPointer.clear()
                gestureActive = false
                longPressFired = false
                send(EV_DOWN, event.x.roundToInt(), event.y.roundToInt(), fingerPressure(event, 0), buttons = 2)
                armLongPress(event.x, event.y)
            }

            MotionEvent.ACTION_POINTER_DOWN -> {
                cancelLongPress()
                if (!gestureActive) {
                    gestureActive = true
                    // Release the absolute contact the first finger was
                    // holding, or the tablet device stays pressed for the whole
                    // gesture and the desktop sees a drag underneath it.
                    send(EV_UP, event.x.roundToInt(), event.y.roundToInt(), buttons = 2)
                    // Then report *every* live contact, including the one that
                    // was already down: libinput needs to see the gesture from
                    // its first frame to classify it at all.
                    for (i in 0 until event.pointerCount) {
                        sendTouch(EV_TOUCH_DOWN, event, i)
                    }
                } else {
                    sendTouch(EV_TOUCH_DOWN, event, event.actionIndex)
                }
            }

            MotionEvent.ACTION_MOVE -> {
                if (gestureActive) {
                    for (i in 0 until event.pointerCount) {
                        sendTouch(EV_TOUCH_MOVE, event, i)
                    }
                } else {
                    val moved = kotlin.math.hypot(
                        event.x - longPressAnchor.first,
                        event.y - longPressAnchor.second
                    )
                    if (moved > longPressSlopPx) cancelLongPress()
                    if (!longPressFired) {
                        send(EV_MOVE, event.x.roundToInt(), event.y.roundToInt(), fingerPressure(event, 0), buttons = 2)
                    }
                }
            }

            MotionEvent.ACTION_POINTER_UP -> {
                if (gestureActive) sendTouch(EV_TOUCH_UP, event, event.actionIndex)
            }

            MotionEvent.ACTION_UP, MotionEvent.ACTION_CANCEL -> {
                cancelLongPress()
                if (gestureActive) {
                    // Lift whatever is still slotted -- ACTION_UP reports only
                    // the last pointer, and a contact left down is a phantom
                    // finger on the pad for the rest of the session.
                    for (i in 0 until event.pointerCount) {
                        sendTouch(EV_TOUCH_UP, event, i)
                    }
                    for (slot in slotOfPointer.values.toList()) {
                        send(EV_TOUCH_UP, 0, 0, slot, buttons = 0)
                    }
                    slotOfPointer.clear()
                    gestureActive = false
                } else if (longPressFired) {
                    // The right click already happened; the left contact was
                    // released when it fired. Nothing left to send.
                    longPressFired = false
                } else {
                    send(EV_UP, event.x.roundToInt(), event.y.roundToInt(), buttons = 2)
                }
            }
        }
        return true
    }

    /** Android's finger "pressure" is a contact-area estimate, not force, but
     * the tablet device needs a nonzero value to move the cursor at all
     * (`daemon/src/uinput_tablet.rs` module doc). */
    private fun fingerPressure(event: MotionEvent, index: Int): Int =
        (event.getPressure(index) * pressureMax).roundToInt().coerceAtLeast(1)

    /** Slots are assigned on first sight and released on lift, so a five-finger
     * mess doesn't permanently consume the four the daemon has. */
    private fun slotFor(pointerId: Int): Int {
        slotOfPointer[pointerId]?.let { return it }
        val used = slotOfPointer.values.toSet()
        val free = (0 until MAX_SLOTS).firstOrNull { it !in used } ?: return -1
        slotOfPointer[pointerId] = free
        return free
    }

    private fun sendTouch(type: Int, event: MotionEvent, index: Int) {
        if (event.getToolType(index) != MotionEvent.TOOL_TYPE_FINGER) return
        val pointerId = event.getPointerId(index)
        val slot = slotFor(pointerId)
        if (slot < 0) return // more fingers than slots; the extras are ignored
        send(
            type,
            event.getX(index).roundToInt(),
            event.getY(index).roundToInt(),
            // The daemon reads the slot out of the pressure field and the
            // contact count out of the buttons byte for these types -- see
            // daemon/src/input_receiver.rs's wire-format comment.
            pressure = slot,
            buttons = event.pointerCount.coerceAtMost(255)
        )
        if (type == EV_TOUCH_UP) slotOfPointer.remove(pointerId)
    }

    private val longPressSlopPx: Float
        get() = LONG_PRESS_SLOP_DP * resources.displayMetrics.density

    /**
     * Press and hold one finger without moving = right click there.
     *
     * The left contact is released first: without that, the press that started
     * the hold is still down, and the desktop would see a drag with a right
     * click inside it. Releasing first costs a plain click before the context
     * menu, which is what a touchscreen user expects anyway (tap selects, hold
     * opens the menu).
     */
    private fun armLongPress(x: Float, y: Float) {
        cancelLongPress()
        longPressAnchor = x to y
        val runnable = Runnable {
            if (gestureActive) return@Runnable
            longPressFired = true
            send(EV_UP, x.roundToInt(), y.roundToInt(), buttons = 2)
            send(EV_RIGHT_DOWN, x.roundToInt(), y.roundToInt())
            send(EV_RIGHT_UP, x.roundToInt(), y.roundToInt())
        }
        longPressPending = runnable
        longPressHandler.postDelayed(runnable, LONG_PRESS_TIMEOUT_MS)
    }

    private fun cancelLongPress() {
        longPressPending?.let { longPressHandler.removeCallbacks(it) }
        longPressPending = null
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
        // The settings screen's preview needs the panel's real geometry for its
        // aspect, and this is where it is already worked out correctly (see the
        // note above on why displayMetrics is not used).
        FramePreview.setPanelSize(widthPx, heightPx)

        // At a quarter turn the daemon is asked for a monitor whose dimensions
        // are the panel's, transposed: a landscape tablet drives a portrait
        // desktop, which the encoder then turns so it fills the panel exactly.
        // Asking for the panel's own shape instead would letterbox a rotated
        // image into it and waste most of the screen.
        val settings = Settings(this)
        val swapAxes = settings.rotationSwapsAxes
        // A desktop smaller than the panel: fewer pixels to lay out, encode and
        // push over the cable, and everything on it is physically bigger. Both
        // axes take the same factor, so the aspect is unchanged and nothing is
        // ever letterboxed.
        val scale = settings.workspaceScalePercent / 100f
        workspaceScale = scale
        val scaledWidth = (widthPx * scale).roundToInt()
        val scaledHeight = (heightPx * scale).roundToInt()
        val monitorWidthPx = if (swapAxes) scaledHeight else scaledWidth
        val monitorHeightPx = if (swapAxes) scaledWidth else scaledHeight
        requestedMonitorWidth = monitorWidthPx
        requestedMonitorHeight = monitorHeightPx
        // The moment these stop being preferences and become what the daemon is
        // actually doing. The settings screen diffs against this to tell a
        // staged change from a settled one.
        SessionConfig.record(settings, widthPx, heightPx)

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
            "handshake: asking for ${monitorWidthPx}x${monitorHeightPx}px " +
                "(panel ${widthPx}x${heightPx}, rotation ${settings.rotationDegrees}deg), " +
                "pressure $pMin..$pMax, tilt -$tMaxDeg..$tMaxDeg (stylus device: ${stylusDevice?.name})"
        )

        // Protocol v2 (see protocol.rs). The magic and version let the daemon
        // reject a stale or mismatched peer outright instead of interpreting
        // whatever bytes arrive as a screen size and acting on them -- which is
        // what v1 did, repeatedly and expensively.
        val body = java.io.ByteArrayOutputStream()
        DataOutputStream(body).apply {
            writeInt(monitorWidthPx)
            writeInt(monitorHeightPx)
            writeInt(pMin)
            writeInt(pMax)
            writeInt(-tMaxDeg)
            writeInt(tMaxDeg)
            // Milestone 7 clock-sync ping -- see clock_sync.rs on the daemon
            // side for the two-message offset calibration this kicks off.
            writeLong(System.currentTimeMillis())
            writeByte(Settings(this@MainActivity).configFlags())
            // Appended in Milestone 9: the virtual touchpad's gesture
            // thresholds are all specified in millimetres, so the daemon needs
            // a real physical resolution or none of them mean anything.
            // `xdpi`/`ydpi` are the panel's true physical density, unlike
            // `densityDpi`, which is the rounded bucket Android uses for
            // layout scaling. Appending is safe by construction -- `body_len`
            // is what makes an older daemon skip what it doesn't know.
            // Swapped along with the dimensions at a quarter turn. These feed
            // the daemon's virtual-touchpad millimetre thresholds (Milestone
            // 9), so leaving them alone here would silently miscalibrate every
            // gesture on both axes -- an easy one to miss, because nothing
            // about the picture would look wrong.
            // Scaled with the workspace: these are dots per inch measured in
            // *monitor* pixels, and a smaller desktop spreads fewer of them
            // over the same glass. The daemon turns pixel deltas into
            // millimetres with these for its touchpad thresholds (Milestone 9),
            // so an unscaled value would misjudge every gesture.
            val xdpi = (resources.displayMetrics.xdpi * scale * 1000).roundToInt()
            val ydpi = (resources.displayMetrics.ydpi * scale * 1000).roundToInt()
            writeInt(if (swapAxes) ydpi else xdpi)
            writeInt(if (swapAxes) xdpi else ydpi)
        }
        val bodyBytes = body.toByteArray()

        out.writeInt(PROTOCOL_MAGIC)
        out.writeShort(PROTOCOL_VERSION)
        out.writeShort(bodyBytes.size)
        out.write(bodyBytes)
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
    private class BufferedAccessoryInput(
        private val underlying: java.io.InputStream,
        /** The `ParcelFileDescriptor` the stream was built from, when there is
         * one. It, not the stream, actually owns the accessory fd -- see
         * [close]. */
        private val owner: java.io.Closeable? = null,
        size: Int = 1 shl 20,
    ) {
        private val buf = ByteArray(size)
        private var pos = 0
        private var limit = 0

        /** Force-unblocks a read() that's currently stuck waiting for bytes
         * that will never come (e.g. the daemon process died without the
         * underlying USB transport itself signaling an error) -- closing
         * the stream out from under a blocked read is the standard way to
         * make it throw instead of hanging forever. Called only from the
         * watchdog thread, never from the thread actually doing the
         * reading.
         *
         * **The `ParcelFileDescriptor` has to be closed too, and it is the one
         * that matters.** Confirmed live: a `FileInputStream` built from
         * `pfd.fileDescriptor` does not own that descriptor, so closing the
         * stream left the blocked `read()` blocked. The watchdog logged
         * "forcing reconnect" and then nothing further ever happened -- the
         * decode thread never returned, the retry loop in `surfaceCreated`
         * never fired, and both sides deadlocked: the app blocked reading, the
         * daemon blocked waiting for a handshake the app would now never send.
         * Only a manual app relaunch recovered it, which is the exact symptom
         * Milestone 11 set out to remove. */
        fun close() {
            try {
                underlying.close()
            } catch (e: Exception) {
                // Already closed/broken -- fine, that's the whole point.
            }
            try {
                owner?.close()
            } catch (e: Exception) {
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
     * Applies a cursor message (see `protocol.rs` for the payload layout).
     *
     * A bitmap is only present when the shape actually changed; the overlay
     * keeps drawing the last one it was given until then, which is why this can
     * pass `null` through without clearing anything.
     */
    private fun applyCursorUpdate(payload: ByteArray) {
        val b = java.io.DataInputStream(payload.inputStream())
        val x = b.readInt()
        val y = b.readInt()
        val visible = b.readByte().toInt() != 0
        val hasBitmap = b.readByte().toInt() != 0
        var bitmap: android.graphics.Bitmap? = null
        var hotX = 0
        var hotY = 0
        if (hasBitmap) {
            val w = b.readInt()
            val h = b.readInt()
            hotX = b.readInt()
            hotY = b.readInt()
            val pixels = ByteArray(w * h * 4)
            b.readFully(pixels)
            // Tightly packed RGBA from the daemon (it repacks away the
            // producer's stride), which is exactly ARGB_8888's byte order on a
            // little-endian device once wrapped.
            bitmap = android.graphics.Bitmap.createBitmap(w, h, android.graphics.Bitmap.Config.ARGB_8888)
            bitmap.copyPixelsFromBuffer(java.nio.ByteBuffer.wrap(pixels))
        }
        cursorOverlay.update(x, y, visible, bitmap, hotX, hotY)
    }

    /** One downstream message: type, daemon send time, and payload. */
    private class Message(val type: Int, val sentAtMs: Long, val payload: ByteArray)

    /**
     * Reads exactly one message. Protocol v2 makes *every* downstream byte part
     * of a typed, length-prefixed message, so this is the only read the loop
     * ever performs -- there are no pre-loop reads left to fall out of step
     * with, which is the entire class of bug that cost Milestones 8, 13, 14, 17
     * and 18.
     */
    private fun readMessage(input: BufferedAccessoryInput): Message {
        val type = input.readExact(1)[0].toInt() and 0xFF
        val sentAtMs = input.readLong()
        val length = input.readInt()
        if (length < 0 || length > 16 * 1024 * 1024) {
            throw java.io.IOException("bogus payload length \$length for message type \$type")
        }
        return Message(type, sentAtMs, if (length == 0) ByteArray(0) else input.readExact(length))
    }

    private fun expect(input: BufferedAccessoryInput, type: Int): Message {
        val m = readMessage(input)
        if (m.type != type) {
            throw java.io.IOException("expected message type \$type, got \${m.type}")
        }
        return m
    }

    /**
     * Computes the android-clock-minus-daemon-clock offset via the standard NTP
     * two-message estimate -- see clock_sync.rs for the derivation. Assumes
     * symmetric one-way transport delay, reasonable for a single local
     * adb-forward/USB link.
     */
    private fun readClockOffset(input: BufferedAccessoryInput): Long {
        val b = java.io.DataInputStream(expect(input, MSG_CLOCK_SYNC).payload.inputStream())
        val daemonSendMs = b.readLong()
        val androidSendEchoMs = b.readLong()
        val daemonRecvMs = b.readLong()
        val androidRecvMs = System.currentTimeMillis()
        val offset = ((androidRecvMs - daemonSendMs) - (daemonRecvMs - androidSendEchoMs)) / 2
        val roundTripSum = (daemonRecvMs - androidSendEchoMs) + (androidRecvMs - daemonSendMs)
        Log.i(tag, "clock-sync: offset=${offset}ms (android-daemon), round-trip sum=${roundTripSum}ms")
        // The overlay needs this to know whether to trust its own numbers; see
        // LatencyOverlay.syncRoundTripMs.
        if (::latencyOverlay.isInitialized) latencyOverlay.setClockSync(roundTripSum)
        return offset
    }

    /** Reads the video resolution the daemon actually negotiated for its
     * virtual monitor (sent once, right after the clock-sync reply and
     * before the first video frame -- see `portal_capture.rs`'s
     * `param_changed` handler). Not a fixed constant: the host's virtual
     * monitor size isn't under this app's control and shouldn't be assumed. */
    private fun readVideoFormat(input: BufferedAccessoryInput): Pair<Int, Int> {
        val b = java.io.DataInputStream(expect(input, MSG_VIDEO_FORMAT).payload.inputStream())
        val videoWidth = b.readInt()
        val videoHeight = b.readInt()
        Log.i(tag, "video format: ${videoWidth}x${videoHeight}")
        checkDaemonUnderstoodRotation(videoWidth, videoHeight)
        return videoWidth to videoHeight
    }

    /**
     * Catches a daemon too old to know about the quarter turns.
     *
     * The rotation is carried as two bits so that 0 and 180 degrees stay
     * bit-for-bit what they always were (see protocol.rs). The cost of that
     * compatibility is one blind spot in the other direction: an old daemon
     * ignores the new bit and honours only the flip, so a client asking for 270
     * gets a bare 180 applied to dimensions that were already swapped for it --
     * a picture that is both the wrong shape and the wrong way up, with nothing
     * saying why.
     *
     * There is no capability field to consult, but there is a fact already on
     * the wire: the encoded video's shape. At a quarter turn we asked for a
     * portrait monitor and the encoder should hand back a landscape stream. If
     * it comes back still portrait, the daemon did not turn anything, so it did
     * not understand. Say so, in words, instead of showing the mess.
     */
    private fun checkDaemonUnderstoodRotation(videoWidth: Int, videoHeight: Int) {
        val settings = Settings(this)
        if (!settings.rotationSwapsAxes) return
        if (requestedMonitorWidth == 0 || requestedMonitorHeight == 0) return
        // We asked for `requested`; a daemon that understood returns its
        // transpose. Anything else means the rotation was dropped.
        val expectedW = requestedMonitorHeight
        val expectedH = requestedMonitorWidth
        if (videoWidth == expectedW && videoHeight == expectedH) return
        Log.w(
            tag,
            "daemon returned ${videoWidth}x${videoHeight} for a " +
                "${requestedMonitorWidth}x${requestedMonitorHeight} monitor at " +
                "${settings.rotationDegrees}deg -- expected ${expectedW}x${expectedH}. " +
                "Falling back to no rotation.",
        )
        settings.rotationDegrees = 0
        showStatus(
            "This computer's Quill daemon is too old for 90° rotation.\n" +
                "Update it, or pick 0° or 180° in settings.\n" +
                "Reconnecting without rotation...",
        )
    }

    /** Original transport (Milestones 3-7): listens on a TCP port reached
     * via `adb forward tcp:PORT tcp:PORT`.
     *
     * Bound to loopback, not every interface. `adbd` runs on the phone and
     * connects to this port from the phone itself, so loopback is all the adb
     * path ever needed -- while a bare `ServerSocket(port)` binds 0.0.0.0, and
     * this is the automatic fallback whenever the app is open with no USB
     * accessory attached. That left any peer on the same Wi-Fi able to connect
     * unauthenticated, feed arbitrary bytes to the H.264 decoder below, and
     * receive the S Pen event stream. */
    private fun runAdbForwardDecodeLoop(holder: SurfaceHolder) {
        Log.i(tag, "Listening on 127.0.0.1:$port, waiting for daemon (adb forward)...")
        try {
            ServerSocket(port, backlog, InetAddress.getLoopbackAddress()).use { server ->
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
            val input = BufferedAccessoryInput(FileInputStream(fd), it)
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
    //
    // Confirmed live (MILESTONES.md, Milestone 16): 3000ms was too tight
    // once orientation::ensure() (daemon side) could recreate the virtual
    // monitor -- a real ~3-4s of silence before the portal/heartbeat loop
    // even starts, since no heartbeat exists until PipeWire is actually
    // streaming. The watchdog firing mid-setup forced a reconnect that
    // raced the daemon's still-in-progress connection, producing garbage
    // handshake/video-format reads (the same connection-reuse framing
    // desync already flagged elsewhere in MILESTONES.md) instead of a
    // clean retry. Bumped well past worst-case recreate time (teardown
    // timeout + ready timeout + portal renegotiation, ~8-10s worst case).
    private val WATCHDOG_TIMEOUT_MS = 15000L

    // Before the first frame arrives, the daemon may legitimately be blocked on
    // a human: the portal's screen-picker dialog appears whenever the saved
    // restore token is missing or rejected, and someone has to walk over and
    // click it. Confirmed live -- a ~37s pick tripped the 15s watchdog, which
    // forced a reconnect whose clock-sync then calibrated against a reply that
    // had been sitting queued (round-trip sum 1119ms instead of 10ms, offset
    // 52ms instead of 610ms). Video was fine; every reported per-frame latency
    // was off by ~550ms for the rest of the session.
    //
    // A separate, much longer budget until the stream is actually running fixes
    // that without blunting the steady-state watchdog, which is the one that
    // matters for detecting a peer that died mid-session.
    private val WATCHDOG_STARTUP_TIMEOUT_MS = 180000L

    private fun runDecodeLoop(holder: SurfaceHolder, input: BufferedAccessoryInput, out: DataOutputStream) {
        var codec: MediaCodec? = null
        val lastDataAtMs = java.util.concurrent.atomic.AtomicLong(System.currentTimeMillis())
        val watchdogRunning = java.util.concurrent.atomic.AtomicBoolean(true)
        // Flips once real video starts, switching the watchdog from its
        // human-in-the-loop startup budget to the steady-state one.
        val streaming = java.util.concurrent.atomic.AtomicBoolean(false)
        val watchdogThread = Thread {
            while (watchdogRunning.get()) {
                val budget =
                    if (streaming.get()) WATCHDOG_TIMEOUT_MS else WATCHDOG_STARTUP_TIMEOUT_MS
                if (System.currentTimeMillis() - lastDataAtMs.get() > budget) {
                    Log.w(tag, "no data for ${budget}ms, forcing reconnect")
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
                runOnUiThread { cursorOverlay.setVideoSize(width, height) }
                lastDataAtMs.set(System.currentTimeMillis())
                inputWriterThread = Thread { runInputWriterLoop(out) }.also { it.start() }

                // Enumerate every AVC decoder this device offers, hardware
                // and software, both to log what's available and to pick a
                // hardware one below without hardcoding a vendor-specific
                // name (project rule: any AOA-capable Android device should
                // work here, not just this one Exynos tablet).
                var hwDecoderName: String? = null
                for (info in android.media.MediaCodecList(android.media.MediaCodecList.ALL_CODECS).codecInfos) {
                    if (!info.isEncoder && info.supportedTypes.contains(MediaFormat.MIMETYPE_VIDEO_AVC)) {
                        Log.i(tag, "available AVC decoder: ${info.name} hw=${info.isHardwareAccelerated} sw=${info.isSoftwareOnly}")
                        if (hwDecoderName == null && info.isHardwareAccelerated) {
                            hwDecoderName = info.name
                        }
                    }
                }

                val format = MediaFormat.createVideoFormat(MediaFormat.MIMETYPE_VIDEO_AVC, width, height)
                // Sized so the codec never has to reallocate an input buffer
                // mid-stream. The wire protocol already refuses anything above
                // 16MB, and real frames are 200 bytes to a few KB, so this is
                // headroom rather than a real allocation target.
                format.setInteger(MediaFormat.KEY_MAX_INPUT_SIZE, 1 shl 20)
                // Standard KEY_LOW_LATENCY re-verified with the corrected
                // (FIFO-based) latency measurement (Milestone 7): genuinely
                // no effect on this decoder. Left enabled anyway -- harmless
                // here, other decoders/devices may honor it.
                format.setInteger(MediaFormat.KEY_LOW_LATENCY, 1)
                // Milestone 7 measured the software decoder (c2.android.avc.decoder)
                // as a real ~20-30ms per-frame latency win over hardware at
                // the 1920x1080-or-smaller resolutions tested then. That
                // measurement doesn't hold at the tablet's real native
                // resolution (2560x1600, only reachable once dynamic
                // resolution + real-panel-size capture landed): software
                // decode can't sustain 60fps at ~3x the pixel count,
                // confirmed live via dequeueInputBuffer starvation dropping
                // ~80% of incoming frames -- the actual cause of a
                // "extremely laggy" report, not encode/transport (both
                // measured clean up to this point). Hardware decode back on
                // by default; software left as the explicit fallback for
                // devices with no hardware AVC decoder at all.
                codec = MediaCodec.createByCodecName(hwDecoderName ?: "c2.android.avc.decoder")
                Log.i(tag, "using decoder: ${codec!!.name} (hardware=${hwDecoderName != null})")
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

                // FIFO of send-timestamps for frames queued into the decoder
                // but not yet emitted -- the decoder buffers several frames
                // internally before it starts producing output, so "the frame
                // we just read" is NOT the frame that renders next. Concurrent
                // because the reader thread pushes and the render thread pops;
                // there is no reordering in this stream (IPPP, no B-frames), so
                // FIFO order matches decode order.
                val pendingSentTimesMs = java.util.concurrent.ConcurrentLinkedQueue<Long>()
                val queuedCount = java.util.concurrent.atomic.AtomicLong(0)

                // The whole point of splitting reader from renderer.
                //
                // This used to be one loop: read a frame off the socket, queue
                // it, then poll for output. That meant a decoded frame could
                // only ever be rendered on an iteration that *also* received a
                // new frame over USB -- so a frame the decoder finished 2ms
                // after it was queued sat there until the next frame arrived,
                // up to a full inter-frame interval (~16ms at 60fps, unbounded
                // while the source screen is briefly idle). Milestone 7 ruled
                // out async MediaCodec mode on the grounds that it "only
                // changes how the client is notified, not how many frames the
                // codec buffers internally" -- true about the codec's own DPB,
                // but it doesn't cover this: the polling delay is a separate,
                // additive cost created purely by the loop's shape.
                //
                // Blocking dequeue on a dedicated thread (rather than async
                // callbacks) matches moonlight-android, which does the same
                // thing for the same reason; the 50ms timeout is only there so
                // the thread notices shutdown, it returns the instant a buffer
                // is actually ready.
                val renderThread = Thread {
                    android.os.Process.setThreadPriority(android.os.Process.THREAD_PRIORITY_URGENT_DISPLAY)
                    val bufferInfo = MediaCodec.BufferInfo()
                    var renderedCount = 0L
                    var latencySumMs = 0L
                    var latencyMinMs = Long.MAX_VALUE
                    var latencyMaxMs = Long.MIN_VALUE
                    var latencySampleCount = 0L
                    try {
                        while (running) {
                            var outIndex = codec!!.dequeueOutputBuffer(bufferInfo, 50_000)
                            // Burst dedup, unchanged from Milestone 12: right as
                            // motion stops, several real frames land back to
                            // back; rendering all of them faster than the panel
                            // can separate them showed up as two overlapping
                            // valid frames ("ghosting", confirmed via a real
                            // tablet screenshot -- two crisp cursors, not a
                            // smear). Render only the newest of a burst, release
                            // the rest without rendering.
                            var pendingRenderIndex = -1
                            var lastSentAtMs: Long? = null
                            while (true) {
                                when {
                                    outIndex >= 0 -> {
                                        if (pendingRenderIndex >= 0) {
                                            codec!!.releaseOutputBuffer(pendingRenderIndex, false)
                                        }
                                        pendingRenderIndex = outIndex
                                        // One decoded buffer == one queued input
                                        // frame, rendered or not -- pop on every
                                        // buffer or the FIFO desyncs over time.
                                        lastSentAtMs = pendingSentTimesMs.poll() ?: lastSentAtMs
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
                            if (pendingRenderIndex < 0) continue

                            codec!!.releaseOutputBuffer(pendingRenderIndex, true)
                            renderedCount++
                            if (renderedCount == 1L) hideStatus()
                            var latencyMs = 0L
                            lastSentAtMs?.let { sentAtMs ->
                                latencyMs = (System.currentTimeMillis() - clockOffsetMs) - sentAtMs
                                latencySumMs += latencyMs
                                if (latencyMs < latencyMinMs) latencyMinMs = latencyMs
                                if (latencyMs > latencyMaxMs) latencyMaxMs = latencyMs
                                latencySampleCount++
                            }
                            if (showLatency) {
                                val avg = if (latencySampleCount > 0) latencySumMs / latencySampleCount else 0
                                latencyOverlay.submit(
                                    latencyMs,
                                    avg,
                                    if (latencySampleCount > 0) latencyMinMs else 0,
                                    if (latencySampleCount > 0) latencyMaxMs else 0,
                                    pendingSentTimesMs.size,
                                )
                            }
                            if (renderedCount == 1L || renderedCount % 30 == 0L) {
                                val avg = if (latencySampleCount > 0) latencySumMs / latencySampleCount else 0
                                // The log stays: logcat is how every latency
                                // question in MILESTONES was actually answered,
                                // and the overlay is a readout, not a replacement.
                                Log.i(
                                    tag,
                                    "queued=${queuedCount.get()} rendered=$renderedCount, " +
                                        "pending=${pendingSentTimesMs.size} latency avg=${avg}ms " +
                                        "min=${latencyMinMs}ms max=${latencyMaxMs}ms (this frame: ${latencyMs}ms)"
                                )
                            }
                        }
                    } catch (e: Exception) {
                        // Expected on teardown: the reader loop below exits,
                        // the finally block stops/releases the codec, and this
                        // thread's in-flight dequeue throws rather than
                        // returning. Not worth an error-level log.
                        Log.i(tag, "render thread stopping: ${e.message}")
                    }
                }.also { it.start() }

                try {
                    var presentationTimeUs = 0L
                    while (running) {
                        val msg = try {
                            readMessage(input)
                        } catch (e: Exception) {
                            Log.i(tag, "stream ended: ${e.message}")
                            break
                        }
                        lastDataAtMs.set(System.currentTimeMillis())
                        when (msg.type) {
                            // Heartbeat (portal_capture.rs's run_capture, ~800ms
                            // interval) -- proof the daemon is alive during a
                            // legitimately idle screen, which produces no video
                            // frames at all. Nothing to do but reset the
                            // watchdog, which the line above already did.
                            MSG_HEARTBEAT -> continue
                            MSG_CURSOR -> {
                                applyCursorUpdate(msg.payload)
                                continue
                            }
                            MSG_VIDEO -> {}
                            else -> {
                                Log.w(tag, "ignoring unknown message type ${msg.type}")
                                continue
                            }
                        }
                        // Video payload: keyframe flag then the access unit. The
                        // flag matters since Milestone 17 added a real GOP --
                        // before that every frame was an IDR and this side
                        // hardcoded BUFFER_FLAG_KEY_FRAME, which has been a lie
                        // on every P frame since.
                        streaming.set(true)
                        val frameSentAtMs = msg.sentAtMs
                        val isKeyFrame = msg.payload[0].toInt() != 0
                        val frameBytes = msg.payload.copyOfRange(1, msg.payload.size)

                        // Never drop a frame here. This used to log a warning
                        // and discard the frame it had already read off the
                        // wire whenever dequeueInputBuffer returned < 0. That
                        // was harmless under all-intra (the next frame was a
                        // fresh IDR), but since the GOP landed a single
                        // discarded P frame corrupts every frame after it until
                        // the next IDR -- up to a full second of visible
                        // garbage. Wait for a buffer instead.
                        var inIndex = -1
                        while (running && inIndex < 0) {
                            inIndex = codec!!.dequeueInputBuffer(10_000)
                            if (inIndex < 0) {
                                Log.w(tag, "waiting for a free input buffer (dequeueInputBuffer=$inIndex)")
                            }
                        }
                        if (inIndex < 0) break // shutting down

                        val inputBuffer = codec!!.getInputBuffer(inIndex)!!
                        inputBuffer.clear()
                        inputBuffer.put(frameBytes)
                        codec!!.queueInputBuffer(
                            inIndex, 0, frameBytes.size, presentationTimeUs,
                            if (isKeyFrame) MediaCodec.BUFFER_FLAG_KEY_FRAME else 0
                        )
                        presentationTimeUs += 16_666 // ~60fps spacing, cosmetic: render is immediate
                        pendingSentTimesMs.add(frameSentAtMs)
                        queuedCount.incrementAndGet()
                    }
                } finally {
                    renderThread.interrupt()
                    renderThread.join(2000)
                }
            }
        } catch (e: Exception) {
            Log.e(tag, "decode loop error", e)
        } finally {
            cursorOverlay.clear()
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
        // Mirrors daemon/src/protocol.rs. Must change together.
        const val PROTOCOL_MAGIC = 0x5155494C // "QUIL"
        const val PROTOCOL_VERSION = 2
        const val MSG_VIDEO = 0
        const val MSG_CURSOR = 1
        const val MSG_HEARTBEAT = 2
        const val MSG_CLOCK_SYNC = 3
        const val MSG_VIDEO_FORMAT = 4

        private const val EV_HOVER_ENTER = 0
        private const val EV_HOVER_MOVE = 1
        private const val EV_HOVER_EXIT = 2
        private const val EV_DOWN = 3
        private const val EV_MOVE = 4
        private const val EV_UP = 5
        private const val EV_BUTTON_DOWN = 6
        private const val EV_BUTTON_UP = 7
        private const val EV_TOUCH_DOWN = 8
        private const val EV_TOUCH_MOVE = 9
        private const val EV_TOUCH_UP = 10
        private const val EV_RIGHT_DOWN = 11
        private const val EV_RIGHT_UP = 12

        /** Must match `MAX_SLOTS` in daemon/src/uinput_touchpad.rs. */
        private const val MAX_SLOTS = 4
        /** Android's own long-press timeout is 500ms; matching it keeps the
         * gesture feeling like the rest of the system. */
        private const val LONG_PRESS_TIMEOUT_MS = 500L
        /** Movement past this cancels the hold rather than firing a right click
         * at a finger that was really starting a drag. */
        private const val LONG_PRESS_SLOP_DP = 10f

        /** Touch target; the drawn gear is 60% of this (see [GearButton]). */
        private const val GEAR_SIZE_DP = 48f
    }
}
