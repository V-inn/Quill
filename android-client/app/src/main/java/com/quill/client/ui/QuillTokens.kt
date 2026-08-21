package com.quill.client.ui

import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp

/**
 * The design tokens, as Kotlin constants rather than `res/values/colors.xml`.
 *
 * This app has never had a `res/values/` directory and does not need one: the
 * only consumer of these is Compose, which takes `Color` and `Dp` values
 * directly. Putting them in XML would mean a resource lookup, a generated `R`
 * class, and the first of a category that then spreads.
 *
 * **On the palette.** A mid-tone graphite ground, not the near-black a dark
 * settings screen usually reaches for. This sheet appears over the user's
 * artwork while they are mid-drawing, and artists surround a canvas with
 * mid-grey precisely so it does not skew colour perception. A screen that
 * blasts them with either black or bright chrome costs them their eye for the
 * moment they go back to work.
 *
 * The single accent is a muted copper, and it only ever appears as a mark --
 * a hairline, a small dot, a focus ring, the cable stub on the slab. Never as a
 * large fill. The brightest thing on this screen is meant to be the captured
 * frame of the user's own desktop, not a piece of UI.
 */
object QuillTokens {
    /** Page ground. */
    val Slate = Color(0xFF3A4048)

    /** Raised surfaces: control rows, chips. */
    val Raise = Color(0xFF454C55)

    /** Pressed state for a raised surface. */
    val RaisePressed = Color(0xFF4E5660)

    /**
     * Recessed surfaces. Reserved for the slab, which is cut *into* the page
     * rather than sitting on it -- it is a window onto another machine, and it
     * should read as one.
     */
    val Recess = Color(0xFF2A2F35)

    /** Primary text. Very slightly warm, so it does not read clinical. */
    val Chalk = Color(0xFFE8E6E1)

    /** Secondary text and captions. 4.8:1 on [Slate]. */
    val Graphite = Color(0xFFAEB6BF)

    /**
     * Marks only -- rules, dots, the focus ring, the cable stub. 3.5:1 on
     * [Slate], which clears the 3:1 that non-text UI needs but **not** the
     * 4.5:1 that small text does. Use [CopperLit] for anything with letters in
     * it.
     */
    val Copper = Color(0xFFD08C4A)

    /** Accent *text*, e.g. a staged control's label. 4.5:1 on [Slate]. */
    val CopperLit = Color(0xFFE8A85E)

    /** Hairline dividers. */
    val Rule = Color(0xFF4E555E)

    /** Disabled foreground. */
    val Muted = Color(0xFF7C848D)

    // Spacing. One scale, used everywhere; no ad-hoc numbers at call sites.
    val SpaceXs = 4.dp
    val SpaceSm = 8.dp
    val SpaceMd = 16.dp
    val SpaceLg = 24.dp
    val SpaceXl = 40.dp

    /** Minimum height for anything you can touch. */
    val TouchTarget = 48.dp

    val RowShape = RoundedCornerShape(10.dp)
    val ChipShape = RoundedCornerShape(6.dp)

    /**
     * Below this the two-pane layout stacks into one column.
     *
     * Driven by the window's own width, never by `Configuration.orientation`:
     * `SettingsActivity` is not orientation-locked the way `MainActivity` is,
     * and split-screen and freeform windows exist.
     */
    val TwoPaneMinWidth = 720.dp
}
