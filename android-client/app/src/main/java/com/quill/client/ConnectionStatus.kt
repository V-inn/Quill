package com.quill.client

/**
 * What the status overlay says while no desktop is on screen.
 *
 * Kept out of [MainActivity] and free of Android types so it can be tested:
 * this text is the entire first-run experience for anyone who installs the app
 * without the daemon, and until now it said "Waiting for connection...
 * (attempt 29)" forever, which tells someone who has never heard of the daemon
 * nothing at all.
 *
 * The states it distinguishes are all things the app can actually know:
 *
 * - **Whether a USB accessory is open.** The daemon is what puts the tablet
 *   into AOA accessory mode (see `aoa.rs`), so an accessory existing is
 *   evidence a Quill daemon is on the other end of the cable, and its absence
 *   is evidence there is nothing to talk to yet.
 * - **Whether this install has ever streamed.** Persisted (see
 *   [Settings.hasEverConnected]) because the difference between "your cable is
 *   unplugged" and "you have not installed the other half of this program" is
 *   the difference between a hint and an explanation, and only the first run
 *   needs the explanation.
 * - **Whether handshakes are going unanswered.** See
 *   [MainActivity.HANDSHAKE_REPLY_TIMEOUT_MS]: a daemon that rejects our
 *   handshake -- the shape a protocol-version mismatch takes -- leaves the
 *   accessory open and says nothing back. Two of those in a row is not a
 *   coincidence.
 */
object ConnectionStatus {

    /** Where to send someone who has nothing installed on the Linux side. */
    const val PROJECT_URL = "github.com/V-inn/Quill"

    /**
     * @param everConnected this install has streamed a desktop at least once.
     * @param accessoryPresent a USB accessory is attached and permitted, i.e.
     *   something put this tablet into accessory mode.
     * @param attempt 1 for the first connection attempt of this session.
     * @param silentHandshakes consecutive connections where the handshake was
     *   written and no reply ever came.
     * @param debugTransport this build can also reach a daemon over adb
     *   forward, so a missing accessory is not the whole story.
     */
    fun message(
        everConnected: Boolean,
        accessoryPresent: Boolean,
        attempt: Int,
        silentHandshakes: Int,
        debugTransport: Boolean = false,
    ): String {
        val body = when {
            // Checked before anything else: a daemon that answers nothing is a
            // worse problem than whatever the attempt count suggests, and the
            // replug advice below is wrong for it -- replugging a mismatched
            // daemon just repeats the same silence.
            silentHandshakes >= SILENT_HANDSHAKES_BEFORE_BLAMING_THE_DAEMON ->
                "This computer's Quill daemon is not answering.\n" +
                    "The cable is connected and the app has introduced itself, " +
                    "but nothing came back.\n\n" +
                    "The daemon is most likely older than this app. " +
                    "Update it from $PROJECT_URL\n" +
                    "and the connection will resume on its own."

            // Never streamed and nothing on the cable: the one case where the
            // app should explain what it is rather than report a status.
            !everConnected && !accessoryPresent && !debugTransport ->
                "Quill needs its other half.\n\n" +
                    "This tablet is the screen. The desktop comes from a Linux " +
                    "computer\nrunning the Quill daemon, reached over the USB cable.\n\n" +
                    "Install it from $PROJECT_URL, then plug\n" +
                    "this tablet into that computer."

            !everConnected && !accessoryPresent ->
                // Debug builds only: the adb-forward transport needs no
                // accessory, so an absent one is not a missing daemon.
                "Waiting for a daemon on port 7777 (adb forward)."

            accessoryPresent ->
                "Connecting..." + replugAdvice(attempt)

            else ->
                "Waiting for connection...\n" +
                    "Plug this tablet into the computer running the Quill daemon." +
                    replugAdvice(attempt)
        }
        return body + "\n\n(tap here for settings)"
    }

    /** After the first attempt, say plainly that a stuck connection needs a
     * replug: the daemon cannot detect and reset a stale AOA session on its
     * own (see [MainActivity]'s class doc), so silent retrying forever leaves
     * someone staring at a black screen with nothing to act on. */
    private fun replugAdvice(attempt: Int): String =
        if (attempt <= 1) "" else "\n\nAttempt $attempt. If this does not clear in a few seconds,\n" +
            "unplug and replug the USB cable."

    /**
     * One unanswered handshake is ordinary -- the daemon may be restarting
     * under systemd, or still tearing down the previous session's portal. Two
     * in a row is a daemon that is running and refusing us, which in practice
     * means a version it does not recognise.
     */
    const val SILENT_HANDSHAKES_BEFORE_BLAMING_THE_DAEMON = 2
}
