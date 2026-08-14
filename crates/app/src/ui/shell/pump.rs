use crate::ui::shell::*;

impl Shell {
    /// Observe a pane so its host events reach the shell pump even when the tab
    /// is not visible (the render-damage/host-event split from the design).
    pub(crate) fn watch_pane(pane: &Entity<TerminalPane>, cx: &mut Context<Self>) {
        cx.observe(pane, |this, pane, cx| this.pump_pane(pane, cx))
            .detach();

        cx.subscribe(pane, |this, pane, _: &AgentInterrupted, cx| {
            let route = pane.read(cx).agent_route().clone();

            let mutation = this.agent_monitor.interrupt(&route, time::Instant::now());

            Self::remove_native_notifications(&mutation.removed_notifications);

            if mutation.visible_changed {
                cx.notify();
            }

            this.reschedule_agent_timer(cx);
        })
        .detach();
    }

    /// The id of the tab whose pane tree contains `pane_id`, searched across
    /// all workspaces (pane ids and tab ids come from the same counter but are
    /// no longer equal once a tab is split).
    fn tab_for_pane(&self, pane_id: PaneId) -> Option<TabId> {
        self.workspaces.find_tab_id(|tree| tree.contains(pane_id))
    }

    /// Host-event pump: drain one pane's events (applying its pane-side effects)
    /// and fold the chrome-visible ones into the owning tab.
    fn pump_pane(&mut self, pane: Entity<TerminalPane>, cx: &mut Context<Self>) {
        let pane_id = PaneId(pane.read(cx).id());
        let agent_route = pane.read(cx).agent_route().clone();
        let events = pane.update(cx, |pane, _cx| pane.drain_host_events());

        let mut chrome_changed = false;
        let mut session_changed = false;

        for event in &events {
            match event {
                HostEvent::Title(title) => {
                    if let Some(tab_id) = self.tab_for_pane(pane_id)
                        && self
                            .workspaces
                            .tab_manager_for(tab_id)
                            .and_then(|tabs| tabs.find(tab_id))
                            .is_some_and(|tab| {
                                tab.surface().tree().is_some_and(|t| t.focused() == pane_id)
                            })
                        && let Some(tabs) = self.workspaces.tab_manager_for_mut(tab_id)
                    {
                        chrome_changed |= tabs.set_title(tab_id, title.clone());
                    }
                }
                HostEvent::Exit => {
                    self.remove_agent_route(&agent_route, cx);
                    if let Some(tab_id) = self.tab_for_pane(pane_id) {
                        // A pane whose shell exits auto-closes when the tab has
                        // other panes (the split collapses around it); the last
                        // pane lingers read-only and marks the tab exited, as
                        // before splits existed.
                        let mut removed = None;

                        if let Some(tabs) = self.workspaces.tab_manager_for_mut(tab_id) {
                            if let Some(tab) = tabs.find_mut(tab_id)
                                && tab.surface().tree().is_some_and(|t| !t.is_single_leaf())
                            {
                                removed = tab.surface_mut().live_mut().remove(pane_id);
                            }
                            if removed.is_none() {
                                tabs.mark_exited(tab_id);
                            }

                            // A dead command sends no state-0 report, so its
                            // bar would otherwise sit at whatever it reached.
                            tabs.clear_progress(tab_id);
                        }

                        if let Some((pane, outcome)) = removed {
                            if let RemoveOutcome::RemovedFromSplit { state, index } = outcome {
                                state.update(cx, |state, cx| state.remove_panel(index, cx));
                            }

                            // Dropping the pane entity releases its surface and
                            // ConPTY, same as an explicit pane close.
                            drop(pane);

                            // Re-run focus_active on the next render (the pump
                            // has no Window).
                            self.needs_focus = true;

                            self.sync_session_memory(cx);
                        }
                        chrome_changed = true;
                    }
                }
                HostEvent::Bell => {
                    // Only background tabs get the indicator: a bell on the tab
                    // in front of you is already conveyed by the sound and the
                    // output itself, and flagging it would need a timer to
                    // expire the flag again.
                    if let Some(tab_id) = self.tab_for_pane(pane_id)
                        && self.workspaces.active_tabs().active_id() != tab_id
                        && let Some(tabs) = self.workspaces.tab_manager_for_mut(tab_id)
                    {
                        tabs.ring_bell(tab_id);
                        chrome_changed = true;
                    }
                }
                HostEvent::Progress(report) => {
                    if let Some(tab_id) = self.tab_for_pane(pane_id)
                        && let Some(tabs) = self.workspaces.tab_manager_for_mut(tab_id)
                    {
                        tabs.set_progress(tab_id, *report);
                        chrome_changed = true;
                    }
                }
                HostEvent::CommandFinished { exit_code } => {
                    // Only a tab the user is not watching records its result: a
                    // command that ends in front of them already shows its own
                    // output, and the record would clear on activation anyway.
                    if let Some(tab_id) = self.tab_for_pane(pane_id)
                        && self.workspaces.active_tabs().active_id() != tab_id
                        && let Some(tabs) = self.workspaces.tab_manager_for_mut(tab_id)
                    {
                        tabs.record_outcome(tab_id, CommandOutcome::from_exit_code(*exit_code));
                    }

                    chrome_changed = true;
                }
                // A command starting flips the workspace indicator, which lives
                // in the chrome rather than in the pane's own grid.
                HostEvent::InteractiveState(_)
                | HostEvent::PromptBoundaryTrusted(_)
                | HostEvent::PromptStarted
                | HostEvent::CommandStarted => chrome_changed = true,
                HostEvent::Cwd(_) => session_changed = true,
                HostEvent::Notification { title, body } => {
                    let mutation = self.agent_monitor.notify(&agent_route, title, body);

                    Self::remove_native_notifications(&mutation.removed_notifications);

                    chrome_changed |= mutation.visible_changed;

                    self.process_native_notifications(cx);
                }
                _ => {}
            }
        }
        if session_changed {
            self.sync_session_memory(cx);
            self.sync_git_target(cx);
        }
        if chrome_changed {
            cx.notify();
        }
    }
}
