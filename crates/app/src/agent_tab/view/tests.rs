use gpui_component::input::Enter;
use nmt_agent::chat::{PromptPlacement, QueuedPrompt};
use nmt_agent::session::capabilities::AgentCapabilities as _;
use nmt_agent::transcript::turns::GenerationSpeed;
use nmt_agent::{AgentWorkspace, MultiRootAccess};
use nmt_config::system::NewlineShortcut;

use crate::agent_tab::AgentKind;
use crate::agent_tab::composer::prompt_with_response_annotations;
use crate::agent_tab::session::UpdateSuspension;
use crate::agent_tab::view::blocking_overlay::update_overlay_label;
use crate::agent_tab::view::composer_layout::{ComposerEnterBehavior, composer_enter_behavior};
use crate::agent_tab::view::composer_notices::{multi_root_notice, queued_message_label};
use crate::agent_tab::view::composer_status::composer_stats_label;

#[test]
fn waiting_message_labels_omit_response_annotation_context_and_name_their_turn() {
    let submitted = prompt_with_response_annotations("Explain this", &["selected text".into()]);

    assert_eq!(
        queued_message_label(&QueuedPrompt::local(
            submitted.clone(),
            PromptPlacement::Queued
        )),
        "Queued message: Explain this"
    );

    // A correction the running turn reads at its next step makes a different
    // promise from one that waits for that turn to end, and the row is the only
    // place the difference shows.
    assert_eq!(
        queued_message_label(&QueuedPrompt::local(submitted, PromptPlacement::Steering)),
        "Read at the next step: Explain this"
    );
}

#[test]
fn composer_stats_append_generation_speed_and_mark_estimates() {
    assert_eq!(
        composer_stats_label(
            0,
            0,
            None,
            None,
            Some(GenerationSpeed {
                tokens_per_second: 42.74,
                estimated: false,
            })
        )
        .as_deref(),
        Some("43 tok/s")
    );

    for (estimated, expected) in [
        (false, "2 turns · 90% cached · 43 tok/s"),
        (true, "2 turns · 90% cached · ~43 tok/s"),
    ] {
        assert_eq!(
            composer_stats_label(
                2,
                0,
                None,
                Some(90),
                Some(GenerationSpeed {
                    tokens_per_second: 42.74,
                    estimated,
                })
            )
            .as_deref(),
            Some(expected)
        );
    }
}

#[test]
fn only_the_teardown_phases_take_the_blocking_overlay() {
    for state in [
        UpdateSuspension::Stopping,
        UpdateSuspension::Updating,
        UpdateSuspension::Reconnecting,
    ] {
        assert!(update_overlay_label(&state).is_some());
    }

    for state in [
        UpdateSuspension::Waiting,
        UpdateSuspension::Failed("failed".into()),
    ] {
        assert!(update_overlay_label(&state).is_none());
    }
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
