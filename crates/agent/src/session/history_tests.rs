use std::time::{Duration, SystemTime};

use nmt_profile::{AgentKind, AgentProfile, EnvVar};

use crate::chat::{SessionOrigin, SessionScope, SessionSummary};
use crate::session::history::{SessionHistory, other_agent_sources};

fn row(id: &str, seconds: u64, origin: Option<(AgentKind, &str)>) -> SessionSummary {
    SessionSummary {
        id: id.into(),
        title: id.into(),
        branch: None,
        cwd: None,
        last_active: SystemTime::UNIX_EPOCH + Duration::from_secs(seconds),
        snippet: None,
        origin: origin.map(|(kind, profile)| SessionOrigin {
            kind,
            profile: profile.into(),
        }),
    }
}

fn ids(history: &SessionHistory) -> Vec<&str> {
    history.sessions.iter().map(|row| row.id.as_str()).collect()
}

fn profile(name: &str, kind: AgentKind) -> AgentProfile {
    AgentProfile {
        name: name.into(),
        kind,
        ..AgentProfile::default()
    }
}

#[test]
fn other_agents_fall_in_among_own_rows_by_recency() {
    let mut history = SessionHistory::default();

    history.append_page(vec![row("own-new", 30, None), row("own-old", 10, None)]);

    let request = history.begin_other_agents();

    assert!(history.publish_other_agents(
        &request,
        vec![row("codex", 20, Some((AgentKind::Codex, "Codex")))]
    ));
    assert!(history.publish_other_agents(
        &request,
        vec![
            row("deepseek", 40, Some((AgentKind::DeepSeek, "DeepSeek"))),
            // The same conversation the own agent listed.
            row("own-old", 10, Some((AgentKind::Codex, "Codex"))),
        ]
    ));

    assert_eq!(ids(&history), ["deepseek", "own-new", "codex", "own-old"]);
    assert_eq!(history.sessions[3].origin, None);

    assert!(history.set_own_agent_only(true));
    assert_eq!(ids(&history), ["own-new", "own-old"]);

    assert!(history.set_own_agent_only(false));
    assert_eq!(history.sessions.len(), 4);
}

#[test]
fn a_replaced_listing_cannot_add_rows() {
    let mut history = SessionHistory::default();

    let old = history.begin_other_agents();
    let new = history.begin_other_agents();

    assert!(!history.publish_other_agents(&old, vec![row("late", 5, None)]));
    assert!(history.publish_other_agents(&new, vec![row("fresh", 5, None)]));

    history.scope = SessionScope::AllDirectories;

    assert!(!history.publish_other_agents(&new, vec![row("wrong-scope", 5, None)]));
    assert_eq!(ids(&history), ["fresh"]);

    history.clear_rows();

    assert!(history.sessions.is_empty());
    assert!(!history.has_other_agents());
}

#[test]
fn own_rows_arriving_later_keep_other_agents_rows() {
    let mut history = SessionHistory::default();

    let request = history.begin_other_agents();

    history.publish_other_agents(
        &request,
        vec![row("claude", 20, Some((AgentKind::Claude, "Claude")))],
    );

    let listing = history.begin_filesystem_history(None);

    history.publish_filesystem_count(&listing, None, 1);
    history.publish_filesystem_rows(&listing, None, vec![row("own", 30, None)]);

    assert_eq!(ids(&history), ["own", "claude"]);

    let listing = history.begin_filesystem_history(None);

    history.publish_filesystem_count(&listing, None, 0);

    assert_eq!(ids(&history), ["claude"]);
}

#[test]
fn a_listing_in_flight_is_reported_until_it_completes() {
    let mut history = SessionHistory::default();

    assert!(!history.list_in_flight());

    let listing = history.begin_filesystem_history(Some("project".into()));

    assert!(history.list_in_flight());

    history.publish_filesystem_count(&listing, Some("project"), 2);

    assert!(history.list_in_flight());

    history.publish_filesystem_rows(&listing, Some("project"), vec![row("own", 30, None)]);

    assert!(!history.list_in_flight());

    let _ = history.begin_other_agents();

    assert!(history.list_in_flight());
}

#[test]
fn a_wait_for_rows_ends_when_its_listing_completes() {
    let mut history = SessionHistory::default();

    history.append_page(vec![row("stale", 30, None)]);

    // `/resume` before any rows arrived: the wait drops what an earlier
    // listing left, so the answer is not mixed with it.
    history.await_rows();

    assert!(history.is_awaiting_rows());
    assert!(history.sessions.is_empty());

    assert!(history.resolve_awaiting_rows());
    assert!(!history.is_awaiting_rows());

    // A second listing completing after the wait ended is not its answer.
    assert!(!history.resolve_awaiting_rows());
}

#[test]
fn a_wait_answers_once() {
    let mut history = SessionHistory::default();

    history.await_rows();

    assert!(history.resolve_awaiting_rows(), "the wait was outstanding");
    assert!(
        !history.resolve_awaiting_rows(),
        "a later listing completing cannot answer a wait that ended"
    );
}

#[test]
fn each_record_store_is_listed_once_and_the_own_one_not_at_all() {
    let mut proxy = profile("Codex proxy", AgentKind::Codex);

    proxy.use_custom_endpoint = true;
    proxy.api_base_url = "https://proxy.example.com/v1".into();

    let mut other_home = profile("Codex work", AgentKind::Codex);

    other_home.env = vec![EnvVar {
        name: "CODEX_HOME".into(),
        value: "/profiles/work".into(),
    }];

    let profiles = [
        profile("Claude", AgentKind::Claude),
        profile("Claude second", AgentKind::Claude),
        profile("Codex", AgentKind::Codex),
        profile("Codex default", AgentKind::Codex),
        proxy,
        other_home,
        profile("DeepSeek", AgentKind::DeepSeek),
    ];

    let origins: Vec<_> = other_agent_sources(&profiles, &profiles[0], "Codex default")
        .iter()
        .map(|source| source.origin().profile.clone())
        .collect();

    // Both Claude profiles read the transcripts the tab lists itself; the two
    // stock Codex profiles share one store, continued by the default.
    assert_eq!(
        origins,
        ["Codex default", "Codex proxy", "Codex work", "DeepSeek"]
    );

    let from_codex: Vec<_> = other_agent_sources(&profiles, &profiles[2], "Claude")
        .iter()
        .map(|source| source.origin().profile.clone())
        .collect();

    assert_eq!(
        from_codex,
        ["Claude", "Codex proxy", "Codex work", "DeepSeek"]
    );
}
