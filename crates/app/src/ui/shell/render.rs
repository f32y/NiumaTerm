use crate::ui::shell::*;

pub(super) struct ShellChrome {
    /// Tab-strip view state (scroll + active-tab reveal) and its renderer.
    pub(super) tab_strip: TabStrip,

    /// Titlebar daily-token-usage widget; rendered only while the
    /// `show_daily_token_usage` setting is on. Rendered by the sidebar status
    /// cluster; the shell owns it so it outlives a sidebar collapse.
    pub(super) token_usage: Entity<TokenUsageView>,

    /// Compact Codex and Claude rate limits, refreshed independently of terminals.
    pub(super) agent_usage: Entity<AgentUsageView>,

    /// Titlebar `+N -M` indicator (self-gating on its setting).
    pub(super) git_status: Entity<GitStatusView>,

    /// Whether we've started observing the wrapping `Root` (so dialog open/close
    /// re-renders the shell, which draws the dialog layer). Set on first render.
    pub(super) root_observed: bool,

    /// Focus the active pane on the first render (the window root is `Root`, so
    /// initial focus can't be set from the app entry point).
    pub(super) needs_focus: bool,
}

impl ShellChrome {
    pub(super) fn new(git_model: Entity<GitStatusModel>, cx: &mut Context<Shell>) -> Self {
        Self {
            tab_strip: TabStrip::new(),
            token_usage: cx.new(|cx| TokenUsageView::new(daily_source(), cx)),
            agent_usage: cx.new(AgentUsageView::new),
            git_status: cx.new(|cx| GitStatusView::new(git_model.clone(), cx)),
            root_observed: false,
            needs_focus: true,
        }
    }

    pub(super) fn observe_root(&mut self, window: &Window, cx: &mut Context<Shell>) {
        // Re-render the shell whenever the wrapping Root changes (dialog
        // open/close), since the shell draws the dialog layer.
        if !self.root_observed
            && let Some(Some(root)) = window.root::<Root>()
        {
            cx.observe(&root, |_, _, cx| cx.notify()).detach();

            self.root_observed = true;
        }
    }
}
