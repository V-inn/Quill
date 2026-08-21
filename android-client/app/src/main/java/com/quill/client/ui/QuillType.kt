package com.quill.client.ui

import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.Font
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.sp
import com.quill.client.R

/**
 * Three faces, each with exactly one job.
 *
 * The files in `res/font/` are instanced and subset from the upstream OFL
 * releases -- see `tools/build-fonts.sh` for the command that produces them and
 * why (1,196 KB upstream, 115 KB as shipped). Licences are in `app/licenses/`
 * and have to stay with them.
 *
 * **Eyebrow** carries the personality and is rationed to two places: section
 * labels and the slab's readout. A display face used anywhere else stops being
 * a display face.
 *
 * **Body** and **Data** are IBM Plex Sans and Mono, drawn together and sharing
 * an engineering heritage that suits an instrument panel. Plex over Inter
 * because Inter is the neutral default every interface reaches for.
 */
object QuillType {
    val Eyebrow = FontFamily(
        Font(R.font.archivo_expanded_semibold, FontWeight.SemiBold)
    )

    val Body = FontFamily(
        Font(R.font.ibm_plex_sans_regular, FontWeight.Normal),
        Font(R.font.ibm_plex_sans_semibold, FontWeight.SemiBold),
    )

    val Data = FontFamily(
        Font(R.font.ibm_plex_mono_regular, FontWeight.Normal)
    )

    /** Section labels. Small, uppercase, widely tracked -- the whole effect. */
    val eyebrow = TextStyle(
        fontFamily = Eyebrow,
        fontWeight = FontWeight.SemiBold,
        fontSize = 11.sp,
        lineHeight = 16.sp,
        letterSpacing = 1.6.sp,
    )

    /** A control's own label. */
    val label = TextStyle(
        fontFamily = Body,
        fontWeight = FontWeight.SemiBold,
        fontSize = 15.sp,
        lineHeight = 21.sp,
    )

    /** Running text. */
    val body = TextStyle(
        fontFamily = Body,
        fontWeight = FontWeight.Normal,
        fontSize = 15.sp,
        lineHeight = 22.sp,
    )

    /** The explanatory note under a control. */
    val caption = TextStyle(
        fontFamily = Body,
        fontWeight = FontWeight.Normal,
        fontSize = 13.sp,
        lineHeight = 20.sp,
    )

    /** Numbers with units: `2560 x 1600`, `60 Hz`, `28 ms`, `180 deg`. */
    val data = TextStyle(
        fontFamily = Data,
        fontWeight = FontWeight.Normal,
        fontSize = 13.sp,
        lineHeight = 18.sp,
        letterSpacing = 0.2.sp,
    )

    /** The one primary action. */
    val button = TextStyle(
        fontFamily = Body,
        fontWeight = FontWeight.SemiBold,
        fontSize = 15.sp,
        lineHeight = 20.sp,
        textAlign = TextAlign.Center,
    )
}
