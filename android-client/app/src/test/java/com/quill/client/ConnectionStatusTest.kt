package com.quill.client

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The status overlay is the whole user interface whenever no desktop is on the
 * panel, and for someone who has installed the app without the daemon it is the
 * *only* interface they will ever see. These assert the distinctions it is
 * supposed to make, since the states behind them are awkward to reach by hand:
 * a version-mismatched daemon needs two builds, and the first-run state exists
 * once per install.
 */
class ConnectionStatusTest {

    private fun message(
        everConnected: Boolean = true,
        accessoryPresent: Boolean = false,
        attempt: Int = 1,
        silentHandshakes: Int = 0,
        debugTransport: Boolean = false,
    ) = ConnectionStatus.message(
        everConnected = everConnected,
        accessoryPresent = accessoryPresent,
        attempt = attempt,
        silentHandshakes = silentHandshakes,
        debugTransport = debugTransport,
    )

    @Test
    fun `a first run with nothing plugged in explains what the daemon is`() {
        val m = message(everConnected = false, accessoryPresent = false)
        assertTrue(m.contains("Linux"))
        assertTrue(m.contains(ConnectionStatus.PROJECT_URL))
        // The old text, which said nothing to someone who has never heard of
        // the daemon.
        assertFalse(m.contains("Waiting for connection"))
    }

    @Test
    fun `once a desktop has been streamed the explanation is gone for good`() {
        val m = message(everConnected = true, accessoryPresent = false)
        assertFalse(m.contains(ConnectionStatus.PROJECT_URL))
        assertTrue(m.contains("Waiting for connection"))
    }

    @Test
    fun `an attached accessory is reported as connecting, not as waiting`() {
        // Something put this tablet into accessory mode, so there is a daemon;
        // telling the user to plug in a cable that is plugged in is worse than
        // saying nothing.
        val m = message(everConnected = false, accessoryPresent = true)
        assertTrue(m.contains("Connecting"))
        assertFalse(m.contains("Plug this tablet"))
    }

    @Test
    fun `the replug advice waits for the second attempt`() {
        assertFalse(message(attempt = 1).contains("replug"))
        assertTrue(message(attempt = 2).contains("replug"))
    }

    @Test
    fun `two unanswered handshakes blame the daemon and not the cable`() {
        val one = message(accessoryPresent = true, attempt = 3, silentHandshakes = 1)
        assertFalse(one.contains("not answering"))

        val two = message(
            accessoryPresent = true,
            attempt = 3,
            silentHandshakes = ConnectionStatus.SILENT_HANDSHAKES_BEFORE_BLAMING_THE_DAEMON,
        )
        assertTrue(two.contains("not answering"))
        assertTrue(two.contains(ConnectionStatus.PROJECT_URL))
        // Replugging a mismatched daemon reproduces the same silence, so that
        // advice must not survive into this state -- it outranks the attempt
        // count deliberately.
        assertFalse(two.contains("replug"))
    }

    @Test
    fun `the unanswered-daemon message wins even on a first run`() {
        val m = message(everConnected = false, accessoryPresent = true, silentHandshakes = 4)
        assertTrue(m.contains("not answering"))
        assertFalse(m.contains("Quill needs its other half"))
    }

    @Test
    fun `a debug build without an accessory names the adb-forward path`() {
        val m = message(everConnected = false, accessoryPresent = false, debugTransport = true)
        assertTrue(m.contains("7777"))
        assertFalse(m.contains("Quill needs its other half"))
    }

    @Test
    fun `every state keeps the way into settings`() {
        // While disconnected the status text is the only entry point to
        // settings there is -- the gear is only drawn over live video.
        for (ever in listOf(false, true)) {
            for (accessory in listOf(false, true)) {
                for (silent in listOf(0, 2)) {
                    val m = message(
                        everConnected = ever,
                        accessoryPresent = accessory,
                        attempt = 2,
                        silentHandshakes = silent,
                    )
                    assertTrue(m, m.contains("tap here for settings"))
                }
            }
        }
    }
}
