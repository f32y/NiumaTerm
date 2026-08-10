use crate::agent_pane::transcript::permission_icon;
use crate::agent_pane::*;

impl AgentPane {
    /// The dropdown row under the input, per agent kind.
    pub(in crate::agent_pane::view) fn render_settings_row(
        &self,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match self.kind {
            AgentKind::Codex => self.render_codex_settings_row(cx).into_any_element(),
            AgentKind::Claude => self.render_claude_settings_row(cx).into_any_element(),
        }
    }

    /// Claude settings: model, permission mode, and reasoning effort. The
    /// model catalog comes from the initialize handshake; model and
    /// permission changes apply via control requests before the next message.
    /// Effort has no control request — it's applied by sending the `/effort`
    /// slash command as a user message, which the CLI handles locally
    /// (instant, no model call), so the picker sends it immediately as its
    /// own mini-turn. The effort levels are per model; models without effort
    /// support (e.g. Haiku) get no picker.
    fn render_claude_settings_row(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let model_options: Vec<(String, String)> = self
            .models
            .iter()
            .map(|m| (m.model.clone(), m.display.clone()))
            .collect();
        let permission_options: Vec<(String, String)> = stream_json::PERMISSION_OPTIONS
            .iter()
            .map(|v| (v.to_string(), v.to_string()))
            .collect();
        let effort_options: Vec<(String, String)> = self
            .models
            .iter()
            .find(|m| Some(&m.model) == self.settings.model.as_ref())
            .map(|m| m.efforts.iter().map(|v| (v.clone(), v.clone())).collect())
            .unwrap_or_default();

        let model = Self::setting_picker(
            cx,
            "agent-model",
            "model",
            IconName::Cpu,
            self.settings.model.clone(),
            model_options,
            true,
            |this, value, cx| {
                this.settings.model = Some(value);
                this.remember_thread_defaults(cx);
            },
        )
        .into_any_element();
        let permission = Self::setting_picker(
            cx,
            "agent-permission",
            "permissions",
            permission_icon(self.settings.approval.as_deref()),
            self.settings.approval.clone(),
            permission_options,
            false,
            |this, value, cx| {
                this.settings.approval = Some(value);
                this.remember_thread_defaults(cx);
            },
        )
        .into_any_element();

        let mut row = h_flex()
            .w_full()
            .gap_1()
            .flex_wrap()
            .text_color(cx.theme().muted_foreground)
            .child(Self::settings_group("Model", vec![model], cx))
            .child(Self::settings_group(
                "Execution policy",
                vec![permission],
                cx,
            ));

        if !effort_options.is_empty() {
            let effort = Self::setting_picker(
                cx,
                "agent-effort",
                "effort",
                IconName::Gauge,
                // The protocol never reports the session's current effort;
                // until the user picks one, the honest label is the CLI's
                // own per-model default rather than an empty dash.
                self.settings
                    .effort
                    .clone()
                    .or_else(|| Some("default".to_string())),
                effort_options,
                false,
                |this, value, cx| {
                    this.settings.effort = Some(value.clone());
                    this.remember_thread_defaults(cx);
                    this.send_text(format!("/effort {value}"), cx);
                },
            )
            .into_any_element();
            row = row.child(Self::settings_group("Quality and cost", vec![effort], cx));
        }

        row
    }

    /// Codex settings: model, approval policy, sandbox, reasoning effort, and
    /// service tier. Values are thread settings sent as overrides on the next
    /// `turn/start`.
    fn render_codex_settings_row(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let model_options: Vec<(String, String)> = self
            .models
            .iter()
            .map(|m| (m.model.clone(), m.display.clone()))
            .collect();
        // Service tiers are per model, and the catalog only lists the
        // additional tiers (e.g. "Fast") — the normal tier is implicit, so
        // the menu carries a synthetic entry for it. Empty protocol value =
        // normal = explicit `serviceTier: null` on the next turn.
        let mut tier_options: Vec<(String, String)> = vec![(String::new(), "normal".to_string())];

        tier_options.extend(
            self.models
                .iter()
                .find(|m| Some(&m.model) == self.settings.model.as_ref())
                .map(|m| m.tiers.clone())
                .unwrap_or_default(),
        );
        let approval_options: Vec<(String, String)> = app_server::APPROVAL_OPTIONS
            .iter()
            .map(|v| (v.to_string(), v.to_string()))
            .collect();
        let sandbox_options: Vec<(String, String)> = app_server::SANDBOX_OPTIONS
            .iter()
            .map(|(v, label)| (v.to_string(), label.to_string()))
            .collect();
        let effort_options: Vec<(String, String)> = app_server::EFFORT_OPTIONS
            .iter()
            .map(|v| (v.to_string(), v.to_string()))
            .collect();

        let model = Self::setting_picker(
            cx,
            "agent-model",
            "model",
            IconName::Cpu,
            self.settings.model.clone(),
            model_options,
            true,
            |this, value, cx| {
                // A tier the new model doesn't offer falls back to that
                // model's default tier instead of erroring the next turn.
                if let Some(info) = this.models.iter().find(|m| m.model == value)
                    && !this
                        .settings
                        .tier
                        .as_ref()
                        .is_some_and(|tier| info.tiers.iter().any(|(id, _)| id == tier))
                {
                    this.settings.tier = info.default_tier.clone();
                }
                this.settings.model = Some(value);
                this.remember_thread_defaults(cx);
            },
        )
        .into_any_element();
        let approval = Self::setting_picker(
            cx,
            "agent-approval",
            "approval",
            permission_icon(self.settings.approval.as_deref()),
            self.settings.approval.clone(),
            approval_options,
            false,
            |this, value, cx| {
                this.settings.approval = Some(value);
                this.remember_thread_defaults(cx);
            },
        )
        .into_any_element();
        let sandbox = Self::setting_picker(
            cx,
            "agent-sandbox",
            "sandbox",
            IconName::Shield,
            self.settings.sandbox.clone(),
            sandbox_options,
            false,
            |this, value, cx| {
                this.settings.sandbox = Some(value);
                this.remember_thread_defaults(cx);
            },
        )
        .into_any_element();
        let effort = Self::setting_picker(
            cx,
            "agent-effort",
            "effort",
            IconName::Gauge,
            self.settings.effort.clone(),
            effort_options,
            false,
            |this, value, cx| {
                this.settings.effort = Some(value);
                this.remember_thread_defaults(cx);
            },
        )
        .into_any_element();
        let tier = Self::setting_picker(
            cx,
            "agent-tier",
            "tier",
            IconName::Zap,
            Some(self.settings.tier.clone().unwrap_or_default()),
            tier_options,
            false,
            |this, value, cx| {
                this.settings.tier = (!value.is_empty()).then_some(value);
                this.remember_thread_defaults(cx);
            },
        )
        .into_any_element();

        h_flex()
            .w_full()
            .gap_1()
            .flex_wrap()
            .text_color(cx.theme().muted_foreground)
            .child(Self::settings_group("Model", vec![model], cx))
            .child(Self::settings_group(
                "Execution policy",
                vec![approval, sandbox],
                cx,
            ))
            .child(Self::settings_group(
                "Quality and cost",
                vec![effort, tier],
                cx,
            ))
    }

    fn settings_group(
        label: &'static str,
        controls: Vec<AnyElement>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        h_flex()
            .id(label)
            .aria_label(label)
            .gap_0p5()
            .p(px(1.))
            .rounded(UI_RADIUS)
            .border_1()
            .border_color(cx.theme().border.opacity(0.65))
            .bg(cx.theme().muted.opacity(0.2))
            .children(controls)
    }

    /// One dropdown showing `icon · current value · chevron`. Every picker uses
    /// the same quiet color treatment; the model remains wider so its value is
    /// easier to scan. Menus keep the existing protocol values and setters.
    fn setting_picker(
        cx: &mut Context<Self>,
        id: &'static str,
        name: &'static str,
        icon: IconName,
        current: Option<String>,
        options: Vec<(String, String)>,
        is_model: bool,
        set: fn(&mut Self, String, &mut Context<Self>),
    ) -> impl IntoElement + use<> {
        let pane = cx.entity();

        // Show the display label of the current protocol value when we know it.
        let current_label = current
            .as_ref()
            .map(|value| {
                options
                    .iter()
                    .find(|(option_value, _)| option_value == value)
                    .map(|(_, label)| label.clone())
                    .unwrap_or_else(|| value.clone())
            })
            .unwrap_or_else(|| "—".to_string());

        Button::new(id)
            .ghost()
            .when(is_model, |this| this.min_w(px(120.)))
            .small()
            .tooltip(name)
            .aria_label(format!("{name}: {current_label}"))
            .child(
                h_flex()
                    .gap_1p5()
                    .items_center()
                    .child(
                        Icon::new(icon)
                            .size_4()
                            .text_color(cx.theme().muted_foreground.opacity(0.8)),
                    )
                    .child(div().text_sm().child(current_label))
                    .child(
                        Icon::new(IconName::ChevronDown)
                            .size_3()
                            .text_color(cx.theme().muted_foreground.opacity(0.7)),
                    ),
            )
            // Anchored bottom-left so the menu opens upward — the row sits at
            // the bottom edge of the pane.
            .dropdown_menu_with_anchor(gpui::Anchor::BottomLeft, move |menu, _, _| {
                let mut menu = menu;

                if options.is_empty() {
                    menu = menu.label("loading…");
                }

                for (value, label) in options.clone() {
                    let pane = pane.clone();
                    menu = menu.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                        pane.update(cx, |this, cx| {
                            set(this, value.clone(), cx);
                            cx.notify();
                        });
                    }));
                }

                menu
            })
    }
}
