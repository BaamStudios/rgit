// SPDX-License-Identifier: MIT
//! The text drawing of `--graph` (Documentation/rev-list-options.adoc,
//! "--graph").
//!
//! The documentation only says that the history is drawn as text to the
//! left of the output. Everything else here was derived from the output of
//! the reference `git` (the expected graphs of the upstream tests t4202,
//! t4214, t4215, t4218 and differential runs on random histories):
//!
//! * Every commit owns a row. Above it, the *lanes* of the commits still
//!   to come are drawn as `|` two characters apart. The commit's own lane
//!   shows its mark (`*`, `<`, `>`, `o`); a commit nobody is waiting for
//!   gets a new lane at the right.
//! * After the commit row the lanes are rearranged: the commit's lane is
//!   replaced by its parents (a merge fans them out with `\`, an octopus
//!   merge draws `-` and `.` on the commit row), lanes waiting for the same
//!   commit join, and lanes move left (`/`, `_` across other lanes) one
//!   step per line until every lane is at its place.
//! * Each line of the commit's text gets the next line of the drawing, the
//!   rest of the drawing follows the text; lines are padded to the width
//!   of the widest part of the row.
//! * Every lane has a colour from `log.graphColors`; a new lane takes the
//!   next colour, the first parent of a non-merge inherits its lane.

use std::collections::HashSet;
use std::rc::Rc;

use rgit_hash::ObjectId;

/// The default lane colours: red, green, yellow, blue, magenta, cyan,
/// then the bold variants.
const DEFAULT_COLORS: [&str; 12] = [
    "\x1b[31m",
    "\x1b[32m",
    "\x1b[33m",
    "\x1b[34m",
    "\x1b[35m",
    "\x1b[36m",
    "\x1b[1;31m",
    "\x1b[1;32m",
    "\x1b[1;33m",
    "\x1b[1;34m",
    "\x1b[1;35m",
    "\x1b[1;36m",
];

const RESET: &str = "\x1b[m";

/// The default `log.graphColors` as escape sequences.
pub fn default_colors() -> Vec<String> {
    DEFAULT_COLORS.iter().map(|c| (*c).to_string()).collect()
}

/// A lane: the commit it leads to and its colour (an index into the
/// palette).
#[derive(Debug, Clone, Copy)]
struct Lane {
    id: ObjectId,
    color: usize,
}

/// Which line of the drawing comes next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// Between commits: the lanes straight down.
    Idle,
    /// `...`: the drawing of the previous commit was cut short.
    Gap,
    /// An octopus merge pushes the lanes to its right out of the way.
    Widen,
    /// The commit's own line.
    Commit,
    /// The parents of a merge leave the commit's lane.
    Fan,
    /// Lanes move left to their places.
    Shift,
    /// The lane of an indented root moves one place to the right.
    Nudge,
}

/// One line being drawn: bytes plus the number of columns drawn so far.
///
/// With a lane limit (`--graph-lane-limit=<n>`) only the first `2n`
/// columns are written; anything else drawn further right turns into a
/// single `~` at column `2n`.
#[derive(Clone)]
struct Line {
    bytes: Vec<u8>,
    cols: usize,
    palette: Option<Rc<Vec<String>>>,
    /// The first column that is cut off (`usize::MAX`: no limit).
    cut: usize,
    /// Something other than a blank was drawn at or after `cut`.
    beyond: bool,
    /// Columns written into `bytes`.
    shown: usize,
    /// Write the next characters even at or after `cut` (the mark of a
    /// commit sitting exactly on the last lane).
    force: bool,
    /// Ignore everything drawn from now on.
    stop: bool,
    /// Extra padding after a cut line.
    extra: usize,
}

impl Line {
    /// Whether the next column is written (and records what is cut off).
    fn visible(&mut self, ch: u8) -> bool {
        let col = self.cols;
        self.cols += 1;
        if self.stop {
            return false;
        }
        if col < self.cut || self.force {
            self.shown = self.cols;
            return true;
        }
        if ch != b' ' {
            self.beyond = true;
        }
        false
    }

    fn plain(&mut self, ch: u8) {
        if self.visible(ch) {
            self.bytes.push(ch);
        }
    }

    fn plain_str(&mut self, s: &[u8]) {
        for &ch in s {
            self.plain(ch);
        }
    }

    fn colored(&mut self, ch: u8, color: usize) {
        if !self.visible(ch) {
            return;
        }
        match self.palette.as_deref() {
            Some(p) if !p.is_empty() => {
                self.bytes.extend_from_slice(p[color % p.len()].as_bytes());
                self.bytes.push(ch);
                self.bytes.extend_from_slice(RESET.as_bytes());
            }
            _ => self.bytes.push(ch),
        }
    }

    /// Drop trailing blanks (covered by the padding).
    fn trim(&mut self) {
        if self.beyond {
            return;
        }
        self.cols = self.shown;
        while self.cols > 0 && self.bytes.last() == Some(&b' ') {
            self.bytes.pop();
            self.cols -= 1;
        }
        self.shown = self.cols;
    }

    /// The finished line, padded to `width` columns (with a lane limit at
    /// most two columns past the cut).
    fn finish(mut self, width: usize) -> Vec<u8> {
        let mut width = width;
        if self.cut != usize::MAX {
            if self.beyond {
                self.bytes.push(b'~');
                self.shown += 1;
            }
            width = width.min(self.cut + 2 + self.extra);
        }
        while self.shown < width {
            self.bytes.push(b' ');
            self.shown += 1;
        }
        self.bytes
    }
}

/// Target lane of a drawing position (`None`: nothing there).
type Slots = Vec<Option<usize>>;

/// The `--graph` drawing state.
#[derive(Clone)]
pub struct Graph {
    /// Escape sequences of the lane colours; `None` without colour.
    palette: Option<Rc<Vec<String>>>,
    /// The colour the next new lane takes.
    next_color: usize,
    /// Lanes above the current commit.
    above: Vec<Lane>,
    /// Lanes below the current commit.
    below: Vec<Lane>,
    commit: Option<ObjectId>,
    /// The parents of the commit that are part of the drawing.
    parents: Vec<ObjectId>,
    /// What the commit's lane shows (`*`, `<`, `>`, `o`, ...).
    mark: Vec<u8>,
    /// Position of the commit among `above` (or `above.len()`).
    at: usize,
    prev_at: usize,
    /// How the first parent of a merge leaves: 0 to the lane at the left,
    /// 1 straight down; `None` before it is decided or for non-merges.
    layout: Option<usize>,
    /// How many lanes to the left the first parent of a merge goes.
    left: usize,
    /// Lanes the commit adds to the right (-1: its last parent joins the
    /// lane right next to it).
    added: isize,
    prev_added: isize,
    /// For every drawing position (two per lane) the lane below it leads
    /// to.
    slots: Slots,
    /// `slots` as the previous line left them.
    prev_slots: Slots,
    /// Width of the drawing of this commit.
    width: usize,
    /// Lines of the `Widen` step drawn so far.
    widen_row: usize,
    step: Step,
    prev_step: Step,
    /// Indent roots that would look like the parent of what follows.
    indent: bool,
    /// Empty lanes drawn before the commit's mark (an indented root).
    shift: usize,
    /// The commit's lane steps right before its line (an indented root).
    nudge: bool,
    /// Depth of the root before (wrapping after four) when the next new
    /// root cascades further right.
    cascade: Option<usize>,
    /// Commits that are the second or a later parent of a merge drawn so
    /// far.
    side_parents: HashSet<ObjectId>,
    /// `--graph-lane-limit` (0: none).
    lane_limit: usize,
}

/// What the graph needs to know about the commit after the current one
/// (for indenting roots).
#[derive(Debug, Clone, Copy)]
pub struct Next {
    pub id: ObjectId,
    /// It has no parents in the drawing and is not a boundary commit.
    pub root: bool,
    /// Nothing follows it.
    pub last: bool,
}

impl Graph {
    /// A drawing; `palette` holds the lane colours when colouring.
    pub fn new(palette: Option<Vec<String>>) -> Graph {
        let n = palette.as_ref().map_or(DEFAULT_COLORS.len(), Vec::len);
        Graph {
            palette: palette.map(Rc::new),
            // The first new lane takes the first colour.
            next_color: n.saturating_sub(1),
            above: Vec::new(),
            below: Vec::new(),
            commit: None,
            parents: Vec::new(),
            mark: b"*".to_vec(),
            at: 0,
            prev_at: 0,
            layout: None,
            left: 0,
            added: 0,
            prev_added: 0,
            slots: Vec::new(),
            prev_slots: Vec::new(),
            width: 0,
            widen_row: 0,
            step: Step::Idle,
            prev_step: Step::Idle,
            indent: false,
            shift: 0,
            nudge: false,
            cascade: None,
            side_parents: HashSet::new(),
            lane_limit: 0,
        }
    }

    /// Indent roots (`--graph-indent`, the default).
    pub fn set_indent(&mut self, on: bool) {
        self.indent = on;
    }

    /// Draw at most `n` lanes (`--graph-lane-limit`, 0: all).
    pub fn set_lane_limit(&mut self, n: usize) {
        self.lane_limit = n;
    }

    fn palette_len(&self) -> usize {
        self.palette
            .as_ref()
            .map_or(DEFAULT_COLORS.len(), |p| p.len())
    }

    fn line(&self) -> Line {
        Line {
            bytes: Vec::new(),
            cols: 0,
            palette: self.palette.clone(),
            cut: if self.lane_limit == 0 {
                usize::MAX
            } else {
                2 * self.lane_limit
            },
            beyond: false,
            shown: 0,
            force: false,
            stop: false,
            extra: 0,
        }
    }

    fn set_step(&mut self, s: Step) {
        self.prev_step = self.step;
        self.step = s;
    }

    /// Start the row of `commit`; `parents` are those of its parents that
    /// are shown (in order), `mark` the character on its lane.
    /// `next` is the commit shown after it.
    pub fn update(
        &mut self,
        commit: ObjectId,
        parents: Vec<ObjectId>,
        mark: &[u8],
        next: Option<Next>,
    ) {
        self.commit = Some(commit);
        self.parents = parents;
        self.mark = mark.to_vec();
        self.prev_at = self.at;
        self.relayout();
        self.place_root(next);
        if self.parents.len() > 1 {
            self.side_parents.extend(self.parents[1..].iter().copied());
        }
        self.widen_row = 0;
        self.step = if self.step != Step::Idle {
            Step::Gap
        } else {
            self.first_step()
        };
    }

    fn first_step(&self) -> Step {
        if self.nudge {
            Step::Nudge
        } else if self.needs_widen() {
            Step::Widen
        } else {
            Step::Commit
        }
    }

    /// A root followed by more commits must not look like the parent of
    /// the next one (a visual root). A root right after another root
    /// cascades: it is drawn one lane further right than the root before
    /// (relative to its own lane, back to it every fourth root). A root
    /// whose lane comes from a child moves one lane to the right when the
    /// next commit starts a new lane; a root starting a new lane moves
    /// right when the next commit would come in the same column. Roots
    /// that are the second or a later parent of a merge stay where they
    /// are and only let a following new root cascade.
    fn place_root(&mut self, next: Option<Next>) {
        self.shift = 0;
        self.nudge = false;
        let cascade = self.cascade.take();
        let Some(next) = next else {
            return;
        };
        let commit = self.commit.expect("commit");
        if !self.indent || !self.parents.is_empty() || self.mark == b"o" {
            return;
        }
        let side = self.side_parents.contains(&commit);
        let natural = self.at;
        let in_lane = self.at < self.above.len();
        let mut depth = 0;
        if !side && let Some(d) = cascade {
            depth = (d + 1) % 4;
        }
        let mut col = natural + depth;
        let next_lane = self.below.iter().position(|l| l.id == next.id);
        let next_side = self.side_parents.contains(&next.id);
        // A cascade starts at a root that starts a new lane; once started
        // it also takes the roots in lanes.
        let chained = cascade.is_some() && !side;
        if side {
            // Stays, and starts no cascade.
        } else if next.root && !next.last && !next_side && (next_lane.is_none() || chained) {
            self.cascade = Some(depth);
        } else if in_lane {
            // (A cascaded root is moved far enough already.)
            if next_lane.is_none() && depth == 0 {
                col += 1;
                self.nudge = true;
            }
        } else if next_lane.unwrap_or(self.below.len()) == col {
            col += 1;
        }
        self.shift = col - natural;
        self.width += 2 * self.shift;
    }

    /// Lay out the lanes below the commit and where every lane above (or
    /// parent) goes.
    fn relayout(&mut self) {
        self.above = std::mem::take(&mut self.below);
        let commit = self.commit.expect("commit");
        self.prev_slots = std::mem::replace(
            &mut self.slots,
            vec![None; 2 * (self.above.len() + self.parents.len()) + 2],
        );
        self.prev_added = self.added;
        self.added = 0;
        self.width = 0;
        let mut in_lanes = true;
        let mut found = false;
        let mut i = 0;
        loop {
            let id = if i < self.above.len() {
                self.above[i].id
            } else if found {
                break;
            } else {
                in_lanes = false;
                commit
            };
            if id == commit {
                found = true;
                self.at = i;
                self.layout = None;
                self.left = 0;
                let parents = self.parents.clone();
                let n = parents.len();
                for p in parents {
                    if n > 1 || !in_lanes {
                        self.next_color = (self.next_color + 1) % self.palette_len().max(1);
                    }
                    self.place(p, Some(i));
                }
                if n == 0 {
                    self.width += 2;
                }
            } else {
                self.place(id, None);
            }
            if i == self.above.len() {
                break;
            }
            i += 1;
        }
    }

    /// Put commit `id` below the row; `from` is the commit's position when
    /// `id` is one of its parents.
    fn place(&mut self, id: ObjectId, from: Option<usize>) {
        let lane = match self.below.iter().position(|l| l.id == id) {
            Some(l) => l,
            None => {
                let color = self
                    .above
                    .iter()
                    .find(|l| l.id == id)
                    .map_or(self.next_color, |l| l.color);
                self.below.push(Lane { id, color });
                self.below.len() - 1
            }
        };
        let cursor = self.width;
        let slot;
        match from {
            Some(at) if self.parents.len() > 1 && self.layout.is_none() => {
                // The first parent of a merge: straight down, or to the
                // left when its lane is there already.
                let left = at.saturating_sub(lane);
                let layout = usize::from(left == 0);
                self.layout = Some(layout);
                self.left = left;
                self.added = self.parents.len() as isize + layout as isize - 2;
                slot = if layout == 1 {
                    cursor
                } else if left > 1 {
                    cursor - (2 * left - 3)
                } else {
                    cursor - 1
                };
                self.width += 2 * layout;
            }
            _ if self.added > 0 && cursor >= 2 && self.slots[cursor - 2] == Some(lane) => {
                // The last parent is the lane right of the commit: join it
                // instead of adding a lane.
                slot = cursor - 2;
                self.added = -1;
            }
            _ => {
                slot = cursor;
                self.width += 2;
            }
        }
        if slot >= self.slots.len() {
            self.slots.resize(slot + 1, None);
        }
        self.slots[slot] = Some(lane);
    }

    fn needs_widen(&self) -> bool {
        self.parents.len() >= 3
            && self.at + 1 < self.above.len()
            && self.widen_row < 2 * (self.parents.len() + self.layout.unwrap_or(1) - 3)
    }

    /// Every lane is at its place (an edge one position right of its lane
    /// counts as arrived).
    fn settled(&self) -> bool {
        self.slots
            .iter()
            .enumerate()
            .all(|(pos, t)| t.is_none_or(|t| t == pos / 2))
    }

    /// The commit's row is drawn completely.
    pub fn finished(&self) -> bool {
        self.step == Step::Idle
    }

    /// The next line of the drawing (without newline).
    pub fn next_line(&mut self) -> Vec<u8> {
        match self.step {
            Step::Idle => self.idle_line(),
            Step::Gap => self.gap_line(),
            Step::Widen => self.widen_line(),
            Step::Nudge => self.nudge_line(),
            Step::Commit => self.commit_line(),
            Step::Fan => self.fan_line(),
            Step::Shift => self.shift_line(),
        }
    }

    /// The lines before the commit's own line, and that line.
    pub fn lines_to_commit(&mut self) -> (Vec<Vec<u8>>, Vec<u8>) {
        let mut before = Vec::new();
        loop {
            let is_commit = self.step == Step::Commit;
            let line = self.next_line();
            if is_commit {
                return (before, line);
            }
            before.push(line);
            if self.step == Step::Idle {
                // Nothing to draw for this commit.
                return (before, Vec::new());
            }
        }
    }

    /// The width of the drawing beside the current commit's text.
    pub fn text_width(&self) -> usize {
        if self.lane_limit == 0 {
            self.width
        } else {
            self.width.min(2 * self.lane_limit + 2)
        }
    }

    /// The line before a commit's separator: the lanes straight down while
    /// the commit's line is next, otherwise the next line of the drawing.
    pub fn padding_line(&mut self) -> Vec<u8> {
        if self.step != Step::Commit {
            return self.next_line();
        }
        let mut line = self.line();
        for lane in &self.above {
            line.colored(b'|', lane.color);
            if Some(lane.id) == self.commit && self.parents.len() > 2 {
                for _ in 0..2 * (self.parents.len() - 2) {
                    line.plain(b' ');
                }
            } else {
                line.plain(b' ');
            }
        }
        let bytes = line.finish(self.width - 2 * self.shift);
        self.prev_step = Step::Idle;
        bytes
    }

    fn idle_line(&mut self) -> Vec<u8> {
        let mut line = self.line();
        for lane in &self.below {
            line.colored(b'|', lane.color);
            line.plain(b' ');
        }
        line.finish(self.width)
    }

    fn gap_line(&mut self) -> Vec<u8> {
        let mut line = self.line();
        line.plain_str(b"...");
        let bytes = line.finish(self.width);
        let next = self.first_step();
        self.set_step(next);
        bytes
    }

    /// The lane of an indented root steps right.
    fn nudge_line(&mut self) -> Vec<u8> {
        let mut line = self.line();
        for (i, lane) in self.above.iter().enumerate() {
            if i == self.at {
                line.plain(b' ');
                line.colored(b'\\', lane.color);
            } else {
                line.colored(b'|', lane.color);
            }
            line.plain(b' ');
        }
        let bytes = line.finish(self.width - 2 * self.shift);
        let next = if self.needs_widen() {
            Step::Widen
        } else {
            Step::Commit
        };
        self.set_step(next);
        bytes
    }

    fn widen_line(&mut self) -> Vec<u8> {
        let mut line = self.line();
        let mut seen = false;
        for (i, lane) in self.above.iter().enumerate() {
            if Some(lane.id) == self.commit {
                seen = true;
                line.colored(b'|', lane.color);
                for _ in 0..self.widen_row {
                    line.plain(b' ');
                }
            } else if seen && self.widen_row == 0 {
                let ch = if self.prev_step == Step::Fan && self.prev_at < i {
                    b'\\'
                } else {
                    b'|'
                };
                line.colored(ch, lane.color);
            } else if seen {
                line.colored(b'\\', lane.color);
            } else {
                line.colored(b'|', lane.color);
            }
            line.plain(b' ');
        }
        let bytes = line.finish(self.width);
        self.widen_row += 1;
        if !self.needs_widen() {
            self.set_step(Step::Commit);
        }
        bytes
    }

    /// The colour of the lane below that parent `k` of the commit leads to.
    fn parent_color(&self, k: usize) -> usize {
        let id = self.parents[k];
        self.below
            .iter()
            .find(|l| l.id == id)
            .map_or(0, |l| l.color)
    }

    fn commit_line(&mut self) -> Vec<u8> {
        let mut line = self.line();
        let commit = self.commit.expect("commit");
        let mut seen = false;
        let n = self.above.len();
        for i in 0..=n {
            let lane = self.above.get(i).copied();
            let id = match lane {
                Some(l) => l.id,
                None if seen => break,
                None => commit,
            };
            if id == commit {
                seen = true;
                for _ in 0..self.shift {
                    line.plain_str(b"  ");
                }
                if line.cols == line.cut {
                    // A commit sitting exactly on the last lane shows its
                    // mark and nothing right of it.
                    line.force = true;
                    line.plain_str(&self.mark);
                    line.force = false;
                    line.stop = true;
                    continue;
                }
                let mark_col = line.cols;
                line.plain_str(&self.mark);
                if self.parents.len() > 2 {
                    self.octopus(&mut line);
                    if mark_col < line.cut && line.beyond {
                        // An octopus cut short keeps one more blank.
                        line.extra = 1;
                    }
                }
            } else {
                let lane = lane.expect("lane");
                let ch = if seen && self.added > 1 {
                    b'\\'
                } else if seen && self.added == 1 {
                    if self.prev_step == Step::Fan && self.prev_added > 0 && self.prev_at < i {
                        b'\\'
                    } else {
                        b'|'
                    }
                } else if self.prev_step == Step::Shift
                    && self.prev_slots.get(2 * i + 1).copied().flatten() == Some(i)
                    && self
                        .slots
                        .get(2 * i)
                        .copied()
                        .flatten()
                        .is_none_or(|t| t < i)
                {
                    b'/'
                } else {
                    b'|'
                };
                line.colored(ch, lane.color);
            }
            line.plain(b' ');
        }
        let bytes = line.finish(self.width);
        let next = if self.parents.len() > 1 && !self.fan_cut() {
            Step::Fan
        } else if self.settled() {
            Step::Idle
        } else {
            Step::Shift
        };
        self.set_step(next);
        bytes
    }

    /// The `-.`, `---.` ... after the mark of an octopus merge: two
    /// characters for every parent beyond the first two that leave
    /// straight down or to the right, the last one a `.`.
    fn octopus(&self, line: &mut Line) {
        let plain = 3 - self.layout.unwrap_or(1);
        let n = self.parents.len();
        if n <= plain {
            return;
        }
        let dashed = n - plain;
        for k in 0..dashed {
            let color = self.parent_color(plain + k);
            line.colored(b'-', color);
            line.colored(if k + 1 == dashed { b'.' } else { b'-' }, color);
        }
    }

    /// A merge at or beyond the lane limit draws no fan line, unless its
    /// first parent crosses lanes to the left.
    fn fan_cut(&self) -> bool {
        self.lane_limit != 0 && self.at >= self.lane_limit && self.left <= 1
    }

    fn fan_line(&mut self) -> Vec<u8> {
        let mut line = self.line();
        let commit = self.commit.expect("commit");
        let layout = self.layout.unwrap_or(1);
        // The lane the first parent goes to (for `_` across the lanes in
        // between when it is further left).
        let first = self
            .below
            .iter()
            .position(|l| Some(&l.id) == self.parents.first());
        let first_color = self.parent_color(0);
        let mut seen = false;
        let n = self.above.len();
        for i in 0..=n {
            let lane = self.above.get(i).copied();
            let id = match lane {
                Some(l) => l.id,
                None if seen => break,
                None => commit,
            };
            if id == commit {
                seen = true;
                let count = self.parents.len();
                for k in 0..count {
                    let color = self.parent_color(k);
                    let kind = (layout + k).min(2);
                    line.colored(b"/|\\"[kind], color);
                    let last = k + 1 == count;
                    if (kind == 2 && (self.added > 0 || !last)) || (kind == 1 && last) {
                        line.plain(b' ');
                    }
                }
            } else if seen {
                let lane = lane.expect("lane");
                line.colored(if self.added > 0 { b'\\' } else { b'|' }, lane.color);
                line.plain(b' ');
            } else {
                let lane = lane.expect("lane");
                line.colored(b'|', lane.color);
                if layout == 0 && i + 1 == self.at {
                    // The first parent's `/` takes the gap.
                } else if layout == 0 && first.is_some_and(|f| i > f) {
                    line.colored(b'_', first_color);
                } else {
                    line.plain(b' ');
                }
            }
        }
        let bytes = line.finish(self.width);
        let next = if self.settled() {
            Step::Idle
        } else {
            Step::Shift
        };
        self.set_step(next);
        bytes
    }

    fn shift_line(&mut self) -> Vec<u8> {
        let size = self.slots.len();
        let mut next: Slots = vec![None; size];
        // The first lane that moves left this line may cross several
        // lanes at once (`_`); `run` is its target and position.
        let mut run: Option<(usize, usize)> = None;
        for pos in 0..size {
            let Some(t) = self.slots[pos] else {
                continue;
            };
            if 2 * t == pos {
                next[pos] = Some(t);
                continue;
            }
            if pos == 0 {
                continue;
            }
            match next[pos - 1] {
                None => {
                    next[pos - 1] = Some(t);
                    if run.is_none() {
                        run = Some((t, pos));
                        let mut j = 2 * t + 3;
                        while j + 2 < pos {
                            next[j] = Some(t);
                            j += 2;
                        }
                    }
                }
                Some(u) if u == t => {}
                Some(_) => {
                    if pos >= 2 {
                        next[pos - 2] = Some(t);
                    }
                    if run.is_none() {
                        run = Some((t, pos - 1));
                        let mut j = 2 * t + 3;
                        while j + 3 < pos {
                            next[j] = Some(t);
                            j += 2;
                        }
                    }
                }
            }
        }
        let mut line = self.line();
        let mut crossed = false;
        for (pos, slot) in next.iter_mut().enumerate() {
            let Some(t) = *slot else {
                line.plain(b' ');
                continue;
            };
            let color = self.below.get(t).map_or(0, |l| l.color);
            if 2 * t == pos {
                line.colored(b'|', color);
            } else if run.is_some_and(|(rt, re)| rt == t && pos + 1 != re) {
                if pos != 2 * t + 3 {
                    *slot = None;
                }
                crossed = true;
                line.colored(b'_', color);
            } else {
                if crossed && run.is_some_and(|(_, re)| pos < re) {
                    *slot = None;
                }
                line.colored(b'/', color);
            }
        }
        line.trim();
        self.slots = next;
        if self.settled() {
            self.set_step(Step::Idle);
        }
        line.finish(self.width)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rgit_hash::HashKind;

    fn id(n: u8) -> ObjectId {
        let mut hex = String::new();
        for _ in 0..20 {
            hex.push_str(&format!("{n:02x}"));
        }
        ObjectId::from_hex(HashKind::Sha1, hex.as_bytes()).unwrap()
    }

    /// Draw `commits` (id, parents) with one-line subjects `c<id>`.
    fn draw(commits: &[(u8, &[u8])]) -> String {
        draw_limited(commits, 0)
    }

    fn draw_limited(commits: &[(u8, &[u8])], limit: usize) -> String {
        let mut g = Graph::new(None);
        g.set_lane_limit(limit);
        let mut out = String::new();
        for (c, parents) in commits {
            g.update(id(*c), parents.iter().map(|p| id(*p)).collect(), b"*", None);
            let (before, line) = g.lines_to_commit();
            for l in before {
                out.push_str(String::from_utf8(l).unwrap().trim_end());
                out.push('\n');
            }
            out.push_str(&String::from_utf8(line).unwrap());
            out.push_str(&format!("c{c}\n"));
            while !g.finished() {
                out.push_str(String::from_utf8(g.next_line()).unwrap().trim_end());
                out.push('\n');
            }
        }
        out
    }

    #[test]
    fn linear_and_merge() {
        let got = draw(&[(4, &[3, 2]), (3, &[1]), (2, &[1]), (1, &[])]);
        assert_eq!(got, "*   c4\n|\\\n* | c3\n| * c2\n|/\n* c1\n");
    }

    #[test]
    fn lane_limit_cuts_lanes() {
        let got = draw_limited(&[(4, &[3, 2]), (3, &[1]), (2, &[1]), (1, &[])], 1);
        assert_eq!(got, "*   c4\n|\\\n* ~ c3\n| * c2\n|/\n* c1\n");
    }
}
