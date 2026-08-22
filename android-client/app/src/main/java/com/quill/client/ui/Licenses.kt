package com.quill.client.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.text.BasicText
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier

/**
 * Attribution for the typefaces this app is set in.
 *
 * Not decoration and not optional. All three faces are under the SIL Open Font
 * License 1.1, which requires two things of anyone redistributing them: the
 * licence text travels with the font files, and the copyright notice is
 * reproduced. The first is why the text sits in `assets/licenses/` -- packaged
 * into the APK -- rather than in a source-only directory that never reaches a
 * shipped build. This is the second.
 *
 * The full text is bundled rather than rendered here: it is nine kilobytes of
 * legal prose and a settings pane is the wrong place to read it. What belongs
 * on screen is who made these and under what terms.
 */
@Composable
fun TypefaceCredits(modifier: Modifier = Modifier) {
    Column(
        modifier = modifier,
        verticalArrangement = Arrangement.spacedBy(QuillTokens.SpaceXs),
    ) {
        Eyebrow("Typefaces")
        BasicText(
            text = CREDITS,
            style = QuillType.caption.copy(color = QuillTokens.Muted),
        )
    }
}

private val CREDITS = """
    Archivo — copyright 2020 The Archivo Project Authors.
    IBM Plex Sans and IBM Plex Mono — copyright 2017 IBM Corp., with Reserved Font Name "Plex".

    Both under the SIL Open Font License 1.1. The full text ships with the app, in assets/licenses.
""".trimIndent()
