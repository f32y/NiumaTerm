//! The reasoning-effort control: its levels, the gauge that stands for one,
//! and the panel for picking between them.
//!
//! Levels are named differently by each harness and there are more of them
//! than a row has width for, so the control reads as a gauge and opens into
//! the list rather than showing every step inline.

use gpui::prelude::*;
use gpui::{Context, FontWeight, IntoElement, MouseButton, div, px, relative};
use gpui_component::button::Button;
use gpui_component::popover::Popover;
use gpui_component::{ActiveTheme as _, Icon, IconName, h_flex, v_flex};
use nmt_agent_utils::claude_code::stream_json;
use nmt_i18n::i18n;

use crate::AgentPane;
use crate::commands::setting_value_label;
use crate::profile::AgentKind;
use crate::view::settings_row::{
    EFFORT_GAUGE_STEPS, SETTINGS_PILL_CHEVRON, SETTINGS_PILL_ICON, SETTINGS_PILL_TEXT,
};

/// The effort pill's gauge, at the level the session stands on. The pill names
/// that level in words already; the gauge is what makes where it stands on the
/// ladder readable without reading them.
pub(super) struct EffortGaugeIcon(pub(super) usize);

/// Which face a level is drawn on. Levels are counted from one, so the
/// cheapest still moves the needle off the empty face — that face is reserved
/// for a session whose level has not been reported, which Claude never does
/// until the user picks one.
pub(in crate::view) fn effort_gauge_step(level: Option<usize>, stops: usize) -> usize {
    let Some(level) = level.filter(|_| stops > 0) else {
        return 0;
    };

    (((level + 1) * EFFORT_GAUGE_STEPS + stops / 2) / stops).min(EFFORT_GAUGE_STEPS)
}

impl AgentPane {
    /// The shared ladder plus the one level a harness puts above it. Each of
    /// these is a single harness's own top mode, so neither belongs in the
    /// ladder the others share.
    pub(super) fn effort_levels(kind: AgentKind) -> Vec<(String, String)> {
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
    pub(super) fn effort_panel(
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

        let trigger = Self::settings_pill(Button::new("agent-effort"))
            .tooltip(name)
            .aria_label(format!("{name}: {current_label}"))
            .child(
                h_flex()
                    .gap_1p5()
                    .items_center()
                    .child(
                        Icon::new(EffortGaugeIcon(effort_gauge_step(selected, options.len())))
                            .size(px(SETTINGS_PILL_ICON))
                            .text_color(cx.theme().muted_foreground.opacity(0.8)),
                    )
                    .child(
                        div()
                            .text_size(px(SETTINGS_PILL_TEXT))
                            .child(current_label.clone()),
                    )
                    .child(
                        Icon::new(IconName::ChevronDown)
                            .size(px(SETTINGS_PILL_CHEVRON))
                            .text_color(cx.theme().muted_foreground.opacity(0.7)),
                    ),
            );

        let panel = Popover::new("agent-effort-panel")
            // The row sits at the bottom edge of the pane, so the panel opens
            // upward from it.
            .anchor(gpui::Anchor::BottomLeft)
            .trigger(trigger)
            .content(move |_, _, cx| {
                let stops = options.len().max(1);
                let width = relative(1.0 / stops as f32);
                // While a drag is in flight the thumb sits where the pointer
                // is rather than where the session is.
                let thumb = pane.read(cx).controls.effort_drag.or(selected);

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
                                        if this.controls.effort_drag.take().is_some() {
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
                                    // The theme's background carries the
                                    // window translucency, which the Mica
                                    // materials drive to zero; the thumb would
                                    // then be nothing but its own shadow. It
                                    // reads as a solid cap over the track, so
                                    // it takes the base color at full alpha.
                                    .bg(cx.theme().background.alpha(1.0))
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
                                                    this.controls.effort_drag = Some(index);
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
                                                    if this.controls.effort_drag.is_none()
                                                        || this.controls.effort_drag == Some(index)
                                                    {
                                                        return;
                                                    }
                                                    this.controls.effort_drag = Some(index);
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
                                                this.controls.effort_drag = None;
                                                set(this, value.clone(), cx);
                                                cx.notify();
                                            });
                                        })
                                }),
                            )),
                    )
            });

        Self::settings_pill_frame(panel, cx)
    }
}
