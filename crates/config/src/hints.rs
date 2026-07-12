use serde::{Deserialize, Serialize};

/// Default alphabet for hint labels
pub const DEFAULT_HINTS_ALPHABET: &str = "jfkdls;ahgurieowpq";

/// Default URL/path regex pattern.
///
/// Ported verbatim from ghostty's `src/config/url.zig`. Requires a regex
/// engine with lookbehind support — rio uses oniguruma via the `onig`
/// crate. Three alternations:
///
/// 1. **Schemed URLs** — `http://`, `https://`, `mailto:`, `file:`, `ssh:`,
///    `magnet:`, `ipfs://`, `gemini://`, etc. IPv6 literals supported.
///    Trailing `.` / `,` and unbalanced parens are excluded via lookbehind.
/// 2. **Rooted or explicitly-relative paths** — `/abs`, `./rel`, `../rel`,
///    `~/x`, `.hidden/x`, `$VAR/x`. Each prefix is guarded by lookbehinds
///    so the `~/` inside `foo~/bar` and the `/` inside `foo/bar` aren't
///    mis-matched. Paths with internal spaces are supported when they
///    contain a dotted filename segment.
/// 3. **Bare relative paths** — `word/.../name.ext`. A dotted segment is
///    required, and lookbehinds prevent matching mid-word starts.
pub const DEFAULT_URL_REGEX: &str = concat!(
    // schemed URLs
    "(?:https?://|mailto:|ftp://|file:|ssh:|git://|ssh://|tel:|magnet:|ipfs://|ipns://|gemini://|gopher://|news:)",
    "(?:",
    r"(?:\[[:0-9a-fA-F]+(?:[:0-9a-fA-F]*)+\](?::[0-9]+)?)",
    "|",
    r"[\w\-.~:/?#@!$&*+,;=%]+(?:[\(\[]\w*[\)\]])?",
    ")+",
    r"(?<![,.])",
    "|",
    // rooted or explicitly-relative paths
    r"(?:\.\./|\./|(?<!\w)~/|(?:[\w][\w\-.]*/)*(?<!\w)\$[A-Za-z_]\w*/|\.[\w][\w\-.]*/|(?<![\w~/])/(?!/))",
    "(?:",
    // Dotted: file-like, allows internal spaces around dotted segments.
    r"(?=[\w\-.~:/?#@!$&*+;=%]*\.)",
    r"[\w\-.~:/?#@!$&*+;=%]+",
    r"(?:(?<!:) (?!\w+://)(?!\.{0,2}/)(?!~/)[\w\-.~:/?#@!$&*+;=%]*[/.])*",
    r"(?<!:)",
    r"(?: +(?= *$))?",
    "|",
    // Non-dotted: directory-like, broader.
    r"(?![\w\-.~:/?#@!$&*+;=%]*\.)",
    r"[\w\-.~:/?#@!$&*+;=%]+",
    r"(?:(?<!:) (?!\w+://)(?!\.{0,2}/)(?!~/)[\w\-.~:/?#@!$&*+;=%]+)*",
    r"(?<!:)",
    r"(?: +(?= *$))?",
    ")",
    "|",
    // bare relative paths (word/foo.ext)
    r"(?=[\w\-.~:/?#@!$&*+;=%]*\.)",
    r"(?<!\$\d*)(?<!\w)[\w][\w\-.]*/",
    r"[\w\-.~:/?#@!$&*+;=%]+",
    r"(?<!:)",
    r"(?: +(?= *$))?",
);

/// Hints configuration
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hints {
    /// Characters used for hint labels
    #[serde(default = "default_hints_alphabet")]
    pub alphabet: String,

    /// List of hint rules
    #[serde(default = "default_hints_enabled")]
    pub rules: Vec<Hint>,
}

impl Default for Hints {
    fn default() -> Self {
        Self {
            alphabet: default_hints_alphabet(),
            rules: default_hints_enabled(),
        }
    }
}

/// Individual hint configuration
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hint {
    /// Regex pattern to match
    #[serde(default)]
    pub regex: Option<String>,

    /// Whether to include OSC 8 hyperlinks
    #[serde(default = "default_bool_false")]
    pub hyperlinks: bool,

    /// Whether to apply post-processing to matches
    #[serde(default = "default_bool_true", rename = "post-processing")]
    pub post_processing: bool,

    /// Whether hints persist after selection
    #[serde(default = "default_bool_false")]
    pub persist: bool,

    /// Action to perform when hint is activated
    #[serde(flatten)]
    pub action: HintAction,

    /// Mouse configuration for this hint
    #[serde(default)]
    pub mouse: HintMouse,

    /// Keyboard binding to activate hint mode
    #[serde(default)]
    pub binding: Option<HintBinding>,
}

/// Actions that can be performed with hints
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HintAction {
    /// Built-in action
    Action { action: HintInternalAction },
    /// Custom command
    Command { command: HintCommand },
}

/// Built-in hint actions
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum HintInternalAction {
    /// Copy the hint text to clipboard
    Copy,
    /// Paste the hint text
    Paste,
    /// Select the hint text
    Select,
    /// Move vi mode cursor to hint
    MoveViModeCursor,
}

/// Custom command configuration
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum HintCommand {
    /// Simple command string
    Simple(String),
    /// Command with arguments
    WithArgs {
        program: String,
        #[serde(default)]
        args: Vec<String>,
    },
}

/// Mouse configuration for hints
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HintMouse {
    /// Whether mouse highlighting is enabled
    #[serde(default = "default_bool_true")]
    pub enabled: bool,

    /// Required modifiers for mouse highlighting
    #[serde(default)]
    pub mods: Vec<String>,
}

impl Default for HintMouse {
    fn default() -> Self {
        #[cfg(target_os = "macos")]
        let default_mods = vec!["Super".to_string()];

        #[cfg(not(target_os = "macos"))]
        let default_mods = vec!["Alt".to_string()];

        Self {
            enabled: true,
            mods: default_mods,
        }
    }
}

/// Keyboard binding for hint activation
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HintBinding {
    /// Key to press
    pub key: String,

    /// Required modifiers
    #[serde(default)]
    pub mods: Vec<String>,

    /// Terminal mode requirements
    #[serde(default)]
    pub mode: Vec<String>,
}

// Default functions
fn default_hints_alphabet() -> String {
    DEFAULT_HINTS_ALPHABET.to_string()
}

fn default_hints_enabled() -> Vec<Hint> {
    vec![Hint {
        regex: Some(DEFAULT_URL_REGEX.to_string()),
        hyperlinks: true,
        post_processing: true,
        persist: false,
        action: HintAction::Command {
            command: default_url_command(),
        },
        mouse: HintMouse::default(),
        binding: Some(HintBinding {
            key: "O".to_string(),
            mods: vec!["Control".to_string(), "Shift".to_string()],
            mode: Vec::new(),
        }),
    }]
}

fn default_url_command() -> HintCommand {
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    return HintCommand::Simple("xdg-open".to_string());

    #[cfg(target_os = "macos")]
    return HintCommand::Simple("open".to_string());

    #[cfg(target_os = "windows")]
    return HintCommand::WithArgs {
        program: "cmd".to_string(),
        args: vec!["/c".to_string(), "start".to_string(), "".to_string()],
    };
}

fn default_bool_true() -> bool {
    true
}

fn default_bool_false() -> bool {
    false
}
