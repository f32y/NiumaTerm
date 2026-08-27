use crate::transcript::last_response_label;
use crate::*;

impl AgentPane {
    /// How long ago the agent last answered, beside the composer's controls.
    /// Absent until a turn has settled, and while one is running: the
    /// transcript's own live "Working for" reading is the answer then, and two
    /// clocks a few pixels apart would be read as disagreeing.
    pub(in crate::view) fn render_last_response(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let at = self.last_response_at?;
        if self.transcript.read(cx).is_working() {
            return None;
        }

        Some(
            div()
                .flex_none()
                .text_xs()
                .whitespace_nowrap()
                .text_color(cx.theme().muted_foreground.opacity(0.7))
                .child(last_response_label(at.elapsed().as_secs()))
                .into_any_element(),
        )
    }
}
