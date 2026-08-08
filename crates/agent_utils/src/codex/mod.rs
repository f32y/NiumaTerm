#[cfg(target_os = "windows")]
pub mod app_server;
pub mod hook;
#[cfg(target_os = "windows")]
pub mod update;
#[cfg(target_os = "windows")]
pub mod usage_fetcher;

/// A profile-scoped provider injected through app-server thread config.
/// The credential stays in the process environment named by `api_key_env`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderConfig {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub api_key_env: Option<String>,
}
