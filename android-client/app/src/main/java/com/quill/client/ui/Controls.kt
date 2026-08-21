package com.quill.client.ui

import androidx.compose.animation.animateColorAsState
import androidx.compose.animation.core.animateDpAsState
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.focusable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.interaction.collectIsFocusedAsState
import androidx.compose.foundation.interaction.collectIsPressedAsState
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.defaultMinSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.foundation.text.BasicText

/**
 * The controls `material3` would have supplied, built to this design instead.
 *
 * The switch is the reason not to take Material's: it is the single most
 * recognisable Material component on a settings screen, and leaving it in place
 * would have undone the rest of the work by itself.
 */

/** A section label. Rationed -- see [QuillType.Eyebrow]. */
@Composable
fun Eyebrow(text: String, modifier: Modifier = Modifier) {
    BasicText(
        text = text.uppercase(),
        modifier = modifier,
        style = QuillType.eyebrow.copy(color = QuillTokens.Graphite),
    )
}

/** The one-line note under an eyebrow, saying when the section takes effect. */
@Composable
fun SectionNote(text: String, modifier: Modifier = Modifier) {
    BasicText(
        text = text,
        modifier = modifier,
        style = QuillType.caption.copy(color = QuillTokens.Muted),
    )
}

/** Explanatory copy under a control. */
@Composable
fun Note(text: String, modifier: Modifier = Modifier) {
    BasicText(
        text = text,
        modifier = modifier,
        style = QuillType.caption.copy(color = QuillTokens.Graphite),
    )
}

/**
 * A switch row: label, optional note, and the toggle itself.
 *
 * The whole row is the touch target, per Android's own convention and because
 * a 2560-wide pane with a thumb-sized hit area at the far right would be
 * absurd. `Role.Switch` plus the label as semantic text means TalkBack
 * announces the label and the state together.
 *
 * `staged` marks a control whose value differs from what the running session is
 * using. It can only ever be true in a section whose settings are deferred to
 * the next connect; see `SettingsScreen`.
 */
@Composable
fun SwitchRow(
    label: String,
    checked: Boolean,
    onCheckedChange: (Boolean) -> Unit,
    modifier: Modifier = Modifier,
    note: String? = null,
    staged: Boolean = false,
) {
    val interaction = remember { MutableInteractionSource() }
    val pressed by interaction.collectIsPressedAsState()
    val focused by interaction.collectIsFocusedAsState()

    val background by animateColorAsState(
        targetValue = if (pressed) QuillTokens.RaisePressed else QuillTokens.Raise,
        animationSpec = quillTween(),
        label = "rowBackground",
    )

    Row(
        modifier = modifier
            .fillMaxWidth()
            .clip(QuillTokens.RowShape)
            .background(background)
            .then(
                if (focused) {
                    Modifier.border(2.dp, QuillTokens.Copper, QuillTokens.RowShape)
                } else {
                    Modifier
                }
            )
            .toggleable(
                value = checked,
                interactionSource = interaction,
                // Explicitly null: foundation's LocalIndication default has
                // moved across versions, and the press feedback here is the
                // background colour above, not a ripple.
                indication = null,
                role = Role.Switch,
                onValueChange = onCheckedChange,
            )
            .focusable(interactionSource = interaction)
            .defaultMinSize(minHeight = QuillTokens.TouchTarget)
            .padding(horizontal = QuillTokens.SpaceMd, vertical = QuillTokens.SpaceMd),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(QuillTokens.SpaceMd),
    ) {
        Column(
            modifier = Modifier.weight(1f),
            verticalArrangement = Arrangement.spacedBy(QuillTokens.SpaceXs),
        ) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(QuillTokens.SpaceSm),
            ) {
                if (staged) StagedDot()
                BasicText(
                    text = label,
                    style = QuillType.label.copy(
                        color = if (staged) QuillTokens.CopperLit else QuillTokens.Chalk,
                    ),
                )
            }
            if (note != null) Note(note)
        }
        QuillSwitch(checked = checked)
    }
}

/**
 * Marks a control that will change something the next time the tablet
 * connects. Deliberately the only staged affordance on the screen: one mark,
 * one meaning, and it cannot appear in a section where nothing is deferred.
 */
@Composable
private fun StagedDot() {
    Box(
        Modifier
            .size(7.dp)
            .clip(CircleShape)
            .background(QuillTokens.Copper)
    )
}

/**
 * The toggle itself. Drawn, not Material's.
 *
 * Presentation only -- the whole row owns the click, so this takes no
 * interaction source and no callback, and carries no semantics of its own
 * (which would make TalkBack announce the control twice).
 */
@Composable
private fun QuillSwitch(checked: Boolean) {
    val trackWidth = 44.dp
    val trackHeight = 26.dp
    val thumb = 20.dp
    val inset = 3.dp

    val track by animateColorAsState(
        targetValue = if (checked) QuillTokens.Copper else QuillTokens.Slate,
        animationSpec = quillTween(),
        label = "switchTrack",
    )
    val thumbColor by animateColorAsState(
        targetValue = if (checked) Color(0xFF2A2013) else QuillTokens.Graphite,
        animationSpec = quillTween(),
        label = "switchThumb",
    )
    val offset by animateDpAsState(
        targetValue = if (checked) trackWidth - thumb - inset else inset,
        animationSpec = quillSpring(),
        label = "switchOffset",
    )

    Box(
        Modifier
            .width(trackWidth)
            .height(trackHeight)
            .clip(RoundedCornerShape(trackHeight / 2))
            .background(track)
            .border(1.dp, if (checked) Color.Transparent else QuillTokens.Rule, RoundedCornerShape(trackHeight / 2)),
    ) {
        Box(
            Modifier
                .align(Alignment.CenterStart)
                .offset(x = offset)
                .size(thumb)
                .clip(CircleShape)
                .background(thumbColor)
        )
    }
}

/** The one primary action on the screen. */
@Composable
fun PrimaryButton(
    text: String,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    enabled: Boolean = true,
) {
    val interaction = remember { MutableInteractionSource() }
    val pressed by interaction.collectIsPressedAsState()
    val focused by interaction.collectIsFocusedAsState()

    val background by animateColorAsState(
        targetValue = when {
            !enabled -> QuillTokens.Raise
            pressed -> QuillTokens.CopperLit
            else -> QuillTokens.Copper
        },
        animationSpec = quillTween(),
        label = "buttonBackground",
    )

    Box(
        modifier = modifier
            .clip(QuillTokens.RowShape)
            .background(background)
            .then(
                if (focused) Modifier.border(2.dp, QuillTokens.Chalk, QuillTokens.RowShape) else Modifier
            )
            .clickable(
                interactionSource = interaction,
                indication = null,
                enabled = enabled,
                role = Role.Button,
                onClick = onClick,
            )
            .focusable(interactionSource = interaction)
            .defaultMinSize(minHeight = QuillTokens.TouchTarget)
            .padding(horizontal = QuillTokens.SpaceLg, vertical = QuillTokens.SpaceMd),
        contentAlignment = Alignment.Center,
    ) {
        BasicText(
            text = text,
            style = QuillType.button.copy(
                color = if (enabled) Color(0xFF241A0E) else QuillTokens.Muted,
                fontWeight = FontWeight.SemiBold,
            ),
        )
    }
}

/** A hairline. One device pixel would disappear; 1dp is the intent. */
@Composable
fun Rule(modifier: Modifier = Modifier) {
    Box(
        modifier
            .fillMaxWidth()
            .height(1.dp)
            .background(QuillTokens.Rule)
    )
}
