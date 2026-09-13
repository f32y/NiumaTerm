use std::collections::HashSet;

use crate::chat::{SkillCatalog, SkillInfo, SkillReference, SlashCommandInfo};
use crate::session::AgentKind;

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

/// Parse only an input whose first byte is `/`. A slash later in ordinary
/// prose is deliberately invisible to command routing.
pub fn parse_slash_command(input: &str) -> Option<ParsedSlashCommand> {
    let tail = input.strip_prefix('/')?;
    let token_end = tail.find(char::is_whitespace).unwrap_or(tail.len());
    let remainder = &tail[token_end..];

    Some(ParsedSlashCommand {
        name: tail[..token_end].to_ascii_lowercase(),
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
/// commands authoritative over adapter commands, and adapter commands over
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

/// One ranked palette hit, borrowed from the catalog it was ranked out of.
/// Ranking runs again on every keystroke and again on every frame the palette
/// paints, so the rows carry references and only the entry the user acts on is
/// ever copied.
fn input_has_bound_skill_token(input: &str, binding: &SkillReference) -> bool {
    input
        .split_whitespace()
        .next()
        .is_some_and(|token| token == format!("${}", binding.name))
}

/// Editing task text after `$name` is safe; changing the first token turns
/// the composer back into ordinary unbound text.
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
