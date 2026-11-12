// SPDX-License-Identifier: MIT

//! Myers' O(ND) difference algorithm, linear space variant.
//!
//! Based on E. W. Myers, "An O(ND) Difference Algorithm and Its Variations",
//! Algorithmica 1 (1986), section 4b: the "middle snake" of an optimal edit
//! path is found by running the greedy forward and reverse searches at the
//! same time; the problem is then split there and both halves are solved
//! recursively.
//!
//! The details that decide *which* of several shortest edit scripts is found
//! (scan direction over the diagonals, tie breaks, where the split point
//! lies), the preprocessing of lines and the cost limit for large inputs are
//! not specified by git's documentation. They were derived from differential
//! tests against the `git` binary (randomized inputs, compared with
//! `git diff --no-index`), see `tests/differential.rs`.

/// Search cost (edit distance from either end) at which the search gives up
/// looking for the exact middle snake, unless a minimal diff was requested.
/// The limit grows with the input size (see [`cost_limit`]).
const MIN_COST_LIMIT: isize = 256;

/// Computes which lines of `a` and `b` are changed (deleted resp. inserted).
///
/// `a` and `b` hold interned line ids; `classes` is one more than the largest
/// id.
pub(crate) fn diff(a: &[u32], b: &[u32], classes: usize, minimal: bool) -> (Vec<bool>, Vec<bool>) {
    let mut ca = vec![false; a.len()];
    let mut cb = vec![false; b.len()];

    // Common leading and trailing lines are never part of the search.
    let mut pre = 0;
    while pre < a.len() && pre < b.len() && a[pre] == b[pre] {
        pre += 1;
    }
    let mut suf = 0;
    while suf < a.len() - pre && suf < b.len() - pre && a[a.len() - 1 - suf] == b[b.len() - 1 - suf]
    {
        suf += 1;
    }
    let ra = pre..a.len() - suf;
    let rb = pre..b.len() - suf;

    // Partner counts are taken over the whole files.
    let mut count_a = vec![0u32; classes];
    let mut count_b = vec![0u32; classes];
    for &x in a {
        count_a[x as usize] += 1;
    }
    for &x in b {
        count_b[x as usize] += 1;
    }
    let ia = select(a, ra, &count_b, &mut ca);
    let ib = select(b, rb, &count_a, &mut cb);

    let sa: Vec<u32> = ia.iter().map(|&i| a[i]).collect();
    let sb: Vec<u32> = ib.iter().map(|&j| b[j]).collect();
    let mut fa = vec![false; sa.len()];
    let mut fb = vec![false; sb.len()];
    let mut ctx = Ctx::new(&sa, &sb);
    if !minimal {
        ctx.max_cost = Some(cost_limit(sa.len() + sb.len()));
    }
    ctx.run(&mut fa, &mut fb);
    for (p, &i) in ia.iter().enumerate() {
        if fa[p] {
            ca[i] = true;
        }
    }
    for (p, &j) in ib.iter().enumerate() {
        if fb[p] {
            cb[j] = true;
        }
    }
    (ca, cb)
}

/// `2^floor(log4(n))` (1 for `n < 4`): a cheap stand-in for a square root.
fn pow2_root(n: usize) -> usize {
    let mut r = 1usize;
    let mut sq = 4usize;
    while sq <= n {
        r *= 2;
        sq = sq.saturating_mul(4);
    }
    r
}

/// Cost limit of the search for `n` lines in total.
fn cost_limit(n: usize) -> isize {
    // Only the lower bound has been confirmed against git (inputs of up to
    // 1.2 million lines); the growth term is a guess that matters only for
    // more than a million lines.
    (pow2_root(n) as isize).max(MIN_COST_LIMIT)
}

/// Line classes before the search.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    /// Has a partner on the other side; takes part in the search.
    Keep,
    /// No partner on the other side: always changed.
    Discard,
    /// Has very many partners; dropped if surrounded by discarded lines.
    Many,
}

/// Number of partner lines from which on a line counts as [`Class::Many`]
/// in a file of `len` lines: 2 below 4 lines, 4 below 16, 8 below 64, ...
fn many_limit(len: usize) -> u32 {
    (2 * pow2_root(len)) as u32
}

/// Classifies the lines of `x` in `range` and returns the indices that take
/// part in the search; discarded lines are flagged in `changed`.
fn select(
    x: &[u32],
    range: std::ops::Range<usize>,
    count_other: &[u32],
    changed: &mut [bool],
) -> Vec<usize> {
    let limit = many_limit(x.len());
    let class: Vec<Class> = x[range.clone()]
        .iter()
        .map(|&id| {
            let n = count_other[id as usize];
            if n == 0 {
                Class::Discard
            } else if n >= limit {
                Class::Many
            } else {
                Class::Keep
            }
        })
        .collect();
    let mut keep = Vec::with_capacity(class.len());
    for (p, &c) in class.iter().enumerate() {
        let drop = match c {
            Class::Keep => false,
            Class::Discard => true,
            Class::Many => surrounded(&class, p),
        };
        if drop {
            changed[range.start + p] = true;
        } else {
            keep.push(range.start + p);
        }
    }
    keep
}

/// Decides whether a [`Class::Many`] line at `p` lies in a stretch of mostly
/// discarded lines: the stretches before and after it (up to the nearest
/// kept line) must each contain a discarded line, and the discarded lines
/// must outnumber the many-partner lines (counting two extra) by more than
/// three to one.
fn surrounded(class: &[Class], p: usize) -> bool {
    let (mut dis_before, mut many) = (0usize, 0usize);
    for &c in class[..p].iter().rev() {
        match c {
            Class::Keep => break,
            Class::Discard => dis_before += 1,
            Class::Many => many += 1,
        }
    }
    if dis_before == 0 {
        return false;
    }
    let mut dis_after = 0usize;
    for &c in &class[p + 1..] {
        match c {
            Class::Keep => break,
            Class::Discard => dis_after += 1,
            Class::Many => many += 1,
        }
    }
    if dis_after == 0 {
        return false;
    }
    dis_before + dis_after > 3 * (many + 2)
}

/// Marks an unused forward entry.
const NEG: isize = isize::MIN / 4;
/// Marks an unused backward entry.
const POS: isize = isize::MAX / 4;

struct Ctx<'a> {
    a: &'a [u32],
    b: &'a [u32],
    /// Forward and backward furthest-reaching x per diagonal, indexed by
    /// `k + off` where `k = x - y`.
    vf: Vec<isize>,
    vb: Vec<isize>,
    off: isize,
    /// Search cost after which the exact middle snake is given up for a
    /// good-enough split point (`None` for minimal diffs).
    max_cost: Option<isize>,
}

impl<'a> Ctx<'a> {
    fn new(a: &'a [u32], b: &'a [u32]) -> Self {
        let size = a.len() + b.len() + 3;
        Ctx {
            a,
            b,
            vf: vec![0; size],
            vb: vec![0; size],
            off: b.len() as isize + 1,
            max_cost: None,
        }
    }

    fn run(&mut self, fa: &mut [bool], fb: &mut [bool]) {
        let mut stack = vec![(0usize, self.a.len(), 0usize, self.b.len())];
        while let Some((mut x0, mut x1, mut y0, mut y1)) = stack.pop() {
            while x0 < x1 && y0 < y1 && self.a[x0] == self.b[y0] {
                x0 += 1;
                y0 += 1;
            }
            while x0 < x1 && y0 < y1 && self.a[x1 - 1] == self.b[y1 - 1] {
                x1 -= 1;
                y1 -= 1;
            }
            if x0 == x1 {
                fb[y0..y1].iter_mut().for_each(|f| *f = true);
                continue;
            }
            if y0 == y1 {
                fa[x0..x1].iter_mut().for_each(|f| *f = true);
                continue;
            }
            let (xm, ym) = self.split(x0, x1, y0, y1);
            stack.push((xm, x1, ym, y1));
            stack.push((x0, xm, y0, ym));
        }
    }

    /// Finds a point on an optimal path from `(x0, y0)` to `(x1, y1)` that
    /// splits the box into two smaller ones. Diagonals are visited from the
    /// highest to the lowest in both directions.
    fn split(&mut self, x0: usize, x1: usize, y0: usize, y1: usize) -> (usize, usize) {
        let (x0, x1, y0, y1) = (x0 as isize, x1 as isize, y0 as isize, y1 as isize);
        let a = self.a;
        let b = self.b;
        let off = self.off;
        let at = |k: isize| (k + off) as usize;
        let kmin = x0 - y1;
        let kmax = x1 - y0;
        let fmid = x0 - y0;
        let bmid = x1 - y1;
        let odd = (fmid - bmid) & 1 != 0;
        let (mut fmin, mut fmax) = (fmid, fmid);
        let (mut bmin, mut bmax) = (bmid, bmid);
        self.vf[at(fmid)] = x0;
        self.vb[at(bmid)] = x1;
        let mut d = 0isize;
        loop {
            d += 1;
            // Forward step: extend the D-paths from the start.
            if fmin > kmin {
                fmin -= 1;
                self.vf[at(fmin - 1)] = NEG;
            } else {
                fmin += 1;
            }
            if fmax < kmax {
                fmax += 1;
                self.vf[at(fmax + 1)] = NEG;
            } else {
                fmax -= 1;
            }
            let mut k = fmax;
            while k >= fmin {
                let left = self.vf[at(k - 1)];
                let up = self.vf[at(k + 1)];
                // Right from diagonal k-1 (deletion) or down from k+1
                // (insertion); a deletion wins a tie.
                let right_ok = left != NEG && left < x1;
                let down_ok = up != NEG && up - (k + 1) < y1;
                let start = match (right_ok, down_ok) {
                    (true, true) => (left + 1).max(up),
                    (true, false) => left + 1,
                    (false, true) => up,
                    (false, false) => NEG,
                };
                if start == NEG {
                    // Only on the box edge: nothing new on this diagonal.
                    self.vf[at(k)] = NEG;
                    k -= 2;
                    continue;
                }
                let mut x = start;
                let mut y = x - k;
                while x < x1 && y < y1 && a[x as usize] == b[y as usize] {
                    x += 1;
                    y += 1;
                }
                self.vf[at(k)] = x;
                if odd && k >= bmin && k <= bmax && self.vb[at(k)] <= x {
                    return (x as usize, y as usize);
                }
                k -= 2;
            }
            // Backward step: extend the D-paths from the end.
            if bmin > kmin {
                bmin -= 1;
                self.vb[at(bmin - 1)] = POS;
            } else {
                bmin += 1;
            }
            if bmax < kmax {
                bmax += 1;
                self.vb[at(bmax + 1)] = POS;
            } else {
                bmax -= 1;
            }
            let mut k = bmax;
            while k >= bmin {
                let below = self.vb[at(k - 1)];
                let right = self.vb[at(k + 1)];
                // Up from diagonal k-1 (keeps x) or left from k+1.
                let up_ok = below != POS && below - (k - 1) > y0;
                let left_ok = right != POS && right > x0;
                let start = match (up_ok, left_ok) {
                    (true, true) => (right - 1).min(below),
                    (true, false) => below,
                    (false, true) => right - 1,
                    (false, false) => POS,
                };
                if start == POS {
                    self.vb[at(k)] = POS;
                    k -= 2;
                    continue;
                }
                let mut x = start;
                let mut y = x - k;
                while x > x0 && y > y0 && a[(x - 1) as usize] == b[(y - 1) as usize] {
                    x -= 1;
                    y -= 1;
                }
                self.vb[at(k)] = x;
                if !odd && k >= fmin && k <= fmax && self.vf[at(k)] >= x {
                    return (x as usize, y as usize);
                }
                k -= 2;
            }
            if let Some(mc) = self.max_cost {
                if d >= mc {
                    return self.give_up((x0, y0, x1, y1), (fmin, fmax), (bmin, bmax));
                }
            }
        }
    }

    /// The search became too expensive: split at the forward or backward
    /// end point that got furthest from its corner (the backward one if both
    /// got equally far).
    fn give_up(
        &self,
        (x0, y0, x1, y1): (isize, isize, isize, isize),
        (fmin, fmax): (isize, isize),
        (bmin, bmax): (isize, isize),
    ) -> (usize, usize) {
        let at = |k: isize| (k + self.off) as usize;
        // Forward: largest x + y; on ties the higher diagonal.
        let mut fbest: Option<(isize, isize)> = None;
        let mut k = fmax;
        while k >= fmin {
            let x = self.vf[at(k)];
            if x != NEG && fbest.is_none_or(|(bx, bk)| 2 * x - k > 2 * bx - bk) {
                fbest = Some((x, k));
            }
            k -= 2;
        }
        // Backward: smallest x + y; on ties the higher diagonal.
        let mut bbest: Option<(isize, isize)> = None;
        let mut k = bmax;
        while k >= bmin {
            let x = self.vb[at(k)];
            if x != POS && bbest.is_none_or(|(bx, bk)| 2 * x - k < 2 * bx - bk) {
                bbest = Some((x, k));
            }
            k -= 2;
        }
        let pick = |(x, k): (isize, isize)| (x as usize, (x - k) as usize);
        match (fbest, bbest) {
            (Some(f), Some(b)) => {
                let fprog = (2 * f.0 - f.1) - (x0 + y0);
                let bprog = (x1 + y1) - (2 * b.0 - b.1);
                if fprog > bprog { pick(f) } else { pick(b) }
            }
            (Some(f), None) => pick(f),
            (None, Some(b)) => pick(b),
            (None, None) => unreachable!("search without any reachable point"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits() {
        assert_eq!(many_limit(3), 2);
        assert_eq!(many_limit(9), 4);
        assert_eq!(many_limit(16), 8);
        assert_eq!(many_limit(63), 8);
        assert_eq!(many_limit(64), 16);
        assert_eq!(many_limit(1024), 64);
        assert_eq!(cost_limit(100), 256);
    }
}
