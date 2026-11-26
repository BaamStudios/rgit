// SPDX-License-Identifier: MIT
//! Messages of partial clones and promisor remotes, verbatim.

/// A promisor object that no promisor remote sent.
pub fn could_not_fetch(oid: &str) -> String {
    format!("could not fetch {oid} from promisor remote")
}

/// `error:` line when the repository format cannot be raised to 1.
pub fn cannot_upgrade_unknown_extension(ext: &str) -> String {
    format!("cannot upgrade repository format: unknown extension {ext}")
}

pub const LAZY_FETCH_DISABLED: &str = "lazy fetching disabled; some objects may not be available";

pub const UNABLE_TO_UPGRADE: &str = "unable to upgrade repository format to support partial clone";
