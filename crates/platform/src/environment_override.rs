/// Find the last child override using the operating system's name comparison.
pub fn override_value<'a>(entries: &'a [(String, String)], target: &str) -> Option<&'a str> {
    entries
        .iter()
        .rev()
        .find(|(name, _)| {
            if cfg!(windows) {
                name.eq_ignore_ascii_case(target)
            } else {
                name == target
            }
        })
        .map(|(_, value)| value.as_str())
}

#[cfg(test)]
mod tests {
    use crate::environment_override::override_value;

    #[test]
    fn final_override_respects_native_name_comparison() {
        let entries = [
            ("Path".into(), "first".into()),
            ("Path".into(), "second".into()),
            ("PATH".into(), "upper".into()),
        ];

        let expected = if cfg!(windows) { "upper" } else { "second" };

        assert_eq!(override_value(&entries, "Path"), Some(expected));
        assert_eq!(override_value(&entries, "PATH"), Some("upper"));
        assert_eq!(override_value(&entries, "missing"), None);
    }
}
