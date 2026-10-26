//! Slash commands and skills on the phone: which commands its composer
//! offers, how a typed line routes, and how refusals read.
//!
//! Routing is the desktop's own, from `nmt_agent::catalog`, so a line means
//! the same on both. The phone offers only what it can carry out: the
//! harness's commands, which the host runs, and starting a new conversation.

#[cfg(test)]
#[path = "commands_tests.rs"]
mod commands_tests;

use std::path::Path;

use nmt_agent::catalog::{
    ChoiceError, SlashRefusal, SlashRoute, adapter_commands, merge_catalog, route_slash,
};
use nmt_agent::chat::{
    SkillInfo, SkillReference, SlashCommandArguments, SlashCommandInfo, SlashCommandRunPolicy,
    SlashCommandSource,
};
use nmt_agent::session::AgentKind;
use nmt_agent::session::capabilities::AgentCapabilities as _;
use nmt_agent::session::lifecycle::Status;
use nmt_agent::session::view::AgentView;

use crate::records::{SkillRecord, SlashCommandRecord};

/// The commands the phone runs itself instead of the harness.
fn local_commands() -> Vec<SlashCommandInfo> {
    ["new", "clear", "resume"]
        .into_iter()
        .map(|name| SlashCommandInfo {
            name: name.to_owned(),
            description: if name == "resume" {
                "Resume a recent conversation"
            } else {
                "Start a new conversation"
            }
            .to_owned(),
            argument_hint: None,
            source: SlashCommandSource::Local,
            arguments: SlashCommandArguments::None,
            run_policy: SlashCommandRunPolicy::IdleOnly,
        })
        .collect()
}

/// Whether a turn or command still holds the session, which decides
/// whether a command runs now or waits for the turn.
fn busy(view: &AgentView) -> bool {
    view.slots.status.status == Status::Running || view.slots.queue.awaiting_turn
}

/// What the phone does with a route: run it on the host, send it as a
/// message, or rename the conversation. Pickers and panels the desktop opens
/// for the other routes have no counterpart here yet.
fn phone_runs(route: &SlashRoute) -> bool {
    matches!(
        route,
        SlashRoute::NewConversation
            | SlashRoute::Resume
            | SlashRoute::Prompt
            | SlashRoute::Rename(_)
            | SlashRoute::Backend { .. }
    )
}

/// The commands the phone offers for `view`, in the desktop's precedence
/// order. A host too old to name its harness gets none, and its slash lines
/// go out as plain messages, as they did before commands existed here.
pub(crate) fn command_catalog(view: &AgentView) -> Vec<SlashCommandInfo> {
    let Some(kind) = view.slots.catalogs.kind else {
        return Vec::new();
    };

    let merged = merge_catalog(
        local_commands(),
        adapter_commands(kind),
        view.slots.catalogs.commands.clone().unwrap_or_default(),
    );

    // A bare command either runs, or asks for a value or a picker. The phone
    // cannot supply those, so a command it could never finish is left out
    // instead of offered and refused.
    merged
        .iter()
        .filter(|command| {
            route_slash(
                &format!("/{}", command.name),
                &merged,
                kind.caps(),
                view.slots.catalogs.skills.as_ref(),
                |_| Vec::new(),
                false,
            )
            .is_some_and(|route| phone_runs(&route))
        })
        .cloned()
        .collect()
}

/// Route a line the person sent, or `None` for a plain message.
pub(crate) fn route_line(text: &str, view: &AgentView) -> Option<(SlashRoute, bool)> {
    let kind = view.slots.catalogs.kind?;
    let busy = busy(view);

    let route = route_slash(
        text,
        &command_catalog(view),
        kind.caps(),
        view.slots.catalogs.skills.as_ref(),
        |_| Vec::new(),
        busy,
    )?;

    Some((route, busy))
}

/// The skill a message starting with `$name` invokes, where the harness
/// takes skills as a structured reference. `path` is the skill the person
/// picked, which tells apart skills of one name in different scopes; a
/// typed name without a pick takes the first enabled skill of that name.
pub(crate) fn bound_skill(
    text: &str,
    path: Option<&str>,
    view: &AgentView,
) -> Option<SkillReference> {
    let kind = view.slots.catalogs.kind?;

    if !kind.caps().skill_references {
        return None;
    }

    let name = text.split_whitespace().next()?.strip_prefix('$')?;

    view.slots
        .catalogs
        .skills
        .as_ref()?
        .skills
        .iter()
        .find(|skill| {
            skill.enabled && skill.name == name && path.is_none_or(|path| path == skill.path)
        })
        .map(|skill| SkillReference {
            name: skill.name.clone(),
            path: skill.path.clone(),
        })
}

pub(crate) fn command_records(view: &AgentView) -> Vec<SlashCommandRecord> {
    command_catalog(view)
        .into_iter()
        .map(|command| SlashCommandRecord {
            takes_arguments: command.arguments == SlashCommandArguments::Freeform,
            name: command.name,
            description: command.description,
            argument_hint: command.argument_hint,
        })
        .collect()
}

/// The skills the composer offers, each with the text that invokes it. A
/// harness that neither references skills nor expands them from a slash
/// line lists them among its commands instead.
pub(crate) fn skill_records(view: &AgentView) -> Vec<SkillRecord> {
    let Some(kind) = view.slots.catalogs.kind else {
        return Vec::new();
    };

    let Some(prefix) = skill_prefix(kind) else {
        return Vec::new();
    };

    let skills: Vec<&SkillInfo> = view
        .slots
        .catalogs
        .skills
        .iter()
        .flat_map(|catalog| &catalog.skills)
        .collect();

    skills
        .iter()
        .map(|skill| SkillRecord {
            token: format!("{prefix}{}", skill.name),
            title: skill
                .display_name
                .clone()
                .unwrap_or_else(|| skill.name.clone()),
            name: skill.name.clone(),
            description: skill.description.clone(),
            source: match skills
                .iter()
                .filter(|other| other.name == skill.name)
                .count()
            {
                1 => skill.scope.clone(),
                _ => skill_folder(&skill.path).map_or_else(
                    || skill.scope.clone(),
                    |folder| format!("{} · {folder}", skill.scope),
                ),
            },
            path: skill.path.clone(),
            enabled: skill.enabled,
        })
        .collect()
}

/// The folder a skill's `skills` directory sits in, such as `.agents` for
/// `.agents/skills/review/SKILL.md`: what differs between copies of one
/// skill that several agents' folders each carry.
fn skill_folder(path: &str) -> Option<&str> {
    Path::new(path)
        .parent()?
        .parent()?
        .parent()?
        .file_name()?
        .to_str()
}

fn skill_prefix(kind: AgentKind) -> Option<char> {
    let caps = kind.caps();

    match (caps.skill_references, caps.slash_skills_are_prompts) {
        (true, _) => Some('$'),
        (false, true) => Some('/'),
        (false, false) => None,
    }
}

pub(crate) fn refusal_text(refusal: SlashRefusal) -> String {
    match refusal {
        SlashRefusal::ChooseCommand => "Type a command name after /.".to_owned(),
        SlashRefusal::Unknown(name) => format!("/{name} is not a command here."),
        SlashRefusal::SkillsLoading => "The agent is still loading its skills.".to_owned(),
        SlashRefusal::NoSkills => "The agent has no skills.".to_owned(),
        SlashRefusal::SkillDiscovery(error) => error,
        SlashRefusal::ChooseSkill => "Pick a skill from the list.".to_owned(),
        SlashRefusal::NoArguments(name) => format!("/{name} takes no arguments."),
        SlashRefusal::ChooseValue(name) => format!("/{name} needs a value."),
        SlashRefusal::Choice {
            error: ChoiceError::Unknown,
            value,
        } => format!("\u{201c}{value}\u{201d} is not one of the choices."),
        SlashRefusal::Choice {
            error: ChoiceError::Ambiguous,
            value,
        } => format!("\u{201c}{value}\u{201d} matches more than one choice."),
        SlashRefusal::IdleOnly(name) => {
            format!("/{name} waits until the agent finishes its turn.")
        }
    }
}
