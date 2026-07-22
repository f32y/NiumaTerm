use nmt_config::renderer::*;
use toml::from_str;

#[test]
fn unknown_backend_falls_back_instead_of_failing() {
    // An old-schema value must not fail the parse (a failure at the
    // Config level would reset the entire file to defaults).
    let renderer: Renderer = from_str("backend = \"Webgpu\"").unwrap();
    assert_eq!(renderer.backend, Backend::default());

    let renderer: Renderer = from_str("backend = \"cpu\"").unwrap();
    assert_eq!(renderer.backend, Backend::Cpu);
}
