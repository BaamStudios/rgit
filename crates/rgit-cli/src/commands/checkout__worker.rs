// SPDX-License-Identifier: MIT
//! `git checkout--worker`: the helper process of parallel checkout
//! (Documentation/config/checkout.adoc, `checkout.workers`). rgit checks
//! out in one process and never starts it; run by hand it answers `-h` and
//! fails like git when its input ends before the first request.

use std::io::Read as _;

use crate::messages::helpers as msg;
use crate::prelude::*;
use crate::repo::{self, Mode};

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let prefix = RefCell::new(None);
    let mut parser = Parser::new(
        msg::CHECKOUT_WORKER_USAGE,
        vec![Opt::string(
            None,
            "prefix",
            &prefix,
            "string",
            msg::CHECKOUT_WORKER_H_PREFIX,
        )],
    );
    let args = parser.parse(argv)?.args;
    if !args.is_empty() {
        return Err(parser.usage_error().into());
    }
    drop(parser);
    repo::setup(ctx, Mode::Required)?;
    let mut first = [0u8; 1];
    match std::io::stdin().read(&mut first) {
        Ok(0) | Err(_) => Err(Die::Fatal(msg::HUNG_UP.to_owned())),
        Ok(_) => Err(Die::Fatal(msg::CHECKOUT_WORKER_UNSUPPORTED.to_owned())),
    }
}
