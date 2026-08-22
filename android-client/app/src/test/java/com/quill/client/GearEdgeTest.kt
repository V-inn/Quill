package com.quill.client

import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * The ordinal mapping is a stored preference and a documented one: the gear's
 * parked edge is persisted as an Int, and `.claude/CLAUDE.md` gives the reader
 * `0=LEFT 1=TOP 2=RIGHT 3=BOTTOM` for working out where the gear is by hand.
 * Reordering the enum would silently move every user's gear and quietly
 * invalidate that note.
 */
class GearEdgeTest {

    @Test
    fun `ordinals are the documented ones`() {
        assertEquals(GearEdge.LEFT, GearEdge.fromOrdinal(0))
        assertEquals(GearEdge.TOP, GearEdge.fromOrdinal(1))
        assertEquals(GearEdge.RIGHT, GearEdge.fromOrdinal(2))
        assertEquals(GearEdge.BOTTOM, GearEdge.fromOrdinal(3))
    }

    @Test
    fun `an out of range ordinal falls back to the default edge`() {
        // Prefs written by another build, or simply absent.
        assertEquals(GearEdge.RIGHT, GearEdge.fromOrdinal(4))
        assertEquals(GearEdge.RIGHT, GearEdge.fromOrdinal(-1))
    }
}
