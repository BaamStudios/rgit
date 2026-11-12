// SPDX-License-Identifier: MIT
//! Batch front-end for differential testing against `git`.
//!
//! `date-probe approx <now> <tz>`      reads strings, prints approxidate results
//! `date-probe parse <tz>`             reads strings, prints `<ts> <tz>` or `None`
//! `date-probe show <mode> <now> <tz>` reads `<ts> <tz>` lines, prints them formatted

use std::io::BufRead;

use rgit_date::{DateContext, TimeZone};

fn zone(name: &str) -> TimeZone {
    TimeZone::get(name)
        .or_else(|_| TimeZone::posix(name))
        .expect("time zone")
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let stdin = std::io::stdin();
    let lines = stdin.lock().lines().map(|l| l.unwrap());
    match args[0].as_str() {
        "approx" => {
            let ctx = DateContext::new(args[1].parse().unwrap(), zone(&args[2]));
            for l in lines {
                println!("{}", rgit_date::approxidate(&l, &ctx));
            }
        }
        "parse" => {
            let ctx = DateContext::new(rgit_date::now_from_env(), zone(&args[1]));
            for l in lines {
                match rgit_date::parse_date(&l, &ctx) {
                    Some((t, tz)) => println!("{}", rgit_date::format_raw(t, tz)),
                    None => println!("None"),
                }
            }
        }
        "show" => {
            let mode = rgit_date::parse_date_mode(&args[1], false).unwrap();
            let ctx = DateContext::new(args[2].parse().unwrap(), zone(&args[3]));
            for l in lines {
                let (t, tz) = l.split_once(' ').unwrap();
                match rgit_date::show_date(t.parse().unwrap(), tz.parse().unwrap(), &mode, &ctx) {
                    Ok(s) => println!("{s}"),
                    Err(e) => println!("fatal: {e}"),
                }
            }
        }
        _ => panic!("unknown command"),
    }
}
