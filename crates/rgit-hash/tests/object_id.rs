// SPDX-License-Identifier: MIT

use std::cmp::Ordering;

use rgit_hash::{Error, HashKind, ObjectId, Prefix};

const BLOB1: &str = "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391";
const BLOB256: &str = "473a0f4c3be8a93681a267e3b1e9a7dcda1185436fe141f7749120a303721813";

#[test]
fn kind_properties() {
    assert_eq!(HashKind::default(), HashKind::Sha1);
    assert_eq!(HashKind::Sha1.byte_len(), 20);
    assert_eq!(HashKind::Sha256.hex_len(), 64);
    for kind in HashKind::ALL {
        assert_eq!(HashKind::from_name(kind.name()), Some(kind));
        assert_eq!(kind.to_string().parse::<HashKind>().unwrap(), kind);
        assert_eq!(HashKind::from_format_id(kind.format_id()), Some(kind));
        assert_eq!(HashKind::from_byte_len(kind.byte_len()), Some(kind));
        assert_eq!(HashKind::from_hex_len(kind.hex_len()), Some(kind));
        assert!(kind.null().is_null());
        assert_eq!(kind.null().kind(), kind);
        assert!(!kind.empty_tree().is_null());
    }
    assert_eq!(HashKind::Sha1.format_id(), 0x7368_6131);
    assert_eq!(HashKind::Sha256.format_id(), 0x7332_3536);
    assert_eq!(HashKind::from_name("SHA1"), None);
    assert_eq!(
        "md5".parse::<HashKind>(),
        Err(Error::UnknownHashKind("md5".into()))
    );
}

#[test]
fn hex_roundtrip() {
    for (s, kind) in [(BLOB1, HashKind::Sha1), (BLOB256, HashKind::Sha256)] {
        let id: ObjectId = s.parse().unwrap();
        assert_eq!(id.kind(), kind);
        assert_eq!(id.to_string(), s);
        assert_eq!(id.to_hex(), s);
        assert_eq!(id, kind.empty_blob());
        assert_eq!(ObjectId::from_hex(kind, s.as_bytes()).unwrap(), id);
        assert_eq!(ObjectId::from_bytes(kind, id.as_bytes()).unwrap(), id);
        assert_eq!(ObjectId::from_bytes_any(id.as_bytes()).unwrap(), id);
        // Upper case is accepted, output is lower case.
        let upper: ObjectId = s.to_ascii_uppercase().parse().unwrap();
        assert_eq!(upper, id);
    }
}

#[test]
fn hex_errors() {
    assert_eq!(
        "e69de29bb2d1d6434b8b29ae775ad8c2e48c539g".parse::<ObjectId>(),
        Err(Error::InvalidHexDigit { position: 39 })
    );
    assert_eq!("e69de29".parse::<ObjectId>(), Err(Error::UnknownLength(7)));
    assert_eq!(
        ObjectId::from_hex(HashKind::Sha256, BLOB1.as_bytes()),
        Err(Error::InvalidLength {
            kind: HashKind::Sha256,
            expected: 64,
            actual: 40
        })
    );
    assert!(ObjectId::from_bytes(HashKind::Sha1, &[0; 19]).is_err());
    assert_eq!(
        ObjectId::from_bytes_any(&[0; 21]),
        Err(Error::UnknownLength(21))
    );
    assert!(
        " e69de29bb2d1d6434b8b29ae775ad8c2e48c539"
            .parse::<ObjectId>()
            .is_err()
    );
}

#[test]
fn display_and_debug() {
    let id: ObjectId = BLOB1.parse().unwrap();
    assert_eq!(format!("{id:.7}"), "e69de29");
    assert_eq!(format!("{id:.100}"), BLOB1);
    assert_eq!(format!("{id:>42}"), format!("  {BLOB1}"));
    assert_eq!(id.to_hex_with_len(4), "e69d");
    assert_eq!(id.to_hex_with_len(99), BLOB1);
    assert_eq!(format!("{id:?}"), format!("Sha1({BLOB1})"));
    let id256 = HashKind::Sha256.empty_blob();
    assert_eq!(format!("{id256:?}"), format!("Sha256({BLOB256})"));
    let mut buf = [0u8; HashKind::MAX_HEX_LEN];
    assert_eq!(id.hex_to_buf(&mut buf), BLOB1);
}

#[test]
fn ordering() {
    let a: ObjectId = "0000000000000000000000000000000000000001".parse().unwrap();
    let b: ObjectId = "0100000000000000000000000000000000000000".parse().unwrap();
    let c: ObjectId = "ff00000000000000000000000000000000000000".parse().unwrap();
    assert!(HashKind::Sha1.null() < a && a < b && b < c);
    // Byte order equals hex string order.
    let mut ids = [c, a, b];
    ids.sort();
    let hexes: Vec<_> = ids.iter().map(|i| i.to_hex()).collect();
    let mut sorted = hexes.clone();
    sorted.sort();
    assert_eq!(hexes, sorted);
    // Kinds sort Sha1 first.
    assert!(c < HashKind::Sha256.null());
}

#[test]
fn prefixes() {
    let id: ObjectId = BLOB1.parse().unwrap();
    for n in 1..=40 {
        let p = Prefix::from_hex(HashKind::Sha1, &BLOB1.as_bytes()[..n]).unwrap();
        assert_eq!(p.hex_len(), n);
        assert_eq!(p.to_string(), &BLOB1[..n]);
        assert!(id.starts_with(&p), "{n}");
        assert_eq!(Prefix::new(id, n).unwrap(), p);
        assert_eq!(p.as_id().to_hex()[..n], BLOB1[..n]);
        assert!(p.as_id().to_hex()[n..].bytes().all(|b| b == b'0'));
    }
    let odd = Prefix::from_hex(HashKind::Sha1, b"e69").unwrap();
    let other: ObjectId = "e6a0000000000000000000000000000000000000".parse().unwrap();
    assert!(!other.starts_with(&odd));
    assert_eq!(odd.cmp_oid(&other), Ordering::Less);
    let lower: ObjectId = "e680000000000000000000000000000000000000".parse().unwrap();
    assert_eq!(odd.cmp_oid(&lower), Ordering::Greater);

    // Uppercase prefix input.
    assert!(id.starts_with(&Prefix::from_hex(HashKind::Sha1, b"E69DE").unwrap()));
    // Other kind never matches.
    assert!(
        !HashKind::Sha256
            .null()
            .starts_with(&Prefix::from_hex(HashKind::Sha1, b"0000").unwrap())
    );

    assert!(Prefix::from_hex(HashKind::Sha1, b"").is_err());
    assert!(Prefix::from_hex(HashKind::Sha1, &[b'a'; 41]).is_err());
    assert!(Prefix::from_hex(HashKind::Sha256, &[b'a'; 64]).is_ok());
    assert_eq!(
        Prefix::from_hex(HashKind::Sha1, b"e6x"),
        Err(Error::InvalidHexDigit { position: 2 })
    );
    assert!(Prefix::new(id, 0).is_err());
    assert_eq!(Prefix::from(id).hex_len(), 40);
}

#[test]
fn prefix_binary_search() {
    let mut ids: Vec<ObjectId> = (0u8..=255)
        .map(|i| {
            let mut b = [0u8; 20];
            b[0] = i;
            b[1] = i.wrapping_mul(37);
            ObjectId::from_bytes(HashKind::Sha1, &b).unwrap()
        })
        .collect();
    ids.sort();
    for (i, id) in ids.iter().enumerate() {
        let p = Prefix::new(*id, 3).unwrap();
        let found = ids
            .binary_search_by(|probe| p.cmp_oid(probe).reverse())
            .unwrap();
        assert_eq!(found, i);
    }
    let missing = Prefix::from_hex(HashKind::Sha1, b"0001").unwrap();
    assert!(
        ids.binary_search_by(|probe| missing.cmp_oid(probe).reverse())
            .is_err()
    );
}
