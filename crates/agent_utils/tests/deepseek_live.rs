//! End-to-end check of the DeepSeek adapter against a real harness host.
//!
//! Ignored by default: it starts `dsh`, spends a model call, and therefore
//! needs both a resolvable installation and a working credential. Run it with
//! `cargo test -p nmt_agent_utils --test deepseek_live -- --ignored --nocapture`.

#![cfg(target_os = "windows")]

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};
use std::{env, fs};

use nmt_agent_utils::LaunchConfig;
use nmt_agent_utils::chat::{Event, Item, SendOutcome};
use nmt_agent_utils::deepseek::{Host, Session};

/// A prompt into an idle conversation starts a turn of its own. Steering means
/// this side thought a turn was running, and a refusal means the harness would
/// not take the prompt at all; either way the scenario below cannot continue.
trait StartsATurn {
    fn assert_started_a_turn(self);
}

impl StartsATurn for SendOutcome {
    fn assert_started_a_turn(self) {
        assert_eq!(self, SendOutcome::StartedTurn);
    }
}

/// A turn long enough that there is always partial output to lose when the
/// cancel lands, which is the property this is checking.
const LONG_PROMPT: &str =
    "Count from 1 to 400, one number per line, with a short remark on each. Do not stop early.";

fn launch() -> LaunchConfig {
    LaunchConfig {
        executable: "dsh".to_string(),
        ..LaunchConfig::default()
    }
}

/// Drain events until `stop` accepts one, or the deadline passes. Returns every
/// event seen, so a failure can be read from the whole stream rather than from
/// the one that was being waited for.
fn collect_until(
    session: &mut Session,
    frames: &Receiver<serde_json::Value>,
    timeout: Duration,
    mut stop: impl FnMut(&Event) -> bool,
) -> (Vec<Event>, bool) {
    let deadline = Instant::now() + timeout;
    let mut seen = Vec::new();

    while Instant::now() < deadline {
        let Ok(frame) = frames.recv_timeout(Duration::from_millis(200)) else {
            continue;
        };
        for event in session.process(frame) {
            let matched = stop(&event);
            seen.push(event);
            if matched {
                return (seen, true);
            }
        }
    }

    (seen, false)
}

fn folded_text(events: &[Event]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            Event::AgentMessageDelta { delta, .. } | Event::ReasoningSummaryDelta { delta, .. } => {
                Some(delta.as_str())
            }
            _ => None,
        })
        .collect()
}

#[test]
#[ignore = "starts a real harness host and spends a model call"]
fn a_turn_streams_and_survives_being_stopped() {
    let (tx, frames) = channel();
    let mut session = Session::create(&launch(), None, move |frame| {
        let _ = tx.send(frame);
    })
    .expect("the harness host should start and open a conversation");

    assert!(session.host_is_running());
    assert!(session.session_id().is_some());

    session
        .send_user_message(LONG_PROMPT, &[])
        .assert_started_a_turn();

    let (started, saw_start) = collect_until(&mut session, &frames, Duration::from_secs(60), |e| {
        matches!(e, Event::TurnStarted)
    });
    assert!(saw_start, "no turn started; saw {started:?}");

    // Let real output accumulate before stopping, so "the partial answer
    // survives" is a claim about text that actually existed.
    let (streamed, _) = collect_until(&mut session, &frames, Duration::from_secs(15), |_| false);
    let before_stop = folded_text(&streamed);
    assert!(
        before_stop.len() > 40,
        "expected streamed text before the stop, got {before_stop:?}"
    );
    assert!(session.has_active_operation(), "the turn should be running");

    session.interrupt();

    let (after, ended) = collect_until(&mut session, &frames, Duration::from_secs(30), |e| {
        matches!(e, Event::TurnCompleted { .. })
    });
    assert!(ended, "the stopped turn never completed; saw {after:?}");
    assert!(
        matches!(
            after.iter().last(),
            Some(Event::TurnCompleted { error: None })
        ),
        "a user stop is not a failure; got {:?}",
        after.iter().last()
    );

    // The harness emits no completed message for a stopped turn, so the streamed
    // rows are the only record of the partial answer.
    assert!(
        !after
            .iter()
            .any(|e| matches!(e, Event::ItemCompleted(Item::AgentMessage { .. }))),
        "a stopped turn should not produce a completed assistant message"
    );

    assert!(
        !session.has_active_operation(),
        "the turn should have ended"
    );
    assert!(session.host_is_running(), "the host outlives its turns");

    // The tab keeps working after a stop. The instruction is emphatic because
    // the abandoned counting task is still in context, and a model that
    // resumes it would outlast any reasonable budget here.
    session
        .send_user_message(
            "Abandon the counting task completely. Do not count. Reply with exactly: ok",
            &[],
        )
        // The stop settled before this line, so the conversation is idle and
        // the prompt starts its own turn rather than steering the old one.
        .assert_started_a_turn();
    let (second, restarted) = collect_until(&mut session, &frames, Duration::from_secs(180), |e| {
        matches!(e, Event::TurnCompleted { .. })
    });
    assert!(restarted, "the second turn never completed; saw {second:?}");
    assert!(
        second
            .iter()
            .any(|e| matches!(e, Event::ItemCompleted(Item::AgentMessage { .. }))),
        "a completed turn should produce a completed assistant message"
    );
}

/// `--no-open` keeps the host from opening the served page in a browser, and a
/// release that predates the flag refuses to start when it is passed. The start
/// path therefore has to reach a serving host on both, which is what this runs.
#[test]
#[ignore = "starts a real harness host"]
fn the_host_serves_whether_or_not_it_knows_the_no_browser_flag() {
    let host = Host::start(&launch()).expect("the installed harness should serve");

    assert!(host.is_running());
}

#[test]
#[ignore = "starts a real harness host"]
fn two_sessions_share_one_host_and_do_not_see_each_other() {
    let (first_tx, first_frames) = channel();
    let mut first = Session::create(&launch(), None, move |frame| {
        let _ = first_tx.send(frame);
    })
    .expect("the first conversation should open");

    let (second_tx, second_frames) = channel();
    let mut second = Session::create(&launch(), None, move |frame| {
        let _ = second_tx.send(frame);
    })
    .expect("the second conversation should reuse the running host");

    assert_ne!(first.session_id(), second.session_id());

    first
        .send_user_message("Reply with exactly: first", &[])
        .assert_started_a_turn();

    let (own, ended) = collect_until(&mut first, &first_frames, Duration::from_secs(60), |e| {
        matches!(e, Event::TurnCompleted { .. })
    });
    assert!(ended, "the first turn never completed; saw {own:?}");

    // The mux stream is aggregated across every attached session, so the second
    // conversation receives the first one's frames and must discard them.
    let (leaked, _) = collect_until(&mut second, &second_frames, Duration::from_secs(2), |_| {
        false
    });
    assert_eq!(
        leaked,
        Vec::new(),
        "another conversation's activity reached this one"
    );
}

#[test]
#[ignore = "resolves the installed harness"]
fn the_installed_release_is_one_this_build_supports() {
    use nmt_agent_utils::deepseek::{SUPPORTED_VERSIONS, VersionSupport, describe_version};
    use nmt_agent_utils::launcher::AgentCli;

    let cli = AgentCli::from_launch(&launch(), "dsh");

    assert_eq!(
        describe_version(&cli),
        VersionSupport::Supported,
        "the installed harness is outside {SUPPORTED_VERSIONS}"
    );
}

#[test]
#[ignore = "starts a real harness host and spends model calls"]
fn an_approval_is_raised_answered_and_the_turn_continues() {
    let outside: PathBuf = env::temp_dir().join("nmt-deepseek-approval-probe.txt");
    let _ = fs::remove_file(&outside);

    let (tx, frames) = channel();
    let mut session = Session::create(&launch(), None, move |frame| {
        let _ = tx.send(frame);
    })
    .expect("the harness host should start and open a conversation");

    // Writing outside the workspace is denied under the default sandbox, and
    // the model escalates, which is what raises the approval.
    session
        .send_user_message(
            &format!(
                "Write the text approval-probe-ok to {} using a shell command. \
             If it is denied, escalate permissions and try again.",
                outside.display()
            ),
            &[],
        )
        .assert_started_a_turn();

    let (before, asked) = collect_until(&mut session, &frames, Duration::from_secs(180), |e| {
        matches!(e, Event::ApprovalRequested { .. })
    });
    assert!(
        asked,
        "no approval was raised, so this cannot check answering one; saw {before:?}"
    );

    let description = before
        .iter()
        .rev()
        .find_map(|e| match e {
            Event::ApprovalRequested { description } => Some(description.clone()),
            _ => None,
        })
        .expect("the request carries a description");
    assert!(
        !description.trim().is_empty(),
        "an approval card with no text tells the user nothing"
    );

    // The turn is blocked here. Before this was handled, nothing answered and
    // it stayed blocked until the user stopped the agent.
    session.respond_approval("accept");

    let (after, ended) = collect_until(&mut session, &frames, Duration::from_secs(180), |e| {
        matches!(e, Event::TurnCompleted { .. })
    });
    assert!(
        ended,
        "the turn did not continue after the approval was answered; saw {after:?}"
    );
    assert!(
        after.iter().any(|e| matches!(e, Event::ApprovalResolved)),
        "the harness never reported the approval resolved; saw {after:?}"
    );

    // The grant reached the harness: the escalated write actually happened.
    assert!(
        outside.is_file(),
        "the approved command did not run: {} was never written",
        outside.display()
    );
    let _ = fs::remove_file(&outside);
}

#[test]
#[ignore = "starts a real harness host and spends a model call"]
fn a_real_turn_shows_its_commands_and_file_changes() {
    // A workspace of its own rather than the temp root itself: the harness
    // refuses to run its shell tool when its ACL temp root and the workspace
    // are the same directory, which a bare temp-dir workspace makes true.
    let workspace = env::temp_dir().join("nmt-deepseek-tool-probe");
    fs::create_dir_all(&workspace).expect("the probe workspace should exist");
    let target = workspace.join("probe-target.txt");
    fs::write(&target, "line one\nbefore\nline three\n").expect("the probe file should be written");

    let (tx, frames) = channel();
    let mut session = Session::create(
        &launch(),
        Some(workspace.display().to_string()),
        move |frame| {
            let _ = tx.send(frame);
        },
    )
    .expect("the harness host should start and open a conversation");

    session
        .send_user_message(
            &format!(
                "Do exactly two things and then stop. First run a shell command that prints \
             tool-probe-ok. Second, edit {} replacing the word before with after.",
                target.display()
            ),
            &[],
        )
        .assert_started_a_turn();

    // A real tab has a user behind it, so anything the harness stops to ask is
    // answered here too; otherwise the turn blocks and this measures nothing.
    let mut events = Vec::new();
    let mut ended = false;
    for _ in 0..12 {
        let (batch, done) = collect_until(&mut session, &frames, Duration::from_secs(60), |e| {
            matches!(
                e,
                Event::TurnCompleted { .. } | Event::ApprovalRequested { .. }
            )
        });
        let asked = matches!(batch.last(), Some(Event::ApprovalRequested { .. }));
        events.extend(batch);
        if done && !asked {
            ended = true;
            break;
        }
        if asked {
            session.respond_approval("accept");
        }
    }
    assert!(
        ended,
        "the turn never completed; saw {} events, last: {:?}",
        events.len(),
        events.last()
    );

    // Every tool row the harness opened has to close, or the transcript keeps a
    // spinner running against work that already finished.
    let started: Vec<&Item> = events
        .iter()
        .filter_map(|e| match e {
            Event::ItemStarted(item) => Some(item),
            _ => None,
        })
        .collect();
    let completed: Vec<&Item> = events
        .iter()
        .filter_map(|e| match e {
            Event::ItemCompleted(item) => Some(item),
            _ => None,
        })
        .collect();

    let command_ran = completed.iter().any(|item| {
        matches!(item, Item::CommandExecution { aggregated_output, status, .. }
            if aggregated_output.as_deref().unwrap_or_default().contains("tool-probe-ok")
                && status.as_deref() == Some("completed"))
    });
    assert!(
        command_ran,
        "no completed command row carried the output; started={started:?} completed={completed:?}"
    );

    let file_changed = completed.iter().any(|item| {
        matches!(item, Item::FileChange { diff, status, .. }
            if diff.as_deref().unwrap_or_default().contains("+after")
                && status.as_deref() == Some("completed"))
    });
    assert!(
        file_changed,
        "no completed file row carried the change; completed={completed:?}"
    );

    // The mapping keys on the card, so a tool with no dedicated row still has
    // to appear rather than being dropped.
    assert!(
        started
            .iter()
            .all(|item| !matches!(item, Item::Other { title, .. } if title.is_empty())),
        "a tool row was opened with nothing to identify it: {started:?}"
    );

    let _ = fs::remove_dir_all(&workspace);
}

#[test]
#[ignore = "starts a real harness host"]
fn a_profile_pinning_an_unserved_effort_is_told_rather_than_ignored() {
    // The levels belong to the adapter behind the route, and DeepSeek's are
    // off/low/high/max, so this names one no route serves. Applying a profile's
    // pick runs in the background with no control waiting on the answer, which
    // is exactly where a refusal would otherwise go unreported.
    let launch = LaunchConfig {
        effort: Some("medium".to_string()),
        model: Some("deepseek-chat".to_string()),
        ..launch()
    };

    let (tx, frames) = channel();
    let mut session = Session::create(&launch, None, move |frame| {
        let _ = tx.send(frame);
    })
    .expect("the harness host should start and open a conversation");

    let (seen, refused) = collect_until(&mut session, &frames, Duration::from_secs(60), |e| {
        matches!(e, Event::EffortRejected { .. })
    });
    assert!(refused, "the refusal should reach the pane, got {seen:?}");

    let Some(Event::EffortRejected { message, effort }) = seen
        .iter()
        .find(|event| matches!(event, Event::EffortRejected { .. }))
    else {
        unreachable!("the refusal was just matched");
    };
    assert!(!message.is_empty());
    // The level reported back is the one the session is on, so the control
    // lands on something the route actually serves.
    assert_ne!(effort.as_deref(), Some("medium"));
}

#[test]
#[ignore = "starts a real harness host"]
fn the_agent_preset_roster_reaches_the_picker() {
    let (tx, frames) = channel();
    let mut session = Session::create(&launch(), None, move |frame| {
        let _ = tx.send(frame);
    })
    .expect("the harness host should start and open a conversation");

    let (seen, listed) = collect_until(&mut session, &frames, Duration::from_secs(60), |e| {
        matches!(e, Event::AgentPresets { .. })
    });
    assert!(listed, "the roster should reach the pane, got {seen:?}");

    let Some(Event::AgentPresets { presets, current }) = seen
        .iter()
        .find(|event| matches!(event, Event::AgentPresets { .. }))
    else {
        unreachable!("the roster was just matched");
    };

    // A deployment composing no presets is a legitimate answer, but then there
    // is no current one either: the two have to agree.
    if presets.is_empty() {
        assert_eq!(current.as_deref(), None);
        return;
    }

    let current = current
        .as_deref()
        .expect("a composed session names its preset");
    assert!(
        presets.iter().any(|preset| preset.value == current),
        "the conversation's own preset {current} should be one the picker offers",
    );
}
