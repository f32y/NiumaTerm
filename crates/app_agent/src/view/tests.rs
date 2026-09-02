use std::time::Duration;

use gpui::{point, px};
use gpui_component::input::Enter;
use nmt_agent_utils::chat::QueuedPrompt;
use nmt_agent_utils::{AgentWorkspace, MultiRootAccess};
use nmt_config::system::NewlineShortcut;

use crate::composer::prompt_with_response_annotations;
use crate::session::UpdateSuspension;
use crate::thread_controls::effort::effort_gauge_step;
use crate::view::banners::{
    UpdateOverlayPhase, composer_stats_label, multi_root_notice, update_overlay_phase,
};
use crate::view::history::queued_message_label;
use crate::view::last_response::{LastResponseTone, last_response_tone};
use crate::view::{ComposerEnterBehavior, composer_enter_behavior};
use crate::{AgentKind, SessionHistoryUi};

#[test]
fn queued_message_label_flattens_a_multi_line_prompt() {
    let prompt = QueuedPrompt::local("first\nline".to_string());

    assert_eq!(queued_message_label(&prompt), "Queued message: first line");
}

#[test]
fn queued_message_label_omits_response_annotation_context() {
    let submitted = prompt_with_response_annotations("Explain this", &["selected text".into()]);
    let prompt = QueuedPrompt::local(submitted);

    assert_eq!(
        queued_message_label(&prompt),
        "Queued message: Explain this"
    );
}

#[test]
fn composer_stats_report_only_what_the_conversation_knows() {
    assert_eq!(composer_stats_label(0, 0, None, None), None);

    // A turn that has run but reported nothing else stands on its own.
    assert_eq!(
        composer_stats_label(3, 0, None, None).as_deref(),
        Some("3 turns")
    );

    assert_eq!(
        composer_stats_label(3, 7, Some(Duration::from_millis(820)), Some(94)).as_deref(),
        Some("3 turns · 7 steps · first 820ms · 94% cached")
    );
    assert_eq!(
        composer_stats_label(1, 1, Some(Duration::from_millis(1_240)), None).as_deref(),
        Some("1 turns · 1 steps · first 1.2s")
    );
}

#[test]
fn update_phases_choose_the_blocking_overlay() {
    assert_eq!(
        update_overlay_phase(&UpdateSuspension::Stopping),
        Some(UpdateOverlayPhase::Stopping)
    );
    assert_eq!(
        update_overlay_phase(&UpdateSuspension::Updating),
        Some(UpdateOverlayPhase::Updating)
    );
    assert_eq!(
        update_overlay_phase(&UpdateSuspension::Reconnecting),
        Some(UpdateOverlayPhase::Reconnecting)
    );
    assert_eq!(update_overlay_phase(&UpdateSuspension::Waiting), None);
    assert_eq!(
        update_overlay_phase(&UpdateSuspension::Failed("failed".into())),
        None
    );
}

#[test]
fn composer_newline_shortcut_controls_enter_behavior() {
    let plain = Enter {
        secondary: false,
        shift: false,
    };
    let ctrl = Enter {
        secondary: true,
        shift: false,
    };
    let shift = Enter {
        secondary: false,
        shift: true,
    };

    assert_eq!(
        composer_enter_behavior(NewlineShortcut::CtrlEnter, &plain),
        ComposerEnterBehavior::ActivateOrSubmit
    );

    for (shortcut, ctrl_behavior, shift_behavior) in [
        (
            NewlineShortcut::CtrlEnter,
            ComposerEnterBehavior::InsertNewline,
            ComposerEnterBehavior::Submit,
        ),
        (
            NewlineShortcut::ShiftEnter,
            ComposerEnterBehavior::Submit,
            ComposerEnterBehavior::InsertNewline,
        ),
        (
            NewlineShortcut::Off,
            ComposerEnterBehavior::Submit,
            ComposerEnterBehavior::Submit,
        ),
    ] {
        assert_eq!(composer_enter_behavior(shortcut, &ctrl), ctrl_behavior);
        assert_eq!(composer_enter_behavior(shortcut, &shift), shift_behavior);
    }
}

#[test]
fn a_harness_with_full_access_discloses_nothing() {
    let workspace = AgentWorkspace::new(Some("C:/A".into()), vec!["C:/B".into(), "C:/C".into()]);

    for kind in [AgentKind::Codex, AgentKind::Claude] {
        assert_eq!(kind.caps().multi_root_access, MultiRootAccess::Full);
        assert_eq!(multi_root_notice(kind, &workspace), None);
    }
}

#[test]
fn a_primary_only_harness_names_the_directories_it_cannot_reach() {
    assert_eq!(
        AgentKind::DeepSeek.caps().multi_root_access,
        MultiRootAccess::PrimaryOnly
    );

    // A single-directory workspace loses nothing, so it is told nothing.
    assert_eq!(
        multi_root_notice(
            AgentKind::DeepSeek,
            &AgentWorkspace::single(Some("C:/A".into()))
        ),
        None
    );

    let notice = multi_root_notice(
        AgentKind::DeepSeek,
        &AgentWorkspace::new(Some("C:/A".into()), vec!["C:/B".into(), "C:/C".into()]),
    )
    .expect("a multi-directory workspace is told what its harness cannot use");

    assert!(notice.contains("C:/A"));
    assert!(notice.contains('2'));
}

/// The gauge keeps its empty face for a session that has not reported a
/// level, so the cheapest level of a ladder still moves the needle off it and
/// the dearest fills the arc — whichever length that ladder happens to be.
#[test]
fn the_effort_gauge_spans_whichever_ladder_it_is_given() {
    assert_eq!(effort_gauge_step(None, 6), 0);

    assert_eq!(effort_gauge_step(Some(0), 6), 1);
    assert_eq!(effort_gauge_step(Some(5), 6), 6);

    assert_eq!(effort_gauge_step(Some(0), 5), 1);
    assert_eq!(effort_gauge_step(Some(4), 5), 6);
}

/// The composer says nothing about a conversation picked up soon enough that
/// it costs what it would have cost immediately, warns once it has drifted
/// half the window, and raises that to the danger colour near the end of it.
/// Past the window the answer stops changing, so it stays at danger.
#[test]
fn the_last_response_mark_tracks_how_far_the_window_has_run() {
    assert_eq!(last_response_tone(0), None);
    assert_eq!(last_response_tone(29 * 60), None);

    assert_eq!(last_response_tone(30 * 60), Some(LastResponseTone::Warning));
    assert_eq!(last_response_tone(53 * 60), Some(LastResponseTone::Warning));

    assert_eq!(last_response_tone(54 * 60), Some(LastResponseTone::Danger));
    assert_eq!(last_response_tone(60 * 60), Some(LastResponseTone::Danger));
    assert_eq!(
        last_response_tone(9 * 60 * 60),
        Some(LastResponseTone::Danger)
    );
}

/// The pointer and the arrow keys move one highlight between them, and the
/// pointer takes it whenever it moves. What it must not take is a highlight
/// the arrow keys just moved: navigating scrolls the list, which slides a
/// different row under a pointer that is standing still, and that is not the
/// reader pointing at anything.
#[test]
fn a_still_pointer_does_not_take_the_highlight_back() {
    let mut history = SessionHistoryUi::default();
    let resting = point(px(40.), px(60.));

    assert!(history.point_at(1, resting), "the pointer arrived at a row");
    assert_eq!(history.selected, 1);

    history.selected = 3;
    assert!(
        !history.point_at(2, resting),
        "a row slid under the pointer while the arrow keys drove"
    );
    assert_eq!(history.selected, 3);

    assert!(history.point_at(2, point(px(40.), px(61.))));
    assert_eq!(history.selected, 2, "movement takes it back");

    assert!(
        !history.point_at(2, point(px(41.), px(61.))),
        "moving within the highlighted row changes nothing"
    );
}
