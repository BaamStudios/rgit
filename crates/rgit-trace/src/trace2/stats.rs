// SPDX-License-Identifier: MIT
//! trace2 threads, stopwatch timers and counters.
//!
//! Formats observed from git 2.56.1 (`test-tool trace2 100timer`,
//! `101timer`, `200counter`, `201counter`) and described in
//! `Documentation/technical/api-trace2.adoc`:
//!
//! - `thread_start` / `thread_exit` carry the thread name `th<NN>:<label>`
//!   (numbered from 1 in the order the threads announce themselves);
//!   `thread_exit` has the time since `thread_start` (`t_rel`). The normal
//!   format does not show them.
//! - A thread that used a timer or counter with per-thread summaries writes
//!   `th_timer` / `th_counter` just before its `thread_exit`.
//! - After `exit` and before `atexit` the process writes one `timer` /
//!   `counter` event per timer or counter used, with the totals over all
//!   threads.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use super::{Ev, Location, emit, enabled, secs, with_state};

/// A stopwatch timer: sums the time between [`Timer::start`] and
/// [`Timer::stop`] over all intervals.
#[derive(Debug)]
pub struct Timer {
    pub category: &'static str,
    pub name: &'static str,
    /// Threads announced with [`thread_start`] write their own `th_timer`.
    pub per_thread: bool,
}

/// A counter: sums the values given to [`Counter::add`] (modulo 2^64).
#[derive(Debug)]
pub struct Counter {
    pub category: &'static str,
    pub name: &'static str,
    /// Threads announced with [`thread_start`] write their own `th_counter`.
    pub per_thread: bool,
}

#[derive(Clone, Copy)]
struct TimerStats {
    intervals: u64,
    total: Duration,
    min: Duration,
    max: Duration,
}

impl TimerStats {
    fn add(&mut self, d: Duration) {
        if self.intervals == 0 {
            self.min = d;
            self.max = d;
        } else {
            self.min = self.min.min(d);
            self.max = self.max.max(d);
        }
        self.intervals += 1;
        self.total += d;
    }

    fn merge(&mut self, o: &TimerStats) {
        if o.intervals == 0 {
            return;
        }
        if self.intervals == 0 {
            *self = *o;
            return;
        }
        self.intervals += o.intervals;
        self.total += o.total;
        self.min = self.min.min(o.min);
        self.max = self.max.max(o.max);
    }
}

pub(super) struct TimerSlot {
    timer: &'static Timer,
    stats: TimerStats,
}

pub(super) struct CounterSlot {
    counter: &'static Counter,
    value: u64,
}

fn timer_slot<'a>(slots: &'a mut Vec<TimerSlot>, timer: &'static Timer) -> &'a mut TimerSlot {
    let i = match slots.iter().position(|s| std::ptr::eq(s.timer, timer)) {
        Some(i) => i,
        None => {
            slots.push(TimerSlot {
                timer,
                stats: TimerStats {
                    intervals: 0,
                    total: Duration::ZERO,
                    min: Duration::ZERO,
                    max: Duration::ZERO,
                },
            });
            slots.len() - 1
        }
    };
    &mut slots[i]
}

fn counter_slot<'a>(
    slots: &'a mut Vec<CounterSlot>,
    counter: &'static Counter,
) -> &'a mut CounterSlot {
    let i = match slots.iter().position(|s| std::ptr::eq(s.counter, counter)) {
        Some(i) => i,
        None => {
            slots.push(CounterSlot { counter, value: 0 });
            slots.len() - 1
        }
    };
    &mut slots[i]
}

/// What a thread knows about itself.
#[derive(Default)]
struct Local {
    /// `th<NN>:<label>` after [`thread_start`]; `None`: `main`.
    name: Option<String>,
    start: Option<Instant>,
    /// Timers started and not yet stopped by this thread.
    running: Vec<(&'static Timer, Instant)>,
    /// This thread's own sums (announced threads only).
    timers: Vec<TimerSlot>,
    counters: Vec<CounterSlot>,
}

thread_local! {
    static LOCAL: RefCell<Local> = RefCell::new(Local::default());
}

/// The name of the calling thread in the events.
pub(super) fn thread_name() -> String {
    LOCAL
        .try_with(|l| l.borrow().name.clone())
        .ok()
        .flatten()
        .unwrap_or_else(|| "main".to_string())
}

fn announced() -> bool {
    LOCAL
        .try_with(|l| l.borrow().name.is_some())
        .unwrap_or(false)
}

impl Timer {
    /// Start an interval (ignored while this thread's interval of the same
    /// timer is still running).
    pub fn start(&'static self) {
        if !enabled() {
            return;
        }
        let _ = LOCAL.try_with(|l| {
            let mut l = l.borrow_mut();
            if !l.running.iter().any(|(t, _)| std::ptr::eq(*t, self)) {
                l.running.push((self, Instant::now()));
            }
        });
    }

    /// End the interval started by [`Timer::start`] on this thread.
    pub fn stop(&'static self) {
        if !enabled() {
            return;
        }
        let now = Instant::now();
        let Ok(Some(d)) = LOCAL.try_with(|l| {
            let mut l = l.borrow_mut();
            let i = l.running.iter().position(|(t, _)| std::ptr::eq(*t, self))?;
            let (_, started) = l.running.remove(i);
            let d = now.duration_since(started);
            if l.name.is_some() {
                timer_slot(&mut l.timers, self).stats.add(d);
            }
            Some(d)
        }) else {
            return;
        };
        if !announced() {
            with_state(|s| timer_slot(&mut s.timers, self).stats.add(d));
        }
    }
}

impl Counter {
    /// Add `value` to the counter.
    pub fn add(&'static self, value: u64) {
        if !enabled() {
            return;
        }
        let local = LOCAL
            .try_with(|l| {
                let mut l = l.borrow_mut();
                if l.name.is_none() {
                    return false;
                }
                let slot = counter_slot(&mut l.counters, self);
                slot.value = slot.value.wrapping_add(value);
                true
            })
            .unwrap_or(false);
        if !local {
            with_state(|s| {
                let slot = counter_slot(&mut s.counters, self);
                slot.value = slot.value.wrapping_add(value);
            });
        }
    }
}

/// `thread_start`: the calling thread names itself `th<NN>:<label>` in the
/// following events and keeps its own timer and counter sums until
/// [`thread_exit`].
#[track_caller]
pub fn thread_start(label: &str) {
    let loc = Location::caller();
    let Some(n) = with_state(|s| {
        s.threads += 1;
        s.threads
    }) else {
        return;
    };
    let _ = LOCAL.try_with(|l| {
        let mut l = l.borrow_mut();
        l.name = Some(format!("th{n:02}:{label}"));
        l.start = Some(Instant::now());
    });
    emit(loc, |s| {
        let mut ev = Ev::new("thread_start");
        ev.t_abs = Some(s.start.elapsed().as_secs_f64());
        ev
    });
}

/// `thread_exit`: the per-thread summaries of the calling thread, then
/// `thread_exit`; its sums go into the process totals and the thread is
/// `main` again.
#[track_caller]
pub fn thread_exit() {
    let loc = Location::caller();
    let Ok(Some((start, timers, counters))) = LOCAL.try_with(|l| {
        let mut l = l.borrow_mut();
        l.name.as_ref()?;
        Some((
            l.start.unwrap_or_else(Instant::now),
            std::mem::take(&mut l.timers),
            std::mem::take(&mut l.counters),
        ))
    }) else {
        return;
    };
    for t in timers.iter().filter(|t| t.timer.per_thread) {
        emit(loc, |_| timer_event("th_timer", t.timer, &t.stats));
    }
    for c in counters.iter().filter(|c| c.counter.per_thread) {
        emit(loc, |_| counter_event("th_counter", c.counter, c.value));
    }
    let t_rel = start.elapsed().as_secs_f64();
    emit(loc, |s| {
        let mut ev = Ev::new("thread_exit");
        ev.t_abs = Some(s.start.elapsed().as_secs_f64());
        ev.t_rel = Some(t_rel);
        ev.json.float("t_rel", t_rel);
        ev
    });
    with_state(|s| {
        for t in &timers {
            timer_slot(&mut s.timers, t.timer).stats.merge(&t.stats);
        }
        for c in &counters {
            let slot = counter_slot(&mut s.counters, c.counter);
            slot.value = slot.value.wrapping_add(c.value);
        }
    });
    let _ = LOCAL.try_with(|l| {
        let mut l = l.borrow_mut();
        l.name = None;
        l.start = None;
    });
}

/// The process totals (`timer`, `counter`), written before `atexit`.
pub(super) fn summaries(loc: Location) {
    let Some((timers, counters)) = with_state(|s| {
        (
            s.timers
                .iter()
                .filter(|t| t.stats.intervals > 0)
                .map(|t| (t.timer, t.stats))
                .collect::<Vec<_>>(),
            s.counters
                .iter()
                .map(|c| (c.counter, c.value))
                .collect::<Vec<_>>(),
        )
    }) else {
        return;
    };
    for (timer, stats) in &timers {
        emit(loc, |_| timer_event("timer", timer, stats));
    }
    for (counter, value) in counters {
        emit(loc, |_| counter_event("counter", counter, value));
    }
}

fn timer_event(name: &'static str, timer: &Timer, st: &TimerStats) -> Ev {
    let (total, min, max) = (
        st.total.as_secs_f64(),
        st.min.as_secs_f64(),
        st.max.as_secs_f64(),
    );
    let mut ev = Ev::new(name);
    ev.category = timer.category.to_string();
    let tail = format!(
        "intervals:{} total:{} min:{} max:{}",
        st.intervals,
        secs(total),
        secs(min),
        secs(max)
    );
    ev.normal = Some(format!("{}/{} {tail}", timer.category, timer.name));
    ev.perf = format!("name:{} {tail}", timer.name);
    ev.json
        .str("category", timer.category)
        .str("name", timer.name)
        .uint("intervals", st.intervals)
        .float("t_total", total)
        .float("t_min", min)
        .float("t_max", max);
    ev
}

fn counter_event(name: &'static str, counter: &Counter, value: u64) -> Ev {
    let mut ev = Ev::new(name);
    ev.category = counter.category.to_string();
    ev.normal = Some(format!(
        "{}/{} value:{value}",
        counter.category, counter.name
    ));
    ev.perf = format!("name:{} value:{value}", counter.name);
    ev.json
        .str("category", counter.category)
        .str("name", counter.name)
        // The event format writes the sum as a signed number (observed:
        // `-3` where the other formats write 18446744073709551613).
        .int("count", value as i64);
    ev
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timer_stats() {
        let mut a = TimerStats {
            intervals: 0,
            total: Duration::ZERO,
            min: Duration::ZERO,
            max: Duration::ZERO,
        };
        a.add(Duration::from_millis(5));
        a.add(Duration::from_millis(2));
        let mut b = a;
        b.add(Duration::from_millis(9));
        a.merge(&b);
        assert_eq!(a.intervals, 5);
        assert_eq!(a.total, Duration::from_millis(23));
        assert_eq!(a.min, Duration::from_millis(2));
        assert_eq!(a.max, Duration::from_millis(9));
    }
}
