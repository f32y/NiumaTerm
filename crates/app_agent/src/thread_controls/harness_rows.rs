//! The strip each harness gets, because each exposes a different set of
//! controls over a different surface.
//!
//! What they share is the pill chrome and the pickers in the module root; what
//! differs is which controls exist and what a change to one is sent as.

use gpui::prelude::*;
use gpui::{Context, IntoElement, px};
use gpui_component::{ActiveTheme as _, IconName, h_flex};
use nmt_agent_utils::claude_code::stream_json;
use nmt_agent_utils::codex::app_server;
use nmt_i18n::i18n;

use crate::AgentPane;
use crate::commands::setting_value_label;
use crate::composer::PendingSlashCommand;
use crate::profile::AgentKind;
use crate::thread_controls::effort::{effort_levels, effort_panel};
use crate::thread_controls::{
    FoldedSetting, SETTINGS_PILL_GAP, ThreadControls, folded_settings_pill, setting_picker,
    settings_group,
};
use crate::transcript::permission_icon;

impl ThreadControls {
    /// Claude settings: model, permission mode, and reasoning effort. The
    /// model catalog comes from the initialize handshake, and all three apply
    /// via control requests before the next message. Models without effort
    /// support (e.g. Haiku) get no effort control.
    pub(super) fn render_claude_row(
        &self,
        kind: AgentKind,
        cx: &mut Context<AgentPane>,
    ) -> impl IntoElement + use<> {
        let model_options = self.model_options(cx);
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

        let model = setting_picker(
            cx,
            "agent-model",
            i18n("agent-setting-model"),
            IconName::Cpu,
            self.settings.model.clone(),
            model_options,
            |this, value, cx| {
                this.controls.settings.model = Some(value);
                this.controls
                    .remember_defaults(this.kind, &this.profile, cx);
            },
        )
        .into_any_element();
        let folded = vec![FoldedSetting {
            name: i18n("agent-setting-permissions"),
            icon: permission_icon(self.settings.approval.as_deref()),
            current: self.settings.approval.clone(),
            options: permission_options,
            set: |this, value, cx| {
                this.controls.settings.approval = Some(value);
                this.controls
                    .remember_defaults(this.kind, &this.profile, cx);
            },
        }];

        let mut row = h_flex()
            .w_full()
            .gap(px(SETTINGS_PILL_GAP))
            .flex_wrap()
            .text_color(cx.theme().muted_foreground)
            .child(settings_group(i18n("agent-settings-model"), vec![model]));

        if supports_effort {
            let effort = effort_panel(
                cx,
                // The protocol never reports the session's current effort;
                // until the user picks one, the honest label is the CLI's
                // own per-model default rather than an empty dash.
                self.settings
                    .effort
                    .clone()
                    .or_else(|| Some("default".to_string())),
                effort_levels(kind),
                |this, value, cx| {
                    this.controls.settings.effort = Some(value);
                    this.controls
                        .remember_defaults(this.kind, &this.profile, cx);
                },
            )
            .into_any_element();
            row = row.child(settings_group(
                i18n("agent-settings-quality-cost"),
                vec![effort],
            ));
        }

        row.children(folded_settings_pill(cx, folded))
    }

    /// DeepSeek settings: model, reasoning effort, and permission preset. Each
    /// takes effect on the session immediately rather than riding along with
    /// the next turn.
    ///
    /// The presets come from the harness because its preset table belongs to
    /// the deployment; a list written here would offer values a deployment does
    /// not serve and hide the ones it does. A composition with no permission
    /// service reports none, and then the control is absent rather than empty.
    pub(super) fn render_deepseek_row(
        &self,
        kind: AgentKind,
        cx: &mut Context<AgentPane>,
    ) -> impl IntoElement + use<> {
        let model_options = self.model_options(cx);
        // The setting belongs to the exact model route, so a model that
        // advertises no levels simply has no effort control; the levels it
        // then offers are the shared ladder.
        let supports_effort = self
            .models
            .iter()
            .find(|m| Some(&m.model) == self.settings.model.as_ref())
            .is_some_and(|m| !m.efforts.is_empty());

        let model = setting_picker(
            cx,
            "agent-model",
            i18n("agent-setting-model"),
            IconName::Cpu,
            self.settings.model.clone(),
            model_options,
            |this, value, cx| {
                this.controls.settings.model = Some(value);
                this.controls
                    .remember_defaults(this.kind, &this.profile, cx);
                this.apply_model_selection(cx);
            },
        )
        .into_any_element();

        let mut folded = Vec::new();

        // A deployment that composes no presets has one composition for every
        // conversation, so the control would offer a choice that does not exist.
        if !self.agent_presets.is_empty() {
            folded.push(FoldedSetting {
                name: i18n("agent-setting-agent-preset"),
                icon: IconName::Bot,
                current: self.agent_preset.clone(),
                options: self
                    .agent_presets
                    .iter()
                    .map(|preset| (preset.value.clone(), preset.label.clone()))
                    .collect(),
                set: |this, value, cx| this.apply_agent_preset(value, cx),
            });
        }

        if !self.approval_presets.is_empty() {
            folded.push(FoldedSetting {
                name: i18n("agent-setting-permissions"),
                icon: permission_icon(self.settings.approval.as_deref()),
                current: self.settings.approval.clone(),
                options: self
                    .approval_presets
                    .iter()
                    .map(|preset| (preset.value.clone(), preset.label.clone()))
                    .collect(),
                set: |this, value, cx| {
                    // The harness owns the switch, and its own command is what
                    // performs it; the projection that follows is what moves
                    // the row, so nothing is recorded here in advance.
                    this.execute_backend_command(PendingSlashCommand::new("permission", value), cx);
                },
            });
        }

        let mut row = h_flex()
            .w_full()
            .gap(px(SETTINGS_PILL_GAP))
            .flex_wrap()
            .text_color(cx.theme().muted_foreground)
            .child(settings_group(i18n("agent-settings-model"), vec![model]));

        if supports_effort {
            let effort = effort_panel(
                cx,
                self.settings.effort.clone(),
                effort_levels(kind),
                |this, value, cx| {
                    this.controls.settings.effort = Some(value);
                    this.controls
                        .remember_defaults(this.kind, &this.profile, cx);
                    this.apply_model_selection(cx);
                },
            )
            .into_any_element();

            row = row.child(settings_group(
                i18n("agent-settings-quality-cost"),
                vec![effort],
            ));
        }

        row.children(folded_settings_pill(cx, folded))
    }

    /// Codex settings: model, approval policy, approval reviewer, sandbox,
    /// reasoning effort, and service tier. Values are thread settings sent as
    /// overrides on the next `turn/start`.
    pub(super) fn render_codex_row(
        &self,
        kind: AgentKind,
        cx: &mut Context<AgentPane>,
    ) -> impl IntoElement + use<> {
        let model_options = self.model_options(cx);
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
        let model = setting_picker(
            cx,
            "agent-model",
            i18n("agent-setting-model"),
            IconName::Cpu,
            self.settings.model.clone(),
            model_options,
            |this, value, cx| {
                // A tier the new model doesn't offer falls back to that
                // model's default tier instead of erroring the next turn.
                if let Some(info) = this.controls.models.iter().find(|m| m.model == value)
                    && !this
                        .controls
                        .settings
                        .tier
                        .as_ref()
                        .is_some_and(|tier| info.tiers.iter().any(|(id, _)| id == tier))
                {
                    this.controls.settings.tier = info.default_tier.clone();
                }
                this.controls.settings.model = Some(value);
                this.controls
                    .remember_defaults(this.kind, &this.profile, cx);
            },
        )
        .into_any_element();
        let folded = vec![
            FoldedSetting {
                name: i18n("agent-setting-approval"),
                icon: permission_icon(self.settings.approval.as_deref()),
                current: self.settings.approval.clone(),
                options: approval_options,
                set: |this, value, cx| {
                    this.controls.settings.approval = Some(value);
                    this.controls
                        .remember_defaults(this.kind, &this.profile, cx);
                },
            },
            FoldedSetting {
                name: i18n("agent-setting-approval-reviewer"),
                icon: IconName::User,
                current: self.settings.approvals_reviewer.clone(),
                options: reviewer_options,
                set: |this, value, cx| {
                    this.controls.settings.approvals_reviewer = Some(value);
                    this.controls
                        .remember_defaults(this.kind, &this.profile, cx);
                },
            },
            FoldedSetting {
                name: i18n("agent-setting-sandbox"),
                icon: IconName::Shield,
                current: self.settings.sandbox.clone(),
                options: sandbox_options,
                set: |this, value, cx| {
                    this.controls.settings.sandbox = Some(value);
                    this.controls
                        .remember_defaults(this.kind, &this.profile, cx);
                },
            },
            FoldedSetting {
                name: i18n("agent-setting-tier"),
                icon: IconName::Zap,
                current: Some(self.settings.tier.clone().unwrap_or_default()),
                options: tier_options,
                set: |this, value, cx| {
                    this.controls.settings.tier = (!value.is_empty()).then_some(value);
                    this.controls
                        .remember_defaults(this.kind, &this.profile, cx);
                },
            },
        ];
        let effort = effort_panel(
            cx,
            self.settings.effort.clone(),
            effort_levels(kind),
            |this, value, cx| {
                this.controls.settings.effort = Some(value);
                this.controls
                    .remember_defaults(this.kind, &this.profile, cx);
            },
        )
        .into_any_element();

        h_flex()
            .w_full()
            .gap(px(SETTINGS_PILL_GAP))
            .flex_wrap()
            .text_color(cx.theme().muted_foreground)
            .child(settings_group(i18n("agent-settings-model"), vec![model]))
            .child(settings_group(
                i18n("agent-settings-quality-cost"),
                vec![effort],
            ))
            .children(folded_settings_pill(cx, folded))
    }
}
