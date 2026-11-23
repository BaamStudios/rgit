// SPDX-License-Identifier: MIT
//! The standard progress display of [`super::index_pack`]: `Receiving
//! objects` (or `Indexing objects`) and `Resolving deltas`, drawn by
//! `rgit-progress`.

use rgit_progress::Progress as Meter;

use super::messages as m;
use super::{Phase, Progress};

/// Received packs show their throughput only once this much arrived (the
/// reference shows none for small packs).
const FIRST_READ: u64 = 1 << 20;

/// Progress meters for indexing a pack.
pub struct PackMeter {
    reading_title: String,
    show: bool,
    throughput: bool,
    phase: Phase,
    meter: Option<Meter>,
    unpack_limit: u64,
    unpacking: bool,
}

impl PackMeter {
    /// Meters titled `reading_title` (default: `Receiving objects` when
    /// `stream`, else `Indexing objects`) and `Resolving deltas`; `show`:
    /// draw them (else only trace2 sees them). Received streams show their
    /// throughput.
    pub fn new(reading_title: Option<&str>, stream: bool, show: bool) -> PackMeter {
        let title = reading_title.unwrap_or(if stream {
            m::PROGRESS_RECEIVING
        } else {
            m::PROGRESS_INDEXING
        });
        PackMeter {
            reading_title: title.to_string(),
            show,
            throughput: stream,
            phase: Phase::Reading,
            meter: None,
            unpack_limit: 0,
            unpacking: false,
        }
    }

    /// A fetched pack with fewer than `limit` objects is shown the way git
    /// shows unpacking it into loose objects (`fetch.unpackLimit`): a
    /// delayed `Unpacking objects` meter and no delta meter.
    pub fn unpack_below(mut self, limit: u64) -> PackMeter {
        self.unpack_limit = limit;
        self
    }
}

impl Progress for PackMeter {
    fn start(&mut self, phase: Phase, total: u64) {
        if let Some(old) = self.meter.take() {
            old.stop();
        }
        self.phase = phase;
        if phase == Phase::Reading {
            self.unpacking = total < self.unpack_limit;
        }
        if self.unpacking {
            if phase == Phase::Reading {
                self.meter = Some(Meter::start_delayed(
                    m::PROGRESS_UNPACKING,
                    total,
                    self.show,
                ));
            }
            return;
        }
        let title = match phase {
            Phase::Reading => self.reading_title.as_str(),
            Phase::Resolving => m::PROGRESS_RESOLVING,
        };
        let mut meter = Meter::start(title, total, self.show);
        if phase == Phase::Resolving {
            // git draws the empty state of this meter at once.
            meter.set(0);
        }
        self.meter = Some(meter);
    }

    fn update(&mut self, done: u64, bytes: u64) {
        if let Some(meter) = &mut self.meter {
            if self.throughput
                && self.phase == Phase::Reading
                && (bytes > FIRST_READ || self.unpacking)
            {
                meter.throughput(bytes);
            }
            meter.set(done);
        }
    }

    fn finish(&mut self, suffix: Option<String>) {
        if let Some(meter) = self.meter.take() {
            match suffix {
                Some(s) => meter.stop_with(&s),
                None => meter.stop(),
            }
        }
    }
}
