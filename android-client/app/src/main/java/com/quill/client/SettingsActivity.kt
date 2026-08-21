package com.quill.client

import android.app.Activity
import android.graphics.Color
import android.os.Bundle
import android.view.Gravity
import android.view.View
import android.widget.Button
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.Switch
import android.widget.TextView

/**
 * Settings, reachable by tapping the status overlay on the main screen (there
 * is no system chrome to hang a menu off -- the app runs edge-to-edge immersive
 * as a monitor replacement, so the video surface itself is the only affordance).
 *
 * Built in code rather than XML to match the rest of this app, which has no
 * layout resources at all.
 */
class SettingsActivity : Activity() {

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val settings = Settings(this)

        val root = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setBackgroundColor(Color.BLACK)
            setPadding(pad, pad, pad, pad)
        }

        root.addView(heading("Quill settings"))
        root.addView(
            note(
                "Changes apply the next time the tablet connects. The daemon " +
                    "decides which kind of capture session to open before the first " +
                    "frame, so these cannot be switched mid-session."
            )
        )

        root.addView(divider())
        root.addView(heading("Cursor"))
        val cursorSwitch = switchRow(
            "Draw the pointer on the tablet (experimental)",
            settings.clientSideCursor
        ) { settings.clientSideCursor = it }
        root.addView(cursorSwitch)
        root.addView(
            note(
                "KNOWN ISSUE on KDE/KWin virtual displays: you will see TWO pointers.\n\n" +
                    "A normal monitor keeps the pointer on a separate hardware layer, so the " +
                    "desktop can leave it out of the picture it sends. A virtual display has " +
                    "no such layer, so the desktop paints the pointer into the picture and " +
                    "cannot be asked not to. Turning this on adds a second, faster pointer " +
                    "drawn by the tablet, leaving the desktop's own pointer trailing a frame " +
                    "behind it.\n\n" +
                    "Measured on this setup, the tablet-drawn pointer was only about 5-10ms " +
                    "ahead of the video anyway, so there is little to gain. Left here because " +
                    "other desktops may behave differently.\n\n" +
                    "Pen ink is unaffected either way: it is drawn by the app on the desktop " +
                    "and comes back with the video."
            )
        )

        root.addView(divider())
        root.addView(heading("Display"))
        root.addView(
            switchRow("Rotate the image 180°", settings.flip180) { settings.flip180 = it }
        )
        root.addView(
            note(
                "Turn this on if the desktop appears upside down -- which way up it " +
                    "belongs depends on which end of the device the USB cable enters, and " +
                    "only you can see that.\n\n" +
                    "Touch and pen coordinates rotate with the image, so they keep lining " +
                    "up either way."
            )
        )

        root.addView(divider())
        root.addView(heading("Touch gestures"))
        root.addView(
            note(
                "Two-finger scroll, pinch, swipes and two-finger tap are recognized by " +
                    "Linux itself, from a virtual touchpad the daemon creates. Configure " +
                    "them in System Settings > Touchpad (or Mouse & Touchpad on GNOME) " +
                    "like any real trackpad.\n\n" +
                    "One finger still points where you touch, and holding one finger still " +
                    "is a right click."
            )
        )
        root.addView(
            switchRow("Zoom with Ctrl+scroll instead of pinch", settings.ctrlScrollZoom) {
                settings.ctrlScrollZoom = it
            }
        )
        root.addView(
            note(
                "OFF sends a real pinch gesture. Only applications that understand Wayland " +
                    "gestures zoom from it -- Firefox and GTK apps do, anything running " +
                    "through XWayland (Krita, older Qt apps) ignores it entirely.\n\n" +
                    "ON has the daemon recognize the pinch itself and send Ctrl+scroll, which " +
                    "almost every application honours, but zooms in fixed steps instead of " +
                    "smoothly."
            )
        )

        root.addView(divider())
        root.addView(heading("Screen"))
        root.addView(
            switchRow("Keep the screen awake", settings.keepScreenAwake) {
                settings.keepScreenAwake = it
            }
        )
        root.addView(
            note(
                "While a desktop is showing. The tablet still sleeps when nothing " +
                    "is connected.\n\nUnlike the settings above, this one takes effect " +
                    "as soon as you leave this screen."
            )
        )

        root.addView(divider())
        root.addView(heading("Diagnostics"))
        root.addView(
            switchRow("Show latency overlay", settings.showLatencyOverlay) {
                settings.showLatencyOverlay = it
            }
        )
        root.addView(note("Draws the running per-frame latency over the video."))

        root.addView(divider())
        root.addView(Button(this).apply {
            text = "Done"
            setOnClickListener { finish() }
        })

        setContentView(ScrollView(this).apply {
            setBackgroundColor(Color.BLACK)
            addView(root)
        })
    }

    private fun heading(text: String) = TextView(this).apply {
        this.text = text
        setTextColor(Color.WHITE)
        textSize = 22f
        setPadding(0, pad / 2, 0, pad / 4)
    }

    private fun note(text: String) = TextView(this).apply {
        this.text = text
        setTextColor(Color.LTGRAY)
        textSize = 14f
        setPadding(0, 0, 0, pad / 2)
    }

    private fun divider() = View(this).apply {
        setBackgroundColor(Color.DKGRAY)
        val hairline = maxOf(1, (resources.displayMetrics.density).toInt())
        layoutParams = LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, hairline).apply {
            topMargin = pad / 2
            bottomMargin = pad / 2
        }
    }

    @Suppress("DEPRECATION") // Switch is fine here; this app pulls in no Material dependency
    private fun switchRow(label: String, initial: Boolean, onChange: (Boolean) -> Unit) =
        Switch(this).apply {
            text = label
            textSize = 18f
            setTextColor(Color.WHITE)
            isChecked = initial
            gravity = Gravity.CENTER_VERTICAL
            setPadding(0, pad / 4, 0, pad / 4)
            setOnCheckedChangeListener { _, checked -> onChange(checked) }
        }

    /** Base spacing unit, in real pixels.
     *
     * Was a bare `PAD = 48` used directly as a pixel count -- so on this
     * tablet's 2x panel every gap came out at half the size it reads as, and on
     * a 3x phone a third. Everything below divides it by 2 or 4, so it has to
     * stay an even multiple. */
    private val pad: Int by lazy { (PAD_DP * resources.displayMetrics.density).toInt() }

    companion object {
        private const val PAD_DP = 24f
    }
}
