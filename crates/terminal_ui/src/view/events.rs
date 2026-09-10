use crate::session::TerminalSessionConfig;
use crate::view::*;

pub(super) fn terminal_surface_for_tab(
    wake: &wake::WakeSignal,
    surface_id: u64,
    state: &TabState,
    profile_name: &str,
    cursor_shape: CursorShape,
    environment_overrides: Vec<(String, String)>,
    manage_process_tree: bool,
) -> Result<TerminalSurface, String> {
    let launch = TerminalSessionConfig {
        shell: state.shell.clone(),
        args: state.args.clone(),
        working_dir: state.cwd.clone(),
        starting_title: Some(profile_name.to_string()),
        cursor_shape,
        environment_overrides,
        manage_process_tree,
        ..TerminalSessionConfig::default()
    };

    match TerminalSurface::for_gpui(wake.clone(), surface_id, launch.clone()) {
        Ok(surface) => Ok(surface),

        Err(error) if state.cwd.is_some() => {
            warn!("restored tab failed with saved cwd, retrying without cwd: {error}");

            TerminalSurface::for_gpui(
                wake.clone(),
                surface_id,
                TerminalSessionConfig {
                    working_dir: None,
                    ..launch
                },
            )
        }
        Err(error) => Err(error),
    }
}

impl TerminalPane {
    /// Drain queued host events, refreshing presentation state and returning them so the
    /// shell pump can update chrome (tab title, exited, window title). Runs for
    /// every pane — active or background — driven by the shell's observer.
    pub fn drain_host_events(&mut self) -> Vec<HostEvent> {
        let events = self.surface.session.poll_events();

        for event in &events {
            match event {
                HostEvent::InteractiveState(on) => {
                    info!(interactive = *on, "terminal interactive state changed");
                }
                HostEvent::PromptBoundaryTrusted(on) => {
                    info!(
                        prompt_boundary_trusted = *on,
                        "terminal prompt boundary trust changed"
                    );
                }
                HostEvent::Exit
                | HostEvent::AltScreen(_)
                | HostEvent::Cwd(_)
                | HostEvent::Title(_)
                | HostEvent::Bell
                | HostEvent::Progress(_)
                | HostEvent::Notification { .. } => {}
                HostEvent::CommandFinished { .. } => {
                    // Finishing transfers the active SCREEN rows into an
                    // immutable block, so live selection anchors no longer
                    // address the content they were created for.
                    self.frame_cache.invalidate();

                    self.refresh_blocks();
                }
                // Mirror the session's split block state for the render path.
                HostEvent::CommandStarted => {
                    self.refresh_blocks();
                }
                HostEvent::PromptStarted => {
                    self.refresh_blocks();
                }
            }

            if matches!(
                event,
                HostEvent::PromptBoundaryTrusted(false) | HostEvent::Exit
            ) {
                self.refresh_blocks();
            }
        }

        events
    }

    /// Mirror the session's live split state onto the pane.
    fn refresh_blocks(&mut self) {
        self.in_flight = self.surface.session.in_flight_block();
        self.open_prompt = self.surface.session.open_prompt_region();
    }
}
