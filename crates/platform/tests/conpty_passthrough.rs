//! Differential harness: does Windows ConPTY pass a kitty graphics APC
//! (`ESC _ G ... ESC \`) through to the conout stream, or strip it the way it
//! strips OSC 111?  rio reads ConPTY's *re-rendered* output, so if the APC does
//! not survive here, rio's engine can never see the image — and the "no image"
//! bug is a ConPTY limitation, not a rio bug.
//!
//! Run with:  cargo test -p teletypewriter --test conpty_passthrough -- --nocapture
#![cfg(windows)]

use std::io::Read;
use std::thread;
use std::time::{Duration, Instant};

use nmt_platform::{ProcessReadWrite, create_pty_with_env};

/// Minimal base64 (standard alphabet, padded) so the test needs no crates.
fn b64(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | (b[2] as u32);
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            T[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

fn utf16le(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect()
}

fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Shared driver: run a PowerShell snippet through the ConPTY path rio uses,
/// collect conout until MARKER_DONE (or timeout), return the raw bytes.
fn drive_conpty(script: &str) -> Vec<u8> {
    drive_conpty_with_title(script, None)
}

fn drive_conpty_with_title(script: &str, title: Option<&str>) -> Vec<u8> {
    let encoded = b64(&utf16le(script));
    let cmdline = format!("powershell -NoProfile -NonInteractive -EncodedCommand {encoded}");
    let mut pty = create_pty_with_env(&cmdline, Vec::new(), &None, 80, 24, &[], title)
        .expect("failed to create ConPTY");

    let mut collected: Vec<u8> = Vec::new();
    let mut buf = [0u8; 4096];
    let deadline = Instant::now() + Duration::from_secs(8);
    let marker = b"MARKER_DONE";
    loop {
        if Instant::now() > deadline {
            break;
        }
        match pty.reader().read(&mut buf) {
            Ok(0) => thread::sleep(Duration::from_millis(20)),
            Ok(n) => {
                collected.extend_from_slice(&buf[..n]);
                if find_subslice(&collected, marker).is_some() {
                    thread::sleep(Duration::from_millis(50));
                    if let Ok(n2) = pty.reader().read(&mut buf) {
                        collected.extend_from_slice(&buf[..n2]);
                    }
                    break;
                }
            }
            Err(_) => break,
        }
    }
    collected
}

#[test]
fn starting_title_reaches_the_console_process() {
    let bytes = drive_conpty_with_title(
        "[Console]::Write([Console]::Title + '|MARKER_DONE')",
        Some("Test Profile"),
    );
    assert!(
        find_subslice(&bytes, b"Test Profile").is_some(),
        "the console process did not inherit STARTUPINFO.lpTitle"
    );
}

fn dump(tag: &str, bytes: &[u8]) {
    let ascii: String = bytes
        .iter()
        .map(|&b| {
            if (0x20..0x7f).contains(&b) {
                b as char
            } else {
                '.'
            }
        })
        .collect();
    eprintln!("[conpty-test:{tag}] bytes={} ascii:\n{ascii}", bytes.len());
}

/// Does ConPTY round-trip an OSC 8 hyperlink (`ESC ] 8 ; ; URI ESC \`)?  This
/// gates whether engine on-hover hyperlink lookup can be verified on
/// Windows at all: if ConPTY strips OSC 8 like it strips OSC 111 / the kitty APC,
/// the engine never records the link and hover lookup is unverifiable on this platform.
#[test]
fn conpty_osc8_hyperlink_roundtrip() {
    let script = r#"
$e = [char]0x1b
[Console]::Out.Write($e + ']8;;https://example.com/abc' + $e + '\' + 'LINKTEXT' + $e + ']8;;' + $e + '\')
[Console]::Out.Write([char]13 + [char]10)
[Console]::Out.Flush()
Start-Sleep -Milliseconds 600
[Console]::Out.Write('MARKER_DONE')
[Console]::Out.Flush()
Start-Sleep -Milliseconds 300
"#;
    let out = drive_conpty(script);
    dump("osc8", &out);
    let saw_marker = find_subslice(&out, b"MARKER_DONE").is_some();
    let saw_osc8 = find_subslice(&out, &[0x1b, 0x5d, 0x38]).is_some(); // ESC ] 8
    let saw_uri = find_subslice(&out, b"example.com").is_some();
    eprintln!("[conpty-test:osc8] saw_marker={saw_marker} saw_osc8={saw_osc8} saw_uri={saw_uri}");
    assert!(saw_marker, "pipe/child broken, inconclusive");
    // Characterization: unlike the kitty APC, ConPTY DOES round-trip OSC 8
    // hyperlinks (it models them as a cell attribute and re-emits them, even
    // auto-assigning an `id=`). The engine therefore receives the link and hover lookup
    // (engine on-hover hyperlink lookup) IS verifiable on Windows. If this ever
    // fails, ConPTY stopped round-tripping OSC 8, making hover lookup unverifiable
    // on this platform, same as kitty graphics.
    assert!(
        saw_osc8 && saw_uri,
        "ConPTY no longer round-trips OSC 8 hyperlinks (osc8={saw_osc8}, uri={saw_uri}); \
         kitty 5.4 hyperlink support can no longer be verified on Windows"
    );
}

/// Does ConPTY pass FinalTerm/iTerm2 OSC 133 shell-integration prompt marks
/// (`ESC ] 133 ; A/B/C/D ESC \`) through to conout? This gates whether a
/// NiumaTerm fixed-prompt dock can ever hide the shell's own prompt rows
/// semantically: if ConPTY strips OSC 133 (the way it strips OSC 111 / the
/// kitty APC), the engine never sees the marks and prompt-row hiding is
/// impossible on Windows without a rio-side OSC sniffer upstream of the engine.
/// If the marks survive, the engine (with upstream work) could tag prompt rows
/// and the dock could hide them. Characterization only — it records the answer
/// without asserting an unknown; flip to a hard assert once we depend on it.
#[test]
fn conpty_osc133_prompt_marks_roundtrip() {
    let script = r#"
$e = [char]0x1b
[Console]::Out.Write($e + ']133;A' + $e + '\')
[Console]::Out.Write('PS TEST> ')
[Console]::Out.Write($e + ']133;B' + $e + '\')
[Console]::Out.Write('echo hi' + [char]13 + [char]10)
[Console]::Out.Write($e + ']133;C' + $e + '\')
[Console]::Out.Write('hi' + [char]13 + [char]10)
[Console]::Out.Write($e + ']133;D;0' + $e + '\')
[Console]::Out.Flush()
Start-Sleep -Milliseconds 600
[Console]::Out.Write('MARKER_DONE')
[Console]::Out.Flush()
Start-Sleep -Milliseconds 300
"#;
    let out = drive_conpty(script);
    dump("osc133", &out);
    let saw_marker = find_subslice(&out, b"MARKER_DONE").is_some();
    // ESC ] 1 3 3  — the OSC 133 introducer.
    let saw_osc133 = find_subslice(&out, &[0x1b, 0x5d, 0x31, 0x33, 0x33]).is_some();
    eprintln!(
        "[conpty-test:osc133] saw_marker={saw_marker} saw_osc133={saw_osc133} \
         RESULT: ConPTY {} OSC 133 prompt marks",
        if saw_osc133 {
            "PASSES THROUGH"
        } else {
            "STRIPS"
        }
    );
    assert!(
        saw_marker,
        "child never produced MARKER_DONE — pipe/child broken, inconclusive"
    );
}

#[test]
fn conpty_osc9_progress_roundtrip() {
    let script = r#"
$e = [char]0x1b
[Console]::Out.Write($e + ']9;4;1;42' + $e + '\')
[Console]::Out.Write('MARKER_DONE')
[Console]::Out.Flush()
Start-Sleep -Milliseconds 300
"#;
    let out = drive_conpty(script);
    assert!(find_subslice(&out, b"MARKER_DONE").is_some());
    assert!(
        find_subslice(&out, b"\x1b]9;4;1;42").is_some(),
        "ConPTY stripped OSC 9;4 progress reporting"
    );
}

/// Positive regression: the bundled ConPTY passes a raw kitty graphics APC
/// (`ESC _ G ... ESC \`) through to conout, so the engine can see transmitted
/// images and live kitty graphics rendering is possible on Windows.
///
/// This used to be an ignored negative canary asserting the APC was *stripped*.
/// The bundled ConPTY now round-trips it, so we assert the positive contract and
/// separately check the trailing marker (pipe/child health) and the `ESC _ G`
/// introducer so a failure distinguishes a broken pipe from APC stripping.
#[test]
fn conpty_passes_through_kitty_graphics_apc() {
    // PowerShell that writes a *raw* kitty graphics APC to stdout, then a text
    // marker so we know the child finished and the pipe is healthy.
    //   APC = ESC _ G <key=val...> ; <base64 payload> ESC \
    //   here: 1x1 RGB pixel (f=24, s=1, v=1), payload "AAAA" = 3 zero bytes.
    let script = r#"
$e = [char]0x1b
[Console]::Out.Write($e + '_Gi=31,a=T,f=24,s=1,v=1;AAAA' + $e + '\')
[Console]::Out.Flush()
Start-Sleep -Milliseconds 600
[Console]::Out.Write('MARKER_DONE')
[Console]::Out.Flush()
Start-Sleep -Milliseconds 300
"#;
    let out = drive_conpty(script);
    dump("kitty-apc", &out);

    let apc = [0x1b, 0x5f, 0x47]; // ESC _ G
    let saw_marker = find_subslice(&out, b"MARKER_DONE").is_some();
    let saw_apc = find_subslice(&out, &apc).is_some();
    eprintln!("[conpty-test:kitty-apc] saw_marker={saw_marker} saw_apc={saw_apc}");

    // Health check first, so a broken pipe/child is not mistaken for stripping.
    assert!(
        saw_marker,
        "child never produced MARKER_DONE — pipe/child broken, test inconclusive"
    );
    // The contract: the kitty graphics APC introducer survives ConPTY. If this
    // fails while the marker arrived, ConPTY regressed to stripping the APC and
    // live kitty graphics can no longer be fed through on Windows.
    assert!(
        saw_apc,
        "bundled ConPTY no longer passes the kitty graphics APC (ESC _ G) through \
         to conout; live kitty graphics can no longer reach the engine on Windows"
    );
}
