//! Backend-neutral runtime model for microsandbox.
//!
//! This module defines traits and types that abstract over the platform-specific
//! VM backend (libkrun on Unix, HCS on Windows), rootfs materialization, and
//! process supervision.

mod spec;
mod traits;
mod types;
#[cfg(unix)]
pub mod unix;
#[cfg(windows)]
pub mod windows;

//--------------------------------------------------------------------------------------------------
// Exports
//--------------------------------------------------------------------------------------------------

pub use spec::*;
pub use traits::*;
pub use types::*;
