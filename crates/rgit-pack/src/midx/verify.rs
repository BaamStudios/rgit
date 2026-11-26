// SPDX-License-Identifier: MIT

//! `git multi-pack-index verify` (also run by `git fsck`).
//!
//! The checks and their order follow what the git binary reports for
//! corrupt files (t5319): the checksum, whether every pack loads, the order
//! of the ids, the pack-int-ids, and every offset against the pack's own
//! index.

use std::path::Path;

use rgit_hash::HashKind;
use rgit_progress::Progress;

use super::messages as m;
use super::{MidxChain, MultiPackIndex, OpenError};
use crate::PackIndex;

/// Result of [`verify`].
#[derive(Debug, Default)]
pub struct VerifyReport {
    /// Lines to print as `error: <line>` (problems loading the file).
    pub load_errors: Vec<String>,
    /// Report lines, printed as they are (git prints them without prefix).
    pub problems: Vec<String>,
}

impl VerifyReport {
    /// Whether verification failed.
    pub fn failed(&self) -> bool {
        !self.load_errors.is_empty() || !self.problems.is_empty()
    }
}

/// Verify the MIDX of `pack_dir`. A missing MIDX verifies fine.
///
/// `Err` is fatal (git dies with that message). Problems are printed to
/// standard error as they are found, and also returned.
pub fn verify(pack_dir: &Path, kind: HashKind, progress: bool) -> Result<VerifyReport, OpenError> {
    let mut report = VerifyReport::default();
    let chain = MidxChain::open(pack_dir, kind, &mut report.load_errors);
    let chain = match chain {
        Ok(c) => c,
        Err(e) => {
            for l in &report.load_errors {
                eprintln!("error: {l}");
            }
            return Err(e);
        }
    };
    let Some(chain) = chain else {
        if !report.load_errors.is_empty() {
            for e in &report.load_errors {
                eprintln!("error: {e}");
            }
            report.load_errors.push(m::EXISTS_BUT_UNPARSABLE.into());
            eprintln!("error: {}", m::EXISTS_BUT_UNPARSABLE);
        }
        return Ok(report);
    };
    let mut say = |r: &mut VerifyReport, line: String| {
        eprintln!("{line}");
        r.problems.push(line);
    };

    if chain.is_incremental() {
        let total: u64 = chain
            .layers()
            .iter()
            .map(|l| u64::from(l.num_packs()))
            .sum();
        let mut p = Progress::start(m::PROGRESS_REFERENCED_PACKS, total, progress);
        for l in chain.layers() {
            for _ in l.pack_names() {
                p.inc();
            }
        }
        p.stop();
    }
    for layer in chain.layers() {
        verify_layer(pack_dir, kind, layer, progress, &mut report, &mut say)?;
    }
    Ok(report)
}

type Say<'a> = dyn FnMut(&mut VerifyReport, String) + 'a;

fn verify_layer(
    pack_dir: &Path,
    kind: HashKind,
    midx: &MultiPackIndex,
    progress: bool,
    report: &mut VerifyReport,
    say: &mut Say<'_>,
) -> Result<(), OpenError> {
    if !midx.checksum_ok() {
        say(report, m::INCORRECT_CHECKSUM.into());
    }
    // Load every pack's index.
    let mut indexes: Vec<Option<PackIndex>> = Vec::with_capacity(midx.num_packs() as usize);
    for (i, name) in midx.pack_names().iter().enumerate() {
        let idx = pack_dir.join(name);
        let loaded = if idx.with_extension("pack").exists() {
            PackIndex::open(&idx, kind).ok()
        } else {
            None
        };
        if loaded.is_none() {
            say(report, m::failed_to_load_pack(i as u32));
        }
        indexes.push(loaded);
    }
    let n = midx.len();
    if n == 0 {
        say(report, m::NO_OID.into());
        return Ok(());
    }

    // git redraws these meters only at the end (and the sorting one at
    // its start).
    let mut p = Progress::start(m::PROGRESS_OID_ORDER, u64::from(n - 1), progress);
    for i in 0..n - 1 {
        let a = midx.id_at(i);
        let b = midx.id_at(i + 1);
        if a >= b {
            say(report, m::oid_lookup_order(i, &a, &b));
        }
    }
    p.set(u64::from(n - 1));
    p.stop();

    let mut p = Progress::start(m::PROGRESS_SORTING, u64::from(n), progress);
    p.set(0);
    let mut order: Vec<(u32, u32)> = Vec::with_capacity(n as usize);
    for i in 0..n {
        let pack = midx.pack_id(i).map_err(OpenError)?;
        order.push((pack, i));
    }
    order.sort();
    p.set(u64::from(n));
    p.stop();

    let mut p = Progress::start(m::PROGRESS_OFFSETS, u64::from(n), progress);
    for (pack, i) in order {
        let id = midx.id_at(i);
        let midx_off = midx.offset(i).map_err(OpenError)?;
        let found = indexes[pack as usize]
            .as_ref()
            .and_then(|idx| idx.lookup(&id).and_then(|pos| idx.offset_at(pos).ok()));
        match found {
            None => say(report, m::failed_pack_entry(i, &id)),
            Some(off) if off != midx_off => {
                say(report, m::incorrect_offset(i, &id, midx_off, off));
            }
            Some(_) => {}
        }
    }
    p.set(u64::from(n));
    p.stop();
    Ok(())
}
