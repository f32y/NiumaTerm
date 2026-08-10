/// Render the command-echo region's bytes (`;B`→`;C`) into the final command line
/// (command-blocks). A plain control-strip is not enough here: PSReadLine redraws the
/// input on nearly every keystroke (syntax highlighting) and ConPTY reprojects each
/// redraw at absolute columns, so concatenating printables duplicates the line once per
/// redraw. Instead emulate a single line: printables write at the cursor column
/// (overwriting earlier redraws), CR/CUP/CHA/CUF/CUB move the cursor, EL/ECH erase.
/// Known ceiling: rows are collapsed onto the one line, so a wrapped multi-row command
/// can self-overwrite; engine-grid readback (per-row SEMANTIC_PROMPT tags) is the
/// upgrade path if that fidelity matters.
pub(super) fn render_command_echo(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let chars: Vec<char> = text.chars().collect();

    let mut line: Vec<char> = Vec::new();
    let mut col = 0usize;
    let mut i = 0;

    while i < chars.len() {
        match chars[i] {
            '\x1b' => {
                i += 1;
                match chars.get(i) {
                    Some('[') => {
                        // CSI: ESC [ params… final byte (0x40..=0x7e).
                        i += 1;
                        let params_start = i;

                        while i < chars.len() && !('\x40'..='\x7e').contains(&chars[i]) {
                            i += 1;
                        }

                        let Some(&fin) = chars.get(i) else { break };

                        let params: String = chars[params_start..i].iter().collect();

                        i += 1;

                        let nth = |n: usize, def: usize| {
                            params
                                .split(';')
                                .nth(n)
                                .and_then(|s| s.parse::<usize>().ok())
                                .unwrap_or(def)
                        };

                        match fin {
                            // CUP row;col — only the column matters on our one line.
                            'H' | 'f' => col = nth(1, 1).saturating_sub(1),
                            'G' => col = nth(0, 1).saturating_sub(1), // CHA
                            'C' => col += nth(0, 1).max(1),           // CUF
                            'D' => col = col.saturating_sub(nth(0, 1).max(1)), // CUB
                            'K' => match nth(0, 0) {
                                0 => line.truncate(col), // EL to end
                                2 => line.clear(),       // EL whole line
                                _ => {
                                    for c in line.iter_mut().take(col) {
                                        *c = ' '; // EL to start
                                    }
                                }
                            },
                            'X' => {
                                // ECH n: blank n cells at the cursor.
                                let end = (col + nth(0, 1).max(1)).min(line.len());

                                for c in line.iter_mut().take(end).skip(col) {
                                    *c = ' ';
                                }
                            }
                            _ => {} // SGR and the rest have no text effect
                        }
                    }
                    Some(']') => {
                        // OSC … (BEL or ST)
                        i += 1;

                        while i < chars.len() {
                            if chars[i] == '\x07' {
                                i += 1;
                                break;
                            }

                            if chars[i] == '\x1b' && chars.get(i + 1) == Some(&'\\') {
                                i += 2;
                                break;
                            }

                            i += 1;
                        }
                    }
                    _ => i += 1, // ESC + single intermediate/final
                }
            }
            '\r' => {
                col = 0;
                i += 1;
            }
            '\x08' => {
                col = col.saturating_sub(1);

                i += 1;
            }
            c if c >= ' ' => {
                while line.len() < col {
                    line.push(' ');
                }
                if col < line.len() {
                    line[col] = c;
                } else {
                    line.push(c);
                }
                col += 1;
                i += 1;
            }
            _ => i += 1, // LF (row collapse), TAB, BEL, other controls — drop
        }
    }
    line.into_iter().collect::<String>().trim().to_string()
}
