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
//! `git diff --no-index`), see `tests/differential.rs`. The heuristics for
//! large inputs (cost limit, long-snake split points, which parts are then
//! diffed minimally) were derived the same way from generated inputs of
//! 30,000 to 120,000 lines; each constant was checked by changing it by one
//! step in both directions and watching the comparison fail.

/// Search cost (edit distance from either end) at which the search gives up
/// looking for the exact middle snake, unless a minimal diff was requested.
/// The limit grows with the input size (see [`cost_limit`]).
const MIN_COST_LIMIT: isize = 256;

/// Search cost above which a long snake that got far enough from its corner
/// is taken as split point (see [`Ctx::good_snake`]). The cost limit is 256
/// up to 65532 lines taking part in the search, so this only matters for
/// larger inputs.
const SNAKE_MIN_COST: isize = 256;

/// Number of equal lines a snake needs to count as long.
const SNAKE_LEN: isize = 20;

/// A long snake is taken only if its end got further from its corner than
/// this factor times the search cost.
const SNAKE_PROGRESS: isize = 4;

/// Computes which lines of `a` and `b` are changed (deleted resp. inserted).
///
/// `a` and `b` hold interned line ids; `classes` is one more than the largest
/// id.
pub(crate) fn diff(a: &[u32], b: &[u32], classes: usize, minimal: bool) -> (Vec<bool>, Vec<bool>) {
    // Small regions of large files (patience and histogram fall back to
    // Myers for them) get dense ids of their own, so that the partner
    // counts below do not cost the size of the whole files each time.
    if classes > 2 * (a.len() + b.len()) + 64 {
        let mut map: crate::lines::FastMap<u32, u32> = Default::default();
        let mut id = |x: &u32| -> u32 {
            let next = map.len() as u32;
            *map.entry(*x).or_insert(next)
        };
        let la: Vec<u32> = a.iter().map(&mut id).collect();
        let lb: Vec<u32> = b.iter().map(&mut id).collect();
        return diff(&la, &lb, map.len(), minimal);
    }
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
    let ia = select(a, ra, &count_b, &mut ca, minimal);
    let ib = select(b, rb, &count_a, &mut cb, minimal);

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

/// Cost limit of the search for `n` lines in total (the lines taking part in
/// the search, after trimming and discarding): `2 * 2^floor(log4(n + 3))`,
/// at least [`MIN_COST_LIMIT`]. The step from 256 to 512 at `n + 3 = 65536`
/// was located exactly with generated inputs of 65532 to 65537 lines.
fn cost_limit(n: usize) -> isize {
    ((2 * pow2_root(n + 3)) as isize).max(MIN_COST_LIMIT)
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
/// in a file of `len` lines: 2 below 4 lines, 4 below 16, 8 below 64, ...,
/// at most [`MAX_MANY_LIMIT`].
fn many_limit(len: usize) -> u32 {
    (2 * pow2_root(len)).min(MAX_MANY_LIMIT) as u32
}

/// Upper bound of [`many_limit`], reached at 2^18 lines. Checked with files
/// of 1.1 million lines in which a line has 1023 resp. 1024 partners.
const MAX_MANY_LIMIT: usize = 1024;

/// Classifies the lines of `x` in `range` and returns the indices that take
/// part in the search; discarded lines are flagged in `changed`.
fn select(
    x: &[u32],
    range: std::ops::Range<usize>,
    count_other: &[u32],
    changed: &mut [bool],
    minimal: bool,
) -> Vec<usize> {
    // A minimal diff keeps lines with many partners.
    let limit = if minimal {
        u32::MAX
    } else {
        many_limit(x.len())
    };
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

/// Lines looked at on each side by [`surrounded`].
const SURROUND_SCAN: usize = 100;

/// Decides whether a [`Class::Many`] line at `p` lies in a stretch of mostly
/// discarded lines: the stretches before and after it (up to the nearest
/// kept line, at most [`SURROUND_SCAN`] lines) must each contain a discarded
/// line, and the discarded lines
/// must outnumber the many-partner lines (counting two extra) by more than
/// three to one.
fn surrounded(class: &[Class], p: usize) -> bool {
    let (mut dis_before, mut many) = (0usize, 0usize);
    for &c in class[..p].iter().rev().take(SURROUND_SCAN) {
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
    for &c in class[p + 1..].iter().take(SURROUND_SCAN) {
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

/// A split point and whether the parts before and after it must be diffed
/// minimally (without the cost limit and the snake heuristic).
type Split = (usize, usize, bool, bool);

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
        let top_min = self.max_cost.is_none();
        let mut stack = vec![(0usize, self.a.len(), 0usize, self.b.len(), top_min)];
        while let Some((mut x0, mut x1, mut y0, mut y1, min)) = stack.pop() {
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
            let (xm, ym, lo, hi) = self.split(x0, x1, y0, y1, min);
            stack.push((xm, x1, ym, y1, hi));
            stack.push((x0, xm, y0, ym, lo));
        }
    }

    /// Finds a point on an optimal path from `(x0, y0)` to `(x1, y1)` that
    /// splits the box into two smaller ones. Diagonals are visited from the
    /// highest to the lowest in both directions; a step onto a diagonal
    /// comes from the neighbour that got further (from the lower diagonal on
    /// ties). End points may lie outside the box (past its right or bottom
    /// edge for the forward search, before its left or top edge for the
    /// backward one); they still take part in later steps.
    ///
    /// Unless `minimal`, the search may stop early: at the end of a long
    /// snake (see [`Ctx::good_snake`]) or, once the cost limit is reached,
    /// at the point that got furthest (see [`Ctx::give_up`]). The part on
    /// the side the search came from is then diffed minimally, the other
    /// part may stop early again. After an exact middle snake both parts are
    /// diffed minimally.
    fn split(&mut self, x0: usize, x1: usize, y0: usize, y1: usize, minimal: bool) -> Split {
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
            // Whether a snake longer than SNAKE_LEN was followed in this
            // step; only then are long snakes looked for.
            let mut got_snake = false;
            // Forward step: extend the D-paths from the start. The entries
            // just outside the visited diagonals act as unreachable.
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
                let start = if left >= up { left + 1 } else { up };
                let mut x = start;
                let mut y = x - k;
                while x >= x0 && y >= y0 && x < x1 && y < y1 && a[x as usize] == b[y as usize] {
                    x += 1;
                    y += 1;
                }
                if x - start > SNAKE_LEN {
                    got_snake = true;
                }
                self.vf[at(k)] = x;
                if odd && k >= bmin && k <= bmax && self.vb[at(k)] <= x {
                    return (x as usize, y as usize, true, true);
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
                let start = if below < right { below } else { right - 1 };
                let mut x = start;
                let mut y = x - k;
                while x > x0
                    && y > y0
                    && x <= x1
                    && y <= y1
                    && a[(x - 1) as usize] == b[(y - 1) as usize]
                {
                    x -= 1;
                    y -= 1;
                }
                if start - x > SNAKE_LEN {
                    got_snake = true;
                }
                self.vb[at(k)] = x;
                if !odd && k >= fmin && k <= fmax && x <= self.vf[at(k)] {
                    return (x as usize, y as usize, true, true);
                }
                k -= 2;
            }
            if minimal {
                continue;
            }
            if got_snake && d > SNAKE_MIN_COST {
                let found = self.good_snake(d, (x0, y0, x1, y1), (fmin, fmax), (bmin, bmax));
                if let Some(p) = found {
                    return p;
                }
            }
            if let Some(mc) = self.max_cost {
                if d >= mc {
                    return self.give_up((x0, y0, x1, y1), (fmin, fmax), (bmin, bmax));
                }
            }
        }
    }

    /// Looks for a split point at the end of a snake of at least
    /// [`SNAKE_LEN`] lines that got far from its corner: both its distances
    /// from the corner (in `x` and in `y`) must exceed `SNAKE_PROGRESS / 2`
    /// times the cost `d`. The forward end points are tried first; among
    /// several, the one with the largest smaller distance wins (the higher
    /// diagonal on ties).
    fn good_snake(
        &self,
        d: isize,
        (x0, y0, x1, y1): (isize, isize, isize, isize),
        (fmin, fmax): (isize, isize),
        (bmin, bmax): (isize, isize),
    ) -> Option<Split> {
        let at = |k: isize| (k + self.off) as usize;
        // Doubled to stay in integers: twice the smaller distance must
        // exceed SNAKE_PROGRESS * d.
        let need = SNAKE_PROGRESS * d;
        let pick = |cands: &mut dyn Iterator<Item = (isize, isize, isize)>| {
            let mut best: Option<(isize, isize, isize)> = None;
            for c in cands {
                if c.2 > need && best.is_none_or(|b| c.2 > b.2) {
                    best = Some(c);
                }
            }
            best
        };
        let equal = |x: isize, y: isize| self.a[x as usize] == self.b[y as usize];
        let mut forward = (0..=(fmax - fmin) / 2).filter_map(|i| {
            let k = fmax - 2 * i;
            let (x, y) = (self.vf[at(k)], self.vf[at(k)] - k);
            let inside = x < x1 && y < y1 && x - SNAKE_LEN >= x0 && y - SNAKE_LEN >= y0;
            let snake = inside && (1..=SNAKE_LEN).all(|s| equal(x - s, y - s));
            snake.then(|| (x, y, 2 * (x - x0).min(y - y0)))
        });
        if let Some((x, y, _)) = pick(&mut forward) {
            return Some((x as usize, y as usize, true, false));
        }
        let mut backward = (0..=(bmax - bmin) / 2).filter_map(|i| {
            let k = bmax - 2 * i;
            let (x, y) = (self.vb[at(k)], self.vb[at(k)] - k);
            let inside = x > x0 && y > y0 && x + SNAKE_LEN <= x1 && y + SNAKE_LEN <= y1;
            let snake = inside && (0..SNAKE_LEN).all(|s| equal(x + s, y + s));
            snake.then(|| (x, y, 2 * (x1 - x).min(y1 - y)))
        });
        pick(&mut backward).map(|(x, y, _)| (x as usize, y as usize, false, true))
    }

    /// The search became too expensive: split at the forward or backward
    /// end point that got furthest from its corner (largest resp. smallest
    /// `x + y`; the higher diagonal on ties), the backward one if both got
    /// equally far. End points outside the box count as the point where
    /// their diagonal leaves the box.
    fn give_up(
        &self,
        (x0, y0, x1, y1): (isize, isize, isize, isize),
        (fmin, fmax): (isize, isize),
        (bmin, bmax): (isize, isize),
    ) -> Split {
        let at = |k: isize| (k + self.off) as usize;
        let mut fbest = (isize::MIN, 0);
        let mut k = fmax;
        while k >= fmin {
            let mut x = self.vf[at(k)].min(x1);
            if x - k > y1 {
                x = y1 + k;
            }
            let sum = 2 * x - k;
            if sum > fbest.0 {
                fbest = (sum, x);
            }
            k -= 2;
        }
        let mut bbest = (isize::MAX, 0);
        let mut k = bmax;
        while k >= bmin {
            let mut x = self.vb[at(k)].max(x0);
            if x - k < y0 {
                x = y0 + k;
            }
            let sum = 2 * x - k;
            if sum < bbest.0 {
                bbest = (sum, x);
            }
            k -= 2;
        }
        if (x1 + y1) - bbest.0 < fbest.0 - (x0 + y0) {
            let (sum, x) = fbest;
            (x as usize, (sum - x) as usize, true, false)
        } else {
            let (sum, x) = bbest;
            (x as usize, (sum - x) as usize, false, true)
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
        assert_eq!(many_limit(1 << 18), 1024);
        assert_eq!(many_limit(1 << 22), 1024);
        assert_eq!(cost_limit(100), 256);
        assert_eq!(cost_limit(65532), 256);
        assert_eq!(cost_limit(65533), 512);
        assert_eq!(cost_limit(262140), 512);
        assert_eq!(cost_limit(262141), 1024);
    }
}
