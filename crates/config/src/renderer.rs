use std::fmt::{self, Display};

use serde::{Deserialize, Deserializer, Serialize};
use tracing::warn;

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct Renderer {
    #[serde(default = "Backend::default", skip_serializing)]
    pub backend: Backend,
    #[serde(default = "bool::default", rename = "disable-unfocused-render")]
    pub disable_unfocused_render: bool,
    #[serde(
        default = "default_disable_occluded_render",
        rename = "disable-occluded-render"
    )]
    pub disable_occluded_render: bool,
    #[serde(default = "RendererStategy::default")]
    pub strategy: RendererStategy,
    /// Use the CPU rasterizer (tiny-skia) instead of the GPU pipeline.
    /// Experimental. v1 supports solid quads + glyphs only; image
    /// overlays, advanced underline styles, and corner radii
    /// are not yet implemented on the CPU path.
    #[serde(default = "default_use_cpu", rename = "use-cpu")]
    pub use_cpu: bool,
}

fn default_use_cpu() -> bool {
    false
}

fn default_disable_occluded_render() -> bool {
    false
}

#[derive(Default, Debug, Clone, PartialEq, Deserialize, Serialize)]
pub enum RendererStategy {
    #[default]
    #[serde(alias = "events")]
    Events,
    #[serde(alias = "game")]
    Game,
}

impl RendererStategy {
    #[inline]
    pub fn is_game(&self) -> bool {
        self == &RendererStategy::Game
    }

    #[inline]
    pub fn is_event_based(&self) -> bool {
        self == &RendererStategy::Events
    }
}

#[allow(clippy::derivable_impls)]
impl Default for Renderer {
    fn default() -> Renderer {
        Renderer {
            backend: Backend::default(),
            disable_unfocused_render: false,
            disable_occluded_render: default_disable_occluded_render(),
            strategy: RendererStategy::Events,
            use_cpu: default_use_cpu(),
        }
    }
}

#[derive(Debug, Serialize, Clone, PartialEq)]
pub enum Backend {
    /// Native Direct3D 12 (Windows only).
    #[cfg(windows)]
    D3D12,
    /// Software rasterizer.
    Cpu,
}

/// Unknown values (e.g. `"Webgpu"` from an older config schema) fall back to
/// the default backend instead of failing the whole config file parse, which
/// would silently reset every setting to defaults.
impl<'de> Deserialize<'de> for Backend {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(match value.as_str() {
            #[cfg(windows)]
            "D3D12" | "d3d12" | "dx12" => Backend::D3D12,
            "Cpu" | "cpu" => Backend::Cpu,
            other => {
                warn!("unknown renderer backend {other:?}, using default");
                Backend::default()
            }
        })
    }
}

impl Default for Backend {
    fn default() -> Self {
        #[cfg(windows)]
        {
            Backend::D3D12
        }

        #[cfg(not(windows))]
        {
            Backend::Cpu
        }
    }
}

impl Display for Backend {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            #[cfg(windows)]
            Backend::D3D12 => write!(f, "D3D12"),
            Backend::Cpu => write!(f, "Cpu"),
        }
    }
}
