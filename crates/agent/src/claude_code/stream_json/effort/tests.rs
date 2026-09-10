use crate::chat::Event;
use crate::claude_code::stream_json::effort::EffortState;

#[test]
fn response_permutations_preserve_the_last_successful_submission() {
    let orders = [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ];

    let ids = ["first", "second", "third"];
    let levels = ["high", "max", "medium"];

    for order in orders {
        for failures in 0..8 {
            let mut state = EffortState::new(Some("low".into()));

            for (id, level) in ids.into_iter().zip(levels) {
                state.record(id.into(), level.into());
            }

            for index in order {
                let error =
                    (failures & (1 << index) != 0).then(|| format!("rejected {}", ids[index]));

                if let Some(Event::EffortRejected { effort, .. }) = state.resolve(ids[index], error)
                {
                    assert_eq!(effort.as_deref(), state.desired());
                }
            }

            let expected = (0..3)
                .rev()
                .find(|index| failures & (1 << index) == 0)
                .map_or("low", |index| levels[index]);

            assert_eq!(
                state.desired(),
                Some(expected),
                "order {order:?}, failures {failures}"
            );
            assert!(!state.has_pending());

            for id in ids {
                assert!(state.resolve(id, Some("duplicate".into())).is_none());
            }
        }
    }
}

#[test]
fn earlier_rejection_does_not_replace_a_newer_pending_selection() {
    let mut state = EffortState::new(Some("low".into()));

    state.record("first".into(), "high".into());
    state.record("second".into(), "max".into());

    assert_eq!(
        state.resolve("first", Some("unavailable".into())),
        Some(Event::EffortRejected {
            message: "unavailable".into(),
            effort: Some("max".into()),
        })
    );
    assert_eq!(state.desired(), Some("max"));
    assert_eq!(
        state.resolve("second", Some("also unavailable".into())),
        Some(Event::EffortRejected {
            message: "also unavailable".into(),
            effort: Some("low".into()),
        })
    );
}

#[test]
fn later_rejection_waits_for_the_confirmed_fallback() {
    let mut state = EffortState::new(Some("low".into()));

    state.record("first".into(), "high".into());
    state.record("second".into(), "max".into());

    assert!(
        state
            .resolve("second", Some("unavailable".into()))
            .is_none()
    );
    assert!(state.resolve("second", None).is_none());
    assert_eq!(
        state.resolve("first", None),
        Some(Event::EffortRejected {
            message: "unavailable".into(),
            effort: Some("high".into()),
        })
    );
    assert_eq!(state.desired(), Some("high"));
}

#[test]
fn close_settles_unknown_changes_without_discarding_known_successes() {
    let mut state = EffortState::new(Some("low".into()));

    state.record("first".into(), "high".into());
    state.record("second".into(), "max".into());

    assert!(state.resolve("second", None).is_none());
    assert_eq!(
        state.close("stopped"),
        Some(Event::EffortRejected {
            message: "stopped".into(),
            effort: Some("max".into()),
        })
    );
    assert!(!state.has_pending());
    assert!(state.close("stopped").is_none());
    assert!(state.resolve("first", None).is_none());
}

#[test]
fn a_rejected_first_change_restores_an_unspecified_launch_level() {
    let mut state = EffortState::default();

    state.record("first".into(), "high".into());

    assert_eq!(
        state.resolve("first", Some("unavailable".into())),
        Some(Event::EffortRejected {
            message: "unavailable".into(),
            effort: None,
        })
    );
    assert_eq!(state.desired(), None);
}

#[test]
fn timeout_keeps_the_selection_uncertain_without_replaying_it() {
    let mut state = EffortState::new(Some("low".into()));

    state.record("first".into(), "high".into());

    assert!(state.expire("first").is_none());
    assert_eq!(state.desired(), Some("high"));
    assert!(!state.has_pending());
    assert!(state.resolve("first", None).is_none());

    state.record("second".into(), "max".into());

    assert_eq!(
        state.resolve("second", Some("unavailable".into())),
        Some(Event::EffortRejected {
            message: "unavailable".into(),
            effort: Some("high".into()),
        })
    );

    state.record("third".into(), "medium".into());

    assert!(state.resolve("third", None).is_none());
    assert_eq!(state.desired(), Some("medium"));
}
