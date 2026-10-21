use std::mem;

use vte::Parser;

use crate::agent_tab::transcript::code::ansi::AnsiText;
use crate::agent_tab::transcript::code::prepared::PreparedCode;
use crate::agent_tab::transcript::code::source::CodeSource;
use crate::agent_tab::transcript::{detect_output_language, strip_read_gutter};

pub(super) struct CodeParser {
    pub(super) prepared: PreparedCode,
    parser: Parser,
    ansi: AnsiText,
    language: Option<String>,
    language_probe: String,
    probe_complete: bool,
    inferred_language: &'static str,
}

impl CodeParser {
    pub(super) fn new(source: CodeSource) -> Self {
        let normalized = source
            .strip_gutter
            .then(|| strip_read_gutter(&source.output))
            .flatten();

        let mut parser = Self {
            prepared: PreparedCode::new(source.command.as_deref()),
            parser: Parser::new(),
            ansi: AnsiText::default(),
            language: source.language,
            language_probe: String::new(),
            probe_complete: false,
            inferred_language: "",
        };

        parser.append(normalized.as_deref().unwrap_or(&source.output));

        parser
    }

    pub(super) fn append(&mut self, source: &str) {
        self.ansi.advance(&mut self.parser, source);

        let text = mem::take(&mut self.ansi.text);
        let spans = mem::take(&mut self.ansi.spans);

        if self.language.is_none() && !self.probe_complete {
            let probe = if self.language_probe.is_empty() {
                text.trim_start()
            } else {
                &text
            };

            let end =
                probe.floor_char_boundary(probe.len().min(16 * 1024 - self.language_probe.len()));

            self.language_probe.push_str(&probe[..end]);

            self.probe_complete = end < probe.len() || self.language_probe.len() == 16 * 1024;

            if end > 0 {
                self.inferred_language = detect_output_language(&self.language_probe);
            }
        }

        let language = self.language.as_deref().unwrap_or(self.inferred_language);

        self.prepared.append_output(&text, spans, language);
    }
}
