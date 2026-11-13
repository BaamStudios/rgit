// SPDX-License-Identifier: MIT
//! The command catalogue behind `git help`, `--list-cmds` and the
//! "did you mean" suggestions: git's commands with their category
//! (Documentation/git.adoc, "GIT COMMANDS") and a one-line description
//! written for rgit, plus the guides and interface documents, and the
//! commands installed as `git-<name>` programs.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::Context;
use crate::commands;
use crate::messages::help as msg;

/// Command categories, in the order `git help -a` prints them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Main,
    AncillaryManipulators,
    AncillaryInterrogators,
    Foreign,
    PlumbingManipulators,
    PlumbingInterrogators,
    Syncing,
    Helpers,
    UserInterfaces,
    DeveloperInterfaces,
    Guide,
}

impl Category {
    /// The categories `git help -a` lists, in order.
    pub const ALL_LISTED: &[Category] = &[
        Category::Main,
        Category::AncillaryManipulators,
        Category::AncillaryInterrogators,
        Category::Foreign,
        Category::PlumbingManipulators,
        Category::PlumbingInterrogators,
        Category::Syncing,
        Category::Helpers,
        Category::UserInterfaces,
        Category::DeveloperInterfaces,
    ];

    pub fn heading(self) -> &'static str {
        match self {
            Category::Main => msg::CAT_MAIN,
            Category::AncillaryManipulators => msg::CAT_ANCILLARY_MANIPULATORS,
            Category::AncillaryInterrogators => msg::CAT_ANCILLARY_INTERROGATORS,
            Category::Foreign => msg::CAT_FOREIGN,
            Category::PlumbingManipulators => msg::CAT_PLUMBING_MANIPULATORS,
            Category::PlumbingInterrogators => msg::CAT_PLUMBING_INTERROGATORS,
            Category::Syncing => msg::CAT_SYNCING,
            Category::Helpers => msg::CAT_HELPERS,
            Category::UserInterfaces => msg::CAT_USER_INTERFACES,
            Category::DeveloperInterfaces => msg::CAT_DEVELOPER_INTERFACES,
            Category::Guide => msg::GUIDES_HEADING,
        }
    }

    /// The name used by `--list-cmds=list-<name>`.
    pub fn list_name(self) -> &'static str {
        match self {
            Category::Main => "mainporcelain",
            Category::AncillaryManipulators => "ancillarymanipulators",
            Category::AncillaryInterrogators => "ancillaryinterrogators",
            Category::Foreign => "foreignscminterface",
            Category::PlumbingManipulators => "plumbingmanipulators",
            Category::PlumbingInterrogators => "plumbinginterrogators",
            Category::Syncing => "synchingrepositories",
            Category::Helpers => "purehelpers",
            Category::UserInterfaces => "userinterfaces",
            Category::DeveloperInterfaces => "developerinterfaces",
            Category::Guide => "guide",
        }
    }

    /// Documents (not commands): their pages are `git<name>`.
    pub fn is_document(self) -> bool {
        matches!(
            self,
            Category::UserInterfaces | Category::DeveloperInterfaces | Category::Guide
        )
    }
}

/// The groups of common commands in `git help`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Init,
    Worktree,
    Info,
    History,
    Remote,
}

impl Group {
    pub const ALL: &[Group] = &[
        Group::Init,
        Group::Worktree,
        Group::Info,
        Group::History,
        Group::Remote,
    ];

    pub fn heading(self) -> &'static str {
        match self {
            Group::Init => msg::GROUP_INIT,
            Group::Worktree => msg::GROUP_WORKTREE,
            Group::Info => msg::GROUP_INFO,
            Group::History => msg::GROUP_HISTORY,
            Group::Remote => msg::GROUP_REMOTE,
        }
    }
}

/// One catalogue entry.
pub struct Entry {
    pub name: &'static str,
    pub category: Category,
    /// Set for the common commands `git help` shows.
    pub group: Option<Group>,
    pub description: &'static str,
}

const fn e(name: &'static str, category: Category, description: &'static str) -> Entry {
    Entry {
        name,
        category,
        group: None,
        description,
    }
}

const fn c(name: &'static str, group: Group, description: &'static str) -> Entry {
    Entry {
        name,
        category: Category::Main,
        group: Some(group),
        description,
    }
}

use Category::*;
use Group::{History as GH, Info as GI, Init as GN, Remote as GR, Worktree as GW};

/// Sorted by name within each category.
pub static CATALOG: &[Entry] = &[
    // Main porcelain
    c("add", GW, "Stage file contents for the next commit"),
    e("am", Main, "Apply patches from a mailbox"),
    e("archive", Main, "Write a tree as a tar or zip archive"),
    c("backfill", GH, "Fetch missing objects of a partial clone"),
    c(
        "bisect",
        GI,
        "Find the commit that introduced a change by binary search",
    ),
    c("branch", GH, "List, create, rename or delete branches"),
    e("bundle", Main, "Move objects and refs in a file"),
    e(
        "checkout",
        Main,
        "Switch branches or restore files in the working tree",
    ),
    e("cherry-pick", Main, "Apply the changes of existing commits"),
    e("citool", Main, "Graphical commit tool"),
    e(
        "clean",
        Main,
        "Delete untracked files from the working tree",
    ),
    c("clone", GN, "Copy a repository into a new directory"),
    c("commit", GH, "Record the staged changes as a new commit"),
    e("describe", Main, "Name a commit after the nearest tag"),
    c(
        "diff",
        GI,
        "Show differences between commits, the index and files",
    ),
    c("fetch", GR, "Get objects and refs from another repository"),
    e("format-patch", Main, "Turn commits into e-mail patches"),
    e("gc", Main, "Clean up and compress the repository"),
    e("gitk", Main, "Graphical history browser"),
    c("grep", GI, "Search tracked files for a pattern"),
    e("gui", Main, "Graphical interface to Git"),
    c("history", GH, "Rewrite history (experimental)"),
    c("init", GN, "Create a new repository, or reinitialize one"),
    c("log", GI, "Show the commit history"),
    e("maintenance", Main, "Run maintenance tasks on a repository"),
    c("merge", GH, "Combine two or more lines of development"),
    c("mv", GW, "Move or rename a file, directory or symlink"),
    e("notes", Main, "Attach notes to objects"),
    c(
        "pull",
        GR,
        "Fetch and integrate another repository or branch",
    ),
    c(
        "push",
        GR,
        "Send local commits and refs to another repository",
    ),
    e("range-diff", Main, "Compare two ranges of commits"),
    c("rebase", GH, "Replay commits onto another base"),
    c(
        "reset",
        GH,
        "Move HEAD, and optionally the index, to another commit",
    ),
    c(
        "restore",
        GW,
        "Restore files in the working tree or the index",
    ),
    e("revert", Main, "Create commits that undo earlier ones"),
    c("rm", GW, "Remove files from the working tree and the index"),
    e("scalar", Main, "Manage large repositories"),
    e("shortlog", Main, "Summarize the history by author"),
    c("show", GI, "Show objects such as commits, tags and trees"),
    e(
        "sparse-checkout",
        Main,
        "Limit the working tree to some paths",
    ),
    e("stash", Main, "Put working tree changes aside"),
    c("status", GI, "Show what is staged, changed and untracked"),
    e("submodule", Main, "Manage repositories nested in this one"),
    c("switch", GH, "Change to another branch"),
    c("tag", GH, "Create, list, delete or verify tags"),
    e("worktree", Main, "Manage additional working trees"),
    // Ancillary manipulators
    e(
        "config",
        AncillaryManipulators,
        "Read and write configuration",
    ),
    e(
        "fast-export",
        AncillaryManipulators,
        "Export history as a stream",
    ),
    e(
        "fast-import",
        AncillaryManipulators,
        "Import history from a stream",
    ),
    e("filter-branch", AncillaryManipulators, "Rewrite branches"),
    e(
        "mergetool",
        AncillaryManipulators,
        "Resolve conflicts with a merge tool",
    ),
    e(
        "pack-refs",
        AncillaryManipulators,
        "Pack refs into one file",
    ),
    e("prune", AncillaryManipulators, "Delete unreachable objects"),
    e(
        "reflog",
        AncillaryManipulators,
        "Show and manage the reflog",
    ),
    e(
        "refs",
        AncillaryManipulators,
        "Low-level access to the refs",
    ),
    e(
        "remote",
        AncillaryManipulators,
        "Manage tracked repositories",
    ),
    e(
        "repack",
        AncillaryManipulators,
        "Combine objects into packs",
    ),
    e(
        "replace",
        AncillaryManipulators,
        "Replace objects with others",
    ),
    // Ancillary interrogators
    e(
        "annotate",
        AncillaryInterrogators,
        "Annotate lines with commit data",
    ),
    e(
        "blame",
        AncillaryInterrogators,
        "Show who last changed each line",
    ),
    e(
        "bugreport",
        AncillaryInterrogators,
        "Collect information for a bug report",
    ),
    e(
        "count-objects",
        AncillaryInterrogators,
        "Count objects and their disk usage",
    ),
    e(
        "diagnose",
        AncillaryInterrogators,
        "Collect diagnostic information",
    ),
    e(
        "difftool",
        AncillaryInterrogators,
        "Show differences with a diff tool",
    ),
    e("fsck", AncillaryInterrogators, "Check the object database"),
    e("gitweb", AncillaryInterrogators, "Web interface to Git"),
    e("help", AncillaryInterrogators, "Show help about Git"),
    e(
        "instaweb",
        AncillaryInterrogators,
        "Browse the repository in gitweb",
    ),
    e(
        "merge-tree",
        AncillaryInterrogators,
        "Merge without touching the index or files",
    ),
    e(
        "rerere",
        AncillaryInterrogators,
        "Reuse recorded conflict resolutions",
    ),
    e(
        "show-branch",
        AncillaryInterrogators,
        "Show branches and their commits",
    ),
    e(
        "verify-commit",
        AncillaryInterrogators,
        "Check commit signatures",
    ),
    e("verify-tag", AncillaryInterrogators, "Check tag signatures"),
    e("version", AncillaryInterrogators, "Show the version"),
    e(
        "whatchanged",
        AncillaryInterrogators,
        "Show logs with the changes of each commit",
    ),
    // Interacting with others
    e("archimport", Foreign, "Import a GNU Arch repository"),
    e(
        "cvsexportcommit",
        Foreign,
        "Export a commit to a CVS checkout",
    ),
    e("cvsimport", Foreign, "Import a CVS repository"),
    e("cvsserver", Foreign, "CVS server emulator"),
    e("imap-send", Foreign, "Upload patches to an IMAP folder"),
    e("p4", Foreign, "Exchange history with Perforce"),
    e("quiltimport", Foreign, "Apply a quilt patch series"),
    e(
        "request-pull",
        Foreign,
        "Summarize changes for a pull request",
    ),
    e("send-email", Foreign, "Send patches by e-mail"),
    e("svn", Foreign, "Exchange history with Subversion"),
    // Plumbing manipulators
    e(
        "apply",
        PlumbingManipulators,
        "Apply a patch to files or the index",
    ),
    e(
        "checkout-index",
        PlumbingManipulators,
        "Copy files from the index to the working tree",
    ),
    e(
        "commit-graph",
        PlumbingManipulators,
        "Write and verify commit-graph files",
    ),
    e(
        "commit-tree",
        PlumbingManipulators,
        "Create a commit object",
    ),
    e(
        "hash-object",
        PlumbingManipulators,
        "Compute an object id, optionally store the object",
    ),
    e(
        "index-pack",
        PlumbingManipulators,
        "Build the index of a pack",
    ),
    e(
        "merge-file",
        PlumbingManipulators,
        "Three-way merge of one file",
    ),
    e(
        "merge-index",
        PlumbingManipulators,
        "Run a merge program on unmerged files",
    ),
    e("mktag", PlumbingManipulators, "Create a tag object"),
    e(
        "mktree",
        PlumbingManipulators,
        "Create a tree object from ls-tree output",
    ),
    e(
        "multi-pack-index",
        PlumbingManipulators,
        "Manage multi-pack-index files",
    ),
    e(
        "pack-objects",
        PlumbingManipulators,
        "Write a pack of objects",
    ),
    e(
        "prune-packed",
        PlumbingManipulators,
        "Delete loose objects that are in packs",
    ),
    e(
        "read-tree",
        PlumbingManipulators,
        "Read trees into the index",
    ),
    e(
        "replay",
        PlumbingManipulators,
        "Replay commits onto a new base (experimental)",
    ),
    e(
        "symbolic-ref",
        PlumbingManipulators,
        "Read, change or delete symbolic refs",
    ),
    e(
        "unpack-objects",
        PlumbingManipulators,
        "Unpack objects from a pack",
    ),
    e(
        "update-index",
        PlumbingManipulators,
        "Change the index directly",
    ),
    e(
        "update-ref",
        PlumbingManipulators,
        "Change the object a ref points to",
    ),
    e(
        "write-tree",
        PlumbingManipulators,
        "Write the index as a tree object",
    ),
    // Plumbing interrogators
    e(
        "cat-file",
        PlumbingInterrogators,
        "Show the content or type of objects",
    ),
    e(
        "cherry",
        PlumbingInterrogators,
        "Find commits not yet applied upstream",
    ),
    e(
        "diff-files",
        PlumbingInterrogators,
        "Compare the working tree with the index",
    ),
    e(
        "diff-index",
        PlumbingInterrogators,
        "Compare a tree with the working tree or index",
    ),
    e(
        "diff-pairs",
        PlumbingInterrogators,
        "Diff pairs of blobs given on standard input",
    ),
    e("diff-tree", PlumbingInterrogators, "Compare two trees"),
    e(
        "for-each-ref",
        PlumbingInterrogators,
        "Print information about each ref",
    ),
    e(
        "for-each-repo",
        PlumbingInterrogators,
        "Run a command in a list of repositories",
    ),
    e("format-rev", PlumbingInterrogators, "Format revisions"),
    e(
        "get-tar-commit-id",
        PlumbingInterrogators,
        "Read the commit id from a git archive",
    ),
    e(
        "last-modified",
        PlumbingInterrogators,
        "Show when files were last changed",
    ),
    e(
        "ls-files",
        PlumbingInterrogators,
        "List files in the index and working tree",
    ),
    e(
        "ls-remote",
        PlumbingInterrogators,
        "List the refs of a remote repository",
    ),
    e(
        "ls-tree",
        PlumbingInterrogators,
        "List the contents of a tree",
    ),
    e(
        "merge-base",
        PlumbingInterrogators,
        "Find common ancestors for a merge",
    ),
    e("name-rev", PlumbingInterrogators, "Name commits after refs"),
    e(
        "pack-redundant",
        PlumbingInterrogators,
        "Find redundant packs",
    ),
    e(
        "repo",
        PlumbingInterrogators,
        "Show information about the repository",
    ),
    e(
        "rev-list",
        PlumbingInterrogators,
        "List commits in reverse chronological order",
    ),
    e(
        "rev-parse",
        PlumbingInterrogators,
        "Parse revisions and other arguments",
    ),
    e(
        "show-index",
        PlumbingInterrogators,
        "Show the contents of a pack index",
    ),
    e(
        "show-ref",
        PlumbingInterrogators,
        "List refs of the local repository",
    ),
    e(
        "unpack-file",
        PlumbingInterrogators,
        "Write a blob to a temporary file",
    ),
    e("var", PlumbingInterrogators, "Show a logical Git variable"),
    e("verify-pack", PlumbingInterrogators, "Check packed objects"),
    // Syncing repositories
    e("daemon", Syncing, "Simple server for Git repositories"),
    e(
        "fetch-pack",
        Syncing,
        "Receive missing objects from another repository",
    ),
    e("http-backend", Syncing, "Server side of Git over HTTP"),
    e("send-pack", Syncing, "Push objects to another repository"),
    e(
        "update-server-info",
        Syncing,
        "Update files for dumb servers",
    ),
    // Internal helpers
    e("check-attr", Helpers, "Show gitattributes information"),
    e(
        "check-ignore",
        Helpers,
        "Show which ignore rules match paths",
    ),
    e(
        "check-mailmap",
        Helpers,
        "Map names and e-mails through the mailmap",
    ),
    e(
        "check-ref-format",
        Helpers,
        "Check that a ref name is well formed",
    ),
    e("column", Helpers, "Print data in columns"),
    e("credential", Helpers, "Get and store user credentials"),
    e(
        "credential-cache",
        Helpers,
        "Keep credentials in memory for a while",
    ),
    e("credential-store", Helpers, "Keep credentials in a file"),
    e("fmt-merge-msg", Helpers, "Write a merge commit message"),
    e("hook", Helpers, "Run hooks"),
    e(
        "interpret-trailers",
        Helpers,
        "Add or parse commit message trailers",
    ),
    e(
        "mailinfo",
        Helpers,
        "Extract a patch and authorship from an e-mail",
    ),
    e("mailsplit", Helpers, "Split a mailbox into single messages"),
    e("merge-one-file", Helpers, "Standard helper for merge-index"),
    e("patch-id", Helpers, "Compute ids for patches"),
    e("sh-i18n", Helpers, "Translation helpers for shell scripts"),
    e("sh-setup", Helpers, "Setup code for shell scripts"),
    e("stripspace", Helpers, "Remove extra whitespace"),
    e("url-parse", Helpers, "Split Git URLs into their parts"),
    // User-facing interfaces
    e("attributes", UserInterfaces, "Settings per path"),
    e("cli", UserInterfaces, "Command-line conventions"),
    e(
        "hooks",
        UserInterfaces,
        "Programs Git runs at certain points",
    ),
    e("ignore", UserInterfaces, "Files Git should not track"),
    e(
        "mailmap",
        UserInterfaces,
        "Canonical names and e-mail addresses",
    ),
    e("modules", UserInterfaces, "Submodule settings"),
    e(
        "repository-layout",
        UserInterfaces,
        "Files and directories of a repository",
    ),
    e(
        "revisions",
        UserInterfaces,
        "How to name revisions and ranges",
    ),
    // Developer-facing interfaces
    e("format-bundle", DeveloperInterfaces, "Bundle file format"),
    e(
        "format-chunk",
        DeveloperInterfaces,
        "Chunk-based file formats",
    ),
    e(
        "format-commit-graph",
        DeveloperInterfaces,
        "Commit-graph file format",
    ),
    e("format-index", DeveloperInterfaces, "Index file format"),
    e(
        "format-pack",
        DeveloperInterfaces,
        "Pack and pack index formats",
    ),
    e("format-signature", DeveloperInterfaces, "Signature formats"),
    e(
        "protocol-capabilities",
        DeveloperInterfaces,
        "Capabilities of protocols v0 and v1",
    ),
    e(
        "protocol-common",
        DeveloperInterfaces,
        "Common protocol elements",
    ),
    e("protocol-http", DeveloperInterfaces, "Protocols over HTTP"),
    e(
        "protocol-pack",
        DeveloperInterfaces,
        "Pack transfer protocols",
    ),
    e(
        "protocol-v2",
        DeveloperInterfaces,
        "Wire protocol version 2",
    ),
    // Guides
    e(
        "core-tutorial",
        Guide,
        "Tutorial for developers on Git's internals",
    ),
    e(
        "credentials",
        Guide,
        "How Git asks for user names and passwords",
    ),
    e("cvs-migration", Guide, "Git for former CVS users"),
    e("datamodel", Guide, "The data model of Git"),
    e("diffcore", Guide, "How diff output can be tuned"),
    e("everyday", Guide, "The commands needed every day"),
    e("faq", Guide, "Frequently asked questions"),
    e("glossary", Guide, "Terms used by Git"),
    e("namespaces", Guide, "Namespaces in one repository"),
    e(
        "remote-helpers",
        Guide,
        "Programs for other kinds of remote repositories",
    ),
    e("submodules", Guide, "Repositories inside repositories"),
    e("tutorial", Guide, "Introduction to Git"),
    e("tutorial-2", Guide, "Introduction to Git, part two"),
    e("workflows", Guide, "Recommended ways of working with Git"),
];

/// Built-ins that may be shadowed by aliases (`--list-cmds=deprecated`).
/// rgit implements none of git's deprecated commands.
pub const DEPRECATED: &[&str] = &[];

/// The catalogue entry of `name`.
pub fn lookup(name: &str) -> Option<&'static Entry> {
    CATALOG.iter().find(|e| e.name == name)
}

/// Is `name` one of the commands `git help` shows as common?
pub fn is_common(name: &str) -> bool {
    lookup(name).is_some_and(|e| e.group.is_some())
}

/// Entries of a category that rgit implements, sorted by name.
pub fn implemented_in(category: Category) -> Vec<&'static Entry> {
    let mut v: Vec<&'static Entry> = CATALOG
        .iter()
        .filter(|e| e.category == category)
        .filter(|e| category.is_document() || commands::find(e.name).is_some())
        .collect();
    v.sort_by_key(|e| e.name);
    v
}

/// Names of the executable `git-<name>` programs in `dir`.
pub fn programs_in(dir: &Path) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for ent in rd.flatten() {
        let file = ent.file_name();
        let Some(file) = file.to_str() else { continue };
        let Some(name) = file.strip_prefix("git-") else {
            continue;
        };
        #[cfg(windows)]
        let name = match name.strip_suffix(".exe") {
            Some(n) => n,
            None => continue,
        };
        let path = ent.path();
        if !name.is_empty()
            && crate::external::is_executable(&path)
            && !crate::external::is_self(&path)
        {
            out.insert(name.to_string());
        }
    }
    out
}

/// The `$PATH` entries, an empty entry meaning the current directory.
pub fn path_dirs() -> Vec<PathBuf> {
    std::env::var_os("PATH")
        .map(|p| {
            std::env::split_paths(&p)
                .map(|d| {
                    if d.as_os_str().is_empty() {
                        PathBuf::from(".")
                    } else {
                        d
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `main`: the built-ins and the programs in the exec path.
pub fn main_commands(ctx: &Context) -> BTreeSet<String> {
    let mut set = programs_in(&ctx.exec_path);
    set.extend(commands::COMMANDS.iter().map(|c| c.name.to_string()));
    set
}

/// `others`: `git-<name>` programs on `$PATH` that are not main commands.
pub fn other_commands(ctx: &Context, main: &BTreeSet<String>) -> BTreeSet<String> {
    let mut set = BTreeSet::new();
    for dir in path_dirs() {
        if dir == ctx.exec_path {
            continue;
        }
        set.extend(programs_in(&dir));
    }
    set.retain(|n| !main.contains(n));
    set
}

/// Is `name` a git command or document known to the catalogue, a built-in
/// or an installed program (for choosing the manual page)?
pub fn is_command(ctx: &Context, name: &str) -> bool {
    if lookup(name).is_some_and(|e| !e.category.is_document()) || commands::find(name).is_some() {
        return true;
    }
    let main = main_commands(ctx);
    main.contains(name) || other_commands(ctx, &main).contains(name)
}
