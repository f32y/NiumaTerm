use nmt_agent_utils::workflow::{
    WorkflowAgent, WorkflowAgentState, WorkflowPhase, WorkflowRun, WorkflowRunState,
};

use crate::ui::workflows::{agent_state_label, group_agents_by_phase, run_state_label, run_totals};

fn agent(index: u64, phase_index: Option<u64>) -> WorkflowAgent {
    WorkflowAgent {
        index,
        agent_id: Some(format!("agent-{index}")),
        label: Some(format!("step-{index}")),
        phase_index,
        phase_title: None,
        agent_type: None,
        isolation: None,
        model: None,
        state: WorkflowAgentState::Done,
        tokens: None,
        tool_calls: None,
        reused: false,
        error: None,
        prompt_preview: None,
        result_preview: None,
    }
}

fn run(phases: Vec<WorkflowPhase>, agents: Vec<WorkflowAgent>) -> WorkflowRun {
    WorkflowRun {
        task_id: "wacob045a".into(),
        run_id: None,
        name: Some("two-ok".into()),
        summary: None,
        state: WorkflowRunState::Running,
        phases,
        agents,
        total_tokens: None,
        total_tool_calls: None,
        result: None,
        refresh_failed: false,
    }
}

#[test]
fn agents_group_under_their_phase_in_provider_order() {
    let run = run(
        vec![
            WorkflowPhase {
                index: 1,
                title: "Find".into(),
            },
            WorkflowPhase {
                index: 2,
                title: "Verify".into(),
            },
        ],
        vec![agent(1, Some(1)), agent(2, Some(2)), agent(3, Some(1))],
    );

    let grouped = group_agents_by_phase(&run);
    assert_eq!(grouped.len(), 2);
    assert_eq!(grouped[0].0, Some("Find"));
    assert_eq!(
        grouped[0].1.iter().map(|a| a.index).collect::<Vec<_>>(),
        vec![1, 3]
    );
    assert_eq!(grouped[1].0, Some("Verify"));
    assert_eq!(
        grouped[1].1.iter().map(|a| a.index).collect::<Vec<_>>(),
        vec![2]
    );
}

#[test]
fn a_phase_with_no_agents_is_not_shown() {
    let run = run(
        vec![
            WorkflowPhase {
                index: 1,
                title: "Find".into(),
            },
            WorkflowPhase {
                index: 2,
                title: "Verify".into(),
            },
        ],
        vec![agent(1, Some(1))],
    );

    let grouped = group_agents_by_phase(&run);
    assert_eq!(grouped.len(), 1);
    assert_eq!(grouped[0].0, Some("Find"));
}

#[test]
fn an_agent_outside_every_reported_phase_is_still_listed() {
    // A run that reported no phases, and an agent naming one the run never
    // listed, both have to remain visible.
    let unphased = run(Vec::new(), vec![agent(1, None), agent(2, Some(7))]);
    let grouped = group_agents_by_phase(&unphased);
    assert_eq!(grouped.len(), 1);
    assert_eq!(grouped[0].0, None);
    assert_eq!(grouped[0].1.len(), 2);

    let orphan = run(
        vec![WorkflowPhase {
            index: 1,
            title: "Find".into(),
        }],
        vec![agent(1, Some(1)), agent(2, Some(9))],
    );
    let grouped = group_agents_by_phase(&orphan);
    assert_eq!(grouped.len(), 2);
    assert_eq!(grouped[0].0, Some("Find"));
    assert_eq!(grouped[1].0, None);
    assert_eq!(grouped[1].1[0].index, 2);
}

#[test]
fn run_totals_omit_what_the_provider_did_not_report() {
    let mut subject = run(Vec::new(), vec![agent(1, None), agent(2, None)]);
    let totals = run_totals(&subject);
    assert!(totals.contains('2'), "{totals}");
    assert!(!totals.contains("tokens"), "{totals}");

    subject.total_tokens = Some(31_154);
    subject.total_tool_calls = Some(4);
    let totals = run_totals(&subject);
    assert!(totals.contains("31154"), "{totals}");
    assert!(totals.contains('4'), "{totals}");
}

#[test]
fn every_state_has_its_own_label() {
    let mut subject = run(Vec::new(), Vec::new());
    let mut seen: Vec<String> = Vec::new();
    for state in [
        WorkflowRunState::Starting,
        WorkflowRunState::Running,
        WorkflowRunState::Done,
        WorkflowRunState::Failed,
        WorkflowRunState::Stopped,
    ] {
        subject.state = state;
        seen.push(run_state_label(&subject));
    }
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), 5, "run states must be distinguishable");

    let mut seen: Vec<String> = [
        WorkflowAgentState::Queued,
        WorkflowAgentState::Running,
        WorkflowAgentState::Done,
        WorkflowAgentState::Failed,
    ]
    .into_iter()
    .map(agent_state_label)
    .collect();
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), 4, "agent states must be distinguishable");
}
