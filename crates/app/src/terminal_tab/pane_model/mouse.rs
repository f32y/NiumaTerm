use nmt_input::keyboard::ModifiersState;
use nmt_terminal::session::SurfaceMouseButton;

use crate::terminal_tab::pane_model::scroll::ScrollOutcome;
use crate::terminal_tab::pane_model::viewport::LocalPoint;

pub(in crate::terminal_tab) struct MouseInput {
    pub position: LocalPoint,
    pub button: Option<SurfaceMouseButton>,
    pub modifiers: ModifiersState,
    pub click_count: usize,
}

pub(in crate::terminal_tab) enum MouseOutcome {
    Ignored,
    OpenUrl(String),
    SelectionChanged,
    FrozenSelectionStarted,
    EngineHandled,
    Scrolled(ScrollOutcome),
    HoverChanged,
}

pub(in crate::terminal_tab) struct MouseRelease {
    pub outcome: MouseOutcome,
    pub scrollbar_released: bool,
}

pub(in crate::terminal_tab) struct WheelOutcome {
    pub handled: bool,
    pub hover_changed: bool,
}
