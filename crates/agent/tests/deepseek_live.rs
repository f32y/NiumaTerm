//! End-to-end check of the DeepSeek adapter against a real harness host.
//!
//! Ignored by default: it starts `dsh`, spends a model call, and therefore
//! needs both a resolvable installation and a working credential. Run it with
//! `cargo test -p nmt_agent --test deepseek_live -- --ignored --nocapture`.
//! Set `NMT_DSH_TEST_LAUNCHER` to `pnpm-dlx` or `npx` for package launchers.

#![cfg(target_os = "windows")]

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};
use std::{env, fs};

use nmt_agent::chat::{Event, Item, SendOutcome, SlashCommandOutcome};
use nmt_agent::dsh::{Host, Session};
use nmt_agent::profile::agent_launch;
use nmt_agent::{AgentWorkspace, LaunchConfig};
use nmt_profile::{AgentKind, AgentProfile, AgentProfileLauncher};
use tempfile::TempDir;
use uuid::Uuid;

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
    let launcher = match env::var("NMT_DSH_TEST_LAUNCHER")
        .as_deref()
        .unwrap_or("custom")
    {
        "custom" => AgentProfileLauncher::Custom,
        "pnpm-dlx" => AgentProfileLauncher::PnpmDlx,
        "npx" => AgentProfileLauncher::Npx,
        other => panic!("unsupported test launcher: {other}"),
    };

    agent_launch(&AgentProfile {
        kind: AgentKind::DeepSeek,
        executable: "dsh".to_string(),
        launcher,
        ..AgentProfile::default()
    })
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

fn item_text(item: &Item) -> &str {
    match item {
        Item::AgentMessage { text, .. } => text.as_deref().unwrap_or_default(),
        Item::Reasoning { summary, .. } => summary.as_deref().unwrap_or_default(),
        _ => "",
    }
}

#[test]
#[ignore = "requires the local provider runner"]
fn a_steered_message_is_consumed_without_another_submission() {
    assert!(env::var("DEEPSEEK_BASE_URL").is_ok_and(|url| url.starts_with("http://127.0.0.1:")));

    let (tx, frames) = channel();

    let mut session = nmt_platform::runtime()
        .block_on(Session::create(
            &launch(),
            &AgentWorkspace::default(),
            move |frame| {
                let _ = tx.send(frame);
            },
        ))
        .unwrap();

    session
        .send_user_message("queue-probe first", &[])
        .assert_started_a_turn();

    let (before, started) = collect_until(
        &mut session,
        &frames,
        Duration::from_secs(30),
        |event| matches!(event, Event::ItemStarted(Item::UserMessage { text: Some(text) }) if text == "queue-probe first"),
    );

    assert!(started, "the first prompt was not consumed: {before:?}");

    assert_eq!(
        session.send_user_message("queue-probe second", &[]),
        SendOutcome::Steered
    );

    let (after, ended) = collect_until(&mut session, &frames, Duration::from_secs(30), |event| {
        matches!(event, Event::TurnCompleted { .. })
    });

    assert!(ended, "the steered turn did not complete: {after:?}");
    assert!(
        after.iter().any(|event| matches!(event,
            Event::ItemCompleted(Item::AgentMessage { text: Some(text), .. })
                if text.contains("queue-probe consumed"))),
        "steering was not consumed: {after:?}"
    );
    assert!(
        after
            .iter()
            .any(|event| matches!(event, Event::QueuedPrompts(prompts) if prompts.is_empty()))
    );
}

/// The profile is a JSON copy of one persisted agent profile entry. Its normal
/// deserializer reads encrypted credentials without printing them to the log.
#[test]
#[ignore = "requires NMT_DSH_TEST_PROFILE_PATH and spends real model calls"]
fn a_configured_profile_consumes_steering_without_resubmission() {
    let profile_path =
        env::var_os("NMT_DSH_TEST_PROFILE_PATH").expect("provide a persisted profile JSON path");

    let profile: AgentProfile = serde_json::from_slice(&fs::read(profile_path).unwrap()).unwrap();

    assert_eq!(profile.kind, AgentKind::DeepSeek);

    let isolated = TempDir::new().unwrap();
    let workspace = isolated.path().join("workspace");

    fs::create_dir(&workspace).unwrap();

    let mut launch = agent_launch(&profile);

    launch
        .env
        .retain(|(name, _)| !name.eq_ignore_ascii_case("DSH_HOME"));

    launch.env.push((
        "DSH_HOME".into(),
        isolated.path().join("home").display().to_string(),
    ));

    let (tx, frames) = channel();

    let mut session = nmt_platform::runtime()
        .block_on(Session::create(
            &launch,
            &AgentWorkspace::single(Some(workspace.display().to_string())),
            move |frame| {
                let _ = tx.send(frame);
            },
        ))
        .unwrap();

    let (_, ready) = collect_until(
        &mut session,
        &frames,
        Duration::from_secs(30),
        |event| matches!(event, Event::Ready(settings) if settings.model == launch.model),
    );

    assert!(ready, "the configured model was not selected");

    println!("profile={} model={}", profile.name, profile.model);

    let first = "Do not use tools or inspect files. Count from 1 to 60, one number per line, with a short sentence about each number. If a later user message arrives, follow it instead.";

    session
        .send_user_message(first, &[])
        .assert_started_a_turn();

    let (_, consumed) = collect_until(
        &mut session,
        &frames,
        Duration::from_secs(30),
        |event| matches!(event, Event::ItemStarted(Item::UserMessage { text: Some(text) }) if text == first),
    );

    assert!(consumed, "the first prompt was not consumed");

    // Leave time for the first model request to begin before adding its correction.
    let _ = collect_until(&mut session, &frames, Duration::from_secs(1), |_| false);

    assert!(
        session.has_active_operation(),
        "the first turn ended before steering"
    );

    let marker = format!("NMT_QUEUE_OK_{}", Uuid::new_v4().simple());

    let queued =
        format!("Stop counting. Do not use tools or inspect files. Reply with exactly: {marker}");

    let started = Instant::now();

    assert_eq!(
        session.send_user_message(&queued, &[]),
        SendOutcome::Steered
    );

    println!("steered while the first turn was active");

    let mut queued_seen = false;
    let mut latest_queue_empty = false;
    let mut user_echo = false;
    let mut model_replied = false;
    let mut turn_ends = 0;
    let mut turn_running = true;

    let (_, finished) = collect_until(&mut session, &frames, Duration::from_secs(180), |event| {
        match event {
            Event::QueuedPrompts(prompts) => {
                queued_seen |= prompts.iter().any(|prompt| prompt.text == queued);
                latest_queue_empty = prompts.is_empty();

                println!(
                    "+{:.1}s pending={}",
                    started.elapsed().as_secs_f32(),
                    prompts.len()
                );
            }
            Event::ItemStarted(Item::UserMessage { text: Some(text) }) if text == &queued => {
                user_echo = true;

                println!(
                    "+{:.1}s queued prompt consumed",
                    started.elapsed().as_secs_f32()
                );
            }
            Event::ItemCompleted(Item::AgentMessage {
                text: Some(text), ..
            }) => {
                model_replied |= text.contains(&marker);

                println!(
                    "+{:.1}s assistant completed marker={model_replied}",
                    started.elapsed().as_secs_f32()
                );
            }
            Event::TurnCompleted { error } => {
                assert!(error.is_none(), "the turn failed: {error:?}");

                turn_ends += 1;
                turn_running = false;
                println!("+{:.1}s turn ended", started.elapsed().as_secs_f32());
            }
            Event::TurnStarted => turn_running = true,
            Event::Error { message, .. } => panic!("the provider failed: {message}"),
            _ => {}
        }

        user_echo && model_replied && latest_queue_empty && !turn_running
    });

    assert!(
        finished,
        "queued_seen={queued_seen} user_echo={user_echo} model_replied={model_replied} queue_empty={latest_queue_empty} turn_ends={turn_ends}"
    );
    assert!(!session.has_active_operation());
}

#[test]
#[ignore = "starts a real harness host and spends a model call"]
fn a_turn_streams_and_survives_being_stopped() {
    let (tx, frames) = channel();

    let mut session = nmt_platform::runtime()
        .block_on(Session::create(
            &launch(),
            &AgentWorkspace::default(),
            move |frame| {
                let _ = tx.send(frame);
            },
        ))
        .expect("the harness host should start and open a conversation");

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

    // The interrupted message records the partial output under the streamed
    // row's identity, allowing the next attachment to restore it.
    let saved: String = after
        .iter()
        .filter_map(|event| match event {
            Event::ItemCompleted(item) => Some(item_text(item)),
            _ => None,
        })
        .collect();

    assert!(
        saved.contains(before_stop.trim()),
        "the interrupted message should retain the streamed text"
    );

    assert!(
        !session.has_active_operation(),
        "the turn should have ended"
    );

    let id = session.session_id().unwrap().to_string();

    assert!(session.resume_thread(&id));

    let (replayed, restored) =
        collect_until(&mut session, &frames, Duration::from_secs(15), |event| {
            matches!(event, Event::Replay(turns) if turns.iter().flat_map(|turn| &turn.items)
            .map(|entry| item_text(&entry.item)).collect::<String>().contains(before_stop.trim()))
        });

    assert!(restored, "partial output was lost on resume: {replayed:?}");

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
    let host = nmt_platform::runtime()
        .block_on(Host::start(&launch()))
        .expect("the installed harness should serve");

    assert!(host.is_running());
}

#[test]
#[ignore = "starts a real harness host without sending a prompt"]
fn a_session_opens_and_receives_its_preset_catalog() {
    let (tx, frames) = channel();

    let mut session = nmt_platform::runtime()
        .block_on(Session::create(
            &launch(),
            &AgentWorkspace::default(),
            move |frame| {
                let _ = tx.send(frame);
            },
        ))
        .expect("the harness should create a conversation through its local API");

    assert!(session.session_id().is_some());

    let (seen, received) = collect_until(&mut session, &frames, Duration::from_secs(15), |event| {
        matches!(event, Event::AgentPresets { .. })
    });

    assert!(
        received,
        "the session should receive its preset catalog: {seen:?}"
    );
}

#[test]
#[ignore = "starts a real harness host"]
fn two_sessions_share_one_host_and_do_not_see_each_other() {
    let (first_tx, first_frames) = channel();

    let mut first = nmt_platform::runtime()
        .block_on(Session::create(
            &launch(),
            &AgentWorkspace::default(),
            move |frame| {
                let _ = first_tx.send(frame);
            },
        ))
        .expect("the first conversation should open");

    let (second_tx, second_frames) = channel();

    let mut second = nmt_platform::runtime()
        .block_on(Session::create(
            &launch(),
            &AgentWorkspace::default(),
            move |frame| {
                let _ = second_tx.send(frame);
            },
        ))
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

    assert!(
        !leaked.iter().any(|event| matches!(
            event,
            Event::TurnStarted
                | Event::TurnCompleted { .. }
                | Event::ItemStarted(_)
                | Event::ItemCompleted(_)
                | Event::AgentMessageDelta { .. }
                | Event::ReasoningSummaryDelta { .. }
        )),
        "another conversation's activity reached this one: {leaked:?}"
    );
}

#[test]
#[ignore = "starts a real harness host and spends model calls"]
fn an_approval_is_raised_answered_and_the_turn_continues() {
    let outside: PathBuf =
        env::temp_dir().join(format!("nmt-deepseek-approval-{}.txt", Uuid::new_v4()));

    let (tx, frames) = channel();

    let mut session = nmt_platform::runtime()
        .block_on(Session::create(
            &launch(),
            &AgentWorkspace::default(),
            move |frame| {
                let _ = tx.send(frame);
            },
        ))
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

    let (before, _) = collect_until(&mut session, &frames, Duration::from_secs(60), |e| {
        matches!(
            e,
            Event::ApprovalRequested { .. } | Event::TurnCompleted { .. }
        )
    });

    assert!(
        before
            .iter()
            .any(|event| matches!(event, Event::ApprovalRequested { .. })),
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
    let workspace = env::temp_dir().join(format!("nmt-deepseek-tool-{}", Uuid::new_v4()));

    fs::create_dir_all(&workspace).expect("the probe workspace should exist");

    let target = workspace.join("probe-target.txt");

    fs::write(&target, "line one\nbefore\nline three\n").expect("the probe file should be written");

    let (tx, frames) = channel();

    let mut session = nmt_platform::runtime()
        .block_on(Session::create(
            &launch(),
            &AgentWorkspace::single(Some(workspace.display().to_string())),
            move |frame| {
                let _ = tx.send(frame);
            },
        ))
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

    let mut session = nmt_platform::runtime()
        .block_on(Session::create(
            &launch,
            &AgentWorkspace::default(),
            move |frame| {
                let _ = tx.send(frame);
            },
        ))
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

    let mut session = nmt_platform::runtime()
        .block_on(Session::create(
            &launch(),
            &AgentWorkspace::default(),
            move |frame| {
                let _ = tx.send(frame);
            },
        ))
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

#[test]
#[ignore = "starts a real harness host and spends a model call"]
fn a_question_is_answered_and_the_turn_continues() {
    let (tx, frames) = channel();

    let mut session = nmt_platform::runtime()
        .block_on(Session::create(
            &launch(),
            &AgentWorkspace::default(),
            move |frame| {
                let _ = tx.send(frame);
            },
        ))
        .unwrap();

    session.send_user_message("Ask the protocol-probe question using ask_user_question. Offer Yes and No, then report the answer.", &[]).assert_started_a_turn();

    let (before, _) = collect_until(&mut session, &frames, Duration::from_secs(60), |event| {
        matches!(
            event,
            Event::InputRequested(_) | Event::TurnCompleted { .. }
        )
    });

    assert!(
        before.iter().any(
            |event| matches!(event, Event::InputRequested(request) if request.questions.len() == 1)
        ),
        "no question arrived: {before:?}"
    );

    let id = before
        .iter()
        .find_map(|event| match event {
            Event::InputRequested(request) => Some(request.id.as_str()),
            _ => None,
        })
        .unwrap();

    session
        .respond_input(id, Some(vec![vec!["Yes".into()]]))
        .unwrap();

    let (after, ended) = collect_until(&mut session, &frames, Duration::from_secs(60), |event| {
        matches!(event, Event::TurnCompleted { .. })
    });

    assert!(ended, "the answer did not resume the turn: {after:?}");
    assert!(
        after
            .iter()
            .any(|event| matches!(event, Event::InputResolved { .. }))
    );
    assert!(after.iter().any(|event| matches!(event, Event::ItemCompleted(Item::Other { output: Some(output), .. }) if output.contains("Yes"))));
}

#[test]
#[ignore = "starts an isolated harness and declares a test model"]
fn a_profile_can_declare_and_select_an_image_model() {
    let isolated = env::temp_dir().join(format!("nmt-deepseek-profile-{}", Uuid::new_v4()));

    let launch = LaunchConfig {
        model: Some("nmt-probe-vision".into()),
        declares_image_input: true,
        env: vec![("DSH_HOME".into(), isolated.display().to_string())],
        ..launch()
    };

    let (tx, frames) = channel();

    let mut session = nmt_platform::runtime()
        .block_on(Session::create(
            &launch,
            &AgentWorkspace::default(),
            move |frame| {
                let _ = tx.send(frame);
            },
        ))
        .unwrap();

    let (seen, _) = collect_until(&mut session, &frames, Duration::from_secs(5), |_| false);

    assert!(seen.iter().any(|event| matches!(event, Event::Ready(settings) if settings.model.as_deref() == Some("nmt-probe-vision"))), "the custom model was not selected: {seen:?}");
    assert!(
        !seen
            .iter()
            .any(|event| matches!(event, Event::EffortRejected { .. } | Event::Error { .. })),
        "model declaration failed: {seen:?}"
    );

    drop(session);

    let _ = fs::remove_dir_all(&isolated);
}

#[test]
#[ignore = "starts an isolated harness host without sending a prompt"]
fn permission_commands_update_the_session_preset() {
    let isolated = TempDir::new().unwrap();

    let launch = LaunchConfig {
        env: vec![
            ("DSH_HOME".into(), isolated.path().display().to_string()),
            ("DEEPSEEK_API_KEY".into(), "local-probe".into()),
        ],
        ..launch()
    };

    let (tx, frames) = channel();

    let mut session = nmt_platform::runtime()
        .block_on(Session::create(
            &launch,
            &AgentWorkspace::default(),
            move |frame| {
                let _ = tx.send(frame);
            },
        ))
        .expect("the isolated harness should open a conversation");

    let (seen, received) = collect_until(&mut session, &frames, Duration::from_secs(15), |event| {
        matches!(
            event,
            Event::ApprovalPresets {
                current: Some(_),
                ..
            }
        )
    });

    assert!(received, "no initial permission preset arrived: {seen:?}");

    let initial = seen
        .iter()
        .find_map(|event| match event {
            Event::ApprovalPresets { current, .. } => current.clone(),
            _ => None,
        })
        .unwrap();

    assert_ne!(initial, "danger-full-access");

    for preset in ["danger-full-access", initial.as_str()] {
        assert_eq!(
            session.execute_slash_command("permission", preset),
            SlashCommandOutcome::Accepted
        );

        // The command's answer and the projection it moved travel on separate
        // paths, so either may arrive first.
        let mut completed = false;
        let mut published = false;

        let (seen, settled) =
            collect_until(&mut session, &frames, Duration::from_secs(15), |event| {
                match event {
                    Event::SlashCommandResult {
                        outcome: SlashCommandOutcome::Completed { approval, .. },
                        ..
                    } => completed = approval.as_deref() == Some(preset),
                    Event::ApprovalPresets {
                        current: Some(current),
                        ..
                    } if current == preset => published = true,
                    _ => {}
                }

                completed && published
            });

        assert!(settled, "switching to {preset} did not settle: {seen:?}");
        assert!(!session.has_active_operation());
    }

    // A remembered pick restored on the user's behalf says nothing when it
    // takes: the projection it moved is the whole report.
    session.select_permission("danger-full-access");

    let (seen, published) = collect_until(
        &mut session,
        &frames,
        Duration::from_secs(15),
        |event| matches!(event, Event::ApprovalPresets { current: Some(current), .. } if current == "danger-full-access"),
    );

    assert!(published, "the restored pick was not applied: {seen:?}");

    // The answer trails the projection by the length of one reply, so the
    // window below is what would catch a report of the success.
    let (trailing, _) = collect_until(&mut session, &frames, Duration::from_secs(2), |_| false);

    assert!(
        !seen
            .iter()
            .chain(&trailing)
            .any(|event| matches!(event, Event::SlashCommandResult { .. })),
        "a restored pick that took was reported: {seen:?} {trailing:?}"
    );

    // One the deployment does not serve is refused out loud, and the picker
    // is put back on the preset still in force.
    session.select_permission("no-such-preset");

    let mut refused = false;
    let mut reverted = false;

    let (seen, settled) = collect_until(&mut session, &frames, Duration::from_secs(15), |event| {
        match event {
            Event::SlashCommandResult {
                outcome: SlashCommandOutcome::Rejected { .. },
                ..
            } => refused = true,
            Event::ApprovalPresets {
                current: Some(current),
                ..
            } if current == "danger-full-access" => reverted = true,
            _ => {}
        }

        refused && reverted
    });

    assert!(settled, "the refused pick was not reported: {seen:?}");
}

/// A conversation change is a call and a stream handshake, and the thread that
/// asks for it draws the window, so the request has to return at once and the
/// tab has to arrive on the conversation through the frames that follow.
#[test]
#[ignore = "starts an isolated harness host without sending a prompt"]
fn a_conversation_change_is_requested_without_waiting() {
    let isolated = TempDir::new().unwrap();

    let launch = LaunchConfig {
        env: vec![
            ("DSH_HOME".into(), isolated.path().display().to_string()),
            ("DEEPSEEK_API_KEY".into(), "local-probe".into()),
        ],
        ..launch()
    };

    let (tx, frames) = channel();

    let mut session = nmt_platform::runtime()
        .block_on(Session::create(
            &launch,
            &AgentWorkspace::default(),
            move |frame| {
                let _ = tx.send(frame);
            },
        ))
        .expect("the isolated harness should open a conversation");

    let id = session.session_id().unwrap().to_string();

    // The opening replay is set aside first, so the one awaited below can only
    // be the reopened conversation's.
    let (seen, opened) = collect_until(&mut session, &frames, Duration::from_secs(15), |event| {
        matches!(event, Event::Replay(_))
    });

    assert!(opened, "the new conversation never replayed: {seen:?}");

    let asked = Instant::now();

    assert!(session.resume_thread(&id));
    assert!(
        asked.elapsed() < Duration::from_millis(100),
        "the request waited on the harness for {:?}",
        asked.elapsed()
    );

    let (seen, replayed) = collect_until(&mut session, &frames, Duration::from_secs(15), |event| {
        matches!(event, Event::Replay(_))
    });

    assert!(
        replayed,
        "the reopened conversation never replayed: {seen:?}"
    );
    assert_eq!(session.session_id(), Some(id.as_str()));

    // The reopened streams serve commands like the first ones did.
    assert_eq!(
        session.execute_slash_command("permission", "danger-full-access"),
        SlashCommandOutcome::Accepted
    );

    let (seen, answered) = collect_until(&mut session, &frames, Duration::from_secs(15), |event| {
        matches!(
            event,
            Event::SlashCommandResult {
                outcome: SlashCommandOutcome::Completed { .. },
                ..
            }
        )
    });

    assert!(answered, "the command was not answered: {seen:?}");
}
