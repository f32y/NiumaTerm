//! The settings each harness offers, because each exposes a different set of
//! controls over a different surface.
//!
//! What they share is the pill chrome, the pickers, and the menu listing in
//! the module root; what differs is which settings exist and what a change to
//! one is sent as.

use gpui::{App, Context};
use gpui_component::IconName;
use nmt_agent::claude_code::stream_json;
use nmt_agent::codex::app_server;
use nmt_agent::session::settings::ConversationSettings;
use rust_i18n::t;

use crate::agent_tab::AgentPane;
use crate::agent_tab::commands::setting_value_label;
use crate::agent_tab::composer::PendingSlashCommand;
use crate::agent_tab::profile::AgentKind;
use crate::agent_tab::thread_controls::effort::effort_levels;
use crate::agent_tab::thread_controls::{
    FoldedSetting, HarnessSettings, model_options, remember_defaults,
};
use crate::agent_tab::transcript::permission_icon;

/// Claude settings: model, permission mode, and reasoning effort. The
/// model catalog comes from the initialize handshake, and all three apply
/// via control requests before the next message. Models without effort
/// support (e.g. Haiku) get no effort control.
pub(super) fn claude_settings(
    state: &ConversationSettings,
    kind: AgentKind,
    cx: &App,
) -> HarnessSettings {
    let permission_options: Vec<(String, String)> = stream_json::PERMISSION_OPTIONS
        .iter()
        .map(|v| (v.to_string(), setting_value_label(v)))
        .collect();

    // Which levels exist is this application's call, but whether the
    // model has the setting at all stays the harness's: a model that
    // advertises none (Haiku) gets no control rather than one whose every
    // value it would reject.
    let supports_effort = state
        .models
        .iter()
        .find(|m| Some(&m.model) == state.settings.model.as_ref())
        .is_some_and(|m| !m.efforts.is_empty());

    let model = FoldedSetting {
        name: t!("agent-setting-model"),
        icon: IconName::Cpu,
        current: state.settings.model.clone(),
        options: model_options(state, cx),
        set: |this, value, cx| {
            update_settings(this, cx, |settings| {
                settings.set_model(value);
            });
        },
    };

    let effort = supports_effort.then(|| FoldedSetting {
        name: t!("agent-setting-effort"),
        icon: IconName::Zap,
        // The protocol never reports the session's current effort;
        // until the user picks one, the honest label is the CLI's
        // own per-model default rather than an empty dash.
        current: state
            .settings
            .effort
            .clone()
            .or_else(|| Some("default".to_string())),
        options: effort_levels(kind),
        set: |this, value, cx| {
            update_settings(this, cx, |settings| {
                settings.settings.effort = Some(value);
            });
        },
    });

    let folded = vec![FoldedSetting {
        name: t!("agent-setting-permissions"),
        icon: permission_icon(state.settings.approval.as_deref()),
        current: state.settings.approval.clone(),
        options: permission_options,
        set: |this, value, cx| {
            update_settings(this, cx, |settings| {
                settings.settings.approval = Some(value);
            });
        },
    }];

    HarnessSettings {
        model,
        effort,
        folded,
    }
}

/// DeepSeek settings: model, reasoning effort, and permission preset. Each
/// takes effect on the session immediately rather than riding along with
/// the next turn.
///
/// The presets come from the harness because its preset table belongs to
/// the deployment; a list written here would offer values a deployment does
/// not serve and hide the ones it does. A composition with no permission
/// service reports none, and then the control is absent rather than empty.
pub(super) fn deepseek_settings(
    state: &ConversationSettings,
    kind: AgentKind,
    cx: &App,
) -> HarnessSettings {
    // The setting belongs to the exact model route, so a model that
    // advertises no levels simply has no effort control; the levels it
    // then offers are the shared ladder.
    let supports_effort = state
        .models
        .iter()
        .find(|m| Some(&m.model) == state.settings.model.as_ref())
        .is_some_and(|m| !m.efforts.is_empty());

    let model = FoldedSetting {
        name: t!("agent-setting-model"),
        icon: IconName::Cpu,
        current: state.settings.model.clone(),
        options: model_options(state, cx),
        set: |this, value, cx| {
            if update_settings(this, cx, |settings| settings.set_model(value)) {
                this.apply_model_selection(cx);
            }
        },
    };

    let effort = supports_effort.then(|| FoldedSetting {
        name: t!("agent-setting-effort"),
        icon: IconName::Zap,
        current: state.settings.effort.clone(),
        options: effort_levels(kind),
        set: |this, value, cx| {
            if update_settings(this, cx, |settings| {
                settings.settings.effort = Some(value);
            }) {
                this.apply_model_selection(cx);
            }
        },
    });

    let mut folded = Vec::new();

    // A deployment that composes no presets has one composition for every
    // conversation, so the control would offer a choice that does not exist.
    if !state.agent_presets.is_empty() {
        folded.push(FoldedSetting {
            name: t!("agent-setting-agent-preset"),
            icon: IconName::Bot,
            current: state.settings.agent_preset.clone(),
            options: state
                .agent_presets
                .iter()
                .map(|preset| (preset.value.clone(), preset.label.clone()))
                .collect(),
            set: |this, value, cx| this.apply_agent_preset(value, cx),
        });
    }

    if !state.approval_presets.is_empty() {
        folded.push(FoldedSetting {
            name: t!("agent-setting-permissions"),
            icon: permission_icon(state.settings.approval.as_deref()),
            current: state.settings.approval.clone(),
            options: state
                .approval_presets
                .iter()
                .map(|preset| (preset.value.clone(), preset.label.clone()))
                .collect(),
            set: |this, value, cx| {
                // The harness owns the switch, and its own command is what
                // performs it; the command path records the pick once the
                // harness accepts it.
                this.execute_backend_command(PendingSlashCommand::new("permission", value), cx);
            },
        });
    }

    HarnessSettings {
        model,
        effort,
        folded,
    }
}

/// Codex settings: model, approval policy, approval reviewer, sandbox,
/// reasoning effort, and service tier. Values are thread settings sent as
/// overrides on the next `turn/start`.
pub(super) fn codex_settings(
    state: &ConversationSettings,
    kind: AgentKind,
    cx: &App,
) -> HarnessSettings {
    // Service tiers are per model, and the catalog only lists the
    // additional tiers (e.g. "Fast") — the normal tier is implicit, so
    // the menu carries a synthetic entry for it. Empty protocol value =
    // normal = explicit `serviceTier: null` on the next turn.
    let mut tier_options: Vec<(String, String)> =
        vec![(String::new(), setting_value_label("normal"))];

    tier_options.extend(
        state
            .models
            .iter()
            .find(|m| Some(&m.model) == state.settings.model.as_ref())
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

    let model = FoldedSetting {
        name: t!("agent-setting-model"),
        icon: IconName::Cpu,
        current: state.settings.model.clone(),
        options: model_options(state, cx),
        set: |this, value, cx| {
            update_settings(this, cx, |settings| {
                settings.set_model(value);
            });
        },
    };

    let effort = Some(FoldedSetting {
        name: t!("agent-setting-effort"),
        icon: IconName::Zap,
        current: state.settings.effort.clone(),
        options: effort_levels(kind),
        set: |this, value, cx| {
            update_settings(this, cx, |settings| {
                settings.settings.effort = Some(value);
            });
        },
    });

    let folded = vec![
        FoldedSetting {
            name: t!("agent-setting-approval"),
            icon: permission_icon(state.settings.approval.as_deref()),
            current: state.settings.approval.clone(),
            options: approval_options,
            set: |this, value, cx| {
                update_settings(this, cx, |settings| {
                    settings.settings.approval = Some(value);
                });
            },
        },
        FoldedSetting {
            name: t!("agent-setting-approval-reviewer"),
            icon: IconName::User,
            current: state.settings.approvals_reviewer.clone(),
            options: reviewer_options,
            set: |this, value, cx| {
                update_settings(this, cx, |settings| {
                    settings.settings.approvals_reviewer = Some(value);
                });
            },
        },
        FoldedSetting {
            name: t!("agent-setting-sandbox"),
            icon: IconName::Shield,
            current: state.settings.sandbox.clone(),
            options: sandbox_options,
            set: |this, value, cx| {
                update_settings(this, cx, |settings| {
                    settings.settings.sandbox = Some(value);
                });
            },
        },
        FoldedSetting {
            name: t!("agent-setting-tier"),
            icon: IconName::Zap,
            current: Some(state.settings.tier.clone().unwrap_or_default()),
            options: tier_options,
            set: |this, value, cx| {
                update_settings(this, cx, |settings| {
                    settings.settings.tier = (!value.is_empty()).then_some(value);
                });
            },
        },
    ];

    HarnessSettings {
        model,
        effort,
        folded,
    }
}

fn update_settings(
    pane: &mut AgentPane,
    cx: &mut Context<AgentPane>,
    update: impl FnOnce(&mut ConversationSettings),
) -> bool {
    let Some(host) = pane.host.upgrade() else {
        return false;
    };

    let kind = host.read(cx).kind;
    let profile = host.read(cx).profile.clone();

    update(&mut pane.session.borrow_mut().controls);

    // A Team member's settings belong to its room, which persists them from
    // the session's controls the next time its runtime looks: that runtime
    // observes the session entity, so the change has to notify it. They are
    // one member's choices, so they never become the profile's defaults for
    // new conversations.
    if pane.team_member {
        host.update(cx, |_, cx| cx.notify());

        return true;
    }

    remember_defaults(&pane.session.borrow().controls, kind, &profile, cx);

    true
}
