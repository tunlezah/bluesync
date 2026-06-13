//! System-effects boundary: narrow traits the integration logic depends on,
//! with real (std) impls and test fakes. Keeping side effects behind these
//! traits is what makes the orchestration logic unit-testable without root,
//! system mutation, or a running audio/BlueZ stack.

pub mod command;
pub mod fs;
pub mod process;
pub mod supervisor;

#[cfg(test)]
pub mod testing;
