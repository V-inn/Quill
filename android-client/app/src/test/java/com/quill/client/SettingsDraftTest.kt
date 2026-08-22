package com.quill.client

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The staged-vs-live diff, which until now was only ever verified by a person
 * reading the settings screen's footer in a screenshot.
 */
class SettingsDraftTest {

    private val draft = SettingsDraft(
        clientSideCursor = false,
        ctrlScrollZoom = false,
        rotationDegrees = 0,
        workspaceScalePercent = 100,
        cap30Fps = false,
        quality = 1,
    )

    private fun sessionOf(d: SettingsDraft) = SessionConfig.Snapshot(
        clientSideCursor = d.clientSideCursor,
        ctrlScrollZoom = d.ctrlScrollZoom,
        rotationDegrees = d.rotationDegrees,
        workspaceScalePercent = d.workspaceScalePercent,
        cap30Fps = d.cap30Fps,
        quality = d.quality,
        widthPx = 2560,
        heightPx = 1600,
    )

    @Test
    fun `with no session running nothing is staged`() {
        // A real state, not a missing one: with nothing running, no control can
        // differ from what is running.
        val staged = draft.copy(rotationDegrees = 90).stagedAgainst(null)
        assertFalse(staged.any)
        assertEquals(0, staged.count)
    }

    @Test
    fun `matching the running session stages nothing`() {
        val staged = draft.stagedAgainst(sessionOf(draft))
        assertFalse(staged.any)
        assertEquals(0, staged.count)
    }

    @Test
    fun `a changed rotation is staged and counted once`() {
        val staged = draft.copy(rotationDegrees = 90).stagedAgainst(sessionOf(draft))
        assertTrue(staged.rotation)
        assertEquals(1, staged.count)
        // The footer says "1 change staged" off exactly this.
        assertTrue(staged.any)
    }

    @Test
    fun `changing a value back to what is running clears the mark`() {
        // The other direction, which is what makes this a diff against the
        // session rather than a dirty flag.
        val edited = draft.copy(rotationDegrees = 90).copy(rotationDegrees = 0)
        assertFalse(edited.stagedAgainst(sessionOf(draft)).any)
    }

    @Test
    fun `each control is marked independently`() {
        val staged = draft.copy(
            clientSideCursor = true,
            workspaceScalePercent = 75,
            quality = 2,
        ).stagedAgainst(sessionOf(draft))

        assertTrue(staged.clientSideCursor)
        assertTrue(staged.workspace)
        assertTrue(staged.quality)
        assertFalse(staged.ctrlScrollZoom)
        assertFalse(staged.rotation)
        assertFalse(staged.cap30Fps)
        assertEquals(3, staged.count)
    }

    @Test
    fun `every stageable control can be staged at once`() {
        val staged = draft.copy(
            clientSideCursor = true,
            ctrlScrollZoom = true,
            rotationDegrees = 180,
            workspaceScalePercent = 60,
            cap30Fps = true,
            quality = 0,
        ).stagedAgainst(sessionOf(draft))
        assertEquals(6, staged.count)
    }

    @Test
    fun `a session that rotated differently to the setting is staged`() {
        // Reachable for real: when a daemon cannot do the quarter turn, the
        // session records the rotation actually asked for (0) while the setting
        // still says 90. The screen should show that as out of step, because it
        // is.
        val session = sessionOf(draft).copy(rotationDegrees = 0)
        assertTrue(draft.copy(rotationDegrees = 90).stagedAgainst(session).rotation)
    }
}
