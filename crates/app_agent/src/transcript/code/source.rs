use std::mem;
use std::ops::Range;

use nmt_agent_utils::chat::Item;

use crate::transcript::{detect_output_language, file_extension_lang};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CodeSource {
    pub(super) command: Option<String>,
    pub(super) output: String,
    pub(super) language: Option<String>,
    pub(super) strip_gutter: bool,
}

impl CodeSource {
    pub(super) fn from_item(item: &Item) -> Option<Self> {
        let (command, output, language, strip_gutter) = match item {
            Item::CommandExecution {
                command,
                aggregated_output,
                ..
            } => (
                Some(command.clone()),
                aggregated_output.clone().unwrap_or_default(),
                read_command_language(command),
                false,
            ),
            Item::FileChange { diff, .. } => (
                None,
                diff.clone().unwrap_or_default(),
                Some("diff".into()),
                false,
            ),
            Item::Other {
                kind,
                title,
                output,
                ..
            } => match kind.as_str() {
                "TodoWrite" | "ExitPlanMode" | "Task" => return None,
                "Read" => (
                    None,
                    output.clone().unwrap_or_default(),
                    Some(file_extension_lang(title)),
                    true,
                ),
                _ => (None, output.clone().unwrap_or_default(), None, false),
            },
            _ => return None,
        };
        Some(Self {
            command,
            output,
            language,
            strip_gutter,
        })
    }

    pub(super) fn output_language<'a>(&'a self, clean_output: &'a str) -> &'a str {
        self.language
            .as_deref()
            .unwrap_or_else(|| detect_output_language(clean_output))
    }
}

/// Prefer an explicitly named interpreter. Bare command text has no shell
/// metadata, so only distinctive PowerShell syntax overrides the Bash default.
pub(super) fn command_syntax(command: &str) -> (&'static str, Range<usize>) {
    let trimmed = command.trim_start();
    let first = trimmed
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_matches(['\'', '"']);
    let executable = first
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(first)
        .to_ascii_lowercase();
    let explicit = match executable.trim_end_matches(".exe") {
        "powershell" | "pwsh" => Some("powershell"),
        "bash" | "sh" | "zsh" => Some("bash"),
        _ => None,
    };
    if let Some(language) = explicit {
        // The script after an interpreter's command flag is executable source;
        // parsing its surrounding argument quotes would color it as one string.
        for flag in [" -Command ", " -command ", " -c ", " -lc "] {
            if let Some(start) = command.find(flag) {
                let mut start = start + flag.len();
                while command
                    .as_bytes()
                    .get(start)
                    .is_some_and(u8::is_ascii_whitespace)
                {
                    start += 1;
                }
                let mut end = command.trim_end().len();
                if end > start + 1
                    && matches!(command.as_bytes()[start], b'\'' | b'"')
                    && command.as_bytes()[start] == command.as_bytes()[end - 1]
                {
                    start += 1;
                    end -= 1;
                }
                return (language, start..end);
            }
        }
        return (language, 0..command.len());
    }

    let powershell = command.contains("$env:")
        || command.split(['\n', ';', '|']).any(|part| {
            let word = part.split_whitespace().next().unwrap_or("");
            let Some((verb, noun)) = word.split_once('-') else {
                return false;
            };
            !noun.is_empty()
                && matches!(
                    verb.to_ascii_lowercase().as_str(),
                    "get"
                        | "set"
                        | "write"
                        | "select"
                        | "where"
                        | "foreach"
                        | "new"
                        | "remove"
                        | "test"
                        | "join"
                        | "split"
                        | "convertto"
                        | "convertfrom"
                        | "out"
                        | "import"
                        | "export"
                        | "invoke"
                        | "start"
                        | "stop"
                )
        });
    (
        if powershell { "powershell" } else { "bash" },
        0..command.len(),
    )
}

/// Recognize a single literal file read. Pipelines, expansion, multiple paths,
/// and content-changing options do not provide a reliable output language.
fn read_command_language(command: &str) -> Option<String> {
    let (_, script) = command_syntax(command);
    let words = literal_words(&command[script])?;
    let (program, arguments) = words.split_first()?;
    let mut args = arguments.iter().map(String::as_str);
    let path = match program.to_ascii_lowercase().as_str() {
        "cat" => {
            let first = args.next()?;
            if first == "--" {
                args.next()?
            } else if first.starts_with('-') {
                return None;
            } else {
                first
            }
        }
        "get-content" => {
            let mut path = None;
            while let Some(arg) = args.next() {
                match arg.to_ascii_lowercase().as_str() {
                    "-raw" => {}
                    "-literalpath" | "-path" if path.is_none() => path = args.next(),
                    _ if !arg.starts_with('-') && path.is_none() => path = Some(arg),
                    _ => return None,
                }
            }
            path?
        }
        _ => return None,
    };
    if args.next().is_some() {
        return None;
    }
    let language = file_extension_lang(path);
    (!language.is_empty()).then_some(language)
}

fn literal_words(text: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    for ch in text.chars() {
        if matches!(ch, '$' | '`' | '\n' | '\r') {
            return None;
        }
        match quote {
            Some(delimiter) if ch == delimiter => quote = None,
            Some(_) => word.push(ch),
            None => match ch {
                '\'' | '"' => quote = Some(ch),
                '|' | '&' | ';' | '<' | '>' | '(' | ')' | '*' | '?' | '[' => return None,
                ch if ch.is_whitespace() => {
                    if !word.is_empty() {
                        words.push(mem::take(&mut word));
                    }
                }
                _ => word.push(ch),
            },
        }
    }
    if quote.is_some() {
        return None;
    }
    if !word.is_empty() {
        words.push(word);
    }
    Some(words)
}
