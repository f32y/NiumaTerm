use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use nmt_terminal::ansi::glyph_protocol::*;

fn b64(data: &[u8]) -> String {
    BASE64.encode(data)
}

#[test]
fn rejects_non_glyph_protocol_bodies() {
    assert_eq!(parse(b"G,a=T;payload"), Err(ParseError::NotGlyphProtocol));
    assert_eq!(parse(b""), Err(ParseError::NotGlyphProtocol));
}

#[test]
fn is_pua_covers_all_three_ranges() {
    assert!(is_pua(0xE000));
    assert!(is_pua(0xE0A0)); // Powerline branch
    assert!(is_pua(0xF8FF)); // end of basic PUA
    assert!(is_pua(0xF_0000)); // start of supp-A
    assert!(is_pua(0xF_FFFD));
    assert!(is_pua(0x10_0000));
    assert!(is_pua(0x10_FFFD));
}

#[test]
fn is_pua_excludes_real_text_and_emoji() {
    assert!(!is_pua(0x0061)); // 'a'
    assert!(!is_pua(0x002D)); // '-'
    assert!(!is_pua(0x1F600)); // grinning face — supplementary but NOT PUA
    assert!(!is_pua(0xFFFE)); // noncharacter just before PUA-A
    assert!(!is_pua(0xF_FFFE)); // noncharacter just after PUA-A
    assert!(!is_pua(0x10_FFFF)); // noncharacter just after PUA-B
}

#[test]
fn parses_query_single_codepoint() {
    let got = parse(b"25a1;q;cp=E0A0").unwrap();
    assert_eq!(got, GlyphCommand::Query { cp: 0xE0A0 });
}

#[test]
fn query_accepts_non_pua_codepoints() {
    // Query probes the world; it does not care about PUA.
    let got = parse(b"25a1;q;cp=61").unwrap();
    assert_eq!(got, GlyphCommand::Query { cp: 0x61 });
}

#[test]
fn query_rejects_sequence() {
    assert!(matches!(
        parse(b"25a1;q;cp=2D,3E"),
        Err(ParseError::Malformed(_))
    ));
}

#[test]
fn query_rejects_surrogate() {
    assert!(matches!(
        parse(b"25a1;q;cp=D800"),
        Err(ParseError::Malformed(_))
    ));
}

#[test]
fn parses_register_at_pua_codepoint() {
    let payload = b64(&[0x01, 0x02, 0x03]);
    let body = format!("25a1;r;cp=E0A0;upm=1000;{}", payload);
    let got = parse(body.as_bytes()).unwrap();
    assert_eq!(
        got,
        GlyphCommand::Register {
            cp: 0xE0A0,
            payload: GlyphPayload::Glyf {
                glyf: vec![0x01, 0x02, 0x03],
                upm: 1000,
            },
            reply: ReplyMode::All,
        }
    );
}

#[test]
fn parses_register_with_explicit_fmt() {
    let payload = b64(&[0xAA]);
    let body = format!("25a1;r;cp=E0A0;fmt=glyf;upm=1000;{}", payload);
    assert!(matches!(
        parse(body.as_bytes()).unwrap(),
        GlyphCommand::Register {
            payload: GlyphPayload::Glyf { .. },
            ..
        }
    ));
}

#[test]
fn register_defaults_upm_to_1000() {
    let payload = b64(&[0x01]);
    let body = format!("25a1;r;cp=E0A0;{}", payload);
    let got = parse(body.as_bytes()).unwrap();
    if let GlyphCommand::Register {
        payload: GlyphPayload::Glyf { upm, .. },
        ..
    } = got
    {
        assert_eq!(upm, 1000);
    } else {
        panic!("expected glyf register");
    }
}

#[test]
fn register_rejects_non_pua_codepoint() {
    let payload = b64(&[0x01]);
    let body = format!("25a1;r;cp=61;upm=1000;{}", payload);
    assert_eq!(
        parse(body.as_bytes()),
        Err(ParseError::RegisterFailed {
            cp: 0x61,
            reason: RegisterError::OutOfNamespace,
            reply: ReplyMode::All,
        })
    );
}

#[test]
fn register_requires_cp() {
    let payload = b64(&[0x01]);
    let body = format!("25a1;r;upm=1000;{}", payload);
    assert!(matches!(
        parse(body.as_bytes()),
        Err(ParseError::Malformed(_))
    ));
}

#[test]
fn register_accepts_each_pua_range() {
    for &cp_hex in &[0xE0A0u32, 0xF_0000, 0x10_0000] {
        let payload = b64(b"x");
        let body = format!("25a1;r;cp={:x};upm=1000;{}", cp_hex, payload);
        assert!(matches!(
            parse(body.as_bytes()).unwrap(),
            GlyphCommand::Register { .. }
        ));
    }
}

#[test]
fn register_rejects_unknown_fmt() {
    let payload = b64(b"x");
    let body = format!("25a1;r;cp=E0A0;fmt=svg;upm=1000;{}", payload);
    assert!(matches!(
        parse(body.as_bytes()),
        Err(ParseError::Malformed(_))
    ));
}

#[test]
fn register_rejects_bad_base64() {
    let body = b"25a1;r;cp=E0A0;upm=1000;$$$$not_base64";
    assert!(matches!(
        parse(body),
        Err(ParseError::RegisterFailed {
            reason: RegisterError::MalformedPayload,
            ..
        })
    ));
}

#[test]
fn register_rejects_oversized_payload() {
    let payload = b64(&vec![0u8; MAX_PAYLOAD_BYTES + 1]);
    let body = format!("25a1;r;cp=E0A0;upm=1000;{}", payload);
    assert!(matches!(
        parse(body.as_bytes()),
        Err(ParseError::RegisterFailed {
            reason: RegisterError::PayloadTooLarge,
            ..
        })
    ));
}

#[test]
fn register_rejects_zero_upm() {
    let payload = b64(b"x");
    let body = format!("25a1;r;cp=E0A0;upm=0;{}", payload);
    assert!(matches!(
        parse(body.as_bytes()),
        Err(ParseError::Malformed(_))
    ));
}

#[test]
fn register_rejects_empty_payload() {
    // Trailing `;` followed by an empty base64 segment. Without the
    // post-decode empty check this would silently produce a
    // `Glyf{glyf: vec![]}` registration that the renderer would
    // later interpret as zero-byte garbage.
    let body = b"25a1;r;cp=E0A0;upm=1000;";
    assert!(matches!(
        parse(body),
        Err(ParseError::RegisterFailed {
            cp: 0xE0A0,
            reason: RegisterError::MalformedPayload,
            ..
        })
    ));
}

#[test]
fn register_rejects_missing_payload_separator() {
    // No `;` between control params and the would-be payload: the
    // last-`;` split treats the whole tail as control, leaving the
    // payload section empty. Same MalformedPayload as the trailing-
    // semicolon case above.
    let body = b"25a1;r;cp=E0A0";
    assert!(matches!(
        parse(body),
        Err(ParseError::RegisterFailed {
            cp: 0xE0A0,
            reason: RegisterError::MalformedPayload,
            ..
        })
    ));
}

#[test]
fn register_rejects_cp_above_unicode_max() {
    // 6-hex-digit cps that overflow the 0x10FFFF Unicode ceiling
    // must fail at hex parse time, not silently wrap. `parse_hex_cp`
    // surfaces this as a generic `Malformed` since we no longer
    // know which `cp` to attach to a `RegisterFailed`.
    let payload = b64(b"x");
    let body = format!("25a1;r;cp=110000;upm=1000;{}", payload);
    assert!(matches!(
        parse(body.as_bytes()),
        Err(ParseError::Malformed(_))
    ));

    let body = format!("25a1;r;cp=FFFFFF;upm=1000;{}", payload);
    assert!(matches!(
        parse(body.as_bytes()),
        Err(ParseError::Malformed(_))
    ));
}

#[test]
fn register_rejects_empty_cp_value() {
    // `cp=` with no value: `parse_hex_cp` returns None on the empty
    // slice, surfacing as Malformed("register cp invalid hex").
    let payload = b64(b"x");
    let body = format!("25a1;r;cp=;upm=1000;{}", payload);
    assert!(matches!(
        parse(body.as_bytes()),
        Err(ParseError::Malformed(_))
    ));
}

#[test]
fn query_rejects_cp_above_unicode_max() {
    let body = b"25a1;q;cp=110000";
    assert!(matches!(parse(body), Err(ParseError::Malformed(_))));
}

#[test]
fn clear_single_pua_slot() {
    let got = parse(b"25a1;c;cp=E0A0").unwrap();
    assert_eq!(got, GlyphCommand::Clear { cp: Some(0xE0A0) });
}

#[test]
fn clear_rejects_non_pua_cp() {
    assert_eq!(parse(b"25a1;c;cp=61"), Err(ParseError::ClearOutOfNamespace));
    assert_eq!(
        parse(b"25a1;c;cp=1F600"),
        Err(ParseError::ClearOutOfNamespace)
    );
}

#[test]
fn clear_rejects_sequence_cp() {
    assert!(matches!(
        parse(b"25a1;c;cp=E0A0,E0A1"),
        Err(ParseError::Malformed(_))
    ));
}

#[test]
fn clear_all() {
    let got = parse(b"25a1;c").unwrap();
    assert_eq!(got, GlyphCommand::Clear { cp: None });
}

#[test]
fn parses_support_with_no_params() {
    assert_eq!(parse(b"25a1;s").unwrap(), GlyphCommand::Support);
}

#[test]
fn support_ignores_unknown_params() {
    // Unknown params are silently ignored so the verb remains
    // parameter-free, but a forward-compatible client may send
    // hints; we still produce a valid reply.
    assert_eq!(
        parse(b"25a1;s;future=1;anything=else").unwrap(),
        GlyphCommand::Support
    );
}

#[test]
fn unknown_verb_is_malformed() {
    assert!(matches!(
        parse(b"25a1;z;cp=0061"),
        Err(ParseError::Malformed(_))
    ));
}

/// Build a colour-payload container from component byte slices.
/// Builds the binary layout consumed by [`ColrContainer`].
fn build_container(glyphs: &[&[u8]], colr: &[u8], cpal: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(glyphs.len() as u16).to_be_bytes());
    for g in glyphs {
        out.extend_from_slice(&(g.len() as u16).to_be_bytes());
        out.extend_from_slice(g);
    }
    out.extend_from_slice(&(colr.len() as u16).to_be_bytes());
    out.extend_from_slice(colr);
    out.extend_from_slice(&(cpal.len() as u16).to_be_bytes());
    out.extend_from_slice(cpal);
    out
}

#[test]
fn parses_colrv0_single_glyph() {
    let container = build_container(&[&[0xAA, 0xBB]], &[0x01; 14], &[0x02; 12]);
    let body = format!("25a1;r;cp=E0A0;fmt=colrv0;upm=1000;{}", b64(&container));
    let got = parse(body.as_bytes()).unwrap();
    match got {
        GlyphCommand::Register {
            cp: 0xE0A0,
            payload:
                GlyphPayload::ColrV0 {
                    container: c,
                    upm: 1000,
                },
            reply: ReplyMode::All,
        } => {
            assert_eq!(c.glyphs.len(), 1);
            assert_eq!(c.glyphs[0], vec![0xAA, 0xBB]);
            assert_eq!(c.colr.len(), 14);
            assert_eq!(c.cpal.len(), 12);
        }
        other => panic!("expected colrv0 register, got {:?}", other),
    }
}

#[test]
fn parses_colrv1_multi_glyph_with_empty_cpal() {
    // CPAL can legitimately be zero-length when the COLR uses only
    // foreground or direct-sRGB paints (v1 doesn't require CPAL at
    // all if no palette index is referenced).
    let container = build_container(
        &[&[0x01], &[0x02, 0x03], &[0x04, 0x05, 0x06]],
        &[0xF0; 32],
        &[],
    );
    let body = format!("25a1;r;cp=100000;fmt=colrv1;upm=2048;{}", b64(&container));
    let got = parse(body.as_bytes()).unwrap();
    match got {
        GlyphCommand::Register {
            cp: 0x100000,
            payload:
                GlyphPayload::ColrV1 {
                    container: c,
                    upm: 2048,
                },
            reply: ReplyMode::All,
        } => {
            assert_eq!(c.glyphs.len(), 3);
            assert_eq!(c.glyphs[2], vec![0x04, 0x05, 0x06]);
            assert_eq!(c.colr.len(), 32);
            assert!(c.cpal.is_empty());
        }
        other => panic!("expected colrv1 register, got {:?}", other),
    }
}

#[test]
fn colr_rejects_zero_glyphs() {
    // Every colour glyph needs at least one outline; `0 glyphs` is
    // meaningless and likely indicates a corrupt payload.
    let container = build_container(&[], &[0x00; 4], &[]);
    let body = format!("25a1;r;cp=E0A0;fmt=colrv0;upm=1000;{}", b64(&container));
    assert!(matches!(
        parse(body.as_bytes()),
        Err(ParseError::RegisterFailed {
            reason: RegisterError::MalformedPayload,
            ..
        })
    ));
}

#[test]
fn colr_rejects_empty_colr_table() {
    let container = build_container(&[&[0x01]], &[], &[]);
    let body = format!("25a1;r;cp=E0A0;fmt=colrv0;upm=1000;{}", b64(&container));
    assert!(matches!(
        parse(body.as_bytes()),
        Err(ParseError::RegisterFailed {
            reason: RegisterError::MalformedPayload,
            ..
        })
    ));
}

#[test]
fn colr_rejects_truncated_payload() {
    // Claim 2 glyphs but only ship one — the cursor runs out of
    // bytes inside the loop.
    let mut bad = Vec::new();
    bad.extend_from_slice(&2u16.to_be_bytes());
    bad.extend_from_slice(&1u16.to_be_bytes());
    bad.push(0xAA);
    // …no second glyph, no COLR, no CPAL.
    let body = format!("25a1;r;cp=E0A0;fmt=colrv1;upm=1000;{}", b64(&bad));
    assert!(matches!(
        parse(body.as_bytes()),
        Err(ParseError::RegisterFailed {
            reason: RegisterError::MalformedPayload,
            ..
        })
    ));
}

#[test]
fn colr_rejects_trailing_garbage() {
    // Extra bytes after the CPAL slice means the sender's layout
    // doesn't match ours; reject rather than silently ignoring.
    let mut container = build_container(&[&[0x01]], &[0x00; 4], &[]);
    container.push(0xFF);
    let body = format!("25a1;r;cp=E0A0;fmt=colrv0;upm=1000;{}", b64(&container));
    assert!(matches!(
        parse(body.as_bytes()),
        Err(ParseError::RegisterFailed {
            reason: RegisterError::MalformedPayload,
            ..
        })
    ));
}

#[test]
fn colr_rejects_excessive_glyph_count() {
    // n_glyphs = MAX_COLR_GLYPHS + 1 blows the bound.
    let mut bad = Vec::new();
    bad.extend_from_slice(&1025_u16.to_be_bytes());
    // … no actual glyph bytes; parse should reject at the count.
    let body = format!("25a1;r;cp=E0A0;fmt=colrv0;upm=1000;{}", b64(&bad));
    assert!(matches!(
        parse(body.as_bytes()),
        Err(ParseError::RegisterFailed {
            reason: RegisterError::MalformedPayload,
            ..
        })
    ));
}

#[test]
fn register_defaults_reply_to_all() {
    let payload = b64(&[0x01]);
    let body = format!("25a1;r;cp=E0A0;upm=1000;{}", payload);
    match parse(body.as_bytes()).unwrap() {
        GlyphCommand::Register { reply, .. } => {
            assert_eq!(reply, ReplyMode::All);
        }
        other => panic!("expected register, got {:?}", other),
    }
}

#[test]
fn register_accepts_every_reply_level() {
    // reply=0 → None, reply=1 → All, reply=2 → ErrorsOnly.
    let payload = b64(&[0x01]);
    for (raw, expected) in [
        ("0", ReplyMode::None),
        ("1", ReplyMode::All),
        ("2", ReplyMode::ErrorsOnly),
    ] {
        let body = format!("25a1;r;cp=E0A0;reply={};upm=1000;{}", raw, payload);
        match parse(body.as_bytes()).unwrap() {
            GlyphCommand::Register { reply, .. } => {
                assert_eq!(
                    reply, expected,
                    "reply={} should map to {:?}",
                    raw, expected
                );
            }
            other => panic!("expected register, got {:?}", other),
        }
    }
}

#[test]
fn register_reply_propagates_on_parse_failure() {
    // Non-PUA cp fails validation; the reply level must propagate
    // into the error so the dispatcher can honour it consistently.
    let payload = b64(&[0x01]);
    let body = format!("25a1;r;cp=61;reply=0;upm=1000;{}", payload);
    assert_eq!(
        parse(body.as_bytes()),
        Err(ParseError::RegisterFailed {
            cp: 0x61,
            reason: RegisterError::OutOfNamespace,
            reply: ReplyMode::None,
        })
    );
}

#[test]
fn register_reply_unknown_values_fall_back_to_all() {
    // Garbage values revert to the default reply behavior so extensions do not
    // break registration.
    let payload = b64(&[0x01]);
    for bad in ["3", "true", "yes", "01", ""].iter() {
        let body = format!("25a1;r;cp=E0A0;reply={};upm=1000;{}", bad, payload);
        match parse(body.as_bytes()).unwrap() {
            GlyphCommand::Register { reply, .. } => {
                assert_eq!(
                    reply,
                    ReplyMode::All,
                    "reply={:?} should fall back to All",
                    bad
                );
            }
            other => panic!("expected register, got {:?}", other),
        }
    }
}

#[test]
fn reply_mode_emit_matrix() {
    // Sanity-check the two helpers the dispatcher relies on.
    assert!(ReplyMode::All.emit_success());
    assert!(ReplyMode::All.emit_error());
    assert!(!ReplyMode::ErrorsOnly.emit_success());
    assert!(ReplyMode::ErrorsOnly.emit_error());
    assert!(!ReplyMode::None.emit_success());
    assert!(!ReplyMode::None.emit_error());
}

#[test]
fn colr_register_respects_pua_check_before_fmt_parse() {
    // Non-PUA should still be rejected for colour formats, and the
    // error should be `out_of_namespace` (not a payload error) so
    // the client sees the same contract as fmt=glyf.
    let container = build_container(&[&[0x01]], &[0x00; 4], &[]);
    let body = format!("25a1;r;cp=61;fmt=colrv0;upm=1000;{}", b64(&container));
    assert_eq!(
        parse(body.as_bytes()),
        Err(ParseError::RegisterFailed {
            cp: 0x61,
            reason: RegisterError::OutOfNamespace,
            reply: ReplyMode::All,
        })
    );
}

#[test]
fn query_response_encodes_coverage_names() {
    assert_eq!(
        format_query_response(0xE0A0, QueryStatus::Free),
        "\x1b_25a1;q;cp=e0a0;status=\x1b\\"
    );
    assert_eq!(
        format_query_response(0xE0A0, QueryStatus::System),
        "\x1b_25a1;q;cp=e0a0;status=system\x1b\\"
    );
    assert_eq!(
        format_query_response(0xE0A0, QueryStatus::Glossary),
        "\x1b_25a1;q;cp=e0a0;status=glossary\x1b\\"
    );
    assert_eq!(
        format_query_response(0xE0A0, QueryStatus::Both),
        "\x1b_25a1;q;cp=e0a0;status=system,glossary\x1b\\"
    );
}

#[test]
fn unknown_params_are_ignored() {
    let got = parse(b"25a1;q;cp=E0A0;future=1").unwrap();
    assert_eq!(got, GlyphCommand::Query { cp: 0xE0A0 });
}
