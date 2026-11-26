// SPDX-License-Identifier: MIT
//! Cancelling a running operation (ADR 0005, "Cancellation").

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::{Error, Result};

/// A token that stops an operation from another thread.
///
/// Clone it, hand one copy to the operation through its options and keep
/// the other; [`Cancel::cancel`] makes the operation return
/// [`Error::Cancelled`] at its next check (between files of a status,
/// between commits of a log). Nothing is left half done: operations that
/// write take their locks with guards that are released on the way out.
///
/// ```
/// let cancel = rgit::Cancel::new();
/// let for_worker = cancel.clone();
/// assert!(!for_worker.is_cancelled());
/// cancel.cancel();
/// assert!(for_worker.is_cancelled());
/// ```
#[derive(Debug, Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    /// A token that is not cancelled.
    #[must_use]
    pub fn new() -> Cancel {
        Cancel::default()
    }

    /// Ask every operation that holds a copy of this token to stop.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    /// Whether [`Cancel::cancel`] was called.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    /// `Err(Error::Cancelled)` once the token is cancelled.
    pub(crate) fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }
}
