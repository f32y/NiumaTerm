use nmt_input::bracket_paste;

pub(super) fn paste_payload(text: &str, bracketed: bool) -> Option<Vec<u8>> {
    if text.is_empty() {
        return None;
    }

    let mut body = text.replace("\r\n", "\r").replace('\n', "\r");

    if bracketed {
        body = body.replace("\x1b[201~", "");
    }

    Some(bracket_paste(body.as_bytes(), bracketed))
}
