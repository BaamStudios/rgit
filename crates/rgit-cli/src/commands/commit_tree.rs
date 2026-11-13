// SPDX-License-Identifier: MIT
//! `git commit-tree` (Documentation/git-commit-tree.adoc): create a commit
//! object from a tree, parents and a message.

use std::io::Read;

use rgit_config::SystemEnv;
use rgit_hash::ObjectId;
use rgit_ident::ident::{IdentConfig, Resolver, Role, Strictness};
use rgit_object::{ObjectKind, Signature, SignatureRef};

use crate::messages::write_tree as msg;
use crate::prelude::*;
use crate::repo::Mode;
use crate::repository::Repo;

/// A message piece in command-line order.
enum Piece {
    Message(String),
    File(String),
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let parents_arg: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let pieces: RefCell<Vec<Piece>> = RefCell::new(Vec::new());
    let mut parser = Parser::new(
        msg::COMMIT_TREE_USAGE,
        vec![
            Opt::string_list('p', None, &parents_arg, "parent", msg::HELP_PARENT),
            Opt::callback('m', None, Some("message"), msg::HELP_MESSAGE, |v, _| {
                pieces
                    .borrow_mut()
                    .push(Piece::Message(v.unwrap_or_default().to_string()));
                Ok(())
            }),
            Opt::callback('F', None, Some("file"), msg::HELP_FILE, |v, _| {
                pieces
                    .borrow_mut()
                    .push(Piece::File(v.unwrap_or_default().to_string()));
                Ok(())
            }),
        ],
    );
    let args = parser.parse(argv)?.args;
    if args.len() != 1 {
        return Err(parser.usage_error().into());
    }
    drop(parser);
    let mut repo = Repo::open(ctx, Mode::Required)?;

    let tree_name = &args[0];
    let tree = resolve(&mut repo, tree_name, ObjectKind::Tree)?;
    let mut parents: Vec<ObjectId> = Vec::new();
    for p in parents_arg.borrow().iter() {
        let id = resolve(&mut repo, p, ObjectKind::Commit)?;
        if parents.contains(&id) {
            eprintln!("error: {}", msg::duplicate_parent(&id.to_hex()));
        } else {
            parents.push(id);
        }
    }

    let mut message: Vec<u8> = Vec::new();
    let pieces = pieces.into_inner();
    if pieces.is_empty() {
        std::io::stdin()
            .read_to_end(&mut message)
            .map_err(|_| Die::Fatal(msg::READ_STDIN_FAILED.into()))?;
    }
    for piece in pieces {
        if !message.is_empty() {
            message.push(b'\n');
        }
        match piece {
            Piece::Message(m) => {
                message.extend_from_slice(m.as_bytes());
                if !message.is_empty() && message.last() != Some(&b'\n') {
                    message.push(b'\n');
                }
            }
            Piece::File(f) => {
                if f == "-" {
                    std::io::stdin()
                        .read_to_end(&mut message)
                        .map_err(|_| Die::Fatal(msg::READ_STDIN_FAILED.into()))?;
                } else {
                    let data = std::fs::read(&f).map_err(|e| {
                        Die::Fatal(msg::could_not_read_file(&f, &crate::global::strerror(&e)))
                    })?;
                    message.extend_from_slice(&data);
                }
            }
        }
    }

    let env = SystemEnv::default();
    let icfg = IdentConfig::from_config(&repo.config).map_err(problem)?;
    let resolver = Resolver::new(&icfg, &env);
    let author = signature(&resolver, Role::Author)?;
    let committer = signature(&resolver, Role::Committer)?;
    let encoding = repo
        .config
        .get("i18n.commitencoding")
        .and_then(|e| e.value.as_ref())
        .map(|v| v.as_slice())
        .filter(|e| !e.eq_ignore_ascii_case(b"utf-8") && !e.eq_ignore_ascii_case(b"utf8"))
        .map(|e| e.to_vec());

    let new = rgit_commit::NewCommit {
        tree,
        parents: &parents,
        author: &author,
        committer: &committer,
        encoding: encoding.as_deref(),
        message: &message,
    };
    let oid = rgit_commit::create_commit(repo.odb(), &new)
        .map_err(|e| Die::Fatal(e.to_string()))?;
    println!("{oid}");
    Ok(0)
}

fn resolve(repo: &mut Repo, name: &str, kind: ObjectKind) -> Result<ObjectId, Die> {
    let not_valid = || Die::Fatal(msg::not_valid_object_name(name));
    let id = repo.resolve(name).ok_or_else(not_valid)?;
    repo.peel_to(id, kind).ok_or_else(not_valid)
}

fn signature(resolver: &Resolver<'_>, role: Role) -> Result<Signature, Die> {
    let ident = resolver.ident(role, Strictness::Strict).map_err(|e| {
        eprint!("{}", e.stderr_text());
        Die::Exit(rgit_options::EXIT_FATAL)
    })?;
    let raw = ident.to_bstring();
    SignatureRef::parse(&raw)
        .and_then(|s| s.to_owned())
        .ok_or_else(|| Die::Fatal(String::from_utf8_lossy(&raw).into_owned()))
}

fn problem(p: rgit_ident::Problem) -> Die {
    p.print();
    Die::Exit(rgit_options::EXIT_FATAL)
}
