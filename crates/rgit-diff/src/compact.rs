// SPDX-License-Identifier: MIT

//! Post-processing of an edit script: sliding groups of changed lines.
//!
//! A run of changed lines can often be moved up or down without changing the
//! meaning of the diff (if the line before the run equals the last line of the
//! run, the run can move up by one, and symmetrically down). Moving runs can
//! also join them with neighbouring runs. Each run is first moved as far up
//! and then as far down as possible (joining neighbours on the way), and is
//! then placed:
//!
//! * next to a change on the other side if such a position exists, so that a
//!   deletion and an insertion appear together, otherwise
//! * at the position chosen by the indent heuristic (if enabled), otherwise
//! * at the lowest position.

use crate::indent;

struct Side<'a> {
    ids: &'a [u32],
    lines: &'a [&'a [u8]],
    changed: &'a mut [bool],
}

/// A group of changed lines `[start, end)` on one side; empty groups sit
/// between two unchanged lines.
#[derive(Clone, Copy, Debug)]
struct Group {
    start: usize,
    end: usize,
}

impl Group {
    fn first(changed: &[bool]) -> Group {
        let mut end = 0;
        while end < changed.len() && changed[end] {
            end += 1;
        }
        Group { start: 0, end }
    }

    /// Moves to the next group; returns false at the end.
    fn next(&mut self, changed: &[bool]) -> bool {
        if self.end >= changed.len() {
            return false;
        }
        self.start = self.end + 1;
        self.end = self.start;
        while self.end < changed.len() && changed[self.end] {
            self.end += 1;
        }
        true
    }

    /// Moves to the previous group; returns false at the start.
    fn prev(&mut self, changed: &[bool]) -> bool {
        if self.start == 0 {
            return false;
        }
        self.end = self.start - 1;
        self.start = self.end;
        while self.start > 0 && changed[self.start - 1] {
            self.start -= 1;
        }
        true
    }
}

impl Side<'_> {
    /// Shifts `g` down by one line if possible, joining a following group.
    fn slide_down(&mut self, g: &mut Group) -> bool {
        if g.end < self.ids.len() && self.ids[g.start] == self.ids[g.end] {
            self.changed[g.start] = false;
            self.changed[g.end] = true;
            g.start += 1;
            g.end += 1;
            while g.end < self.ids.len() && self.changed[g.end] {
                g.end += 1;
            }
            true
        } else {
            false
        }
    }

    /// Shifts `g` up by one line if possible, joining a preceding group.
    fn slide_up(&mut self, g: &mut Group) -> bool {
        if g.start > 0 && self.ids[g.start - 1] == self.ids[g.end - 1] {
            self.changed[g.start - 1] = true;
            self.changed[g.end - 1] = false;
            g.start -= 1;
            g.end -= 1;
            while g.start > 0 && self.changed[g.start - 1] {
                g.start -= 1;
            }
            true
        } else {
            false
        }
    }
}

/// Slides the changed groups of `x` (with `y` being the other side).
fn compact_side(
    x_ids: &[u32],
    x_lines: &[&[u8]],
    x_changed: &mut [bool],
    y_changed: &[bool],
    heuristic: bool,
) {
    let mut x = Side {
        ids: x_ids,
        lines: x_lines,
        changed: x_changed,
    };
    let mut g = Group::first(x.changed);
    let mut go = Group::first(y_changed);
    loop {
        if g.end > g.start {
            let mut end_matching_other;
            let mut earliest_end;
            loop {
                let size = g.end - g.start;
                while x.slide_up(&mut g) {
                    let ok = go.prev(y_changed);
                    debug_assert!(ok);
                }
                earliest_end = g.end;
                end_matching_other = if go.end > go.start { Some(g.end) } else { None };
                while x.slide_down(&mut g) {
                    let ok = go.next(y_changed);
                    debug_assert!(ok);
                    if go.end > go.start {
                        end_matching_other = Some(g.end);
                    }
                }
                if g.end - g.start == size {
                    break;
                }
            }
            let target = if g.end == earliest_end {
                None
            } else if let Some(e) = end_matching_other {
                Some(e)
            } else if heuristic {
                Some(indent::best_end(x.lines, g.start, g.end, earliest_end))
            } else {
                None
            };
            if let Some(t) = target {
                while g.end > t {
                    let ok = x.slide_up(&mut g);
                    debug_assert!(ok);
                    let ok = go.prev(y_changed);
                    debug_assert!(ok);
                }
            }
        }
        if !g.next(x.changed) {
            break;
        }
        let ok = go.next(y_changed);
        debug_assert!(ok);
    }
}

/// Slides changed groups on both sides (see the module documentation).
pub(crate) fn compact(
    a_ids: &[u32],
    a_lines: &[&[u8]],
    ca: &mut [bool],
    b_ids: &[u32],
    b_lines: &[&[u8]],
    cb: &mut [bool],
    heuristic: bool,
) {
    compact_side(a_ids, a_lines, ca, cb, heuristic);
    compact_side(b_ids, b_lines, cb, ca, heuristic);
}
