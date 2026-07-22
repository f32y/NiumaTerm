use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};
use gpui_component_assets::Assets;
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
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
        Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut items = Assets.list(path)?;
        items.extend(
            ProjectAssets::iter()
                .filter(|p| p.starts_with(path))
                .map(|p| SharedString::from(p.to_string())),
        );
        Ok(items)
    }
}
