//! Pure slash-command parsing and catalog logic for the agent composer.

pub(super) use nmt_agent::catalog::{
    merge_catalog, parse_skill_prefix, parse_slash_command, reconcile_skill_binding,
};

#[cfg(test)]
#[path = "commands_tests.rs"]
mod commands_tests;

use std::borrow::Cow;

use nmt_agent::catalog::{
    ChoiceError, SkillError, prepare_skill_selection as prepare_core_skill_selection,
    resolve_choice as resolve_core_choice, validate_skill_binding as validate_core_skill_binding,
};
use nmt_agent::chat::{
    SkillCatalog, SkillInfo, SkillReference, SlashCommandArguments, SlashCommandInfo,
    SlashCommandRunPolicy, SlashCommandSource, ThreadSettings,
};
use nmt_agent::session::capabilities::Capabilities;
use nmt_agent::session::commands::PendingSlashCommand;
use nmt_agent::session::lifecycle::Status;
use rust_i18n::t;

use crate::agent_tab::profile::AgentKind;

pub(super) fn validate_skill_binding(
    input: &str,
    binding: Option<&SkillReference>,
    catalog: Option<&SkillCatalog>,
) -> Result<Option<SkillReference>, String> {
    validate_core_skill_binding(input, binding, catalog).map_err(|error| match error {
        SkillError::Loading => t!("agent-command-skill-loading").into_owned(),
        SkillError::Unavailable(name) => {
            t!("agent-command-skill-unavailable", name = &name).into_owned()
        }
        SkillError::Disabled(name) => t!("agent-command-skill-disabled", name = &name).into_owned(),
    })
}

pub(super) fn prepare_skill_selection(
    skill: &SkillInfo,
) -> Result<(String, SkillReference), String> {
    prepare_core_skill_selection(skill)
        .map_err(|_| t!("agent-command-skill-disabled-by-codex", name = &skill.name).into_owned())
}

pub(super) fn resolve_choice(input: &str, choices: &[(String, String)]) -> Result<String, String> {
    resolve_core_choice(input, choices).map_err(|error| {
        t!(
            match error {
                ChoiceError::Unknown => "agent-command-value-unknown",
                ChoiceError::Ambiguous => "agent-command-value-ambiguous",
            },
            value = input
        )
        .into_owned()
    })
}

pub(super) fn local_commands() -> Vec<SlashCommandInfo> {
    vec![
        command(
            "new",
            t!("agent-command-new-description"),
            None,
            SlashCommandArguments::None,
            SlashCommandRunPolicy::IdleOnly,
        ),
        command(
            "clear",
            t!("agent-command-clear-description"),
            None,
            SlashCommandArguments::None,
            SlashCommandRunPolicy::IdleOnly,
        ),
        command(
            "resume",
            t!("agent-command-resume-description"),
            None,
            SlashCommandArguments::None,
            SlashCommandRunPolicy::IdleOnly,
        ),
        command(
            "model",
            t!("agent-command-model-description"),
            Some(t!("agent-command-model-hint")),
            SlashCommandArguments::Choices,
            SlashCommandRunPolicy::Immediate,
        ),
        command(
            "permissions",
            t!("agent-command-permissions-description"),
            Some(t!("agent-command-permissions-hint")),
            SlashCommandArguments::Choices,
            SlashCommandRunPolicy::Immediate,
        ),
        command(
            "status",
            t!("agent-command-status-description"),
            None,
            SlashCommandArguments::None,
            SlashCommandRunPolicy::Immediate,
        ),
    ]
}

pub(super) fn setting_value_label(value: &str) -> String {
    let key = match value {
        "default" => "agent-setting-value-default",
        "auto" => "agent-setting-value-auto",
        "acceptEdits" => "agent-setting-value-accept-edits",
        "plan" => "agent-setting-value-plan",
        "bypassPermissions" => "agent-setting-value-bypass-permissions",
        "untrusted" => "agent-setting-value-untrusted",
        "on-request" => "agent-setting-value-on-request",
        "never" => "agent-setting-value-never",
        "user" => "agent-setting-value-user",
        "auto_review" => "agent-setting-value-auto-review",
        "readOnly" | "read-only" => "agent-setting-value-read-only",
        "workspaceWrite" | "workspace-write" => "agent-setting-value-workspace-write",
        "dangerFullAccess" | "full-access" => "agent-setting-value-full-access",
        "none" => "agent-setting-value-none",
        "minimal" => "agent-setting-value-minimal",
        "low" => "agent-setting-value-low",
        "medium" => "agent-setting-value-medium",
        "high" => "agent-setting-value-high",
        "xhigh" => "agent-setting-value-xhigh",
        "max" => "agent-setting-value-max",
        "ultra" => "agent-setting-value-ultra",
        "ultracode" => "agent-setting-value-ultracode",
        "normal" => "agent-setting-value-normal",
        _ => return value.to_string(),
    };

    t!(key).to_string()
}

fn command(
    name: &str,
    description: Cow<'static, str>,
    argument_hint: Option<Cow<'static, str>>,
    arguments: SlashCommandArguments,
    run_policy: SlashCommandRunPolicy,
) -> SlashCommandInfo {
    SlashCommandInfo {
        name: name.to_string(),
        description: description.into_owned(),
        argument_hint: argument_hint.map(Cow::into_owned),
        source: SlashCommandSource::Local,
        arguments,
        run_policy,
    }
}

/// Normalize a provider/adapter name. Whitespace would make the advertised
/// command impossible to address as one slash token, so such names are
/// discarded rather than shown as entries that can never execute.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PaletteCatalogEntry<'a> {
    Command(&'a SlashCommandInfo),
    Skill(&'a SkillInfo),
}

/// Case-insensitive `starts_with` over ASCII, without lowercasing either side
/// into a fresh `String`. UTF-8 is self-synchronizing, so comparing raw bytes
/// can only match on a character boundary.
fn starts_with_ignore_ascii_case(haystack: &str, needle: &str) -> bool {
    haystack
        .as_bytes()
        .get(..needle.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(needle.as_bytes()))
}

/// Case-insensitive `contains` over ASCII, on the same terms as
/// [`starts_with_ignore_ascii_case`]. An empty needle is contained by
/// everything, which also keeps `windows` away from a zero width.
fn contains_ignore_ascii_case(haystack: &str, needle: &str) -> bool {
    needle.is_empty()
        || haystack
            .as_bytes()
            .windows(needle.len())
            .any(|window| window.eq_ignore_ascii_case(needle.as_bytes()))
}

fn text_match_rank<'a>(fields: impl IntoIterator<Item = &'a str>, query: &str) -> Option<usize> {
    // Each field is read three times at most and the ranks are ordered, so the
    // fields are collected once rather than re-walking the iterator per rank.
    let fields = fields.into_iter().collect::<Vec<_>>();

    if fields.iter().any(|field| field.eq_ignore_ascii_case(query)) {
        Some(0)
    } else if fields
        .iter()
        .any(|field| starts_with_ignore_ascii_case(field, query))
    {
        Some(1)
    } else if fields
        .iter()
        .any(|field| contains_ignore_ascii_case(field, query))
    {
        Some(2)
    } else {
        None
    }
}

fn command_match_rank(command: &SlashCommandInfo, query: &str) -> Option<usize> {
    text_match_rank([command.name.as_str()], query)
}

fn skill_match_rank(skill: &SkillInfo, query: &str) -> Option<usize> {
    text_match_rank(
        [
            Some(skill.name.as_str()),
            skill.display_name.as_deref(),
            Some(skill.description.as_str()),
        ]
        .into_iter()
        .flatten(),
        query,
    )
}

/// Merge commands and skills into one ranked result without erasing their
/// different activation semantics. Within each rank, command catalog order
/// is stable and is followed by provider skill order.
pub(super) fn filter_palette_catalog<'a>(
    commands: &'a [SlashCommandInfo],
    skills: &'a [SkillInfo],
    query: &str,
) -> Vec<PaletteCatalogEntry<'a>> {
    let mut buckets = [Vec::new(), Vec::new(), Vec::new()];

    for command in commands {
        if let Some(rank) = command_match_rank(command, query) {
            buckets[rank].push(PaletteCatalogEntry::Command(command));
        }
    }

    for skill in skills {
        if let Some(rank) = skill_match_rank(skill, query) {
            buckets[rank].push(PaletteCatalogEntry::Skill(skill));
        }
    }

    buckets.into_iter().flatten().collect()
}

/// Skill results use the command palette's exact/prefix/substring ranking,
/// but identity stays attached to each full row so duplicate names from
/// different scopes are never collapsed.
pub(super) fn filter_skill_catalog(catalog: &[SkillInfo], query: &str) -> Vec<SkillInfo> {
    let mut buckets = [Vec::new(), Vec::new(), Vec::new()];

    for skill in catalog {
        if let Some(rank) = skill_match_rank(skill, query) {
            buckets[rank].push(skill.clone());
        }
    }

    buckets.into_iter().flatten().collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PaletteDirection {
    Previous,
    Next,
}

pub(super) fn move_palette_selection(
    current: usize,
    row_count: usize,
    direction: PaletteDirection,
) -> Option<usize> {
    if row_count == 0 {
        return None;
    }

    Some(match direction {
        PaletteDirection::Previous if current == 0 => row_count - 1,
        PaletteDirection::Previous => current.min(row_count - 1) - 1,
        PaletteDirection::Next => (current + 1) % row_count,
    })
}

/// What a slash line typed into the composer asks for, once it has been
/// checked against the catalog and the harness's capabilities.
pub(super) enum SlashRoute {
    /// Refused with a message; the line stays in the composer for correction.
    Refused(String),
    /// A skill written as a command, which the harness expands when it
    /// arrives as an ordinary message.
    Prompt,
    Model(String),
    Permissions(String),
    NewConversation,
    Resume,
    Status,
    Rewind,
    Rename(String),
    Fork,
    Find(String),
    /// A setting command whose value applies nowhere this side.
    Unapplied,
    /// A command the harness itself runs.
    Backend {
        command: PendingSlashCommand,
        policy: SlashCommandRunPolicy,
    },
}

/// Route slash line `input`, or `None` when it is not one. `catalog` is the
/// merged command list, `skills` the harness's skill catalog, `choices` the
/// values a choice command accepts, and `busy` whether a turn or command
/// still holds the session.
pub(super) fn route_slash(
    input: &str,
    catalog: &[SlashCommandInfo],
    caps: &Capabilities,
    skills: Option<&SkillCatalog>,
    choices: impl FnOnce(&str) -> Vec<(String, String)>,
    busy: bool,
) -> Option<SlashRoute> {
    let parsed = parse_slash_command(input)?;

    if parsed.name.is_empty() {
        return Some(SlashRoute::Refused(
            t!("agent-composer-choose-command").to_string(),
        ));
    }

    let Some(command) = catalog.iter().find(|command| command.name == parsed.name) else {
        // Where a skill is invoked by writing its name into the prompt, a
        // slash line naming one is a message the harness expands, so refusing
        // it as an unknown command would block the only way to reach a skill
        // at all.
        let names_a_skill = skills
            .is_some_and(|catalog| catalog.skills.iter().any(|skill| skill.name == parsed.name));

        return Some(match caps.slash_skills_are_prompts && names_a_skill {
            true => SlashRoute::Prompt,
            false => SlashRoute::Refused(
                t!("agent-composer-unknown-command", name = &parsed.name).into_owned(),
            ),
        });
    };

    // `/skills` owns a picker stage. A selected row rewrites the composer to
    // `$name`; the slash input itself is never a provider command or an
    // ordinary user turn.
    if command.arguments == SlashCommandArguments::Skills {
        let message = match skills {
            None => t!("agent-composer-skill-discovery-loading-period").to_string(),
            Some(catalog) if catalog.skills.is_empty() && !catalog.errors.is_empty() => {
                catalog.errors[0].clone()
            }
            Some(catalog) if catalog.skills.is_empty() => {
                t!("agent-composer-no-skills-period").to_string()
            }
            Some(_) => t!("agent-composer-choose-skill").to_string(),
        };

        return Some(SlashRoute::Refused(message));
    }

    if command.arguments == SlashCommandArguments::None && !parsed.arguments.trim().is_empty() {
        return Some(SlashRoute::Refused(
            t!("agent-composer-command-no-arguments", name = &command.name).into_owned(),
        ));
    }

    if command.arguments == SlashCommandArguments::Choices {
        if parsed.arguments.trim().is_empty() {
            return Some(SlashRoute::Refused(
                t!("agent-composer-choose-value", name = &command.name).into_owned(),
            ));
        }

        match resolve_choice(&parsed.arguments, &choices(&command.name)) {
            Ok(value) if command.name == "model" => return Some(SlashRoute::Model(value)),
            Ok(value) if command.name == "permissions" => {
                return Some(SlashRoute::Permissions(value));
            }
            Ok(_) => {}
            Err(message) => return Some(SlashRoute::Refused(message)),
        }
    }

    Some(match command.name.as_str() {
        "new" | "clear" if busy => SlashRoute::Refused(
            t!("agent-composer-command-idle-only", name = &command.name).into_owned(),
        ),
        "new" | "clear" => SlashRoute::NewConversation,
        "resume" => SlashRoute::Resume,
        "status" => SlashRoute::Status,
        "rewind" if caps.file_rewind => SlashRoute::Rewind,
        "rename" if caps.session_rename => SlashRoute::Rename(parsed.arguments),
        "fork" if caps.session_fork => SlashRoute::Fork,
        // Where the conversation is a file this side rewrites, the rewind
        // picker cuts the same branch and offers restoring the files that turn
        // touched alongside it. Opening a second picker for the smaller half
        // of what one command already does would only hide the choice behind
        // the name it was reached by.
        "fork" if caps.file_rewind => SlashRoute::Rewind,
        "find" if caps.session_search => SlashRoute::Find(parsed.arguments),
        "model" | "permissions" => SlashRoute::Unapplied,
        _ => SlashRoute::Backend {
            command: PendingSlashCommand {
                name: command.name.clone(),
                arguments: parsed.arguments,
            },
            policy: command.run_policy,
        },
    })
}

/// The `/status` answer: the harness, its state, the settings the next
/// message goes out with, and how many commands wait behind the running turn.
pub(super) fn status_summary(
    kind: AgentKind,
    status: Status,
    settings: &ThreadSettings,
    queued: usize,
) -> String {
    let status = match status {
        Status::Starting => t!("agent-composer-status-starting"),
        Status::Idle => t!("agent-composer-status-idle"),
        Status::Running => t!("agent-composer-status-running"),
        Status::Exited => t!("agent-composer-status-exited"),
    };

    let mut fields = vec![
        t!(
            "agent-composer-status-field",
            name = t!("agent-composer-status-backend"),
            value = kind.display()
        )
        .into_owned(),
        t!(
            "agent-composer-status-field",
            name = t!("agent-composer-status-label"),
            value = status
        )
        .into_owned(),
    ];

    for (name, value) in [
        (t!("agent-setting-model"), settings.model.as_deref()),
        (
            t!("agent-setting-permissions"),
            settings.approval.as_deref(),
        ),
        (t!("agent-setting-sandbox"), settings.sandbox.as_deref()),
        (t!("agent-setting-effort"), settings.effort.as_deref()),
        (t!("agent-setting-tier"), settings.tier.as_deref()),
    ] {
        if let Some(value) = value {
            fields.push(t!("agent-composer-status-field", name = name, value = value).into_owned());
        }
    }

    if queued > 0 {
        fields.push(
            t!(
                "agent-composer-status-field",
                name = t!("agent-composer-status-queued"),
                value = queued
            )
            .into_owned(),
        );
    }

    fields.join(" · ")
}
