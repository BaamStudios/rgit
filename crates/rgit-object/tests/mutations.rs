// SPDX-License-Identifier: MIT

//! Fuzz-style robustness: 10k+ truncated, bit-flipped and spliced variants
//! of valid objects must never panic. Whatever parses must round-trip, and
//! the validator must not panic either.

mod common;

use common::Rng;
use rgit_hash::HashKind;
use rgit_object::{Error, Object, ObjectKind, Tree, fsck};

fn seeds(hash: HashKind) -> Vec<(ObjectKind, Vec<u8>)> {
    let t = hash.empty_tree().to_hex();
    let mut tree = Vec::new();
    for (mode, name) in [
        ("100644", "a.c"),
        ("40000", "a"),
        ("120000", "l"),
        ("160000", "s"),
    ] {
        tree.extend_from_slice(format!("{mode} {name}\0").as_bytes());
        tree.extend_from_slice(hash.empty_blob().as_bytes());
    }
    vec![
        (ObjectKind::Tree, tree),
        (
            ObjectKind::Commit,
            format!(
                "tree {t}\nparent {t}\nauthor A U Thor <a@x> 1112911993 -0700\n\
                 committer C <c@x> 1112912053 +0200\nencoding UTF-8\ngpgsig -----BEGIN\n \n x\n \
                 -----END\n\nsubject\n\nbody\n"
            )
            .into_bytes(),
        ),
        (
            ObjectKind::Tag,
            format!(
                "object {t}\ntype tree\ntag v1.0\ntagger T <t@x> 1 +0000\n\nmsg\n\
                 -----BEGIN PGP SIGNATURE-----\nabc\n-----END PGP SIGNATURE-----\n"
            )
            .into_bytes(),
        ),
    ]
}

fn mutate(rng: &mut Rng, data: &[u8]) -> Vec<u8> {
    let mut d = data.to_vec();
    match rng.below(5) {
        0 => d.truncate(rng.below(d.len() + 1)),
        1 => {
            for _ in 0..1 + rng.below(4) {
                let i = rng.below(d.len());
                d[i] ^= 1 << rng.below(8);
            }
        }
        2 => {
            let i = rng.below(d.len());
            d[i] = [0, b'\n', b' ', b'<', b'>', b'0', b'+'][rng.below(7)];
        }
        3 => {
            let i = rng.below(d.len());
            d.remove(i);
        }
        _ => {
            let i = rng.below(d.len());
            let j = rng.below(d.len());
            let piece = data[j..(j + rng.below(16)).min(data.len())].to_vec();
            d.splice(i..i, piece);
        }
    }
    d
}

#[test]
fn ten_thousand_mutations_do_not_panic() {
    let mut rng = Rng::new(0x5eed_1234_abcd);
    let mut parsed = 0;
    let mut rejected = 0;
    for hash in HashKind::ALL {
        for (kind, seed) in seeds(hash) {
            for _ in 0..2000 {
                let d = mutate(&mut rng, &seed);
                for k in ObjectKind::ALL {
                    match Object::parse(k, hash, &d) {
                        Ok(o) => {
                            parsed += 1;
                            assert_eq!(o.serialize(), d, "{k} from {kind}");
                        }
                        Err(_) => rejected += 1,
                    }
                    let _ = fsck::findings(k, hash, &d);
                }
                let _ = Object::parse_with_header(hash, &d);
            }
        }
    }
    assert!(parsed + rejected >= 10_000);
    assert!(rejected > 0);
}

#[test]
fn truncated_trees_give_typed_errors() {
    for hash in HashKind::ALL {
        let (_, tree) = seeds(hash).remove(0);
        let full = Tree::parse(hash, &tree).unwrap();
        let mut boundaries = vec![0];
        for e in &full.entries {
            let last = *boundaries.last().unwrap();
            let len = e.mode.digits().len() + 1 + e.name.len() + 1 + hash.byte_len();
            boundaries.push(last + len);
        }
        for n in 1..tree.len() {
            match Tree::parse(hash, &tree[..n]) {
                Ok(t) => {
                    let k = boundaries
                        .iter()
                        .position(|&b| b == n)
                        .expect("cut on entry boundary");
                    assert_eq!(t.entries, full.entries[..k]);
                }
                Err(e) => assert!(
                    matches!(
                        e,
                        Error::TreeTooShort | Error::TreeMalformedMode | Error::TreeEmptyFilename
                    ),
                    "{n}: {e:?}"
                ),
            }
        }
    }
}

#[test]
fn unterminated_headers_are_errors() {
    for hash in HashKind::ALL {
        for (kind, data) in seeds(hash).into_iter().skip(1) {
            let header_end = data.windows(2).position(|w| w == b"\n\n").unwrap();
            for n in 1..=header_end {
                if data[n - 1] == b'\n' {
                    continue;
                }
                assert_eq!(
                    Object::parse(kind, hash, &data[..n]),
                    Err(Error::UnterminatedHeader),
                    "{kind} cut at {n}"
                );
            }
        }
    }
}
