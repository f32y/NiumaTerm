//! Team identities and state independent of provider execution and presentation.

pub mod attempt;
pub mod budget;
pub mod content;
pub mod context;
pub mod discussion;
pub mod execution_slots;
pub mod identity;
pub mod member;
pub mod moderation;
pub mod room;
pub mod session;
pub mod storage;

#[cfg(test)]
mod context_tests;
#[cfg(test)]
mod tests;
