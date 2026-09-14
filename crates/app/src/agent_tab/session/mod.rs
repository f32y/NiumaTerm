pub use nmt_agent::session::RecoveryIdentity;
pub use nmt_agent::session::lifecycle::{
    RecoveryReadiness, RecoverySnapshot, RestorationReadiness,
};

pub(super) use nmt_agent::session::Backend;
pub(super) use nmt_agent::session::lifecycle::{Status, UpdateSuspension};
pub(super) use nmt_agent::session::restore::directories_match;
#[cfg(test)]
pub(super) use nmt_agent::session::test_support::TestBackend;

pub(super) mod errors;
pub(super) mod history;
pub(super) mod prompts;
pub(super) mod turn;

#[cfg(test)]
mod tests;

pub(super) fn directory_label(cwd: &str) -> String {
    let parts: Vec<&str> = cwd
        .trim_end_matches(['/', '\\'])
        .split(['/', '\\'])
        .filter(|part| !part.is_empty())
        .collect();

    match parts.len() {
        0 => cwd.to_string(),
        1 => parts[0].to_string(),
        length => format!("{}/{}", parts[length - 2], parts[length - 1]),
    }
}
