#[cfg(test)]
#[path = "catalog_tests.rs"]
mod catalog_tests;

use std::collections::HashSet;

use crate::chat::{
    SkillCatalog, SkillInfo, SkillReference, SlashCommandArguments, SlashCommandInfo,
    SlashCommandRunPolicy,
};
use crate::session::AgentKind;
use crate::session::capabilities::Capabilities;
use crate::session::commands::PendingSlashCommand;

#[derive(Debug, PartialEq, Eq)]
pub enum SkillError {
    Loading,
    Unavailable(String),
    Disabled(String),
}

#[derive(Debug, PartialEq, Eq)]
pub enum ChoiceError {
    Unknown,
    Ambiguous,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedSlashCommand {
    pub name: String,
    pub arguments: String,
    pub has_argument_separator: bool,
}

/// Parse a leading `/command` token without path separators. Paths remain
/// prompt text, while separators in command arguments are preserved.
pub fn parse_slash_command(input: &str) -> Option<ParsedSlashCommand> {
    let tail = input.strip_prefix('/')?;
    let token_end = tail.find(char::is_whitespace).unwrap_or(tail.len());
    let name = &tail[..token_end];

    if name.contains(['/', '\\']) {
        return None;
    }

    let remainder = &tail[token_end..];

    Some(ParsedSlashCommand {
        name: name.to_ascii_lowercase(),
        arguments: remainder
            .trim_start_matches(char::is_whitespace)
            .to_string(),
        has_argument_separator: !remainder.is_empty(),
    })
}

/// Parse only an input whose first token is `$name`. Once the user types past
/// that token the composer holds a skill invocation with arguments, so the
/// picker stops claiming the input.
pub fn parse_skill_prefix(input: &str) -> Option<String> {
    let tail = input.strip_prefix('$')?;

    (!tail.contains(char::is_whitespace)).then(|| tail.to_ascii_lowercase())
}

fn normalize_command(mut command: SlashCommandInfo) -> Option<SlashCommandInfo> {
    let name = command.name.trim().trim_start_matches('/');

    if name.is_empty() || name.chars().any(char::is_whitespace) {
        return None;
    }

    command.name = name.to_ascii_lowercase();

    Some(command)
}

/// Merge in precedence order. Keeping the first normalized name makes local
/// commands win over adapter commands, and adapter commands over
/// provider discovery, without relying on hash iteration order.
pub fn merge_catalog(
    local: Vec<SlashCommandInfo>,
    adapter: Vec<SlashCommandInfo>,
    provider: Vec<SlashCommandInfo>,
) -> Vec<SlashCommandInfo> {
    let mut seen = HashSet::new();

    local
        .into_iter()
        .chain(adapter)
        .chain(provider)
        .filter_map(normalize_command)
        .filter(|command| seen.insert(command.name.clone()))
        .collect()
}

/// Whether the composer text still opens with the `$name` token the bound
/// skill was inserted as. The token is the only trace of the binding in the
/// text, so its removal is what releases the binding.
fn input_has_bound_skill_token(input: &str, binding: &SkillReference) -> bool {
    input
        .split_whitespace()
        .next()
        .is_some_and(|token| token == format!("${}", binding.name))
}

/// Editing task text after `$name` is safe; changing the first token turns
/// the composer back into plain unbound text.
pub fn reconcile_skill_binding(input: &str, binding: &mut Option<SkillReference>) {
    if binding
        .as_ref()
        .is_some_and(|binding| !input_has_bound_skill_token(input, binding))
    {
        *binding = None;
    }
}

/// Re-check the live replacement snapshot immediately before submission so
/// a file watcher cannot leave the UI holding a removed or disabled path.
pub fn validate_skill_binding(
    input: &str,
    binding: Option<&SkillReference>,
    catalog: Option<&SkillCatalog>,
) -> Result<Option<SkillReference>, SkillError> {
    let Some(binding) = binding else {
        return Ok(None);
    };

    if !input_has_bound_skill_token(input, binding) {
        return Ok(None);
    }

    let Some(catalog) = catalog else {
        return Err(SkillError::Loading);
    };

    let Some(skill) = catalog
        .skills
        .iter()
        .find(|skill| skill.name == binding.name && skill.path == binding.path)
    else {
        return Err(SkillError::Unavailable(binding.name.clone()));
    };

    if !skill.enabled {
        return Err(SkillError::Disabled(binding.name.clone()));
    }

    Ok(Some(binding.clone()))
}

pub fn prepare_skill_selection(skill: &SkillInfo) -> Result<(String, SkillReference), SkillError> {
    if !skill.enabled {
        return Err(SkillError::Disabled(skill.name.clone()));
    }

    Ok((
        format!("${} ", skill.name),
        SkillReference {
            name: skill.name.clone(),
            path: skill.path.clone(),
        },
    ))
}

/// Resolve a typed choice by exact value/display label, then by a unique
/// prefix. Ambiguous or unknown input is rejected instead of silently
/// selecting a different setting.
pub fn resolve_choice(input: &str, choices: &[(String, String)]) -> Result<String, ChoiceError> {
    let query = input.trim().to_ascii_lowercase();

    let exact = choices.iter().find(|(value, label)| {
        value.eq_ignore_ascii_case(&query) || label.eq_ignore_ascii_case(&query)
    });

    if let Some((value, _)) = exact {
        return Ok(value.clone());
    }

    let candidates: Vec<&(String, String)> = choices
        .iter()
        .filter(|(value, label)| {
            value.to_ascii_lowercase().starts_with(&query)
                || label.to_ascii_lowercase().starts_with(&query)
        })
        .collect();

    match candidates.as_slice() {
        [(value, _)] => Ok(value.clone()),
        [] => Err(ChoiceError::Unknown),
        _ => Err(ChoiceError::Ambiguous),
    }
}

pub fn adapter_commands(kind: AgentKind) -> Vec<SlashCommandInfo> {
    use crate::claude_code::stream_json;
    use crate::codex::app_server;
    use crate::dsh;

    match kind {
        AgentKind::Codex => app_server::Session::adapter_commands(),
        AgentKind::Claude => stream_json::Session::adapter_commands(),
        AgentKind::DeepSeek => dsh::Session::adapter_commands(),
    }
}

/// Why a slash line was refused. Each view words the refusal in its own
/// language, so the reason stays structured until it reaches one.
#[derive(Debug, PartialEq, Eq)]
pub enum SlashRefusal {
    /// A bare `/` names no command.
    ChooseCommand,
    Unknown(String),
    /// `/skills` before the harness reported its skills.
    SkillsLoading,
    NoSkills,
    /// Skill discovery failed and found nothing; the harness's first error.
    SkillDiscovery(String),
    /// `/skills` is a picker; its row, not the line, names the skill.
    ChooseSkill,
    NoArguments(String),
    ChooseValue(String),
    Choice {
        error: ChoiceError,
        value: String,
    },
    /// The command replaces the conversation and waits for the running turn.
    IdleOnly(String),
}

/// What a slash line asks for, once it has been checked against the
/// catalog and the harness's capabilities.
#[derive(Debug, PartialEq, Eq)]
pub enum SlashRoute {
    /// Refused; the line stays in the composer for correction.
    Refused(SlashRefusal),
    /// A skill written as a command, which the harness expands when it
    /// arrives as a normal message.
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
    /// A question answered beside the conversation.
    Side(String),
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
pub fn route_slash(
    input: &str,
    catalog: &[SlashCommandInfo],
    caps: &Capabilities,
    skills: Option<&SkillCatalog>,
    choices: impl FnOnce(&str) -> Vec<(String, String)>,
    busy: bool,
) -> Option<SlashRoute> {
    let parsed = parse_slash_command(input)?;

    if parsed.name.is_empty() {
        return Some(SlashRoute::Refused(SlashRefusal::ChooseCommand));
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
            false => SlashRoute::Refused(SlashRefusal::Unknown(parsed.name)),
        });
    };

    // `/skills` owns a picker stage. A selected row rewrites the composer to
    // `$name`; the slash input itself is never a provider command or an
    // normal user turn.
    if command.arguments == SlashCommandArguments::Skills {
        let refusal = match skills {
            None => SlashRefusal::SkillsLoading,
            Some(catalog) if catalog.skills.is_empty() && !catalog.errors.is_empty() => {
                SlashRefusal::SkillDiscovery(catalog.errors[0].clone())
            }
            Some(catalog) if catalog.skills.is_empty() => SlashRefusal::NoSkills,
            Some(_) => SlashRefusal::ChooseSkill,
        };

        return Some(SlashRoute::Refused(refusal));
    }

    if command.arguments == SlashCommandArguments::None && !parsed.arguments.trim().is_empty() {
        return Some(SlashRoute::Refused(SlashRefusal::NoArguments(
            command.name.clone(),
        )));
    }

    if command.arguments == SlashCommandArguments::Choices {
        if parsed.arguments.trim().is_empty() {
            return Some(SlashRoute::Refused(SlashRefusal::ChooseValue(
                command.name.clone(),
            )));
        }

        match resolve_choice(&parsed.arguments, &choices(&command.name)) {
            Ok(value) if command.name == "model" => return Some(SlashRoute::Model(value)),
            Ok(value) if command.name == "permissions" => {
                return Some(SlashRoute::Permissions(value));
            }
            Ok(_) => {}
            Err(error) => {
                return Some(SlashRoute::Refused(SlashRefusal::Choice {
                    error,
                    value: parsed.arguments,
                }));
            }
        }
    }

    Some(match command.name.as_str() {
        "new" | "clear" if busy => {
            SlashRoute::Refused(SlashRefusal::IdleOnly(command.name.clone()))
        }
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
        "side" if caps.side_questions || caps.side_threads => SlashRoute::Side(parsed.arguments),
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
