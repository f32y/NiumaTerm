use std::ops::Range;

use gpui::{
    FontStyle, FontWeight, HighlightStyle, Hsla, StrikethroughStyle, UnderlineStyle, px, rgb,
};
use vte::{Params, Parser, Perform};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AnsiColor {
    Indexed(u8),
    Rgb(u8, u8, u8),
}

impl AnsiColor {
    fn resolve(self) -> Hsla {
        let value = match self {
            Self::Rgb(r, g, b) => u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b),
            Self::Indexed(index) => match index {
                0..=15 => [
                    0x000000, 0xcd3131, 0x0dbc79, 0xe5e510, 0x2472c8, 0xbc3fbc, 0x11a8cd, 0xe5e5e5,
                    0x666666, 0xf14c4c, 0x23d18b, 0xf5f543, 0x3b8eea, 0xd670d6, 0x29b8db, 0xffffff,
                ][usize::from(index)],
                16..=231 => {
                    let index = u32::from(index - 16);
                    let component = |value| if value == 0 { 0 } else { 55 + 40 * value };
                    component(index / 36) << 16
                        | component(index / 6 % 6) << 8
                        | component(index % 6)
                }
                232..=255 => u32::from(8 + 10 * (index - 232)) * 0x010101,
            },
        };
        rgb(value).into()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct AnsiStyle {
    pub(super) foreground: Option<AnsiColor>,
    pub(super) background: Option<AnsiColor>,
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
    strike: bool,
    reverse: bool,
}

impl AnsiStyle {
    pub(super) fn resolve(self, foreground: Hsla, background: Hsla) -> HighlightStyle {
        let mut color = self.foreground.map(AnsiColor::resolve);
        let mut background_color = self.background.map(AnsiColor::resolve);
        if self.reverse {
            (color, background_color) = (
                Some(background_color.unwrap_or(background)),
                Some(color.unwrap_or(foreground)),
            );
        }
        HighlightStyle {
            color,
            background_color,
            font_weight: self.bold.then_some(FontWeight::BOLD),
            font_style: self.italic.then_some(FontStyle::Italic),
            fade_out: self.dim.then_some(0.4),
            underline: self.underline.then_some(UnderlineStyle {
                thickness: px(1.),
                color: None,
                wavy: false,
                dashed: false,
            }),
            strikethrough: self.strike.then_some(StrikethroughStyle {
                thickness: px(1.),
                color: None,
            }),
        }
    }
}

#[derive(Default)]
pub(super) struct AnsiText {
    pub(super) text: String,
    pub(super) spans: Vec<(Range<usize>, AnsiStyle)>,
    style: AnsiStyle,
    pending_cr: bool,
}

impl AnsiText {
    pub(super) fn parse(source: &str) -> Self {
        let mut output = Self::default();
        Parser::new().advance(&mut output, source.as_bytes());
        output
    }

    fn push(&mut self, ch: char) {
        let start = self.text.len();
        self.text.push(ch);
        if self.style == AnsiStyle::default() {
            return;
        }
        if let Some((range, style)) = self.spans.last_mut()
            && *style == self.style
            && range.end == start
        {
            range.end = self.text.len();
        } else {
            self.spans.push((start..self.text.len(), self.style));
        }
    }
}

impl Perform for AnsiText {
    fn print(&mut self, ch: char) {
        // A transcript retains progress updates as lines; cursor motion does
        // not modify earlier records. CRLF remains a single line break.
        if self.pending_cr {
            self.push('\n');
            self.pending_cr = false;
        }
        self.push(ch);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\r' => self.pending_cr = true,
            b'\n' => {
                self.pending_cr = false;
                self.push('\n');
            }
            b'\t' => self.print('\t'),
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        if ignore || !intermediates.is_empty() || action != 'm' {
            return;
        }
        let mut params = params.iter();
        while let Some(group) = params.next() {
            let code = group[0];
            match code {
                0 => self.style = AnsiStyle::default(),
                1 => self.style.bold = true,
                2 => self.style.dim = true,
                3 => self.style.italic = true,
                4 => self.style.underline = true,
                7 => self.style.reverse = true,
                9 => self.style.strike = true,
                22 => {
                    self.style.bold = false;
                    self.style.dim = false;
                }
                23 => self.style.italic = false,
                24 => self.style.underline = false,
                27 => self.style.reverse = false,
                29 => self.style.strike = false,
                30..=37 => self.style.foreground = Some(AnsiColor::Indexed((code - 30) as u8)),
                40..=47 => self.style.background = Some(AnsiColor::Indexed((code - 40) as u8)),
                90..=97 => self.style.foreground = Some(AnsiColor::Indexed((code - 90 + 8) as u8)),
                100..=107 => {
                    self.style.background = Some(AnsiColor::Indexed((code - 100 + 8) as u8))
                }
                39 => self.style.foreground = None,
                49 => self.style.background = None,
                38 | 48 => {
                    let values = if group.len() > 1 {
                        group[1..].to_vec()
                    } else {
                        let Some(mode) = params.next().map(|group| group[0]) else {
                            break;
                        };
                        let count = match mode {
                            2 => 3,
                            5 => 1,
                            _ => break,
                        };
                        let mut values = vec![mode];
                        values.extend(params.by_ref().take(count).map(|group| group[0]));
                        values
                    };
                    let color = match values.as_slice() {
                        [5, index] => u8::try_from(*index).ok().map(AnsiColor::Indexed),
                        [2, r, g, b] | [2, 0, r, g, b] => u8::try_from(*r)
                            .ok()
                            .zip(u8::try_from(*g).ok())
                            .zip(u8::try_from(*b).ok())
                            .map(|((r, g), b)| AnsiColor::Rgb(r, g, b)),
                        _ => None,
                    };
                    if let Some(color) = color {
                        if code == 38 {
                            self.style.foreground = Some(color);
                        } else {
                            self.style.background = Some(color);
                        }
                    }
                }
                _ => {}
            }
        }
    }
}
