//! Asset source for the GPUI shell: serves the project's own icons
//! (`<workspace>/assets/icons/*.svg`) and falls back to gpui-component's
//! bundled icons for everything else.

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

/// The project's icons, embedded at build time.
#[derive(rust_embed::RustEmbed)]
#[folder = "$CARGO_MANIFEST_DIR/../../assets"]
#[include = "icons/**/*.svg"]
struct ProjectAssets;

/// Registered with the `Application` so `svg().path(..)` resolves both project
/// icons and gpui-component icons.
pub(crate) struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(file) = ProjectAssets::get(path) {
            return Ok(Some(file.data));
        }
        gpui_component_assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut items = gpui_component_assets::Assets.list(path)?;
        items.extend(
            ProjectAssets::iter()
                .filter(|p| p.starts_with(path))
                .map(|p| SharedString::from(p.to_string())),
        );
        Ok(items)
    }
}
