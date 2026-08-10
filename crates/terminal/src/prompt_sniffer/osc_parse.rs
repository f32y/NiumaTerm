use std::str;

use crate::event::{ProgressReport, ProgressState};
use crate::prompt_sniffer::PromptRegion;

/// `ESC ] 1 3 3 ;` - the OSC 133 introducer.
const OSC133_PREFIX: &[u8] = b"\x1b]133;";
const OSC_PROGRESS_PREFIX: &[u8] = b"\x1b]9;4;";
/// Max bytes of one mark we buffer/scan (`ESC]133;D;<exit>ST` is far shorter). A malformed
/// mark longer than this resyncs as ordinary bytes; also bounds the inline carry buffer.
pub(super) const OSC133_MAX: usize = 32;

/// Outcome of scanning an ESC in the PTY stream for sequences the sniffer
/// cares about: OSC 133 prompt marks and OSC 9;4 progress reports.
pub(super) enum SniffedOsc {
    /// A complete mark: consume `len` bytes; `next` is the region to switch to (`None` =
    /// a recognized 133 mark with no transition, e.g. `;P` right-prompt). `exit` is the
    /// command exit code carried by a `;D;<code>` mark (`None` for a bare `;D`).
    Mark {
        len: usize,
        sub: u8,
        exit: Option<i32>,
        next: Option<PromptRegion>,
    },
    Progress {
        len: usize,
        report: ProgressReport,
    },
    /// Looks like the start of a 133 mark but the slice ends before the terminator — carry.
    Incomplete,
    /// Not a 133 mark; the ESC is an ordinary terminal byte.
    NotMark,
    /// Starts like OSC 133 but is malformed or too long.
    Malformed,
    /// Starts like OSC 9;4 but is malformed or too long.
    ProgressMalformed,
}

fn region_for(sub: u8) -> Option<PromptRegion> {
    match sub {
        b'A' => Some(PromptRegion::Prompt),
        b'B' => Some(PromptRegion::Command),
        b'C' => Some(PromptRegion::Output),
        b'D' => Some(PromptRegion::None),
        _ => None, // recognized mark, no region transition (e.g. ;P)
    }
}

/// The `;D` mark's exit-code argument: `arg` is the bytes between the subcommand and the
/// terminator (`;42` for `ESC]133;D;42<BEL>`; empty for a bare `;D`).
fn parse_exit_code(arg: &[u8]) -> Option<i32> {
    let digits = arg.strip_prefix(b";")?;
    str::from_utf8(digits).ok()?.parse().ok()
}

/// Parse a potential OSC 133 mark at `s` (caller guarantees `s[0] == 0x1b`).
pub(super) fn parse_sniffed_osc(s: &[u8]) -> SniffedOsc {
    let progress_prefix_len = s.len().min(OSC_PROGRESS_PREFIX.len());

    if s[..progress_prefix_len] == OSC_PROGRESS_PREFIX[..progress_prefix_len] {
        if s.len() <= OSC_PROGRESS_PREFIX.len() + 1 {
            return SniffedOsc::Incomplete;
        }

        let state = match s[OSC_PROGRESS_PREFIX.len()] {
            b'0' => ProgressState::Remove,
            b'1' => ProgressState::Set,
            b'2' => ProgressState::Error,
            b'3' => ProgressState::Indeterminate,
            b'4' => ProgressState::Pause,
            _ => return SniffedOsc::ProgressMalformed,
        };

        if s[OSC_PROGRESS_PREFIX.len() + 1] != b';' {
            return SniffedOsc::ProgressMalformed;
        }

        let end = s.len().min(OSC133_MAX);
        let arg_start = OSC_PROGRESS_PREFIX.len() + 2;
        let mut i = arg_start;

        // The percentage argument. Values above 100 are clamped rather than
        // rejected, so a miscounting emitter still gets a full bar.
        let percent = |term: usize| {
            str::from_utf8(&s[arg_start..term])
                .ok()?
                .parse::<u32>()
                .ok()
                .map(|value| value.min(100) as u8)
        };

        while i < end {
            match s[i] {
                0x07 => {
                    return SniffedOsc::Progress {
                        len: i + 1,
                        report: ProgressReport {
                            state,
                            progress: percent(i),
                        },
                    };
                }
                0x1b if i + 1 < s.len() && s[i + 1] == b'\\' => {
                    return SniffedOsc::Progress {
                        len: i + 2,
                        report: ProgressReport {
                            state,
                            progress: percent(i),
                        },
                    };
                }
                0x1b if i + 1 == s.len() => return SniffedOsc::Incomplete,
                0x1b => return SniffedOsc::ProgressMalformed,
                _ => i += 1,
            }
        }

        return if s.len() < OSC133_MAX {
            SniffedOsc::Incomplete
        } else {
            SniffedOsc::ProgressMalformed
        };
    }

    let pn = s.len().min(OSC133_PREFIX.len());

    if s[..pn] != OSC133_PREFIX[..pn] {
        return SniffedOsc::NotMark;
    }

    if s.len() <= OSC133_PREFIX.len() {
        return SniffedOsc::Incomplete; // prefix matched so far; need the subcommand + terminator
    }

    let sub = s[OSC133_PREFIX.len()];
    let arg_start = OSC133_PREFIX.len() + 1;
    let exit_at = |term: usize| {
        (sub == b'D')
            .then(|| parse_exit_code(&s[arg_start..term]))
            .flatten()
    };

    // Find the terminator: BEL (0x07) or ST (ESC \), bounded by OSC133_MAX.
    let end = s.len().min(OSC133_MAX);

    let mut i = OSC133_PREFIX.len();

    while i < end {
        match s[i] {
            0x07 => {
                return SniffedOsc::Mark {
                    len: i + 1,
                    sub,
                    exit: exit_at(i),
                    next: region_for(sub),
                };
            }
            0x1b if i + 1 < s.len() && s[i + 1] == 0x5c => {
                return SniffedOsc::Mark {
                    len: i + 2,
                    sub,
                    exit: exit_at(i),
                    next: region_for(sub),
                };
            }
            0x1b if i + 1 == s.len() => return SniffedOsc::Incomplete, // maybe a split ST
            0x1b => return SniffedOsc::Malformed,                      // ESC mid-mark that isn't ST
            _ => i += 1,
        }
    }

    if s.len() < OSC133_MAX {
        SniffedOsc::Incomplete // a terminator may still arrive next read
    } else {
        SniffedOsc::Malformed // too long, resync on the ESC
    }
}
