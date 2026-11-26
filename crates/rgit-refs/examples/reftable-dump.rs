// SPDX-License-Identifier: MIT
//! Prints the blocks and records of reftable files (debugging aid).

use std::sync::Arc;

use rgit_refs::reftable::{RefValue, Table};

fn main() {
    for path in std::env::args().skip(1) {
        let data = std::fs::read(&path).expect("read");
        let table = Arc::new(Table::parse(path.clone(), data).expect("parse"));
        println!(
            "{path}: v{} block_size={} update_index={}..{} hash={} len={}",
            table.version(),
            table.block_size(),
            table.min_update_index(),
            table.max_update_index(),
            table.hash_kind(),
            table.file_len(),
        );
        for b in table.blocks().expect("blocks") {
            println!(
                "  block {} @{} len={} restarts={}",
                b.typ as char, b.offset, b.len, b.restarts
            );
        }
        let mut refs = table.refs();
        while let Some(r) = refs.next_record().expect("ref") {
            let value = match &r.value {
                RefValue::Deletion => "deletion".to_owned(),
                RefValue::Direct(id) => id.to_hex(),
                RefValue::Peeled(id, p) => format!("{id} peeled {p}"),
                RefValue::Symbolic(t) => format!("-> {}", String::from_utf8_lossy(t)),
            };
            println!(
                "  ref {} @{} {value}",
                String::from_utf8_lossy(&r.name),
                r.update_index
            );
        }
        for o in table.objs().expect("objs") {
            let hex: String = o.prefix.iter().map(|b| format!("{b:02x}")).collect();
            println!("  obj {hex} {:?}", o.offsets);
        }
        let mut logs = table.logs();
        while let Some(l) = logs.next_record().expect("log") {
            match &l.update {
                None => println!(
                    "  log {} @{} deletion",
                    String::from_utf8_lossy(&l.refname),
                    l.update_index
                ),
                Some(u) => println!(
                    "  log {} @{} {} {} {} <{}> {} {} {:?}",
                    String::from_utf8_lossy(&l.refname),
                    l.update_index,
                    u.old,
                    u.new,
                    String::from_utf8_lossy(&u.name),
                    String::from_utf8_lossy(&u.email),
                    u.time,
                    u.tz_offset,
                    String::from_utf8_lossy(&u.message)
                ),
            }
        }
    }
}
