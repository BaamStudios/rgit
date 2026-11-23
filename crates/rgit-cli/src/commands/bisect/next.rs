// SPDX-License-Identifier: MIT
//! The next step of a bisection: the merge base checks
//! (git-bisect-lk2009.adoc, "Checking merge bases"), the choice of the
//! commit to test ([`rgit_revwalk::bisect`], with the skip algorithm), and
//! the result: a checkout, the first new commit, or only skipped commits.

use std::io::Write;

use rgit_hash::ObjectId;
use rgit_revwalk::bisect;

use super::{Bisect, Outcome, Terms};
use crate::messages::bisect as msg;
use crate::prelude::*;

/// The index into `m` candidates that the skip algorithm takes: a
/// pseudo-random number `p` in [0, 32768) from the classic `rand()`
/// linear congruential step seeded with `m`, biased towards 0 by its
/// square root (git-bisect-lk2009.adoc, "Skip algorithm"). The scaling
/// was matched against the `git` binary on many generated histories:
/// `m * p / 32768` in integers, then times `sqrt(p / 32768)`.
fn skip_index(m: usize) -> usize {
    if m <= 1 {
        return 0;
    }
    let seed = m as u32;
    let x = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
    let p = (x / 65_536) % 32_768;
    let scaled = m * p as usize / 32_768;
    let i = (scaled as f64 * (f64::from(p) / 32_768.0).sqrt()) as usize;
    i.min(m - 1)
}

impl Bisect<'_> {
    /// Choose and check out the next commit to test (both kinds of marks
    /// exist).
    pub fn next_step(&mut self, terms: &Terms) -> Result<Outcome, Die> {
        let (Some(bad), goods, skips) = self.marks(terms) else {
            return Ok(Outcome::Continue);
        };
        if goods.contains(&bad) {
            self.say(&msg::both_good_and_bad(&bad.to_hex(), &terms.old, &terms.new));
            return Ok(Outcome::Failed(1));
        }
        if !goods.is_empty()
            && !self.path("BISECT_ANCESTORS_OK").exists()
            && let Some(o) = self.check_merge_bases(terms, &bad, &goods, &skips)?
        {
            return Ok(o);
        }
        let names = self.names();
        let mut args: Vec<String> = Vec::new();
        if self.path("BISECT_FIRST_PARENT").exists() {
            args.push("--first-parent".into());
        }
        args.push(bad.to_hex());
        for g in &goods {
            args.push(format!("^{}", g.to_hex()));
        }
        args.push("--".into());
        args.extend(names.into_iter().filter(|n| n != "--"));
        let list = crate::history::walk_with_parents(self.ctx, &args)?;
        if list.is_empty() {
            self.flush();
            if goods.iter().any(|g| self.revs.is_ancestor(&bad, g)) {
                self.remove("BISECT_EXPECTED_REV");
                self.remove("BISECT_ANCESTORS_OK");
                eprintln!("{}", msg::not_ancestors(&terms.old, &terms.new));
                return Ok(Outcome::Failed(1));
            }
            eprintln!("{}", msg::NO_TESTABLE);
            return Ok(Outcome::Failed(4));
        }
        let ids: Vec<ObjectId> = list.iter().map(|(id, _)| *id).collect();
        let parents: std::collections::HashMap<ObjectId, Vec<ObjectId>> =
            list.into_iter().collect();
        let cands = bisect::candidates(
            &ids,
            |id| parents.get(id).cloned().unwrap_or_default(),
            self.path("BISECT_FIRST_PARENT").exists(),
        );
        let all = cands.len();
        let skipped = |i: usize| skips.contains(&cands[i].id);
        let (chosen, weight, order) = if (0..all).any(skipped) {
            // Sorted by distance; the best unless it is skipped, then one
            // of the others, biased towards the better ones.
            let full = bisect::find(&cands, true);
            let order = bisect::sorted(&cands, &full.weights);
            let top = order[0];
            let chosen = if skipped(top) {
                let filtered: Vec<usize> = order.iter().copied().filter(|&i| !skipped(i)).collect();
                if filtered.is_empty() {
                    // Every candidate, the new commit included, is skipped.
                    let sorted: Vec<ObjectId> = order.iter().map(|&i| cands[i].id).collect();
                    return self.only_skipped(terms, &sorted, &ids);
                }
                filtered[skip_index(filtered.len())]
            } else {
                top
            };
            (chosen, full.weights[top], Some(order))
        } else {
            let found = bisect::find(&cands, false);
            let Some(best) = found.best else {
                return Ok(Outcome::Continue);
            };
            (best, found.weights[best], None)
        };
        let id = cands[chosen].id;
        if id == bad {
            return match order {
                Some(order) => {
                    let sorted: Vec<ObjectId> = order.iter().map(|&i| cands[i].id).collect();
                    self.only_skipped(terms, &sorted, &ids)
                }
                None => self.found(terms, &bad),
            };
        }
        let nr = all as i64 - weight as i64 - 1;
        self.say(&msg::bisecting(nr, bisect::estimate_steps(all)));
        self.check_out(&id)
    }

    /// Check out `id` for testing (or point `BISECT_HEAD` at it).
    pub fn check_out(&mut self, id: &ObjectId) -> Result<Outcome, Die> {
        self.write_pseudo_ref("BISECT_EXPECTED_REV", id)?;
        if self.no_checkout() {
            self.write_pseudo_ref("BISECT_HEAD", id)?;
        } else {
            self.flush();
            let code = self.checkout(&id.to_hex(), true)?;
            if code != 0 {
                return Ok(Outcome::Failed(code));
            }
        }
        let line = format!("[{}] {}", id.to_hex(), self.subject(id));
        self.say(&line);
        Ok(Outcome::Continue)
    }

    /// The merge bases of the new commit and the old ones must be old: test
    /// them first. `None` when they are.
    fn check_merge_bases(
        &mut self,
        terms: &Terms,
        bad: &ObjectId,
        goods: &[ObjectId],
        skips: &[ObjectId],
    ) -> Result<Option<Outcome>, Die> {
        let bases = self
            .revs
            .with_resolver(|r| r.merge_bases_many(bad, goods))
            .map_err(|e| Die::Fatal(e.to_string()))?;
        let good_list = goods
            .iter()
            .map(ObjectId::to_hex)
            .collect::<Vec<_>>()
            .join(",");
        for mb in bases {
            if goods.contains(&mb) {
                continue;
            }
            if mb == *bad {
                self.flush();
                let expected = self
                    .read("BISECT_EXPECTED_REV")
                    .map(|s| s.trim_end().to_string());
                if expected.as_deref() == Some(bad.to_hex().as_str()) {
                    let m = msg::merge_base_is_bad(&mb.to_hex(), &terms.new, &terms.old, &good_list);
                    eprintln!("{m}");
                    return Ok(Some(Outcome::Failed(3)));
                }
                self.remove("BISECT_EXPECTED_REV");
                eprintln!("{}", msg::not_ancestors(&terms.old, &terms.new));
                return Ok(Some(Outcome::Failed(1)));
            }
            if skips.contains(&mb) {
                self.flush();
                eprintln!(
                    "warning: {}",
                    msg::skipped_merge_base(&bad.to_hex(), &good_list, &terms.new, &mb.to_hex())
                );
                continue;
            }
            self.say(msg::MERGE_BASE_MUST_BE_TESTED);
            return self.check_out(&mb).map(Some);
        }
        self.write("BISECT_ANCESTORS_OK", "")?;
        Ok(None)
    }

    /// The first new commit is `id`.
    fn found(&mut self, terms: &Terms, id: &ObjectId) -> Result<Outcome, Die> {
        let hex = id.to_hex();
        self.say(&msg::first_bad(&hex, &terms.new));
        let show = match self.show(id)? {
            Ok(text) => text,
            Err(stderr) => {
                // The diffstat could not be computed (a broken tree): its
                // error, then git's, and nothing is logged.
                self.flush();
                let _ = std::io::stderr().write_all(&stderr);
                return Err(Die::Fatal(msg::unable_to_start_show(&hex)));
            }
        };
        self.out.push_str(&show);
        let subject = self.subject(id);
        self.append_log(&format!("{}\n", msg::first_bad_log(&terms.new, &hex, &subject)))?;
        if let Some(w) = self.read("BISECT_RESET_WHEN_FOUND") {
            self.flush();
            if w.trim_end() == "found" {
                self.clean_state();
            } else {
                self.reset_to_start()?;
            }
        }
        Ok(Outcome::Found)
    }

    /// End the bisection where it started.
    pub fn reset_to_start(&mut self) -> Result<(), Die> {
        if let Some(start) = self.read("BISECT_START") {
            let start = start.trim_end_matches('\n').to_string();
            if !self.no_checkout() && self.setup.work_tree.is_some() && !start.is_empty() {
                let _ = self.checkout(&start, true)?;
            }
        }
        self.clean_state();
        Ok(())
    }

    /// Only skipped commits are left between the old and the new ones.
    /// `list` is sorted by distance, `walk` in walk order (for the log).
    fn only_skipped(
        &mut self,
        terms: &Terms,
        list: &[ObjectId],
        walk: &[ObjectId],
    ) -> Result<Outcome, Die> {
        self.say(&msg::only_skipped(&terms.new));
        for id in list {
            self.say(&id.to_hex());
        }
        self.say(msg::CANNOT_BISECT_MORE);
        let mut log = format!("{}\n", msg::ONLY_SKIPPED_LOG);
        for id in walk {
            let subject = self.subject(id);
            log.push_str(&msg::possible_first_bad_log(&terms.new, &id.to_hex(), &subject));
            log.push('\n');
        }
        self.append_log(&log)?;
        Ok(Outcome::OnlySkipped)
    }

    /// The log entry of the first new commit with its diffstat and
    /// summary; `Err` with the error output of `diff-tree` when the
    /// diffstat fails.
    fn show(&self, id: &ObjectId) -> Result<Result<String, Vec<u8>>, Die> {
        let hex = id.to_hex();
        let _ = std::io::stdout().flush();
        let head = self
            .child(&["--no-pager", "log", "-1", "--no-decorate", "--format=medium", &hex])
            .output()
            .map_err(|e| Die::Fatal(e.to_string()))?;
        let mut text = String::from_utf8_lossy(&head.stdout).into_owned();
        let parents = self
            .revs
            .with_resolver(|r| r.peel_to_commit(id).ok())
            .and_then(|_| {
                let (_, data) = self.objects.read(id)?;
                let c = rgit_object::Commit::parse(self.setup.format.object_format, &data).ok()?;
                c.parents().ok()
            })
            .unwrap_or_default();
        let base = match parents.first() {
            Some(p) => p.to_hex(),
            None => ObjectId::empty_tree(self.setup.format.object_format).to_hex(),
        };
        let stat = self
            .child(&["--no-pager", "diff-tree", "--stat", "--summary", &base, &hex])
            .output()
            .map_err(|e| Die::Fatal(e.to_string()))?;
        if !stat.status.success() {
            return Ok(Err(stat.stderr));
        }
        if !stat.stdout.is_empty() {
            text.push('\n');
            text.push_str(&String::from_utf8_lossy(&stat.stdout));
        }
        Ok(Ok(text))
    }
}

#[cfg(test)]
mod tests {
    use super::skip_index;

    #[test]
    fn skip_indices() {
        // Observed choices of the reference git for small lists.
        assert_eq!(skip_index(5), 1);
        assert_eq!(skip_index(4), 0);
        assert_eq!(skip_index(3), 0);
        assert_eq!(skip_index(2), 0);
        assert_eq!(skip_index(1), 0);
        // Where the order of the integer scaling shows (merge-heavy
        // histories, git 2.56).
        let observed = [
            (7, 3),
            (9, 3),
            (11, 5),
            (13, 6),
            (14, 0),
            (15, 8),
            (17, 10),
            (18, 1),
            (19, 12),
            (21, 14),
            (23, 16),
            (25, 19),
        ];
        for (m, i) in observed {
            assert_eq!(skip_index(m), i, "{m}");
        }
    }
}
