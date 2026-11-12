// SPDX-License-Identifier: MIT
//! The `agent=` capability value.
//!
//! The version string and the agent live in `rgit-version`, the single
//! source shared with `rgit version`; this module re-exports them for
//! protocol code.

pub use rgit_version::{GIT_COMPAT_VERSION, agent, agent_from, redact, sysname, version};
