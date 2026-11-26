// SPDX-License-Identifier: MIT
//! Minimum-cost assignment for `range-diff`: the shortest augmenting path
//! algorithm of R. Jonker and A. Volgenant, "A shortest augmenting path
//! algorithm for dense and sparse linear assignment problems", Computing
//! 38 (1987), 325-340, <https://doi.org/10.1007/BF02278710>, written from
//! the description and pseudocode in the paper. A permissively licensed
//! (BSD-2-Clause) implementation of the same published method is
//! `lap` (<https://github.com/gatagat/lap>); no code was taken from it or
//! from anywhere else.
//!
//! The method works in four phases:
//!
//! 1. Column reduction: every column's dual value `v[j]` becomes the cost
//!    of its cheapest row, and the column is assigned to that row if the
//!    row is still free.
//! 2. Reduction transfer: a row that got exactly one column lowers that
//!    column's dual value by the reduced cost of its second best column.
//! 3. Augmenting row reduction (two rounds): a free row takes its best
//!    column, evicting the previous owner, and lowers that column's dual
//!    value by the gap between its best and second best reduced cost.
//! 4. Augmentation: each remaining free row is assigned along a shortest
//!    alternating path (a Dijkstra-like search on reduced costs), and the
//!    dual values are updated so the reduced costs stay non-negative.
//!
//! The phases and their scan orders (column reduction from the last
//! column to the first, rows and columns in index order elsewhere) are
//! kept exactly as published, because the order in which they look at
//! rows and columns decides which of several equally cheap assignments
//! is found, and `range-diff` must pair commits with tied costs the same
//! way git does. Following the published phase structure is also why
//! parts of this file resemble other implementations of the algorithm.

/// Solves the square assignment problem for `cost` (`n * n` entries, row
/// `i` at `cost[i * n..]`). Returns the column of every row.
///
/// The scans over `col[up..]` keep the bounds they started with while
/// `up` grows, as in the paper; rows and columns are indices into
/// several arrays at once.
#[allow(clippy::mut_range_bound, clippy::needless_range_loop)]
pub fn assign(n: usize, cost: &[i64]) -> Vec<usize> {
    assert_eq!(cost.len(), n * n);
    if n == 0 {
        return Vec::new();
    }
    if n == 1 {
        return vec![0];
    }
    let c = |i: usize, j: usize| cost[i * n + j];
    // `x[i]`: the column of row `i`; `y[j]`: the row of column `j`.
    let mut x: Vec<Option<usize>> = vec![None; n];
    let mut y: Vec<Option<usize>> = vec![None; n];
    // Rows that got more than one column in the column reduction.
    let mut taken_twice = vec![false; n];
    let mut v = vec![0i64; n];

    // Column reduction, last column first: every column goes to its
    // cheapest row (the first one on ties) unless that row has one.
    for j in (0..n).rev() {
        let mut imin = 0;
        let mut min = c(0, j);
        for i in 1..n {
            if c(i, j) < min {
                min = c(i, j);
                imin = i;
            }
        }
        v[j] = min;
        if x[imin].is_none() {
            x[imin] = Some(j);
            y[j] = Some(imin);
        } else {
            taken_twice[imin] = true;
            y[j] = None;
        }
    }

    // Reduction transfer: rows with exactly one column pass the slack to
    // their second best column on to the price of their column.
    let mut free: Vec<usize> = Vec::with_capacity(n);
    for i in 0..n {
        match x[i] {
            None => free.push(i),
            Some(_) if taken_twice[i] => taken_twice[i] = false,
            Some(j1) => {
                let mut min = i64::MAX;
                for j in 0..n {
                    if j != j1 && c(i, j) - v[j] < min {
                        min = c(i, j) - v[j];
                    }
                }
                v[j1] -= min;
            }
        }
    }

    // Augmenting row reduction, two rounds.
    for _ in 0..2 {
        let pending = std::mem::take(&mut free);
        let mut queue = pending;
        let mut k = 0;
        while k < queue.len() {
            let i = queue[k];
            k += 1;
            let mut u1 = c(i, 0) - v[0];
            let mut j1 = 0;
            let mut u2 = i64::MAX;
            let mut j2 = 0;
            for j in 1..n {
                let h = c(i, j) - v[j];
                if h < u2 {
                    if h >= u1 {
                        u2 = h;
                        j2 = j;
                    } else {
                        u2 = u1;
                        u1 = h;
                        j2 = j1;
                        j1 = j;
                    }
                }
            }
            let mut i0 = y[j1];
            if u1 < u2 {
                v[j1] -= u2 - u1;
            } else if i0.is_some() {
                j1 = j2;
                i0 = y[j1];
            }
            x[i] = Some(j1);
            y[j1] = Some(i);
            if let Some(i0) = i0 {
                x[i0] = None;
                if u1 < u2 {
                    // The displaced row is reduced right away.
                    k -= 1;
                    queue[k] = i0;
                } else {
                    free.push(i0);
                }
            }
        }
    }

    // Augmentation: a shortest path from every free row to a free column.
    let mut d = vec![0i64; n];
    let mut pred = vec![0usize; n];
    let mut col: Vec<usize> = (0..n).collect();
    for &free_row in &free {
        for j in 0..n {
            d[j] = c(free_row, j) - v[j];
            pred[j] = free_row;
            col[j] = j;
        }
        // `col[..low]`: done; `col[low..up]`: at the current minimum,
        // still to scan; `col[up..]`: the others.
        let mut low = 0;
        let mut up = 0;
        let mut last = 0;
        let mut min = 0i64;
        let end = 'search: loop {
            if up == low {
                last = low;
                min = d[col[up]];
                up += 1;
                for k in up..n {
                    let j = col[k];
                    let h = d[j];
                    if h <= min {
                        if h < min {
                            up = low;
                            min = h;
                        }
                        col[k] = col[up];
                        col[up] = j;
                        up += 1;
                    }
                }
                for &j in &col[low..up] {
                    if y[j].is_none() {
                        break 'search j;
                    }
                }
            }
            let j1 = col[low];
            low += 1;
            let i = y[j1].expect("scanned columns are assigned");
            let u1 = c(i, j1) - v[j1] - min;
            for k in up..n {
                let j = col[k];
                let h = c(i, j) - v[j] - u1;
                if h < d[j] {
                    d[j] = h;
                    pred[j] = i;
                    if h == min {
                        if y[j].is_none() {
                            break 'search j;
                        }
                        col[k] = col[up];
                        col[up] = j;
                        up += 1;
                    }
                }
            }
        };
        // Update the prices of the finished columns.
        for &j in &col[..last] {
            v[j] += d[j] - min;
        }
        // Flip the path.
        let mut j = end;
        loop {
            let i = pred[j];
            y[j] = Some(i);
            let next = x[i];
            x[i] = Some(j);
            if i == free_row {
                break;
            }
            j = next.expect("rows on the path are assigned");
        }
    }
    x.into_iter()
        .map(|j| j.expect("every row is assigned"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::assign;

    fn total(n: usize, cost: &[i64], x: &[usize]) -> i64 {
        (0..n).map(|i| cost[i * n + x[i]]).sum()
    }

    fn brute(n: usize, cost: &[i64]) -> i64 {
        fn go(i: usize, n: usize, cost: &[i64], used: &mut Vec<bool>) -> i64 {
            if i == n {
                return 0;
            }
            let mut best = i64::MAX;
            for j in 0..n {
                if !used[j] {
                    used[j] = true;
                    let r = go(i + 1, n, cost, used);
                    used[j] = false;
                    best = best.min(r.saturating_add(cost[i * n + j]));
                }
            }
            best
        }
        go(0, n, cost, &mut vec![false; n])
    }

    /// Reference: the O(n^3) Hungarian method with potentials.
    fn hungarian(n: usize, cost: &[i64]) -> i64 {
        let inf = i64::MAX / 4;
        let mut u = vec![0i64; n + 1];
        let mut v = vec![0i64; n + 1];
        let mut p = vec![0usize; n + 1];
        let mut way = vec![0usize; n + 1];
        for i in 1..=n {
            p[0] = i;
            let mut j0 = 0;
            let mut minv = vec![inf; n + 1];
            let mut used = vec![false; n + 1];
            loop {
                used[j0] = true;
                let i0 = p[j0];
                let mut delta = inf;
                let mut j1 = 0;
                for j in 1..=n {
                    if !used[j] {
                        let cur = cost[(i0 - 1) * n + j - 1] - u[i0] - v[j];
                        if cur < minv[j] {
                            minv[j] = cur;
                            way[j] = j0;
                        }
                        if minv[j] < delta {
                            delta = minv[j];
                            j1 = j;
                        }
                    }
                }
                for j in 0..=n {
                    if used[j] {
                        u[p[j]] += delta;
                        v[j] -= delta;
                    } else {
                        minv[j] -= delta;
                    }
                }
                j0 = j1;
                if p[j0] == 0 {
                    break;
                }
            }
            loop {
                let j1 = way[j0];
                p[j0] = p[j1];
                j0 = j1;
                if j0 == 0 {
                    break;
                }
            }
        }
        (1..=n).map(|j| cost[(p[j] - 1) * n + j - 1]).sum()
    }

    #[test]
    fn finds_minimum_large() {
        let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = |m: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % m) as i64
        };
        for n in [7, 10, 20, 50, 120] {
            for _ in 0..40 {
                let range = 1 + next(5) as u64;
                let cost: Vec<i64> = (0..n * n).map(|_| next(range)).collect();
                let x = assign(n, &cost);
                let mut seen = vec![false; n];
                for &j in &x {
                    assert!(!seen[j]);
                    seen[j] = true;
                }
                assert_eq!(total(n, &cost, &x), hungarian(n, &cost), "n={n}");
            }
        }
    }

    #[test]
    fn finds_minimum() {
        // A small deterministic generator.
        let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
        let mut next = |m: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % m) as i64
        };
        for n in 1..=6 {
            for _ in 0..300 {
                let range = 1 + next(20) as u64;
                let cost: Vec<i64> = (0..n * n).map(|_| next(range)).collect();
                let x = assign(n, &cost);
                let mut seen = vec![false; n];
                for &j in &x {
                    assert!(!seen[j]);
                    seen[j] = true;
                }
                assert_eq!(total(n, &cost, &x), brute(n, &cost), "{cost:?}");
            }
        }
    }
}
