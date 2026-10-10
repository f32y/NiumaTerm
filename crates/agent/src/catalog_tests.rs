use crate::catalog::{
    ChoiceError, ParsedSlashCommand, SlashRefusal, SlashRoute, parse_slash_command, route_slash,
};
use crate::chat::{
    SkillCatalog, SkillInfo, SlashCommandArguments, SlashCommandInfo, SlashCommandRunPolicy,
    SlashCommandSource,
};
use crate::session::AgentKind;
use crate::session::capabilities::AgentCapabilities as _;
use crate::session::commands::PendingSlashCommand;

fn command(name: &str, arguments: SlashCommandArguments) -> SlashCommandInfo {
    SlashCommandInfo {
        name: name.to_owned(),
        description: name.to_owned(),
        argument_hint: None,
        source: SlashCommandSource::Local,
        arguments,
        run_policy: SlashCommandRunPolicy::QueueUntilIdle,
    }
}

fn catalog() -> Vec<SlashCommandInfo> {
    vec![
        command("new", SlashCommandArguments::None),
        command("clear", SlashCommandArguments::None),
        command("model", SlashCommandArguments::Choices),
        command("skills", SlashCommandArguments::Skills),
        command("compact", SlashCommandArguments::Freeform),
    ]
}

fn skills(names: &[&str]) -> SkillCatalog {
    SkillCatalog {
        skills: names
            .iter()
            .map(|name| SkillInfo {
                name: (*name).to_owned(),
                display_name: None,
                description: String::new(),
                path: format!("/skills/{name}"),
                scope: "user".to_owned(),
                enabled: true,
            })
            .collect(),
        errors: Vec::new(),
    }
}

fn route(input: &str, kind: AgentKind, skills: Option<&SkillCatalog>, busy: bool) -> SlashRoute {
    let choices = |_: &str| vec![("gpt-5".to_owned(), "GPT-5".to_owned())];

    route_slash(input, &catalog(), kind.caps(), skills, choices, busy).unwrap()
}

#[test]
fn only_a_leading_slash_is_routed() {
    let caps = AgentKind::Codex.caps();

    assert!(route_slash("say /new", &catalog(), caps, None, |_| Vec::new(), false).is_none());
    assert_eq!(
        route("/", AgentKind::Codex, None, false),
        SlashRoute::Refused(SlashRefusal::ChooseCommand)
    );
}

#[test]
fn absolute_paths_are_prompt_text_for_every_agent_kind() {
    for input in [
        "/xxx/xxx",
        "/Users/test/My Documents/report.md",
        "/review/report.md",
        "//server/share/report.md",
        "/tmp/",
        r"/Users\test\report.md",
    ] {
        assert_eq!(parse_slash_command(input), None, "{input}");

        for kind in [AgentKind::Codex, AgentKind::Claude, AgentKind::DeepSeek] {
            assert_eq!(
                route_slash(input, &catalog(), kind.caps(), None, |_| Vec::new(), false),
                None,
                "{kind:?}: {input}"
            );
        }
    }
}

#[test]
fn namespaced_commands_preserve_paths_in_arguments() {
    assert_eq!(
        parse_slash_command("/plugin:review /Users/test/My Documents/report.md"),
        Some(ParsedSlashCommand {
            name: "plugin:review".into(),
            arguments: "/Users/test/My Documents/report.md".into(),
            has_argument_separator: true,
        })
    );

    assert_eq!(
        route("/compact /xxx/xxx", AgentKind::Codex, None, false),
        SlashRoute::Backend {
            command: PendingSlashCommand {
                name: "compact".into(),
                arguments: "/xxx/xxx".into(),
            },
            policy: SlashCommandRunPolicy::QueueUntilIdle,
        }
    );
}

#[test]
fn a_new_conversation_waits_for_the_running_turn() {
    assert_eq!(
        route("/clear", AgentKind::Claude, None, false),
        SlashRoute::NewConversation
    );
    assert_eq!(
        route("/new", AgentKind::Claude, None, true),
        SlashRoute::Refused(SlashRefusal::IdleOnly("new".into()))
    );
    assert_eq!(
        route("/new now", AgentKind::Claude, None, false),
        SlashRoute::Refused(SlashRefusal::NoArguments("new".into()))
    );
}

#[test]
fn a_harness_command_keeps_its_arguments_and_run_policy() {
    assert_eq!(
        route("/compact keep the plan", AgentKind::Codex, None, true),
        SlashRoute::Backend {
            command: PendingSlashCommand {
                name: "compact".into(),
                arguments: "keep the plan".into(),
            },
            policy: SlashCommandRunPolicy::QueueUntilIdle,
        }
    );
}

#[test]
fn a_choice_command_resolves_its_value_or_says_why_not() {
    assert_eq!(
        route("/model gpt", AgentKind::Codex, None, false),
        SlashRoute::Model("gpt-5".into())
    );
    assert_eq!(
        route("/model claude", AgentKind::Codex, None, false),
        SlashRoute::Refused(SlashRefusal::Choice {
            error: ChoiceError::Unknown,
            value: "claude".into(),
        })
    );
}

#[test]
fn a_skill_named_as_a_command_is_a_prompt_only_where_the_harness_expands_it() {
    let catalog = skills(&["review"]);

    assert_eq!(
        route("/review", AgentKind::DeepSeek, Some(&catalog), false),
        SlashRoute::Prompt
    );
    assert_eq!(
        route("/review", AgentKind::Codex, Some(&catalog), false),
        SlashRoute::Refused(SlashRefusal::Unknown("review".into()))
    );
    assert_eq!(
        route("/skills", AgentKind::Codex, None, false),
        SlashRoute::Refused(SlashRefusal::SkillsLoading)
    );
}
