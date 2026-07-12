use nmt_config::renderer::*;

#[test]
fn unknown_backend_falls_back_instead_of_failing() {
    // An old-schema value must not fail the parse (a failure at the
    // Config level would reset the entire file to defaults).
    let renderer: Renderer = toml::from_str("backend = \"Webgpu\"").unwrap();
    assert_eq!(renderer.backend, Backend::default());

    let renderer: Renderer = toml::from_str("backend = \"cpu\"").unwrap();
    assert_eq!(renderer.backend, Backend::Cpu);
}
