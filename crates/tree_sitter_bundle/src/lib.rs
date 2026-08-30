use std::ptr;

const ABI_VERSION: u32 = 1;
const LANGUAGE_COUNT: u32 = 36;

type LanguageBuilder = unsafe extern "C" fn() -> *const ();

#[derive(Clone, Copy)]
#[repr(C)]
pub struct RawSlice {
    data: *const u8,
    len: usize,
}

impl RawSlice {
    fn new(value: &'static str) -> Self {
        Self {
            data: value.as_ptr(),
            len: value.len(),
        }
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
pub struct LanguageDescriptor {
    name: RawSlice,
    aliases: RawSlice,
    injection_languages: RawSlice,
    language: Option<LanguageBuilder>,
    highlights: RawSlice,
    injections: RawSlice,
    locals: RawSlice,
}

impl LanguageDescriptor {
    fn new(
        name: &'static str,
        aliases: &'static str,
        injection_languages: &'static str,
        language: LanguageBuilder,
        highlights: &'static str,
        injections: &'static str,
        locals: &'static str,
    ) -> Self {
        Self {
            name: RawSlice::new(name),
            aliases: RawSlice::new(aliases),
            injection_languages: RawSlice::new(injection_languages),
            language: Some(language),
            highlights: RawSlice::new(highlights),
            injections: RawSlice::new(injections),
            locals: RawSlice::new(locals),
        }
    }
}

macro_rules! language {
    ($name:literal, $aliases:literal, $injected:literal, $language:expr, $highlights:expr, $injections:expr, $locals:expr) => {
        LanguageDescriptor::new(
            $name,
            $aliases,
            $injected,
            ($language).into_raw(),
            $highlights,
            $injections,
            $locals,
        )
    };
}

fn descriptor(index: u32) -> Option<LanguageDescriptor> {
    let value = match index {
        0 => language!(
            "astro",
            "",
            "html\0css\0javascript\0typescript",
            tree_sitter_astro_next::LANGUAGE,
            tree_sitter_astro_next::HIGHLIGHTS_QUERY,
            tree_sitter_astro_next::INJECTIONS_QUERY,
            ""
        ),
        1 => language!(
            "bash",
            "sh",
            "",
            tree_sitter_bash::LANGUAGE,
            tree_sitter_bash::HIGHLIGHT_QUERY,
            "",
            ""
        ),
        2 => language!(
            "c",
            "",
            "",
            tree_sitter_c::LANGUAGE,
            tree_sitter_c::HIGHLIGHT_QUERY,
            "",
            ""
        ),
        3 => language!("cmake", "", "", tree_sitter_cmake::LANGUAGE, "", "", ""),
        4 => language!(
            "csharp",
            "cs",
            "",
            tree_sitter_c_sharp::LANGUAGE,
            "",
            "",
            ""
        ),
        5 => language!(
            "cpp",
            "c++",
            "",
            tree_sitter_cpp::LANGUAGE,
            tree_sitter_cpp::HIGHLIGHT_QUERY,
            "",
            ""
        ),
        6 => language!(
            "css",
            "scss",
            "",
            tree_sitter_css::LANGUAGE,
            tree_sitter_css::HIGHLIGHTS_QUERY,
            "",
            ""
        ),
        7 => language!(
            "diff",
            "",
            "",
            tree_sitter_diff::LANGUAGE,
            tree_sitter_diff::HIGHLIGHTS_QUERY,
            "",
            ""
        ),
        8 => language!(
            "ejs",
            "",
            "",
            tree_sitter_embedded_template::LANGUAGE,
            tree_sitter_embedded_template::HIGHLIGHTS_QUERY,
            tree_sitter_embedded_template::INJECTIONS_EJS_QUERY,
            ""
        ),
        9 => language!(
            "elixir",
            "ex",
            "",
            tree_sitter_elixir::LANGUAGE,
            tree_sitter_elixir::HIGHLIGHTS_QUERY,
            tree_sitter_elixir::INJECTIONS_QUERY,
            ""
        ),
        10 => language!(
            "erb",
            "",
            "",
            tree_sitter_embedded_template::LANGUAGE,
            tree_sitter_embedded_template::HIGHLIGHTS_QUERY,
            tree_sitter_embedded_template::INJECTIONS_EJS_QUERY,
            ""
        ),
        11 => language!(
            "go",
            "",
            "",
            tree_sitter_go::LANGUAGE,
            include_str!(
                "../../../third_party/gpui-component/ui/src/highlighter/languages/go/highlights.scm"
            ),
            "",
            ""
        ),
        12 => language!("graphql", "", "", tree_sitter_graphql::LANGUAGE, "", "", ""),
        13 => language!(
            "html",
            "",
            "javascript\0css",
            tree_sitter_html::LANGUAGE,
            include_str!(
                "../../../third_party/gpui-component/ui/src/highlighter/languages/html/highlights.scm"
            ),
            include_str!(
                "../../../third_party/gpui-component/ui/src/highlighter/languages/html/injections.scm"
            ),
            ""
        ),
        14 => language!(
            "java",
            "",
            "",
            tree_sitter_java::LANGUAGE,
            tree_sitter_java::HIGHLIGHTS_QUERY,
            "",
            ""
        ),
        15 => language!(
            "javascript",
            "js",
            "jsdoc\0json\0css\0html\0sql\0typescript\0javascript\0tsx\0yaml\0graphql",
            tree_sitter_javascript::LANGUAGE,
            include_str!(
                "../../../third_party/gpui-component/ui/src/highlighter/languages/javascript/highlights.scm"
            ),
            include_str!(
                "../../../third_party/gpui-component/ui/src/highlighter/languages/javascript/injections.scm"
            ),
            tree_sitter_javascript::LOCALS_QUERY
        ),
        16 => language!(
            "jsdoc",
            "",
            "",
            tree_sitter_jsdoc::LANGUAGE,
            tree_sitter_jsdoc::HIGHLIGHTS_QUERY,
            "",
            ""
        ),
        17 => language!(
            "kotlin",
            "kt\0kts\0ktm",
            "",
            tree_sitter_kotlin_sg::LANGUAGE,
            include_str!(
                "../../../third_party/gpui-component/ui/src/highlighter/languages/kotlin/highlights.scm"
            ),
            "",
            ""
        ),
        18 => language!(
            "lua",
            "",
            "",
            tree_sitter_lua::LANGUAGE,
            include_str!(
                "../../../third_party/gpui-component/ui/src/highlighter/languages/lua/highlights.scm"
            ),
            tree_sitter_lua::INJECTIONS_QUERY,
            tree_sitter_lua::LOCALS_QUERY
        ),
        19 => language!(
            "make",
            "makefile",
            "",
            tree_sitter_make::LANGUAGE,
            tree_sitter_make::HIGHLIGHTS_QUERY,
            "",
            ""
        ),
        20 => language!(
            "markdown",
            "md\0mdx",
            "markdown_inline\0html\0toml\0yaml",
            tree_sitter_md::LANGUAGE,
            include_str!(
                "../../../third_party/gpui-component/ui/src/highlighter/languages/markdown/highlights.scm"
            ),
            include_str!(
                "../../../third_party/gpui-component/ui/src/highlighter/languages/markdown/injections.scm"
            ),
            ""
        ),
        21 => language!(
            "markdown_inline",
            "markdown-inline",
            "",
            tree_sitter_md::INLINE_LANGUAGE,
            include_str!(
                "../../../third_party/gpui-component/ui/src/highlighter/languages/markdown_inline/highlights.scm"
            ),
            "",
            ""
        ),
        22 => language!(
            "php",
            "php3\0php4\0php5\0phtml",
            "php\0html\0css\0javascript\0json\0jsdoc\0graphql",
            tree_sitter_php::LANGUAGE_PHP,
            tree_sitter_php::HIGHLIGHTS_QUERY,
            include_str!(
                "../../../third_party/gpui-component/ui/src/highlighter/languages/php/injections.scm"
            ),
            ""
        ),
        23 => language!(
            "proto",
            "protobuf",
            "",
            tree_sitter_proto::LANGUAGE,
            "",
            "",
            ""
        ),
        24 => language!(
            "python",
            "py",
            "",
            tree_sitter_python::LANGUAGE,
            tree_sitter_python::HIGHLIGHTS_QUERY,
            "",
            ""
        ),
        25 => language!(
            "ruby",
            "rb",
            "",
            tree_sitter_ruby::LANGUAGE,
            tree_sitter_ruby::HIGHLIGHTS_QUERY,
            "",
            tree_sitter_ruby::LOCALS_QUERY
        ),
        26 => language!(
            "rust",
            "rs",
            "rust",
            tree_sitter_rust::LANGUAGE,
            include_str!(
                "../../../third_party/gpui-component/ui/src/highlighter/languages/rust/highlights.scm"
            ),
            include_str!(
                "../../../third_party/gpui-component/ui/src/highlighter/languages/rust/injections.scm"
            ),
            ""
        ),
        27 => language!(
            "scala",
            "",
            "",
            tree_sitter_scala::LANGUAGE,
            tree_sitter_scala::HIGHLIGHTS_QUERY,
            "",
            tree_sitter_scala::LOCALS_QUERY
        ),
        28 => language!(
            "sql",
            "",
            "",
            tree_sitter_sequel::LANGUAGE,
            tree_sitter_sequel::HIGHLIGHTS_QUERY,
            "",
            ""
        ),
        29 => language!(
            "svelte",
            "",
            "svelte\0html\0css\0typescript",
            tree_sitter_svelte_next::LANGUAGE,
            tree_sitter_svelte_next::HIGHLIGHTS_QUERY,
            tree_sitter_svelte_next::INJECTIONS_QUERY,
            tree_sitter_svelte_next::LOCALS_QUERY
        ),
        30 => language!("swift", "", "", tree_sitter_swift::LANGUAGE, "", "", ""),
        31 => language!(
            "toml",
            "",
            "",
            tree_sitter_toml_ng::LANGUAGE,
            tree_sitter_toml_ng::HIGHLIGHTS_QUERY,
            "",
            ""
        ),
        32 => language!(
            "tsx",
            "",
            "",
            tree_sitter_typescript::LANGUAGE_TSX,
            tree_sitter_typescript::HIGHLIGHTS_QUERY,
            "",
            tree_sitter_typescript::LOCALS_QUERY
        ),
        33 => language!(
            "typescript",
            "ts",
            "jsdoc\0json\0css\0html\0sql\0typescript\0javascript\0tsx\0yaml\0graphql",
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
            include_str!(
                "../../../third_party/gpui-component/ui/src/highlighter/languages/typescript/highlights.scm"
            ),
            include_str!(
                "../../../third_party/gpui-component/ui/src/highlighter/languages/javascript/injections.scm"
            ),
            tree_sitter_typescript::LOCALS_QUERY
        ),
        34 => language!(
            "yaml",
            "yml",
            "",
            tree_sitter_yaml::LANGUAGE,
            tree_sitter_yaml::HIGHLIGHTS_QUERY,
            "",
            ""
        ),
        35 => language!(
            "zig",
            "",
            "",
            tree_sitter_zig::LANGUAGE,
            include_str!(
                "../../../third_party/gpui-component/ui/src/highlighter/languages/zig/highlights.scm"
            ),
            include_str!(
                "../../../third_party/gpui-component/ui/src/highlighter/languages/zig/injections.scm"
            ),
            ""
        ),
        _ => return None,
    };

    Some(value)
}

#[unsafe(no_mangle)]
pub extern "system" fn nmt_tree_sitter_abi_version() -> u32 {
    ABI_VERSION
}

#[unsafe(no_mangle)]
pub extern "system" fn nmt_tree_sitter_language_count() -> u32 {
    LANGUAGE_COUNT
}

/// Writes one descriptor into caller-owned storage.
///
/// # Safety
///
/// `output` must be null or point to valid, aligned, writable storage for one
/// `LanguageDescriptor`.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn nmt_tree_sitter_language(
    index: u32,
    output: *mut LanguageDescriptor,
) -> u32 {
    let Some(value) = descriptor(index) else {
        return 0;
    };
    if output.is_null() {
        return 0;
    }

    // The null check above establishes writable storage as the remaining
    // caller obligation; `write` avoids reading uninitialized output bytes.
    unsafe { ptr::write(output, value) };
    1
}

#[cfg(test)]
mod tests;
