rust_i18n::i18n!("locales", fallback = "en");

// The translation macro reads outside Rust's dependency tracking. These inputs
// let compiler caches invalidate this target when either catalog changes.
const _: (&str, &str) = (
    include_str!("../../../assets/i18n/en.toml"),
    include_str!("../../../assets/i18n/zh-CN.toml"),
);
