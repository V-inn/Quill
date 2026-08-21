package com.quill.client.ui

import android.provider.Settings as AndroidSettings
import androidx.compose.animation.core.AnimationSpec
import androidx.compose.animation.core.FiniteAnimationSpec
import androidx.compose.animation.core.snap
import androidx.compose.animation.core.spring
import androidx.compose.animation.core.tween
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.compositionLocalOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.platform.LocalContext

/**
 * What `material3` would have supplied, in the ~30 lines it actually takes.
 *
 * Not a theme in the Material sense -- there is no colour scheme to swap and no
 * dark/light pair. This app is one appearance, over video, in a dim room. All
 * this provides is the two things a `foundation`-only tree genuinely lacks: a
 * way to reach the tokens without threading them through every signature, and
 * the reduced-motion scale.
 */

/**
 * The system's animation duration scale (Settings > Developer options, or an
 * accessibility preference). `0f` means the user has asked for no animation.
 *
 * **This matters more in Compose than in Views.** `ValueAnimator` and
 * `ViewPropertyAnimator` -- everything `GearButton` uses -- consult this scale
 * themselves and collapse to instant at zero. Compose runs its own clock and
 * ignores it entirely, so every animation on this screen has to ask. Use
 * [quillSpring] / [quillTween] rather than calling `spring()` directly.
 */
val LocalAnimationScale = staticCompositionLocalOf { 1f }

/**
 * Whether the desktop the tablet is showing is currently live.
 *
 * Read by the header chip and, later, by the slab -- both of which have a
 * genuinely different empty state rather than a greyed-out version of the
 * connected one.
 */
val LocalSessionLive = compositionLocalOf { false }

@Composable
fun QuillTheme(sessionLive: Boolean = false, content: @Composable () -> Unit) {
    val context = LocalContext.current
    val scale = remember(context) {
        runCatching {
            AndroidSettings.Global.getFloat(
                context.contentResolver,
                AndroidSettings.Global.ANIMATOR_DURATION_SCALE,
                1f,
            )
        }.getOrDefault(1f)
    }
    CompositionLocalProvider(
        LocalAnimationScale provides scale,
        LocalSessionLive provides sessionLive,
        content = content,
    )
}

/** A spring that honours the user's reduced-motion setting. */
@Composable
fun <T> quillSpring(
    dampingRatio: Float = 0.75f,
    stiffness: Float = 380f,
): FiniteAnimationSpec<T> =
    if (LocalAnimationScale.current == 0f) snap() else spring(dampingRatio, stiffness)

/** A tween that honours the user's reduced-motion setting. */
@Composable
fun <T> quillTween(durationMillis: Int = 140): AnimationSpec<T> =
    if (LocalAnimationScale.current == 0f) snap() else tween(durationMillis)
