//! When to feed the watchdog.
//!
//! The firmware runs the WDT in window mode, as AN2632 §3.2 recommends: a
//! `wdr` in the closed window after the last one resets the chip, and so
//! does no `wdr` by the end of the open window that follows it. So a loop
//! that's stuck fails, and so does a jump into the wrong code that happens
//! to land on the feed.
//!
//! The WDT and the 64 Hz tick both run off the ultra-low-power oscillator,
//! so a tick is always 16 WDT clocks however far that oscillator drifts,
//! and the window can be sized in ticks. Feeding every tick would land on
//! a window edge whatever the setting, so the loop feeds every third tick:
//! 48 clocks, halfway through a 32-clock open window that starts 32 clocks
//! after the last feed.
//!
//! Window mode only starts with the first `wdr` (DS40002204A §19.3.3.2).
//! Until then the WDT most likely times out after the open window alone
//! (the datasheet implies it rather than saying so), so the first
//! pass feeds it straight away, which also lines the windows up with the
//! ticks.

use crate::TICKS_PER_SECOND;

/// Cycles of the WDT's 1.024 kHz clock in one tick.
pub const CLOCKS_PER_TICK: u16 = 1024 / TICKS_PER_SECOND;
/// The closed window after each feed, in WDT clocks. `hw::init` sets
/// `WDT.CTRLA.WINDOW` to match.
pub const CLOSED_CLOCKS: u16 = 32;
/// The open window after the closed one, in WDT clocks. `hw::init` sets
/// `WDT.CTRLA.PERIOD` to match.
pub const OPEN_CLOCKS: u16 = 32;
/// How many ticks apart the feeds are.
pub const FEED_TICKS: u8 = 3;

// A feed lands strictly inside the open window, and the first one comes
// before the open window alone runs out: the first tick arrives within a
// tick of `hw::init` turning the WDT on. `wdr` takes two to three WDT clocks to sync
// (§19.3.6), the same for every feed, so it shifts the windows without
// changing the gap between feeds.
const _: () = {
    let gap = FEED_TICKS as u16 * CLOCKS_PER_TICK;
    assert!(gap > CLOSED_CLOCKS && gap < CLOSED_CLOCKS + OPEN_CLOCKS);
    assert!(CLOCKS_PER_TICK < OPEN_CLOCKS);
};

/// Counts ticks between feeds.
#[derive(Clone, Copy, Debug, Default)]
pub struct Feeder {
    /// Ticks until the next feed.
    wait: u8,
}

impl Feeder {
    pub const fn new() -> Self {
        Self { wait: 0 }
    }

    /// Call once a tick, at the end of the loop. True when this pass feeds
    /// the watchdog: the first one, then every [`FEED_TICKS`]th.
    pub fn tick(&mut self) -> bool {
        if self.wait == 0 {
            self.wait = FEED_TICKS - 1;
            true
        } else {
            self.wait -= 1;
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tick_is_16_wdt_clocks() {
        assert_eq!(CLOCKS_PER_TICK, 16);
    }

    #[test]
    fn feeds_on_the_first_tick() {
        assert!(Feeder::new().tick());
    }

    #[test]
    fn feeds_every_third_tick_after_that() {
        let mut feeder = Feeder::new();
        let feeds: [bool; 10] = core::array::from_fn(|_| feeder.tick());
        let (t, f) = (true, false);
        assert_eq!(feeds, [t, f, f, t, f, f, t, f, f, t]);
    }

    #[test]
    fn every_feed_lands_mid_window() {
        let middle = CLOSED_CLOCKS + OPEN_CLOCKS / 2;
        let mut feeder = Feeder::new();
        let mut last = None;
        for tick in 0..1000u16 {
            if feeder.tick() {
                if let Some(last) = last {
                    assert_eq!((tick - last) * CLOCKS_PER_TICK, middle, "tick {tick}");
                }
                last = Some(tick);
            }
        }
        assert!(last.is_some());
    }
}
