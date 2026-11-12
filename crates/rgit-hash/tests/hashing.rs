// SPDX-License-Identifier: MIT

//! Known-answer tests (FIPS 180 examples) and SHA-1 collision detection.

use std::io::Write;

use rgit_hash::{HashKind, Hasher, ObjectId, hash, hash_object, messages, object_hasher};

const ABC_448: &[u8] = b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq";
const ABC_896: &[u8] = b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu";

fn hex(kind: HashKind, data: &[u8]) -> String {
    hash(kind, data).unwrap().to_hex()
}

#[test]
fn sha1_known_answers() {
    let k = HashKind::Sha1;
    assert_eq!(hex(k, b""), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
    assert_eq!(hex(k, b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
    assert_eq!(hex(k, ABC_448), "84983e441c3bd26ebaae4aa1f95129e5e54670f1");
    assert_eq!(hex(k, ABC_896), "a49b2446a02c645bf419f995b67091253a04a259");
    assert_eq!(
        hex(k, &vec![b'a'; 1_000_000]),
        "34aa973cd4c4daa4f61eeb2bdbad27316534016f"
    );
}

#[test]
fn sha256_known_answers() {
    let k = HashKind::Sha256;
    assert_eq!(
        hex(k, b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        hex(k, b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        hex(k, ABC_448),
        "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
    );
    assert_eq!(
        hex(k, ABC_896),
        "cf5b16a778af8380036ce59e7b0492370b249b11e8f07a51afac45037afee9d1"
    );
    assert_eq!(
        hex(k, &vec![b'a'; 1_000_000]),
        "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
    );
}

#[test]
fn streaming_equals_one_shot() {
    let data: Vec<u8> = (0..10_000u32).map(|i| (i * 7 + i / 13) as u8).collect();
    for kind in HashKind::ALL {
        let expected = hash(kind, &data).unwrap();
        for chunk in [1, 3, 63, 64, 65, 1000] {
            let mut h = Hasher::new(kind);
            for c in data.chunks(chunk) {
                h.write_all(c).unwrap();
            }
            assert_eq!(h.kind(), kind);
            assert_eq!(h.finalize().unwrap(), expected, "{kind} chunk {chunk}");
        }
    }
}

#[test]
fn well_known_ids_match_hashing() {
    for kind in HashKind::ALL {
        assert_eq!(hash_object(kind, "blob", b"").unwrap(), kind.empty_blob());
        assert_eq!(hash_object(kind, "tree", b"").unwrap(), kind.empty_tree());
        assert_eq!(hash(kind, b"blob 0\0").unwrap(), kind.empty_blob());
    }
}

#[test]
fn object_header() {
    // Values from `git hash-object` (t1007 "hello"/"example" fixtures).
    let cases = [
        (
            &b"Hello World"[..],
            "5e1c309dae7f45e0f39b1bf3ac3cd9db12e7d689",
            "1e3b6c04d2eeb2b3e45c8a330445404c0b7cc7b257e2b097167d26f5230090c4",
        ),
        (
            &b"This is an example"[..],
            "ddd3f836d3e3fbb7ae289aa9ae83536f76956399",
            "b44fe1fe65589848253737db859bd490453510719d7424daab03daf0767b85ae",
        ),
    ];
    for (data, sha1, sha256) in cases {
        assert_eq!(
            hash_object(HashKind::Sha1, "blob", data).unwrap().to_hex(),
            sha1
        );
        assert_eq!(
            hash_object(HashKind::Sha256, "blob", data)
                .unwrap()
                .to_hex(),
            sha256
        );
        let mut h = object_hasher(HashKind::Sha1, "blob", data.len() as u64);
        h.update(data);
        assert_eq!(h.finalize().unwrap().to_hex(), sha1);
    }
}

/// The first 320 bytes of `shattered-1.pdf` (Stevens et al., "The first
/// collision for full SHA-1", 2017, <https://shattered.io>): a 192 byte
/// prefix followed by the two near-collision blocks. That is enough to
/// trigger collision detection.
const SHATTERED_1_PREFIX: &str = concat!(
    "255044462d312e330a25e2e3cfd30a0a0a312030206f626a0a3c3c2f57696474",
    "682032203020522f4865696768742033203020522f547970652034203020522f",
    "537562747970652035203020522f46696c7465722036203020522f436f6c6f72",
    "53706163652037203020522f4c656e6774682038203020522f42697473506572",
    "436f6d706f6e656e7420383e3e0a73747265616d0affd8fffe00245348412d31",
    "20697320646561642121212121852fec092339759c39b1a1c63c4c97e1fffe01",
    "7346dc9166b67e118f029ab621b2560ff9ca67cca8c7f85ba84c79030c2b3de2",
    "18f86db3a90901d5df45c14f26fedfb3dc38e96ac22fe7bd728f0e45bce046d2",
    "3c570feb141398bb552ef5a0a82be331fea48037b8b5d71f0e332edf93ac3500",
    "eb4ddc0decc1a864790c782c76215660dd309791d06bd0af3f98cda4bc4629b1",
);

fn shattered_prefix() -> Vec<u8> {
    (0..SHATTERED_1_PREFIX.len() / 2)
        .map(|i| u8::from_str_radix(&SHATTERED_1_PREFIX[2 * i..2 * i + 2], 16).unwrap())
        .collect()
}

#[test]
fn shattered_prefix_is_detected() {
    let data = shattered_prefix();
    assert_eq!(data.len(), 320);
    let err = hash(HashKind::Sha1, &data).unwrap_err();
    // The reported id is the plain SHA-1 of the input.
    assert_eq!(err.id.to_hex(), "f92d74e3874587aaf443d1db961d4e26dde13e9c");
    assert_eq!(
        err.to_string(),
        format!(
            "{}f92d74e3874587aaf443d1db961d4e26dde13e9c",
            messages::COLLISION
        )
    );
    assert!(err.to_string().contains("collision"));

    // Without the attack blocks nothing is detected.
    assert!(hash(HashKind::Sha1, &data[..256]).is_ok());
    // SHA-256 is not affected.
    assert!(hash(HashKind::Sha256, &data).is_ok());
}

#[test]
fn shattered_prefix_as_blob_is_not_detected() {
    // The object header shifts the input off the attack's block alignment
    // and chaining value, so `git hash-object` on the PDFs succeeds; so must we.
    let data = shattered_prefix();
    assert!(hash_object(HashKind::Sha1, "blob", &data).is_ok());
}

/// The full PDF is not part of this repository. Set `RGIT_SHATTERED_PDF`
/// to the path of `shattered-1.pdf` to run this check (t0013 expectations).
#[test]
fn shattered_full_pdf() {
    let Some(path) = std::env::var_os("RGIT_SHATTERED_PDF") else {
        eprintln!("RGIT_SHATTERED_PDF not set, skipping");
        return;
    };
    let data = std::fs::read(path).unwrap();
    let err = hash(HashKind::Sha1, &data).unwrap_err();
    assert_eq!(
        err.id,
        "38762cf7f55934b34d179ae6a4c80cadccbb7f0a"
            .parse::<ObjectId>()
            .unwrap()
    );
    // `git hash-object shattered-1.pdf` (blob header) succeeds with this id.
    assert_eq!(
        hash_object(HashKind::Sha1, "blob", &data).unwrap().to_hex(),
        "ba9aaa145ccd24ef760cf31c74d8f7ca1a2e47b0"
    );
}
