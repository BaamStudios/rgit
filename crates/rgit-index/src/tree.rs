// SPDX-License-Identifier: MIT

//! The cache tree (`TREE` extension).

use rgit_hash::{HashKind, ObjectId};

/// Deepest directory nesting accepted when parsing; deeper (crafted) data
/// is treated as corrupt and the cache tree is dropped.
const MAX_DEPTH: usize = 2048;

/// One directory of the cache tree.
///
/// A valid node knows the tree object that the entries below it would
/// produce and how many index entries it covers. An invalid node (written
/// as entry count `-1`) has no object id but keeps its subtrees.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeNode {
    /// Path component relative to the parent (empty for the root).
    pub name: Vec<u8>,
    entry_count: i32,
    id: Option<ObjectId>,
    /// Subdirectories, in the order they are stored.
    pub children: Vec<TreeNode>,
}

impl TreeNode {
    /// A valid node covering `entry_count` index entries.
    pub fn new_valid(name: impl Into<Vec<u8>>, entry_count: u32, id: ObjectId) -> TreeNode {
        TreeNode {
            name: name.into(),
            entry_count: i32::try_from(entry_count).unwrap_or(i32::MAX),
            id: Some(id),
            children: Vec::new(),
        }
    }

    /// An invalid node.
    pub fn new_invalid(name: impl Into<Vec<u8>>) -> TreeNode {
        TreeNode {
            name: name.into(),
            entry_count: -1,
            id: None,
            children: Vec::new(),
        }
    }

    /// True if the node has a tree id.
    pub fn is_valid(&self) -> bool {
        self.entry_count >= 0 && self.id.is_some()
    }

    /// Number of index entries covered, `None` if invalid.
    pub fn entry_count(&self) -> Option<u32> {
        if self.is_valid() {
            u32::try_from(self.entry_count).ok()
        } else {
            None
        }
    }

    /// The tree id, `None` if invalid.
    pub fn id(&self) -> Option<&ObjectId> {
        if self.is_valid() {
            self.id.as_ref()
        } else {
            None
        }
    }

    /// Marks the node valid.
    pub fn set_valid(&mut self, entry_count: u32, id: ObjectId) {
        self.entry_count = i32::try_from(entry_count).unwrap_or(i32::MAX);
        self.id = Some(id);
    }

    /// Marks the node invalid (subtrees are kept).
    pub fn invalidate(&mut self) {
        self.entry_count = -1;
        self.id = None;
    }

    /// The direct subdirectory `name`.
    pub fn child(&self, name: &[u8]) -> Option<&TreeNode> {
        self.children.iter().find(|c| c.name == name)
    }

    /// The direct subdirectory `name`, mutable.
    pub fn child_mut(&mut self, name: &[u8]) -> Option<&mut TreeNode> {
        self.children.iter_mut().find(|c| c.name == name)
    }
}

/// The cache tree: the root node and its subdirectories.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CacheTree {
    /// The top-level directory.
    pub root: TreeNode,
}

impl CacheTree {
    /// A cache tree with an invalid root.
    pub fn new() -> CacheTree {
        CacheTree {
            root: TreeNode::new_invalid(Vec::new()),
        }
    }

    /// The node for directory `dir` (`a/b`, empty for the root).
    pub fn find(&self, dir: &[u8]) -> Option<&TreeNode> {
        let mut node = &self.root;
        if dir.is_empty() {
            return Some(node);
        }
        for comp in dir.split(|&b| b == b'/') {
            node = node.child(comp)?;
        }
        Some(node)
    }

    /// Invalidates the nodes of all directories containing `path` (the
    /// root and each leading directory that has a node), as done when the
    /// entry at `path` is added, changed or removed.
    pub fn invalidate_path(&mut self, path: &[u8]) {
        let mut node = &mut self.root;
        node.invalidate();
        let Some(slash) = path.iter().rposition(|&b| b == b'/') else {
            return;
        };
        for comp in path[..slash].split(|&b| b == b'/') {
            match node.child_mut(comp) {
                Some(child) => {
                    child.invalidate();
                    node = child;
                }
                None => return,
            }
        }
    }

    /// Parses the `TREE` extension data. Returns `None` if it is malformed
    /// (the cache tree is only an optimisation, so it is dropped).
    pub fn parse(data: &[u8], kind: HashKind) -> Option<CacheTree> {
        let mut pos = 0;
        // Iterative depth-first parse: each stack item is a node still
        // waiting for `remaining` subtrees.
        let (root, n) = parse_node(data, &mut pos, kind)?;
        let mut stack = vec![(root, n)];
        loop {
            let (_, remaining) = stack.last_mut().expect("non-empty");
            if *remaining == 0 {
                let (done, _) = stack.pop().expect("non-empty");
                match stack.last_mut() {
                    Some((parent, _)) => parent.children.push(done),
                    None => {
                        return (pos == data.len()).then_some(CacheTree { root: done });
                    }
                }
            } else {
                *remaining -= 1;
                if stack.len() > MAX_DEPTH {
                    return None;
                }
                let child = parse_node(data, &mut pos, kind)?;
                stack.push(child);
            }
        }
    }

    /// Serializes the `TREE` extension data (without signature and size).
    pub fn write(&self, out: &mut Vec<u8>) {
        let mut stack = vec![&self.root];
        while let Some(node) = stack.pop() {
            out.extend_from_slice(&node.name);
            out.push(0);
            match node.id() {
                Some(id) => {
                    out.extend_from_slice(
                        format!("{} {}\n", node.entry_count, node.children.len()).as_bytes(),
                    );
                    out.extend_from_slice(id.as_bytes());
                }
                None => out.extend_from_slice(format!("-1 {}\n", node.children.len()).as_bytes()),
            }
            stack.extend(node.children.iter().rev());
        }
    }
}

impl Default for CacheTree {
    fn default() -> Self {
        CacheTree::new()
    }
}

/// Parses one node without its subtrees; returns it and its subtree count.
fn parse_node(data: &[u8], pos: &mut usize, kind: HashKind) -> Option<(TreeNode, usize)> {
    let rest = &data[*pos..];
    let nul = rest.iter().position(|&b| b == 0)?;
    let name = rest[..nul].to_vec();
    let rest = &rest[nul + 1..];
    let nl = rest.iter().position(|&b| b == b'\n')?;
    let line = std::str::from_utf8(&rest[..nl]).ok()?;
    let (count, subtrees) = line.split_once(' ')?;
    let entry_count = parse_decimal(count, true)?;
    let subtrees = usize::try_from(parse_decimal(subtrees, false)?).ok()?;
    *pos += nul + 1 + nl + 1;
    let id = if entry_count >= 0 {
        let len = kind.byte_len();
        let bytes = data.get(*pos..*pos + len)?;
        *pos += len;
        Some(ObjectId::from_bytes(kind, bytes).ok()?)
    } else {
        None
    };
    // Each subtree takes at least 6 bytes; avoid huge reservations from
    // crafted counts.
    let children = Vec::with_capacity(subtrees.min((data.len() - *pos) / 6));
    let node = TreeNode {
        name,
        entry_count: if entry_count < 0 { -1 } else { entry_count },
        id,
        children,
    };
    Some((node, subtrees))
}

/// ASCII decimal without leading `+`; a leading `-` only if `signed`.
fn parse_decimal(s: &str, signed: bool) -> Option<i32> {
    let digits = s.strip_prefix('-').filter(|_| signed).unwrap_or(s);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u8) -> ObjectId {
        ObjectId::from_bytes(HashKind::Sha1, &[n; 20]).unwrap()
    }

    fn sample() -> CacheTree {
        let mut root = TreeNode::new_valid("", 4, id(1));
        let mut a = TreeNode::new_valid("a", 2, id(2));
        a.children.push(TreeNode::new_valid("b", 1, id(3)));
        root.children.push(a);
        root.children.push(TreeNode::new_invalid("c"));
        CacheTree { root }
    }

    #[test]
    fn round_trip() {
        let t = sample();
        let mut out = Vec::new();
        t.write(&mut out);
        assert_eq!(CacheTree::parse(&out, HashKind::Sha1), Some(t));
    }

    #[test]
    fn invalidate() {
        let mut t = sample();
        t.invalidate_path(b"a/b/x");
        assert!(!t.root.is_valid());
        assert!(!t.find(b"a").unwrap().is_valid());
        assert!(!t.find(b"a/b").unwrap().is_valid());
        let mut t = sample();
        t.invalidate_path(b"top");
        assert!(!t.root.is_valid());
        assert!(t.find(b"a").unwrap().is_valid());
        let mut t = sample();
        t.invalidate_path(b"zz/y");
        assert!(t.find(b"a/b").unwrap().is_valid());
    }

    #[test]
    fn malformed_is_dropped() {
        assert_eq!(CacheTree::parse(b"", HashKind::Sha1), None);
        assert_eq!(CacheTree::parse(b"\0-1 1\n", HashKind::Sha1), None);
        assert_eq!(CacheTree::parse(b"\0+1 0\n", HashKind::Sha1), None);
        assert_eq!(CacheTree::parse(b"\0-1 0\nx", HashKind::Sha1), None);
        assert!(CacheTree::parse(b"\0-1 0\n", HashKind::Sha1).is_some());
        // Negative counts other than -1 are read as invalid.
        let t = CacheTree::parse(b"\0-5 0\n", HashKind::Sha1).unwrap();
        assert!(!t.root.is_valid());
    }

    #[test]
    fn deep_nesting_is_dropped() {
        let mut data = Vec::new();
        for _ in 0..=MAX_DEPTH + 1 {
            data.extend_from_slice(b"d\0-1 1\n");
        }
        assert_eq!(CacheTree::parse(&data, HashKind::Sha1), None);
    }
}
