// SPDX-License-Identifier: MIT
//! The command stream of `fast-import`: command lines (comments skipped,
//! one line of look-ahead), raw `data` blocks and the recent commands for
//! the crash report.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read};

use crate::Die;
use crate::messages::fast_import as msg;

/// How many command lines the crash report shows.
const HISTORY: usize = 100;

pub struct Input<R: Read> {
    reader: BufReader<R>,
    /// The current command line, without its LF.
    pub line: Vec<u8>,
    /// `line` was handed back and is returned by the next [`Input::next`].
    unread: bool,
    /// The end of the stream was reached.
    pub eof: bool,
    /// Recent command lines (comments included, data excluded).
    pub history: VecDeque<Vec<u8>>,
}

fn io_fatal(e: std::io::Error) -> Die {
    Die::Fatal(crate::global::strerror(&e))
}

impl<R: Read> Input<R> {
    pub fn new(r: R) -> Input<R> {
        Input {
            reader: BufReader::with_capacity(1 << 16, r),
            line: Vec::new(),
            unread: false,
            eof: false,
            history: VecDeque::new(),
        }
    }

    /// The current line as text (for messages).
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.line).into_owned()
    }

    fn raw_line(&mut self) -> Result<Option<Vec<u8>>, Die> {
        let mut buf = Vec::new();
        let n = self.reader.read_until(b'\n', &mut buf).map_err(io_fatal)?;
        if n == 0 {
            return Ok(None);
        }
        if buf.last() == Some(&b'\n') {
            buf.pop();
        }
        Ok(Some(buf))
    }

    fn remember(&mut self, line: &[u8]) {
        if self.history.len() == HISTORY {
            self.history.pop_front();
        }
        self.history.push_back(line.to_vec());
    }

    /// Read the next command line, skipping comments. Returns `false` at
    /// the end of the stream (`line` is then empty).
    pub fn next(&mut self) -> Result<bool, Die> {
        if self.unread {
            self.unread = false;
            return Ok(!self.eof || !self.line.is_empty());
        }
        loop {
            match self.raw_line()? {
                None => {
                    self.eof = true;
                    self.line.clear();
                    return Ok(false);
                }
                Some(l) => {
                    self.remember(&l);
                    if l.first() == Some(&b'#') {
                        continue;
                    }
                    self.line = l;
                    return Ok(true);
                }
            }
        }
    }

    /// Hand the current line back to the next [`Input::next`].
    pub fn unread(&mut self) {
        self.unread = true;
    }

    /// Skip one empty line, if the next line is one.
    pub fn skip_optional_lf(&mut self) -> Result<(), Die> {
        if self.unread {
            if self.line.is_empty() && !self.eof {
                self.unread = false;
            }
            return Ok(());
        }
        let buf = self.reader.fill_buf().map_err(io_fatal)?;
        if buf.first() == Some(&b'\n') {
            self.reader.consume(1);
        }
        Ok(())
    }

    /// The length of a `data <count>` command in `line`.
    pub fn data_count(&self) -> Option<u64> {
        let arg = self.line.strip_prefix(b"data ")?;
        if arg.starts_with(b"<<") {
            return None;
        }
        Some(count(arg))
    }

    /// Hand the `len` bytes of a `data <count>` payload to `sink` in
    /// pieces (the optional LF after it is left).
    pub fn stream_data(
        &mut self,
        len: u64,
        sink: &mut dyn FnMut(&[u8]) -> Result<(), Die>,
    ) -> Result<(), Die> {
        let mut left = len;
        while left > 0 {
            let buf = self.reader.fill_buf().map_err(io_fatal)?;
            if buf.is_empty() {
                return Err(Die::Fatal(msg::eof_in_data_count(left)));
            }
            let n = buf.len().min(usize::try_from(left).unwrap_or(usize::MAX));
            sink(&buf[..n])?;
            self.reader.consume(n);
            left -= n as u64;
        }
        Ok(())
    }

    /// The payload of the `data` command in `line`: `data <count>` or
    /// `data <<<delim>`, then an optional LF.
    pub fn read_data(&mut self) -> Result<Vec<u8>, Die> {
        let Some(arg) = self.line.strip_prefix(b"data ") else {
            return Err(Die::Fatal(msg::expected_data(&self.text())));
        };
        let out = if let Some(term) = arg.strip_prefix(b"<<") {
            let term = term.to_vec();
            let mut out = Vec::new();
            loop {
                match self.raw_line()? {
                    None => {
                        return Err(Die::Fatal(msg::eof_in_data_term(
                            &String::from_utf8_lossy(&term),
                        )));
                    }
                    Some(l) if l == term => break,
                    Some(l) => {
                        out.extend_from_slice(&l);
                        out.push(b'\n');
                    }
                }
            }
            out
        } else {
            let len = count(arg);
            let mut out = Vec::with_capacity(len.min(1 << 24) as usize);
            let got = (&mut self.reader)
                .take(len)
                .read_to_end(&mut out)
                .map_err(io_fatal)? as u64;
            if got < len {
                return Err(Die::Fatal(msg::eof_in_data_count(len - got)));
            }
            out
        };
        self.skip_optional_lf()?;
        Ok(out)
    }
}

/// The count of `data <count>` (leading digits; saturating).
fn count(arg: &[u8]) -> u64 {
    let digits = arg.iter().take_while(|c| c.is_ascii_digit()).count();
    let mut len: u64 = 0;
    for &d in &arg[..digits] {
        len = len.saturating_mul(10).saturating_add(u64::from(d - b'0'));
    }
    len
}
