use crate::chat::{QueuedPrompt, SendOutcome};
use crate::session::AgentKind;
use crate::session::delivery::{MessageDelivery, RecoverablePrompt, Submission};

fn submit(delivery: &mut MessageDelivery, outcome: SendOutcome, text: &str) -> Submission {
    delivery.submit(outcome, text.into(), || None)
}

fn drain(delivery: &mut MessageDelivery) -> Vec<String> {
    let mut confirmed = Vec::new();

    while let Some(text) = delivery.pop_confirmed() {
        confirmed.push(text);
    }

    confirmed
}

fn pending(id: &str, text: &str) -> QueuedPrompt {
    QueuedPrompt {
        id: Some(id.into()),
        text: text.into(),
    }
}

#[test]
fn running_turn_messages_wait_for_agent_output_or_completion() {
    let mut delivery = MessageDelivery::new(AgentKind::Codex);

    assert_eq!(
        submit(&mut delivery, SendOutcome::StartedTurn, "first"),
        Submission::Started {
            text: "first".into()
        }
    );
    assert_eq!(
        submit(&mut delivery, SendOutcome::Steered, "second"),
        Submission::Queued
    );
    assert_eq!(
        submit(&mut delivery, SendOutcome::Steered, "third"),
        Submission::Queued
    );
    assert!(!delivery.provider_started());

    delivery.visible_output();

    assert!(drain(&mut delivery).is_empty());

    delivery.agent_message();

    assert_eq!(drain(&mut delivery), ["second", "third"]);
    assert!(drain(&mut delivery).is_empty());

    submit(&mut delivery, SendOutcome::Steered, "fourth");

    delivery.completed();

    assert_eq!(drain(&mut delivery), ["fourth"]);
    assert!(!delivery.is_active());
}

#[test]
fn following_turn_messages_are_not_published_in_the_previous_turn() {
    let mut delivery = MessageDelivery::new(AgentKind::Claude);

    submit(&mut delivery, SendOutcome::StartedTurn, "first");

    submit(&mut delivery, SendOutcome::Steered, "second");

    delivery.agent_message();

    assert!(drain(&mut delivery).is_empty());

    delivery.completed();

    assert!(drain(&mut delivery).is_empty());

    assert!(delivery.provider_started());
    assert_eq!(delivery.turn(), 2);
    assert_eq!(drain(&mut delivery), ["second"]);
    assert!(!delivery.provider_started());
    assert_eq!(delivery.turn(), 2);
}

#[test]
fn command_turns_do_not_claim_prompts_waiting_for_a_provider_opened_turn() {
    let mut delivery = MessageDelivery::new(AgentKind::Claude);

    submit(&mut delivery, SendOutcome::StartedTurn, "first");

    submit(&mut delivery, SendOutcome::Steered, "second");

    delivery.completed();

    delivery.begin_turn();

    assert!(!delivery.provider_started());
    assert!(drain(&mut delivery).is_empty());

    delivery.completed();

    assert!(delivery.provider_started());
    assert_eq!(drain(&mut delivery), ["second"]);
}

#[test]
fn snapshots_assign_ids_then_claim_disappeared_prompts_in_order() {
    let mut delivery = MessageDelivery::new(AgentKind::DeepSeek);

    submit(&mut delivery, SendOutcome::Steered, "run tests");

    submit(&mut delivery, SendOutcome::Steered, "push");

    submit(&mut delivery, SendOutcome::Steered, "tag");

    assert!(
        delivery
            .snapshot(vec![
                pending("1", "run tests"),
                pending("2", "push"),
                pending("3", "tag"),
            ])
            .is_empty()
    );
    assert_eq!(delivery.pending()[2].id.as_deref(), Some("3"));

    delivery.agent_message();

    delivery.completed();

    assert!(drain(&mut delivery).is_empty());

    assert_eq!(
        delivery.snapshot(vec![pending("2", "push"), pending("3", "tag")]),
        ["run tests"]
    );
    assert_eq!(delivery.snapshot(vec![]), ["push", "tag"]);
}

#[test]
fn snapshots_suppress_the_already_published_first_prompt_until_it_is_claimed() {
    let mut delivery = MessageDelivery::new(AgentKind::DeepSeek);

    submit(&mut delivery, SendOutcome::StartedTurn, "first");

    submit(&mut delivery, SendOutcome::Steered, "second");

    for _ in 0..2 {
        assert!(
            delivery
                .snapshot(vec![pending("1", "first"), pending("2", "second")])
                .is_empty()
        );
        assert_eq!(delivery.pending().len(), 1);
        assert_eq!(delivery.pending()[0].text, "second");
    }

    assert!(delivery.snapshot(vec![pending("2", "second")]).is_empty());
    assert_eq!(delivery.snapshot(vec![]), ["second"]);

    submit(&mut delivery, SendOutcome::Steered, "first");

    assert!(delivery.snapshot(vec![pending("3", "first")]).is_empty());
    assert_eq!(delivery.pending()[0].text, "first");
}

#[test]
fn an_echo_and_a_snapshot_publish_a_pending_message_only_once_in_either_order() {
    for echo_first in [false, true] {
        let mut delivery = MessageDelivery::new(AgentKind::DeepSeek);

        submit(&mut delivery, SendOutcome::Steered, "queued");

        if echo_first {
            assert_eq!(delivery.echoed("queued").as_deref(), Some("queued"));
            assert!(delivery.snapshot(vec![]).is_empty());
        } else {
            assert_eq!(delivery.snapshot(vec![]), ["queued"]);
            assert!(delivery.echoed("queued").is_none());
        }
    }
}

#[test]
fn identical_pending_text_keeps_existing_occurrence_order() {
    let mut delivery = MessageDelivery::new(AgentKind::DeepSeek);

    submit(&mut delivery, SendOutcome::Steered, "same");

    submit(&mut delivery, SendOutcome::Steered, "same");

    assert_eq!(delivery.echoed("same").as_deref(), Some("same"));
    assert_eq!(delivery.pending().len(), 1);
    assert!(
        delivery
            .snapshot(vec![pending("second", "same")])
            .is_empty()
    );
    assert_eq!(delivery.snapshot(vec![]), ["same"]);
}

#[test]
fn out_of_order_echoes_do_not_remove_the_head_and_confirmed_removal_is_not_published() {
    let mut delivery = MessageDelivery::new(AgentKind::DeepSeek);

    delivery.snapshot(vec![pending("1", "first"), pending("2", "second")]);

    assert!(delivery.echoed("second").is_none());
    assert_eq!(delivery.pending().len(), 2);

    delivery.removed("missing");

    assert_eq!(delivery.pending().len(), 2);

    delivery.removed("2");

    assert!(delivery.snapshot(vec![pending("1", "first")]).is_empty());
    assert_eq!(delivery.snapshot(vec![]), ["first"]);
}

fn recoverable(delivery: &mut MessageDelivery) {
    delivery.submit(SendOutcome::StartedTurn, "submitted".into(), || {
        Some(RecoverablePrompt {
            text: "draft".into(),
            response_annotations: vec!["quoted answer".into()],
            skill: None,
        })
    });
}

#[test]
fn an_immediate_interrupt_returns_original_text_and_annotations_only_once() {
    let mut delivery = MessageDelivery::new(AgentKind::Claude);

    recoverable(&mut delivery);

    let (turn, prompt) = delivery.take_interrupted_prompt().unwrap();

    assert_eq!(turn, 1);
    assert_eq!(prompt.text, "draft");
    assert_eq!(prompt.response_annotations, ["quoted answer"]);
    assert!(delivery.take_interrupted_prompt().is_none());
    assert!(!delivery.is_active());
}

#[test]
fn visible_output_completion_exit_and_another_turn_revoke_prompt_recovery() {
    for finish in [
        MessageDelivery::visible_output,
        MessageDelivery::completed,
        MessageDelivery::exited,
        |delivery: &mut MessageDelivery| {
            delivery.begin_turn();
        },
    ] {
        let mut delivery = MessageDelivery::new(AgentKind::Codex);

        recoverable(&mut delivery);

        finish(&mut delivery);

        assert!(delivery.take_interrupted_prompt().is_none());
    }
}

#[test]
fn refused_and_queued_sends_do_not_build_recovery_or_replace_the_active_draft() {
    let mut delivery = MessageDelivery::new(AgentKind::Codex);

    recoverable(&mut delivery);

    for outcome in [
        SendOutcome::NotReady,
        SendOutcome::Rejected {
            message: "busy".into(),
        },
    ] {
        let result = delivery.submit(outcome.clone(), "rejected".into(), || {
            panic!("refused draft")
        });

        match outcome {
            SendOutcome::NotReady => assert_eq!(result, Submission::NotReady),
            SendOutcome::Rejected { message } => {
                assert_eq!(result, Submission::Rejected { message })
            }
            _ => unreachable!(),
        }

        assert_eq!(delivery.turn(), 1);
        assert!(delivery.pending().is_empty());
    }

    delivery.submit(SendOutcome::Steered, "queued".into(), || {
        panic!("queued draft")
    });

    assert_eq!(delivery.take_interrupted_prompt().unwrap().1.text, "draft");
}

#[test]
fn exit_flushes_all_policies_while_update_stop_waits_for_turn_completion() {
    for kind in AgentKind::ALL {
        let mut delivery = MessageDelivery::new(kind);

        recoverable(&mut delivery);

        submit(&mut delivery, SendOutcome::Steered, "queued");

        delivery.stopping_for_update();

        assert_eq!(drain(&mut delivery), ["queued"]);
        assert!(delivery.is_active());

        submit(&mut delivery, SendOutcome::Steered, "later");

        delivery.exited();

        assert_eq!(drain(&mut delivery), ["later"]);
        assert!(!delivery.is_active());
        assert!(delivery.take_interrupted_prompt().is_none());
    }
}

#[test]
fn reset_discards_old_pending_and_recovery_and_replay_reserves_turn_numbers() {
    let mut delivery = MessageDelivery::new(AgentKind::DeepSeek);

    recoverable(&mut delivery);

    submit(&mut delivery, SendOutcome::Steered, "old");

    delivery.reset();

    assert_eq!(delivery.turn(), 0);
    assert!(!delivery.is_active());
    assert!(delivery.pending().is_empty());
    assert!(delivery.take_interrupted_prompt().is_none());
    assert_eq!(delivery.replay_turn(), 1);
    assert_eq!(delivery.replay_turn(), 2);
    assert!(!delivery.is_active());
    assert!(delivery.provider_started());
    assert_eq!(delivery.turn(), 3);
    assert!(drain(&mut delivery).is_empty());
}

#[test]
fn a_failed_start_discards_pending_without_restarting_the_turn_sequence() {
    let mut delivery = MessageDelivery::new(AgentKind::Claude);

    recoverable(&mut delivery);

    submit(&mut delivery, SendOutcome::Steered, "pending");

    delivery.start_failed();

    assert_eq!(delivery.turn(), 1);
    assert!(delivery.pending().is_empty());
    assert!(delivery.take_interrupted_prompt().is_none());
    assert!(drain(&mut delivery).is_empty());
}

#[test]
fn a_provider_start_after_recovering_a_prompt_opens_a_fresh_turn() {
    let mut delivery = MessageDelivery::new(AgentKind::Claude);

    recoverable(&mut delivery);

    assert_eq!(delivery.take_interrupted_prompt().unwrap().0, 1);
    assert!(delivery.provider_started());
    assert_eq!(delivery.turn(), 2);
    assert!(delivery.take_interrupted_prompt().is_none());
}

#[test]
fn confirmed_text_keeps_its_allocation_through_echo_and_snapshot_paths() {
    let mut delivery = MessageDelivery::new(AgentKind::DeepSeek);

    let text: String = "echoed message".into();
    let address = text.as_ptr();

    delivery.submit(SendOutcome::Steered, text, || None);

    let echoed = delivery.echoed("echoed message").unwrap();

    assert_eq!(echoed.as_ptr(), address);

    let text: String = "claimed message".into();
    let address = text.as_ptr();

    delivery.submit(SendOutcome::Steered, text, || None);

    let claimed = delivery.snapshot(vec![]);

    assert_eq!(claimed[0].as_ptr(), address);

    let text: String = "flushed message".into();
    let address = text.as_ptr();

    delivery.submit(SendOutcome::Steered, text, || None);

    delivery.exited();

    let flushed = delivery.pop_confirmed().unwrap();

    assert_eq!(flushed.as_ptr(), address);
}
