use nmt_config::CursorShape;
use nmt_config::local_state::TabState;
use nmt_platform::PromptIntegration;

pub(crate) fn default_shell() -> String {
    nmt_platform::default_shell()
}

/// Local terminal session configuration. `None` and empty fields fall back to
/// defaults (`shell` → the platform's default shell).
#[derive(Debug, Clone)]
pub struct TerminalSessionConfig {
    pub shell: Option<String>,
    pub args: Vec<String>,
    pub working_dir: Option<String>,
    pub starting_title: Option<String>,
    pub cols: u16,
    pub rows: u16,
    /// Default cursor shape until the running program selects one with DECSCUSR.
    pub cursor_shape: CursorShape,
    /// Scrollback budget in lines; converted to the engine's byte budget.
    pub scrollback_lines: usize,
    /// Engine-blocks mode is the default because completed commands can freeze
    /// into engine-side blocks at each trusted `;D`; rendering reads
    /// them through `BlockRef` handles. `false` is the internal classic-grid
    /// fallback: no freezing, no boundary clears, no block events, intact
    /// scrollback. The GPUI app keeps this enabled and toggles block chrome only.
    pub engine_blocks: bool,
    /// Child-only values merged into the shell's inherited environment.
    /// Runtime metadata is deliberately excluded from persisted tab state.
    pub environment_overrides: Vec<(String, String)>,
    pub manage_process_tree: bool,
    /// Bytes the launch places in the terminal's input queue before the shell
    /// starts, for a platform whose shell integration is typed at the shell
    /// rather than found by it. Not part of the restorable tab state: it is a
    /// property of this launch, not of the command the user configured.
    pub bootstrap: Option<String>,
}

impl TerminalSessionConfig {
    pub(crate) fn restorable_tab_state(&self) -> TabState {
        TabState {
            name: None,
            user_named: false,
            shell: self.shell.clone(),
            args: self.args.clone(),
            cwd: self.working_dir.clone(),
            agent: None,
            agent_profile: None,
            panes: None,
        }
    }

    /// Augment a session config so the shell evaluates the bundled OSC 133
    /// integration at startup. Whether that rides on startup arguments or on
    /// the child environment is the platform's answer, not this layer's.
    pub(crate) fn with_shell_integration(mut self: TerminalSessionConfig) -> TerminalSessionConfig {
        let Some(integration) = self.prompt_integration() else {
            return self;
        };

        self.args = integration.args;
        self.environment_overrides.extend(integration.environment);
        self.bootstrap = integration.bootstrap;

        self
    }

    /// The launch adjustments the platform's prompt integration needs, or
    /// `None` when the shell has none. Caller-supplied args are the user's own
    /// launch command, so a config that carries them is left alone.
    fn prompt_integration(&self) -> Option<PromptIntegration> {
        if !self.args.is_empty() {
            return None;
        }

        nmt_platform::prompt_integration(self.shell.as_deref())
    }

    /// Whether this launch will carry an integration the terminal can trust
    /// the block boundaries of. `with_shell_integration` decides for itself;
    /// this is the same question asked without applying the answer.
    #[cfg(test)]
    pub(crate) fn has_trusted_prompt_integration(&self) -> bool {
        self.prompt_integration().is_some()
    }
}

impl Default for TerminalSessionConfig {
    fn default() -> Self {
        TerminalSessionConfig {
            shell: None,
            args: Vec::new(),
            working_dir: None,
            starting_title: None,
            cols: 80,
            rows: 24,
            cursor_shape: CursorShape::Block,
            scrollback_lines: 10_000,
            engine_blocks: true,
            environment_overrides: Vec::new(),
            manage_process_tree: false,
            bootstrap: None,
        }
    }
}
