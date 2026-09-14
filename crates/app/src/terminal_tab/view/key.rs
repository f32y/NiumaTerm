use gpui::{Keystroke, Modifiers};
use nmt_input::keyboard::ModifiersState;
use nmt_terminal::input::{KeyPhase, TerminalKey};

pub(super) fn terminal_key(key: &Keystroke) -> TerminalKey<'_> {
    TerminalKey {
        key: &key.key,
        key_char: key.key_char.as_deref(),
        modifiers: modifiers_state(key.modifiers),
        function: key.modifiers.function,
        phase: KeyPhase::Press,
    }
}

pub(super) fn modifiers_state(modifiers: Modifiers) -> ModifiersState {
    let mut state = ModifiersState::empty();

    state.set(ModifiersState::SHIFT, modifiers.shift);

    state.set(ModifiersState::ALT, modifiers.alt);

    state.set(ModifiersState::CONTROL, modifiers.control);

    state.set(ModifiersState::SUPER, modifiers.platform);

    state
}
