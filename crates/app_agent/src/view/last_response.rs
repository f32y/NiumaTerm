use gpui::prelude::*;
use gpui::{AnyElement, Context, div, px};
use gpui_component::tooltip::Tooltip;
use gpui_component::{ActiveTheme as _, Icon, IconName};

use crate::AgentPane;
use crate::transcript::{LAST_RESPONSE_LIMIT, last_response_label};

/// Edge of the mark. Set to the size of a settings pill's own glyph, so the
/// row it stands in keeps one glyph size across its whole width.
const LAST_RESPONSE_MARK: f32 = 12.0;

/// How far into the window a conversation has to have drifted before the
/// composer says so, and before it says so in the danger colour. The window is
/// the one a provider's prompt cache is expected to hold, so the first mark
/// says the next message is going to start costing more than the last one did,
/// and the second says it is about to cost a full re-read of the context.
const LAST_RESPONSE_WARNING: f32 = 0.5;
const LAST_RESPONSE_DANGER: f32 = 0.9;

/// How loudly the composer marks a conversation that has been sitting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LastResponseTone {
    Warning,
    Danger,
}

/// The mark a settled conversation carries, from how long it has been sitting.
///
/// Under half the window there is nothing worth saying: a conversation picked
/// up that soon costs what it would have cost immediately, and a reading that
/// is always on screen is one the eye stops seeing. Past the window the answer
/// stops changing, which is the same answer as the last reading inside it.
pub(super) fn last_response_tone(seconds: u64) -> Option<LastResponseTone> {
    let drift = seconds as f32 / LAST_RESPONSE_LIMIT.as_secs() as f32;

    if drift >= LAST_RESPONSE_DANGER {
        Some(LastResponseTone::Danger)
    } else if drift >= LAST_RESPONSE_WARNING {
        Some(LastResponseTone::Warning)
    } else {
        None
    }
}

impl AgentPane {
    /// How long ago the agent last answered, beside the composer's controls.
    ///
    /// Drawn as a mark rather than as a reading: the number itself only
    /// matters once it is large enough to change what the next message costs,
    /// and until then a line of text beside the settings is one more thing to
    /// read past on the way to sending. The wording it used to carry is on the
    /// mark's tooltip, and in its accessible label.
    ///
    /// Absent until a turn has settled, and while one is running: the
    /// transcript's own live "Working for" reading is the answer then, and two
    /// clocks a few pixels apart would be read as disagreeing.
    pub(super) fn render_last_response(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let at = self.turn.last_response_at()?;
        if self.transcript.read(cx).is_working() {
            return None;
        }

        let seconds = at.elapsed().as_secs();
        let color = match last_response_tone(seconds)? {
            LastResponseTone::Warning => cx.theme().warning,
            LastResponseTone::Danger => cx.theme().danger,
        };
        let label = last_response_label(seconds);
        let tooltip = label.clone();

        Some(
            div()
                .id("agent-last-response")
                .flex_none()
                .flex()
                .items_center()
                .aria_label(label)
                .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                .child(
                    Icon::new(IconName::TriangleAlert)
                        .size(px(LAST_RESPONSE_MARK))
                        .text_color(color),
                )
                .into_any_element(),
        )
    }
}
