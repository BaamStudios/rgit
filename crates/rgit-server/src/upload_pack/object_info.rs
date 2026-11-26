// SPDX-License-Identifier: MIT
//! The `object-info` command: size and type of objects, without their
//! content (`transfer.advertiseObjectInfo`).

use std::io::{Read, Write};

use rgit_hash::ObjectId;

use super::serve::{END_WITHOUT_FLUSH, Session};
use super::{Result, messages as msg};

pub(crate) fn run<R: Read, W: Write>(s: &mut Session<'_, R, W>, lines: &[String]) -> Result<()> {
    let mut size = false;
    let mut kind = false;
    let mut ids: Vec<ObjectId> = Vec::new();
    // A bad request is answered with an `ERR` packet, not by dying.
    let mut problem: Option<String> = None;
    for line in lines {
        match line.as_str() {
            "size" => size = true,
            "type" => kind = true,
            END_WITHOUT_FLUSH => {
                problem = Some(format!("object-info: {}", msg::expected_flush_after("object-info")));
                break;
            }
            _ => match line.strip_prefix("oid ") {
                Some(hex) => match ObjectId::from_hex(s.repo.hash, hex.as_bytes()) {
                    Ok(id) => ids.push(id),
                    Err(_) => {
                        problem = Some(msg::object_info_expected_oid(hex));
                        break;
                    }
                },
                None => {
                    problem = Some(msg::object_info_unexpected_line(line));
                    break;
                }
            },
        }
    }
    if let Some(problem) = problem {
        s.out.error(&problem);
        return s.out.flush();
    }
    // The lines of this command carry no LF.
    if size {
        s.out.data(b"size")?;
    }
    if kind {
        s.out.data(b"type")?;
    }
    for id in ids {
        let mut line = id.to_hex();
        match s.repo.odb.header(&id).ok().flatten() {
            // An unknown object: the id and a space, whatever was asked.
            None => line.push(' '),
            Some(header) => {
                if size {
                    line.push_str(&format!(" {}", header.size));
                }
                if kind {
                    line.push_str(&format!(" {}", header.kind.name()));
                }
            }
        }
        s.out.data(line.as_bytes())?;
    }
    s.out.flush()
}
