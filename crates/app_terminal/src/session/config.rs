use std::sync::LazyLock;

use nmt_config::CursorShape;
use nmt_config::local_state::TabState;
use nmt_platform::windows::powershell;

static ENCODED_POWERSHELL_INTEGRATION: LazyLock<String> =
    LazyLock::new(|| powershell::encode_command(powershell::INTEGRATION_SCRIPT));

pub(crate) fn default_shell() -> String {
    powershell::DEFAULT_SHELL.to_string()
}

/// Whether a configured shell is PowerShell (the only shell we have an OSC 133
/// integration script for). `None` resolves to the PowerShell default.
pub(crate) fn shell_is_powershell(shell: Option<&str>) -> bool {
    powershell::is_shell(shell)
}

/// Local terminal session configuration. `None` and empty fields fall back to
/// defaults (`shell` → `powershell.exe`).
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
    /// Child-only values merged into the shell's inherited Windows environment.
    /// Runtime metadata is deliberately excluded from persisted tab state.
    pub environment_overrides: Vec<(String, String)>,
    pub manage_process_tree: bool,
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

    /// Augment a session config so a PowerShell shell evaluates the bundled OSC 133
    /// integration at startup. Only applied to a PowerShell shell with no caller-supplied
    /// args, so explicit args (and non-PowerShell shells) are left untouched.
    pub(crate) fn with_shell_integration(mut self: TerminalSessionConfig) -> TerminalSessionConfig {
        if !self.has_trusted_prompt_integration() {
            return self;
        }

        self.args = vec![
            "-NoExit".to_string(),
            "-EncodedCommand".to_string(),
            (*ENCODED_POWERSHELL_INTEGRATION).clone(),
        ];

        self
    }

    pub(crate) fn has_trusted_prompt_integration(&self) -> bool {
        self.args.is_empty() && shell_is_powershell(self.shell.as_deref())
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
        }
    }
}
