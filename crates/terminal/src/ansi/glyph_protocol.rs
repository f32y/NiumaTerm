// Glyph Protocol parser.
//
// Protocol framing:
//   ESC _ 25a1 ; <verb> [ ; key=value ]* [ ; <payload> ] ESC \
//
// Verbs:
//   s — advertise supported payload formats; also serves as protocol
//       detection (any reply = protocol implemented)
//   q — query the state of a codepoint (status is a comma-separated
//       list of coverage names: `system`, `glossary`, both, or empty)
//   r — register a PUA codepoint with a glyph
//   c — clear one PUA codepoint or every registration in this session
//
// Payload formats (selected via `fmt=<name>` on the `r` verb):
//   glyf   — single monochrome OpenType simple-glyph outline.
//   colrv0 — up to 16 flat-color layers; each layer is an sRGBA colour
//            (or a "foreground" sentinel) plus a `glyf` outline; layers
//            composite in painter-order.
//   colrv1 — same layer model as colrv0 but each layer carries a paint:
//            solid, linear gradient, radial gradient, or foreground. No
//            affine transforms and no sweep gradients in v1.
//
// `cp` is always a single codepoint. For `r` and `c`, `cp` MUST be in
// one of the three Unicode Private Use Area ranges; otherwise the
// request is rejected with `reason=out_of_namespace`. `q` accepts any
// valid Unicode scalar value so applications can probe system-font
// coverage for codepoints they intend to register.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;

/// Protocol identifier — the literal ASCII string `"25a1"` that
/// prefixes every Glyph Protocol APC body. Terminals MUST drop APC
/// messages whose body does not begin with this identifier.
pub const GLYPH_PROTOCOL_PREFIX: &[u8] = b"25a1";

/// Upper bound on a single registered payload, post-base64-decode.
/// Capped at 64 KiB to bound decode-time allocation; larger payloads fail with
/// `payload_too_large`.
pub const MAX_PAYLOAD_BYTES: usize = 64 * 1024;

/// Payload formats this build supports, advertised in the reply to
/// the `s` verb as a comma-separated list of names. An empty slice
/// produces `fmt=` (protocol implemented but no payload format
/// accepted) — a degenerate state reserved for future negotiation,
/// never produced by this build.
pub const SUPPORTED_FORMATS: &[&str] = &["glyf", "colrv0", "colrv1"];

/// Check whether a codepoint is in any of the three Unicode Private
/// Use Areas.
#[inline]
pub fn is_pua(cp: u32) -> bool {
    (0xE000..=0xF8FF).contains(&cp)          // basic
        || (0xF_0000..=0xF_FFFD).contains(&cp)  // supplementary A
        || (0x10_0000..=0x10_FFFD).contains(&cp) // supplementary B
}

/// Parsed Glyph Protocol command, ready for dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GlyphCommand {
    /// Advertise supported payload formats. Parameter-free; doubles as
    /// the protocol-detection ping.
    Support,
    /// Query state of a single codepoint.
    Query { cp: u32 },
    /// Register a glyph at a PUA codepoint chosen by the client. The
    /// `payload` carries format-specific data (monochrome `glyf`, or a
    /// `colrv0`/`colrv1` colour container wrapping OpenType tables).
    /// The `reply` level controls which replies (if any) the
    /// dispatcher emits — see [`ReplyMode`] for the three tiers.
    Register {
        cp: u32,
        payload: GlyphPayload,
        reply: ReplyMode,
    },
    /// Clear a single PUA codepoint (`Some`) or every slot (`None`).
    Clear { cp: Option<u32> },
}

/// Upper bound on the number of glyph outlines carried in a single
/// colour payload. Keeps the glossary's decode cost bounded and sits
/// well within the 16-bit GlyphId namespace used by COLR.
const MAX_COLR_GLYPHS: u16 = 1024;

/// Payload shipped with an `r` (register) request.
///
/// `Glyf` is a single OpenType simple-glyph record, rendered in the
/// current foreground colour.
///
/// `ColrV0` and `ColrV1` share a binary container ([`ColrContainer`]) —
/// a length-prefixed array of simple-glyph outlines plus raw OpenType
/// `COLR` and `CPAL` tables. The outer variant distinguishes the COLR
/// table version the terminal should expect (v0 is layer-only, v1 is
/// the full paint graph). Reusing the OpenType binary layout means
/// applications can slice existing fonts directly; the terminal uses
/// `ttf_parser::colr::Table` to walk the paint graph and our own
/// `glyf` decoder (same as `fmt=glyf`) for the leaf outlines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GlyphPayload {
    Glyf { glyf: Vec<u8>, upm: u16 },
    ColrV0 { container: ColrContainer, upm: u16 },
    ColrV1 { container: ColrContainer, upm: u16 },
}

/// Binary container for `fmt=colrv0` and `fmt=colrv1` payloads.
///
/// Layout after base64-decode:
/// ```text
///   u16 BE  n_glyphs
///   per glyph:
///     u16 BE  glyf_len
///     glyf_len bytes  (simple-glyph, same encoding as fmt=glyf)
///   u16 BE  colr_len
///   colr_len bytes   (OpenType COLR table, v0 or v1)
///   u16 BE  cpal_len
///   cpal_len bytes   (OpenType CPAL table; may be zero-length when
///                     the COLR references only foreground / direct
///                     sRGB values in the v1 paint graph)
/// ```
///
/// Glyph IDs in the COLR table resolve to indices into `glyphs`.
/// CPAL reserves palette index `0xFFFF` for the current foreground colour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColrContainer {
    pub glyphs: Vec<Vec<u8>>,
    pub colr: Vec<u8>,
    pub cpal: Vec<u8>,
}

/// Three-level reply control for the `r` verb, selected with the
/// `reply` parameter on a register request. The values mirror the
/// parameter encoding (`reply=0` / `reply=1` / `reply=2`) so dispatchers
/// can skip a round of translation.
///
/// Fire-and-forget bulk registrations should use [`ReplyMode::None`]
/// so `status=0` ACKs don't queue in the PTY and spill to the shell
/// when the client exits. Bulk registrations that want failure
/// telemetry without the success noise should use
/// [`ReplyMode::ErrorsOnly`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReplyMode {
    /// `reply=0`: the dispatcher emits nothing for this registration.
    None,
    /// `reply=1` (default): the dispatcher emits both success (`status=0`)
    /// and failure (`status=<nonzero>`) replies. The default when
    /// `reply` is omitted or holds an unrecognised value.
    #[default]
    All,
    /// `reply=2`: the dispatcher emits only failure replies, dropping
    /// the `status=0` ACK on success. Handy for large bulk
    /// registrations that want errors surfaced without the noise of
    /// 256 ACKs on the happy path.
    ErrorsOnly,
}

impl ReplyMode {
    /// Whether a successful register should emit `status=0`.
    pub fn emit_success(self) -> bool {
        matches!(self, ReplyMode::All)
    }
    /// Whether a failed register should emit `status=<nonzero>;reason=…`.
    pub fn emit_error(self) -> bool {
        matches!(self, ReplyMode::All | ReplyMode::ErrorsOnly)
    }

    fn from_reply_param(raw: &[u8]) -> Self {
        match raw {
            b"0" => ReplyMode::None,
            b"2" => ReplyMode::ErrorsOnly,
            // `reply=1`, an unrecognised value, or an absent parameter
            // all land here. Emitting both is the forward-compatible fallback.
            _ => ReplyMode::All,
        }
    }
}

/// Query status: the set of sources covering `cp`.
/// Encoded in the response as a comma-separated list of coverage names
/// (`system`, `glossary`, both, or empty for no coverage).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryStatus {
    Free,
    System,
    Glossary,
    Both,
}

impl QueryStatus {
    /// Serialized form of the `status=` value: a comma-separated list of
    /// coverage names. `Free` returns the empty string.
    pub fn as_str(self) -> &'static str {
        match self {
            QueryStatus::Free => "",
            QueryStatus::System => "system",
            QueryStatus::Glossary => "glossary",
            QueryStatus::Both => "system,glossary",
        }
    }
}

/// Stable register-error codes encoded in protocol responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegisterError {
    OutOfNamespace,
    CompositeUnsupported,
    HintingUnsupported,
    MalformedPayload,
    PayloadTooLarge,
}

/// Error returned when the APC body is not a valid Glyph Protocol
/// message, or when protocol validation rejects the request before
/// it reaches the handler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// Body does not start with `25a1` — not our protocol; caller
    /// should fall through to other APC dispatchers.
    NotGlyphProtocol,
    /// Framing was recognised but malformed.
    Malformed(&'static str),
    /// Register rejected at parse time. Dispatcher formats this as
    /// `status=<nonzero>; reason=<code>` with the supplied `cp`,
    /// unless the original `r` request carried a `reply` level that
    /// disables error replies (see [`ReplyMode::emit_error`]).
    RegisterFailed {
        cp: u32,
        reason: RegisterError,
        reply: ReplyMode,
    },
    /// `c;cp=<hex>` where the codepoint is not in any PUA range.
    ClearOutOfNamespace,
}

/// Parse a raw APC body (minus the `ESC _` introducer and `ESC \`
/// terminator) into a [`GlyphCommand`].
pub fn parse(body: &[u8]) -> Result<GlyphCommand, ParseError> {
    if !body.starts_with(GLYPH_PROTOCOL_PREFIX) {
        return Err(ParseError::NotGlyphProtocol);
    }
    let rest = &body[GLYPH_PROTOCOL_PREFIX.len()..];
    let rest = rest
        .strip_prefix(b";")
        .ok_or(ParseError::Malformed("missing verb separator"))?;

    let (verb, rest) = split_once(rest, b';');
    let verb = trim(verb);
    if verb.len() != 1 {
        return Err(ParseError::Malformed("verb must be a single byte"));
    }
    match verb[0] {
        b's' => parse_support(rest),
        b'q' => parse_query(rest),
        b'r' => parse_register(rest),
        b'c' => parse_clear(rest),
        _ => Err(ParseError::Malformed("unknown verb")),
    }
}

fn parse_support(_rest: &[u8]) -> Result<GlyphCommand, ParseError> {
    // `s` takes no parameters. Unknown params are ignored so future
    // clients that send extra hints (e.g. a client-advertised format
    // preference) still get a valid reply from this implementation.
    Ok(GlyphCommand::Support)
}

fn parse_query(rest: &[u8]) -> Result<GlyphCommand, ParseError> {
    let params = parse_params(rest);
    let cp_raw = params
        .get("cp")
        .ok_or(ParseError::Malformed("query missing cp"))?;
    if cp_raw.contains(&b',') {
        return Err(ParseError::Malformed("cp must be a single codepoint"));
    }
    let cp = parse_hex_cp(cp_raw).ok_or(ParseError::Malformed("query cp invalid hex"))?;
    Ok(GlyphCommand::Query { cp })
}

fn parse_register(rest: &[u8]) -> Result<GlyphCommand, ParseError> {
    // Register splits control parameters from the base64 payload at
    // the LAST `;`. Base64 has no `;` so this is unambiguous.
    let (control, payload_b64) = split_last(rest, b';');
    let params = parse_params(control);

    let cp_raw = params
        .get("cp")
        .ok_or(ParseError::Malformed("register missing cp"))?;
    if cp_raw.contains(&b',') {
        return Err(ParseError::Malformed("cp must be a single codepoint"));
    }
    let cp = parse_hex_cp(cp_raw).ok_or(ParseError::Malformed("register cp invalid hex"))?;

    // Extract `reply` before any can-fail validation so every error
    // path below can honour the level. Unrecognised values fall back
    // to the default (emit both success and failure replies).
    let reply = params
        .get("reply")
        .map(|v| ReplyMode::from_reply_param(v))
        .unwrap_or_default();

    // PUA check is the protocol's security contract — reject early so
    // we don't bother decoding the payload.
    if !is_pua(cp) {
        return Err(ParseError::RegisterFailed {
            cp,
            reason: RegisterError::OutOfNamespace,
            reply,
        });
    }

    let fmt = params.get("fmt").copied().unwrap_or(b"glyf");
    if fmt != b"glyf" && fmt != b"colrv0" && fmt != b"colrv1" {
        return Err(ParseError::Malformed("register fmt unknown"));
    }

    let upm = match params.get("upm") {
        Some(raw) => parse_decimal_u16(raw).ok_or(ParseError::Malformed("register upm invalid"))?,
        None => 1000,
    };
    if upm == 0 {
        return Err(ParseError::Malformed("register upm must be non-zero"));
    }

    let payload_b64 = trim(payload_b64);
    let raw = BASE64
        .decode(payload_b64)
        .map_err(|_| ParseError::RegisterFailed {
            cp,
            reason: RegisterError::MalformedPayload,
            reply,
        })?;
    if raw.len() > MAX_PAYLOAD_BYTES {
        return Err(ParseError::RegisterFailed {
            cp,
            reason: RegisterError::PayloadTooLarge,
            reply,
        });
    }
    // Empty payload is never a valid registration: a `glyf` outline
    // needs at least the simple-glyph header bytes, and a colr
    // container needs the `n_glyphs` u16 plus per-glyph data. An
    // empty body usually means the trailing `;<base64>` segment was
    // omitted altogether (e.g. `r;cp=E000;` with no payload), which
    // would otherwise silently register an empty glyph for the slot.
    if raw.is_empty() {
        return Err(ParseError::RegisterFailed {
            cp,
            reason: RegisterError::MalformedPayload,
            reply,
        });
    }

    let payload = match fmt {
        b"glyf" => GlyphPayload::Glyf { glyf: raw, upm },
        b"colrv0" => {
            let container = parse_colr_container(&raw)
                .map_err(|reason| ParseError::RegisterFailed { cp, reason, reply })?;
            GlyphPayload::ColrV0 { container, upm }
        }
        b"colrv1" => {
            let container = parse_colr_container(&raw)
                .map_err(|reason| ParseError::RegisterFailed { cp, reason, reply })?;
            GlyphPayload::ColrV1 { container, upm }
        }
        _ => unreachable!("fmt validated above"),
    };

    Ok(GlyphCommand::Register { cp, payload, reply })
}

/// Decode a `colrv0`/`colrv1` container (see [`ColrContainer`] doc for
/// the binary layout). Validation is structural only: the OpenType COLR
/// and CPAL tables are handed off to the renderer, which parses them
/// with `ttf_parser::colr::Table` when the glyph is rasterised — that
/// way any COLR-version-specific validation lives next to the code
/// that actually interprets it.
fn parse_colr_container(data: &[u8]) -> Result<ColrContainer, RegisterError> {
    let mut cur = Cursor::new(data);

    let n_glyphs = cur.u16_be().ok_or(RegisterError::MalformedPayload)?;
    if n_glyphs == 0 || n_glyphs > MAX_COLR_GLYPHS {
        return Err(RegisterError::MalformedPayload);
    }

    let mut glyphs: Vec<Vec<u8>> = Vec::with_capacity(n_glyphs as usize);
    for _ in 0..n_glyphs {
        let glyf_len = cur.u16_be().ok_or(RegisterError::MalformedPayload)? as usize;
        let glyf = cur
            .slice(glyf_len)
            .ok_or(RegisterError::MalformedPayload)?
            .to_vec();
        glyphs.push(glyf);
    }

    let colr_len = cur.u16_be().ok_or(RegisterError::MalformedPayload)? as usize;
    if colr_len == 0 {
        return Err(RegisterError::MalformedPayload);
    }
    let colr = cur
        .slice(colr_len)
        .ok_or(RegisterError::MalformedPayload)?
        .to_vec();

    let cpal_len = cur.u16_be().ok_or(RegisterError::MalformedPayload)? as usize;
    let cpal = cur
        .slice(cpal_len)
        .ok_or(RegisterError::MalformedPayload)?
        .to_vec();

    if cur.remaining() != 0 {
        return Err(RegisterError::MalformedPayload);
    }

    Ok(ColrContainer { glyphs, colr, cpal })
}

/// Minimal big-endian byte cursor. Used by the `colrv0`/`colrv1`
/// container parser — the OpenType tables nested inside are parsed by
/// `ttf-parser` downstream, so we only need enough here to carve out
/// their byte ranges.
struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }
    fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }
    fn u16_be(&mut self) -> Option<u16> {
        if self.pos + 2 > self.data.len() {
            return None;
        }
        let hi = self.data[self.pos] as u16;
        let lo = self.data[self.pos + 1] as u16;
        self.pos += 2;
        Some((hi << 8) | lo)
    }
    fn slice(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.pos + n > self.data.len() {
            return None;
        }
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Some(s)
    }
}

fn parse_clear(rest: &[u8]) -> Result<GlyphCommand, ParseError> {
    let params = parse_params(rest);
    match params.get("cp") {
        Some(cp_raw) => {
            if cp_raw.contains(&b',') {
                return Err(ParseError::Malformed("cp must be a single codepoint"));
            }
            let cp = parse_hex_cp(cp_raw).ok_or(ParseError::Malformed("clear cp invalid hex"))?;
            if !is_pua(cp) {
                return Err(ParseError::ClearOutOfNamespace);
            }
            Ok(GlyphCommand::Clear { cp: Some(cp) })
        }
        None => Ok(GlyphCommand::Clear { cp: None }),
    }
}

/// Minimal parameter parser: semicolon-separated `key=value` pairs.
/// Keys are compared case-sensitively; unknown keys are silently kept
/// so callers can ignore extensions they do not understand.
fn parse_params(data: &[u8]) -> Params<'_> {
    let mut out = Params::default();
    for part in data.split(|&b| b == b';') {
        let part = trim(part);
        if part.is_empty() {
            continue;
        }
        if let Some(eq) = part.iter().position(|&b| b == b'=') {
            let k = trim(&part[..eq]);
            let v = trim(&part[eq + 1..]);
            out.insert(k, v);
        }
    }
    out
}

/// Hex-parse a single codepoint (no leading `0x`, up to 6 digits).
fn parse_hex_cp(raw: &[u8]) -> Option<u32> {
    let raw = trim(raw);
    if raw.is_empty() || raw.len() > 6 {
        return None;
    }
    let mut out: u32 = 0;
    for &b in raw {
        let d = match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'f' => b - b'a' + 10,
            b'A'..=b'F' => b - b'A' + 10,
            _ => return None,
        } as u32;
        out = (out << 4) | d;
    }
    if out > 0x10FFFF || (0xD800..=0xDFFF).contains(&out) {
        return None;
    }
    Some(out)
}

fn parse_decimal_u16(raw: &[u8]) -> Option<u16> {
    let raw = trim(raw);
    if raw.is_empty() {
        return None;
    }
    let mut out: u32 = 0;
    for &b in raw {
        if !b.is_ascii_digit() {
            return None;
        }
        out = out.checked_mul(10)?.checked_add((b - b'0') as u32)?;
        if out > u16::MAX as u32 {
            return None;
        }
    }
    Some(out as u16)
}

fn split_once(data: &[u8], sep: u8) -> (&[u8], &[u8]) {
    if let Some(pos) = data.iter().position(|&b| b == sep) {
        (&data[..pos], &data[pos + 1..])
    } else {
        (data, &[])
    }
}

fn split_last(data: &[u8], sep: u8) -> (&[u8], &[u8]) {
    if let Some(pos) = data.iter().rposition(|&b| b == sep) {
        (&data[..pos], &data[pos + 1..])
    } else {
        (data, &[])
    }
}

fn trim(data: &[u8]) -> &[u8] {
    let mut start = 0;
    let mut end = data.len();
    while start < end && matches!(data[start], b' ' | b'\t' | b'\r' | b'\n') {
        start += 1;
    }
    while end > start && matches!(data[end - 1], b' ' | b'\t' | b'\r' | b'\n') {
        end -= 1;
    }
    &data[start..end]
}

#[derive(Default)]
struct Params<'a> {
    entries: Vec<(&'a [u8], &'a [u8])>,
}

impl<'a> Params<'a> {
    fn insert(&mut self, k: &'a [u8], v: &'a [u8]) {
        for e in &mut self.entries {
            if e.0 == k {
                e.1 = v;
                return;
            }
        }
        self.entries.push((k, v));
    }

    fn get(&self, k: &str) -> Option<&&'a [u8]> {
        self.entries
            .iter()
            .find(|e| e.0 == k.as_bytes())
            .map(|e| &e.1)
    }
}

/// Format the reply to `q;cp=<hex>`. Public because the frontend
/// is the one that has access to both
/// `FontLibrary` and the per-route registry needed to compute the
/// status; it formats the reply itself and writes it back to the PTY.
pub fn format_query_response(cp: u32, status: QueryStatus) -> String {
    format!("\x1b_25a1;q;cp={:x};status={}\x1b\\", cp, status.as_str())
}
