package com.quill.client

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The geometry the tablet could not be made to exercise.
 *
 * On this device the window keeps its orientation while the app is in front, so
 * a physical turn never produced a transposed surface and the branch that
 * matters most here was only ever reasoned about. These are the cases hardware
 * refused to reach.
 *
 * The panel numbers throughout are the Tab S9 FE+: 2560x1600 landscape,
 * 1600x2560 the other way up.
 */
class PanelGeometryTest {

    // --- swapsAxes ---------------------------------------------------------

    @Test
    fun `quarter turns swap the axes`() {
        assertTrue(PanelGeometry.swapsAxes(90))
        assertTrue(PanelGeometry.swapsAxes(270))
    }

    @Test
    fun `half turn and no turn do not swap the axes`() {
        // 180 is the same rectangle the other way up: the daemon turns the
        // picture, it is not asked for a different shape.
        assertFalse(PanelGeometry.swapsAxes(0))
        assertFalse(PanelGeometry.swapsAxes(180))
    }

    // --- monitorSize -------------------------------------------------------

    @Test
    fun `unrotated full scale asks for the panel itself`() {
        val m = PanelGeometry.monitorSize(2560, 1600, rotationDegrees = 0, workspaceScalePercent = 100)
        assertEquals(2560, m.widthPx)
        assertEquals(1600, m.heightPx)
    }

    @Test
    fun `a quarter turn asks for the panel transposed`() {
        // The whole point of the setting: a landscape tablet drives a portrait
        // desktop, which the encoder then turns to fill the panel exactly.
        val m = PanelGeometry.monitorSize(2560, 1600, rotationDegrees = 90, workspaceScalePercent = 100)
        assertEquals(1600, m.widthPx)
        assertEquals(2560, m.heightPx)
    }

    @Test
    fun `270 transposes exactly like 90`() {
        val at90 = PanelGeometry.monitorSize(2560, 1600, 90, 100)
        val at270 = PanelGeometry.monitorSize(2560, 1600, 270, 100)
        assertEquals(at90, at270)
    }

    @Test
    fun `a half turn keeps the panel's own shape`() {
        val m = PanelGeometry.monitorSize(2560, 1600, rotationDegrees = 180, workspaceScalePercent = 100)
        assertEquals(2560, m.widthPx)
        assertEquals(1600, m.heightPx)
    }

    @Test
    fun `scaling takes both axes equally so the aspect never changes`() {
        val full = PanelGeometry.monitorSize(2560, 1600, 0, 100)
        val small = PanelGeometry.monitorSize(2560, 1600, 0, 75)
        assertEquals(1920, small.widthPx)
        assertEquals(1200, small.heightPx)
        // Nothing is ever letterboxed, which is also what keeps the input
        // mapping a single multiply.
        assertEquals(
            full.widthPx.toDouble() / full.heightPx,
            small.widthPx.toDouble() / small.heightPx,
            0.0001,
        )
    }

    @Test
    fun `scale and rotation compose`() {
        val m = PanelGeometry.monitorSize(2560, 1600, rotationDegrees = 90, workspaceScalePercent = 60)
        // 60%: 1536x960, then transposed.
        assertEquals(960, m.widthPx)
        assertEquals(1536, m.heightPx)
    }

    @Test
    fun `scaling rounds rather than truncating`() {
        // 1605 * 0.6 = 963.0 exactly, but 2561 * 0.6 = 1536.6 -> 1537. A
        // truncating implementation would give 1536 and lose a pixel column.
        val m = PanelGeometry.monitorSize(2561, 1605, rotationDegrees = 0, workspaceScalePercent = 60)
        assertEquals(1537, m.widthPx)
        assertEquals(963, m.heightPx)
    }

    // --- isSamePanel -------------------------------------------------------

    @Test
    fun `the panel unchanged is the same panel`() {
        assertTrue(PanelGeometry.isSamePanel(2560, 1600, 2560, 1600))
    }

    @Test
    fun `the panel turned a quarter is still the same panel`() {
        // The case the tablet would not reproduce, and the one that broke the
        // rotation setting when it was treated as a resize: renegotiating here
        // re-derives the panel and the swap compounds.
        assertTrue(PanelGeometry.isSamePanel(1600, 2560, 2560, 1600))
    }

    @Test
    fun `a square panel is trivially the same either way round`() {
        assertTrue(PanelGeometry.isSamePanel(1600, 1600, 1600, 1600))
    }

    @Test
    fun `half the width is not the same panel`() {
        // Split-screen: the window no longer owns the whole display.
        assertFalse(PanelGeometry.isSamePanel(1280, 1600, 2560, 1600))
    }

    @Test
    fun `a freeform window is not the same panel`() {
        assertFalse(PanelGeometry.isSamePanel(1200, 800, 2560, 1600))
    }

    @Test
    fun `one axis matching is not enough`() {
        assertFalse(PanelGeometry.isSamePanel(2560, 800, 2560, 1600))
    }

    // --- effectiveRotation -------------------------------------------------

    @Test
    fun `with nothing rejected the saved setting is used`() {
        assertEquals(90, PanelGeometry.effectiveRotation(90, rejectedRotation = null))
        assertEquals(0, PanelGeometry.effectiveRotation(0, rejectedRotation = null))
    }

    @Test
    fun `a rejected rotation is dropped from the request`() {
        assertEquals(0, PanelGeometry.effectiveRotation(90, rejectedRotation = 90))
    }

    @Test
    fun `picking a different rotation is a fresh request and gets tried`() {
        // Matched by value on purpose: the user choosing 270 after 90 failed is
        // asking a new question, and deserves an answer rather than silence.
        assertEquals(270, PanelGeometry.effectiveRotation(270, rejectedRotation = 90))
        assertEquals(180, PanelGeometry.effectiveRotation(180, rejectedRotation = 90))
    }

    @Test
    fun `the saved setting is never mutated by suppression`() {
        // The regression this guards: the old code wrote rotationDegrees = 0 to
        // SharedPreferences, so a transient mismatch destroyed the preference.
        // Suppression is a fact about a connection, so the same inputs must keep
        // giving the same answer no matter how often they are asked.
        val saved = 90
        repeat(3) {
            assertEquals(0, PanelGeometry.effectiveRotation(saved, rejectedRotation = 90))
        }
        assertEquals(90, PanelGeometry.effectiveRotation(saved, rejectedRotation = null))
    }
}
