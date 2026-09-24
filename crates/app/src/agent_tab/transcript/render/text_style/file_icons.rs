use gpui::SharedString;
use percent_encoding::percent_decode_str;

use crate::agent_tab::transcript::render::text_style::{has_uri_scheme, strip_source_position};

pub(super) fn file_link_icon(target: &str) -> Option<SharedString> {
    let target = target.trim();
    let decoded;

    let target = if let Some(path) = target.strip_prefix("file://") {
        decoded = percent_decode_str(path).decode_utf8().ok()?;

        decoded.as_ref()
    } else {
        target
    };

    let target = strip_source_position(target);

    if target.is_empty() || target.starts_with('#') || has_uri_scheme(target) {
        return None;
    }

    let name = target.rsplit(['/', '\\']).next()?.to_ascii_lowercase();

    if name.is_empty() || matches!(name.as_str(), "." | "..") {
        return None;
    }

    let icon = match name.as_str() {
        "cargo.toml" | "cargo.lock" => "Config-Rust",
        "package.json" | "package-lock.json" | ".npmrc" => "NPM",
        "dockerfile"
        | "compose.yaml"
        | "compose.yml"
        | "docker-compose.yaml"
        | "docker-compose.yml" => "Docker",
        "cmakelists.txt" => "Cmake",
        ".editorconfig" => "EditorConfig",
        "tsconfig.json" => "Config-TypeScript",
        "pyproject.toml" | "requirements.txt" | "pipfile" => "Config-Python",
        _ => match name.rsplit_once('.').map(|(_, extension)| extension) {
            Some("rs") => "Config-Rust",
            Some("py" | "pyi" | "pyw") => "Config-Python",
            Some("js" | "jsx" | "mjs" | "cjs") => "Config-JS",
            Some("ts" | "tsx" | "mts" | "cts") => "TypeScript",
            Some("c" | "h" | "cc" | "cpp" | "cxx" | "hpp" | "hxx") => "C++",
            Some("cs" | "csx") => "C#",
            Some("go") => "Go",
            Some("php") => "PHP",
            Some("kt" | "kts") => "Kotlin",
            Some("lua") => "Lua",
            Some("zig") => "Zig",
            Some("rb") => "Config-Ruby",
            Some("vue") => "Vue",
            Some("json" | "jsonl" | "ipynb") => "JSON-1",
            Some("json5") => "JSON5",
            Some("yaml" | "yml") => "YAML",
            Some("toml") => "TOML",
            Some("ini" | "cfg" | "conf") => "Config",
            Some("cmake") => "Cmake",
            Some("ps1" | "psm1" | "psd1") => "PowerShell",
            Some("sh" | "bash" | "zsh" | "fish" | "bat" | "cmd") => "Terminal",
            Some("sql" | "sqlite" | "sqlite3" | "db") => "SQLite",
            Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "ico" | "avif") => {
                "Image"
            }
            Some("pdf") => "Adobe-Acrobat",
            Some("doc" | "docx" | "odt") => "Microsoft-Word",
            Some("xls" | "xlsx" | "csv" | "tsv" | "ods") => "Microsoft-Excel",
            Some("ppt" | "pptx" | "odp") => "Microsoft-PowerPoint",
            _ => "Default",
        },
    };

    Some(format!("file-icons/{icon}.svg").into())
}
