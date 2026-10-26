use nmt_agent::catalog::{SlashRefusal, SlashRoute};
use nmt_agent::chat::{SkillCatalog, SkillInfo, SkillReference};
use nmt_agent::session::AgentKind;
use nmt_agent::session::controller::SessionController;
use nmt_agent::session::lifecycle::Status;
use nmt_agent::session::view::{AgentView, ViewPublisher};

use crate::commands::{bound_skill, command_catalog, route_line, skill_records};

fn view(kind: AgentKind) -> AgentView {
    let mut view = ViewPublisher::default().snapshot(&SessionController::new(kind));

    view.slots.catalogs.skills = Some(SkillCatalog {
        skills: vec![SkillInfo {
            name: "review".into(),
            description: "Review the change".into(),
            path: "/skills/review/SKILL.md".into(),
            scope: "user".into(),
            enabled: true,
            display_name: None,
        }],
        errors: Vec::new(),
    });

    view
}

fn names(view: &AgentView) -> Vec<String> {
    command_catalog(view)
        .into_iter()
        .map(|command| command.name)
        .collect()
}

#[test]
fn the_phone_offers_only_commands_it_can_carry_out() {
    // `/skills` opens a picker, and fork and side chats open panels the
    // phone does not have.
    assert_eq!(
        names(&view(AgentKind::Codex)),
        ["new", "clear", "resume", "compact", "review", "goal"]
    );
}

#[test]
fn resume_opens_the_picker_for_each_agent_without_sending_a_prompt() {
    for kind in [AgentKind::Codex, AgentKind::DeepSeek, AgentKind::Claude] {
        let mut view = view(kind);

        assert_eq!(
            route_line("/resume", &view),
            Some((SlashRoute::Resume, false))
        );

        view.slots.status.status = Status::Running;

        assert_eq!(
            route_line("/resume", &view),
            Some((SlashRoute::Resume, true))
        );
        assert!(matches!(
            route_line("/resume unexpected", &view),
            Some((SlashRoute::Refused(SlashRefusal::NoArguments(_)), true))
        ));
    }
}

#[test]
fn a_host_that_does_not_name_its_harness_gets_plain_messages() {
    let mut view = view(AgentKind::Codex);

    view.slots.catalogs.kind = None;

    assert!(command_catalog(&view).is_empty());
    assert!(skill_records(&view).is_empty());
    assert!(route_line("/new", &view).is_none());
}

#[test]
fn a_new_conversation_waits_for_the_running_turn() {
    let mut view = view(AgentKind::Claude);

    assert_eq!(
        route_line("/clear", &view),
        Some((SlashRoute::NewConversation, false))
    );

    view.slots.status.status = Status::Running;

    assert_eq!(
        route_line("/new", &view),
        Some((
            SlashRoute::Refused(SlashRefusal::IdleOnly("new".into())),
            true
        ))
    );
}

#[test]
fn skills_are_invoked_the_way_their_harness_takes_them() {
    let codex = view(AgentKind::Codex);
    let deepseek = view(AgentKind::DeepSeek);

    assert_eq!(skill_records(&codex)[0].token, "$review");
    assert_eq!(skill_records(&deepseek)[0].token, "/review");

    assert_eq!(
        bound_skill("$review the parser", None, &codex),
        Some(SkillReference {
            name: "review".into(),
            path: "/skills/review/SKILL.md".into(),
        })
    );
    assert_eq!(bound_skill("$review the parser", None, &deepseek), None);
    assert_eq!(
        route_line("/review", &deepseek),
        Some((SlashRoute::Prompt, false))
    );
}

#[test]
fn a_picked_skill_keeps_its_scope_when_another_shares_its_name() {
    let mut codex = view(AgentKind::Codex);

    let skills = codex.slots.catalogs.skills.as_mut().unwrap();

    let mut workspace = skills.skills[0].clone();

    workspace.path = "/repo/.agents/skills/review/SKILL.md".into();
    workspace.scope = "repo".into();

    skills.skills.push(workspace);

    let sources: Vec<String> = skill_records(&codex)
        .into_iter()
        .map(|skill| skill.source)
        .collect();

    // The first sits in no named folder, so only its scope says where it is.
    assert_eq!(sources, ["user", "repo · .agents"]);

    let picked = bound_skill(
        "$review the parser",
        Some("/repo/.agents/skills/review/SKILL.md"),
        &codex,
    );

    assert_eq!(
        picked.map(|skill| skill.path),
        Some("/repo/.agents/skills/review/SKILL.md".into())
    );

    // Renaming the token away from the picked skill leaves plain text.
    assert_eq!(
        bound_skill(
            "$other the parser",
            Some("/repo/.agents/skills/review/SKILL.md"),
            &codex
        ),
        None
    );
}
