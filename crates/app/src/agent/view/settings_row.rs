use nmt_i18n::i18n;

use crate::agent::transcript::permission_icon;
use crate::agent::*;

impl AgentPane {
    /// The dropdown row under the input, per agent kind.
    pub(in crate::agent::view) fn render_settings_row(&self, cx: &mut Context<Self>) -> AnyElement {
        match self.kind {
            AgentKind::Codex => self.render_codex_settings_row(cx).into_any_element(),
            AgentKind::Claude => self.render_claude_settings_row(cx).into_any_element(),
            AgentKind::DeepSeek => self.render_deepseek_settings_row(cx).into_any_element(),
        }
    }

    /// The effort ladder the composer offers, cheapest first. It is this
    /// application's rather than the harness's: Codex reports no per-model
    /// levels at all, so reading them from the session spread its whole
    /// serialization range across the control, including values no model
    /// answers to.
    const EFFORT_LEVELS: [&'static str; 5] = ["low", "medium", "high", "xhigh", "max"];

    /// The shared ladder plus the one level a harness puts above it. Each of
    /// these is a single harness's own top mode, so neither belongs in the
    /// ladder the others share.
    fn effort_levels(kind: AgentKind) -> Vec<(String, String)> {
        let top = match kind {
            AgentKind::Claude => Some(stream_json::ULTRACODE_EFFORT),
            AgentKind::Codex => Some("ultra"),
            AgentKind::DeepSeek => None,
        };

        Self::EFFORT_LEVELS
            .iter()
            .copied()
            .chain(top)
            .map(|value| (value.to_string(), setting_value_label(value)))
            .collect()
    }

    /// Claude settings: model, permission mode, and reasoning effort. The
    /// model catalog comes from the initialize handshake, and all three apply
    /// via control requests before the next message. Models without effort
    /// support (e.g. Haiku) get no effort control.
    fn render_claude_settings_row(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let model_options: Vec<(String, String)> = self
            .models
            .iter()
            .map(|m| (m.model.clone(), m.display.clone()))
            .collect();
        let permission_options: Vec<(String, String)> = stream_json::PERMISSION_OPTIONS
            .iter()
            .map(|v| (v.to_string(), setting_value_label(v)))
            .collect();
        // Which levels exist is this application's call, but whether the
        // model has the setting at all stays the harness's: a model that
        // advertises none (Haiku) gets no control rather than one whose every
        // value it would reject.
        let supports_effort = self
            .models
            .iter()
            .find(|m| Some(&m.model) == self.settings.model.as_ref())
            .is_some_and(|m| !m.efforts.is_empty());

        let model = Self::setting_picker(
            cx,
            "agent-model",
            i18n("agent-setting-model"),
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
            i18n("agent-setting-permissions"),
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
            .child(Self::settings_group(
                i18n("agent-settings-model"),
                vec![model],
                cx,
            ))
            .child(Self::settings_group(
                i18n("agent-settings-execution-policy"),
                vec![permission],
                cx,
            ));

        if supports_effort {
            let effort = Self::effort_panel(
                cx,
                // The protocol never reports the session's current effort;
                // until the user picks one, the honest label is the CLI's
                // own per-model default rather than an empty dash.
                self.settings
                    .effort
                    .clone()
                    .or_else(|| Some("default".to_string())),
                Self::effort_levels(self.kind),
                |this, value, cx| {
                    this.settings.effort = Some(value);
                    this.remember_thread_defaults(cx);
                },
            )
            .into_any_element();
            row = row.child(Self::settings_group(
                i18n("agent-settings-quality-cost"),
                vec![effort],
                cx,
            ));
        }

        row
    }

    /// DeepSeek settings: model, reasoning effort, and permission preset. Each
    /// takes effect on the session immediately rather than riding along with
    /// the next turn.
    ///
    /// The presets come from the harness because its preset table belongs to
    /// the deployment; a list written here would offer values a deployment does
    /// not serve and hide the ones it does. A composition with no permission
    /// service reports none, and then the control is absent rather than empty.
    fn render_deepseek_settings_row(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let model_options: Vec<(String, String)> = self
            .models
            .iter()
            .map(|m| (m.model.clone(), m.display.clone()))
            .collect();
        // The setting belongs to the exact model route, so a model that
        // advertises no levels simply has no effort control; the levels it
        // then offers are the shared ladder.
        let supports_effort = self
            .models
            .iter()
            .find(|m| Some(&m.model) == self.settings.model.as_ref())
            .is_some_and(|m| !m.efforts.is_empty());

        let model = Self::setting_picker(
            cx,
            "agent-model",
            i18n("agent-setting-model"),
            IconName::Cpu,
            self.settings.model.clone(),
            model_options,
            true,
            |this, value, cx| {
                this.settings.model = Some(value);
                this.remember_thread_defaults(cx);
                this.apply_model_selection(cx);
            },
        )
        .into_any_element();

        let mut row = h_flex()
            .w_full()
            .gap_1()
            .flex_wrap()
            .text_color(cx.theme().muted_foreground)
            .child(Self::settings_group(
                i18n("agent-settings-model"),
                vec![model],
                cx,
            ));

        // A deployment that composes no presets has one composition for every
        // conversation, so the control would offer a choice that does not exist.
        if !self.agent_presets.is_empty() {
            let composition_options: Vec<(String, String)> = self
                .agent_presets
                .iter()
                .map(|preset| (preset.value.clone(), preset.label.clone()))
                .collect();

            let composition = Self::setting_picker(
                cx,
                "agent-composition",
                i18n("agent-setting-agent-preset"),
                IconName::Bot,
                self.agent_preset.clone(),
                composition_options,
                false,
                |this, value, cx| this.apply_agent_preset(value, cx),
            )
            .into_any_element();

            row = row.child(Self::settings_group(
                i18n("agent-settings-agent-preset"),
                vec![composition],
                cx,
            ));
        }

        if !self.approval_presets.is_empty() {
            let preset_options: Vec<(String, String)> = self
                .approval_presets
                .iter()
                .map(|preset| (preset.value.clone(), preset.label.clone()))
                .collect();

            let permission = Self::setting_picker(
                cx,
                "agent-permission",
                i18n("agent-setting-permissions"),
                permission_icon(self.settings.approval.as_deref()),
                self.settings.approval.clone(),
                preset_options,
                false,
                |this, value, cx| {
                    // The harness owns the switch, and its own command is what
                    // performs it; the projection that follows is what moves
                    // the row, so nothing is recorded here in advance.
                    this.execute_backend_command(PendingSlashCommand::new("permission", value), cx);
                },
            )
            .into_any_element();

            row = row.child(Self::settings_group(
                i18n("agent-settings-execution-policy"),
                vec![permission],
                cx,
            ));
        }

        if supports_effort {
            let effort = Self::effort_panel(
                cx,
                self.settings.effort.clone(),
                Self::effort_levels(self.kind),
                |this, value, cx| {
                    this.settings.effort = Some(value);
                    this.remember_thread_defaults(cx);
                    this.apply_model_selection(cx);
                },
            )
            .into_any_element();

            row = row.child(Self::settings_group(
                i18n("agent-settings-quality-cost"),
                vec![effort],
                cx,
            ));
        }

        row
    }

    /// Codex settings: model, approval policy, approval reviewer, sandbox,
    /// reasoning effort, and service tier. Values are thread settings sent as
    /// overrides on the next `turn/start`.
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
        let mut tier_options: Vec<(String, String)> =
            vec![(String::new(), setting_value_label("normal"))];

        tier_options.extend(
            self.models
                .iter()
                .find(|m| Some(&m.model) == self.settings.model.as_ref())
                .map(|m| m.tiers.clone())
                .unwrap_or_default(),
        );
        let approval_options: Vec<(String, String)> = app_server::APPROVAL_OPTIONS
            .iter()
            .map(|v| (v.to_string(), setting_value_label(v)))
            .collect();
        let reviewer_options: Vec<(String, String)> = app_server::APPROVAL_REVIEWER_OPTIONS
            .iter()
            .map(|v| (v.to_string(), setting_value_label(v)))
            .collect();
        let sandbox_options: Vec<(String, String)> = app_server::SANDBOX_OPTIONS
            .iter()
            .map(|(v, label)| (v.to_string(), setting_value_label(label)))
            .collect();
        let model = Self::setting_picker(
            cx,
            "agent-model",
            i18n("agent-setting-model"),
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
            i18n("agent-setting-approval"),
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
            i18n("agent-setting-sandbox"),
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
        let reviewer = Self::setting_picker(
            cx,
            "agent-approval-reviewer",
            i18n("agent-setting-approval-reviewer"),
            IconName::User,
            self.settings.approvals_reviewer.clone(),
            reviewer_options,
            false,
            |this, value, cx| {
                this.settings.approvals_reviewer = Some(value);
                this.remember_thread_defaults(cx);
            },
        )
        .into_any_element();
        let effort = Self::effort_panel(
            cx,
            self.settings.effort.clone(),
            Self::effort_levels(self.kind),
            |this, value, cx| {
                this.settings.effort = Some(value);
                this.remember_thread_defaults(cx);
            },
        )
        .into_any_element();
        let tier = Self::setting_picker(
            cx,
            "agent-tier",
            i18n("agent-setting-tier"),
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
            .child(Self::settings_group(
                i18n("agent-settings-model"),
                vec![model],
                cx,
            ))
            .child(Self::settings_group(
                i18n("agent-settings-execution-policy"),
                vec![approval, reviewer, sandbox],
                cx,
            ))
            .child(Self::settings_group(
                i18n("agent-settings-quality-cost"),
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

    /// Height of the effort track, and the inset its thumb keeps from the
    /// track's edge.
    const EFFORT_TRACK_HEIGHT: Pixels = px(26.0);
    const EFFORT_THUMB_INSET: Pixels = px(3.0);

    /// Effort as a small panel instead of a menu: the levels are one ordered
    /// axis from cheapest to most thorough, which a list of names does not
    /// show. The track carries a stop per level and names both ends, so which
    /// way is "more" needs no explaining.
    ///
    /// A press starts a drag the thumb follows, and the release commits the
    /// stop it ends on. Committing on release rather than on every stop the
    /// pointer crosses is what keeps one drag across the track from applying
    /// every level between, which for Claude would send an `/effort` command
    /// per stop.
    fn effort_panel(
        cx: &mut Context<Self>,
        current: Option<String>,
        options: Vec<(String, String)>,
        set: fn(&mut Self, String, &mut Context<Self>),
    ) -> impl IntoElement + use<> {
        let pane = cx.entity();
        let name = i18n("agent-setting-effort");
        let current_label = current
            .as_ref()
            .map(|value| {
                options
                    .iter()
                    .find(|(option_value, _)| option_value == value)
                    .map(|(_, label)| label.clone())
                    .unwrap_or_else(|| setting_value_label(value))
            })
            .unwrap_or_else(|| "-".to_string());
        // Claude never reports the level its session is on, so the label can
        // name a level that is not one of the stops. The track then carries no
        // thumb rather than pointing at a stop the session may not be on.
        let selected = current
            .as_ref()
            .and_then(|value| options.iter().position(|(option, _)| option == value));

        let trigger = Button::new("agent-effort")
            .ghost()
            .small()
            .tooltip(name)
            .aria_label(format!("{name}: {current_label}"))
            .child(
                h_flex()
                    .gap_1p5()
                    .items_center()
                    .child(
                        Icon::new(IconName::Gauge)
                            .size_4()
                            .text_color(cx.theme().muted_foreground.opacity(0.8)),
                    )
                    .child(div().text_sm().child(current_label.clone()))
                    .child(
                        Icon::new(IconName::ChevronDown)
                            .size_3()
                            .text_color(cx.theme().muted_foreground.opacity(0.7)),
                    ),
            );

        Popover::new("agent-effort-panel")
            // The row sits at the bottom edge of the pane, so the panel opens
            // upward from it.
            .anchor(gpui::Anchor::BottomLeft)
            .trigger(trigger)
            .content(move |_, _, cx| {
                let stops = options.len().max(1);
                let width = relative(1.0 / stops as f32);
                // While a drag is in flight the thumb sits where the pointer
                // is rather than where the session is.
                let thumb = pane.read(cx).effort_drag.or(selected);

                v_flex()
                    .w(px(260.))
                    .gap_2()
                    .child(
                        h_flex()
                            .gap_1p5()
                            .items_baseline()
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(i18n("agent-effort-panel-title")),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(current_label.clone()),
                            ),
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .justify_between()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(i18n("agent-effort-faster"))
                            .child(i18n("agent-effort-smarter")),
                    )
                    .child(
                        div()
                            .relative()
                            .w_full()
                            .h(Self::EFFORT_TRACK_HEIGHT)
                            .rounded_full()
                            .bg(cx.theme().muted)
                            // A release away from the stops ends the drag
                            // without choosing, rather than parking the thumb
                            // on a level the session is not on.
                            .on_mouse_up_out(MouseButton::Left, {
                                let pane = pane.clone();
                                move |_, _, cx| {
                                    pane.update(cx, |this, cx| {
                                        if this.effort_drag.take().is_some() {
                                            cx.notify();
                                        }
                                    });
                                }
                            })
                            // The thumb is painted before the stops so they
                            // stay on top of it and keep taking the clicks.
                            .children(thumb.map(|index| {
                                div()
                                    .absolute()
                                    .top(Self::EFFORT_THUMB_INSET)
                                    .bottom(Self::EFFORT_THUMB_INSET)
                                    .left(relative(index as f32 / stops as f32))
                                    .w(width)
                                    .rounded_full()
                                    .bg(cx.theme().background)
                                    .shadow_sm()
                            }))
                            .child(h_flex().absolute().inset_0().children(
                                options.iter().enumerate().map(|(index, (value, label))| {
                                    let value = value.clone();
                                    let pane = pane.clone();

                                    div()
                                        .id(("agent-effort-stop", index))
                                        .flex_1()
                                        .h_full()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .cursor_pointer()
                                        .aria_label(label.clone())
                                        // The stop under the thumb is the
                                        // one already chosen; marking it
                                        // again would only show through it.
                                        .when(Some(index) != thumb, |this| {
                                            this.child(
                                                div()
                                                    .size(px(4.))
                                                    .rounded_full()
                                                    .bg(cx.theme().muted_foreground.opacity(0.45)),
                                            )
                                        })
                                        .on_mouse_down(MouseButton::Left, {
                                            let pane = pane.clone();
                                            move |_, _, cx| {
                                                pane.update(cx, |this, cx| {
                                                    this.effort_drag = Some(index);
                                                    cx.notify();
                                                });
                                            }
                                        })
                                        .on_mouse_move({
                                            let pane = pane.clone();
                                            move |event, _, cx| {
                                                if !event.dragging() {
                                                    return;
                                                }
                                                pane.update(cx, |this, cx| {
                                                    // Moving within the stop
                                                    // the drag already holds
                                                    // is not a change, and a
                                                    // move with no drag in
                                                    // flight started outside
                                                    // the track.
                                                    if this.effort_drag.is_none()
                                                        || this.effort_drag == Some(index)
                                                    {
                                                        return;
                                                    }
                                                    this.effort_drag = Some(index);
                                                    cx.notify();
                                                });
                                            }
                                        })
                                        // The panel stays open on release: a
                                        // level is worth comparing against
                                        // its neighbours, and closing on the
                                        // first pick would make trying two of
                                        // them two round trips.
                                        .on_mouse_up(MouseButton::Left, move |_, _, cx| {
                                            pane.update(cx, |this, cx| {
                                                this.effort_drag = None;
                                                set(this, value.clone(), cx);
                                                cx.notify();
                                            });
                                        })
                                }),
                            )),
                    )
            })
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
                    .unwrap_or_else(|| setting_value_label(value))
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
                    menu = menu.label(i18n("agent-setting-loading"));
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
