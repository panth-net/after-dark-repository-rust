//! Holding emulated time to the wall clock.
//!
//! # Why this is needed at all
//!
//! [`ad_host_v2::CYCLES_PER_TICK`] advances the Mac's 60 Hz tick from *executed
//! cycles*, modelling an ~8 MHz 68000. That is deliberate and right for the lab:
//! Musashi's cycle counts are deterministic, so a replay is reproducible and a
//! module that paces itself inside `DrawFrame` still sees time move.
//!
//! It is wrong for a person watching the screen. Nothing in that scheme is tied
//! to a clock, so the emulator runs as fast as the host can go — on a modern
//! machine, tens of times faster than an 8 MHz 68000. Every module that paces
//! itself on `TickCount` then runs tens of times too fast, and the host CPU sits
//! at 100% because there is never a reason to stop.
//!
//! Both symptoms were reported from the same session: Lunatic Fringe cycling
//! through its attract screens too quickly to start a game, and the window being
//! sluggish to switch away from. One cause.
//!
//! # How it paces
//!
//! Against an absolute schedule — tick *n* is due at `start + n/60 s` — not by
//! sleeping a fixed slice per tick. A per-tick sleep accumulates every rounding
//! error and every overshoot, so it drifts slow and never recovers. An absolute
//! target self-corrects: a tick that ran long is followed by a shorter sleep.
//!
//! The tick number comes from the caller, which takes it from the emulator, so
//! pacing does not depend on how often the caller happens to be called.

use std::time::{Duration, Instant};

/// Ticks per second.
///
/// A real Macintosh runs at 60.15 Hz, but the emulator derives its tick from
/// `CYCLES_PER_TICK = 8_000_000 / 60`, so the clock here matches the cycle model
/// rather than the hardware. The 0.25% difference is far below what anyone can
/// see, and having the two definitions agree is worth more than closing it.
pub const TICKS_PER_SECOND: u32 = 60;

/// Nanoseconds per tick.
const TICK_NANOS: u64 = 1_000_000_000 / TICKS_PER_SECOND as u64;

/// How far behind schedule we tolerate before giving up on the backlog.
///
/// Being behind is normal and mostly self-correcting — a garbage collection, a
/// slow frame, the window being dragged. Being behind by a *lot* means the
/// process was not running at all: the laptop slept, or the app was suspended in
/// the background. Trying to catch up on thirty seconds of ticks would then run
/// the module at maximum speed for as long as it took to work through them,
/// which is the exact failure this type exists to prevent. Past this point the
/// backlog is abandoned and the schedule restarts from now.
const MAX_LAG: Duration = Duration::from_millis(250);

/// The longest a single call will block, however far ahead the tick is.
///
/// [`MAX_LAG`] guards being *behind*. This guards being far *ahead*, which is the
/// same failure wearing the opposite sign and does far more damage. Sleeping is
/// how pacing works, so this is not a performance tweak: the window's only input
/// path runs inside the present hook that calls this, so every millisecond spent
/// here is a millisecond the player cannot press a key, and macOS draws the
/// spinning wheel for it.
///
/// A tick far in the future means the module executed a long burst of emulated
/// time between two presents — a death sequence, or a high-score table being
/// sorted and drawn. The emulated clock advances from *executed cycles*, so a
/// burst that takes the host half a second can claim ten emulated seconds, and
/// honouring that literally sleeps for all ten with the window shut.
///
/// Worse, it sustains itself whenever the module is *waiting for a key*: the key
/// can only arrive from the hook that is asleep, so the game spins, burns more
/// emulated time, and computes an even longer sleep. Lunatic Fringe's name entry
/// after a game is exactly that shape, and it froze the application outright.
///
/// The wait is **clamped, not abandoned**, and the difference is the whole point.
/// Abandoning it — restarting the schedule, as a backlog does — leaves the module
/// running unpaced for exactly the stretch it was ahead, and a module's own
/// timeouts are measured in the clock this paces. Lunatic Fringe's high-score
/// entry then expired in a fraction of a second and returned to the title screen
/// before anyone could type a name. Clamping keeps the schedule, so the remainder
/// is slept off across the calls that follow and the window is serviced between
/// them: still paced, never blocked for long.
const MAX_SLEEP: Duration = Duration::from_millis(100);

/// Paces a tick sequence against real time.
#[derive(Debug)]
pub struct Pacer {
    /// When the current schedule's `epoch_tick` notionally happened.
    epoch: Instant,
    /// Tick the epoch corresponds to, or `None` until the first tick is seen.
    ///
    /// Anchoring on the *first tick observed* rather than on zero is essential,
    /// and getting it wrong was a real freeze. The tick counter belongs to the
    /// Toolbox and has been running since it was built — through `Initialize` and
    /// `Blank`, which for a module like Lunatic Fringe is a great deal of work. So
    /// the first tick a player paces is never 0; it might be 300 or 30,000. With a
    /// zero epoch the very first call computes "tick 30,000 is due 500 seconds
    /// after I started" and sleeps for eight minutes, holding the window's only
    /// input path shut, before pacing correctly forever after.
    epoch_tick: Option<u64>,
    /// Ticks slept through, for reporting.
    slept: u64,
    /// Times the backlog was abandoned, for reporting.
    resyncs: u64,
}

impl Default for Pacer {
    fn default() -> Self {
        Self::new()
    }
}

impl Pacer {
    #[must_use]
    pub fn new() -> Self {
        Self {
            epoch: Instant::now(),
            epoch_tick: None,
            slept: 0,
            resyncs: 0,
        }
    }

    /// Block until `tick` is due.
    ///
    /// Returns immediately when the emulator is already behind, which is what
    /// makes this a ceiling on speed rather than a fixed rate: a module too heavy
    /// to run in real time runs as fast as it can and no faster, and one that is
    /// cheap gives its time back to the rest of the machine.
    pub fn wait_for_tick(&mut self, tick: u32) {
        let tick = u64::from(tick);
        // The first tick seen defines the schedule. It is not tick zero: the clock
        // has been running since the Toolbox was built.
        let Some(epoch_tick) = self.epoch_tick else {
            self.epoch = Instant::now();
            self.epoch_tick = Some(tick);
            return;
        };
        // A tick before the current epoch means the caller restarted a sequence
        // without a new `Pacer`. Treat it as a fresh schedule rather than
        // computing a target in the past and never sleeping again.
        if tick < epoch_tick {
            self.resync(tick);
            return;
        }
        let offset = tick.saturating_sub(epoch_tick);
        let due = self.epoch + Duration::from_nanos(offset.saturating_mul(TICK_NANOS));
        let now = Instant::now();
        if now < due {
            // Clamped, never abandoned. See `MAX_SLEEP`: this call holds the
            // window's only input path shut for as long as it blocks, and the
            // schedule has to survive so the module stays paced.
            std::thread::sleep((due - now).min(MAX_SLEEP));
            self.slept = self.slept.saturating_add(1);
        } else if now.duration_since(due) > MAX_LAG {
            self.resync(tick);
        }
    }

    /// Start a fresh schedule at `tick`, abandoning any backlog.
    fn resync(&mut self, tick: u64) {
        self.epoch = Instant::now();
        self.epoch_tick = Some(tick);
        self.resyncs = self.resyncs.saturating_add(1);
    }

    /// Ticks that had to wait. A healthy session sleeps on nearly every tick;
    /// a number far below the tick count means the module cannot keep up.
    #[must_use]
    pub fn slept(&self) -> u64 {
        self.slept
    }

    /// How many times the backlog was abandoned. Expect zero, or one per
    /// suspend.
    #[must_use]
    pub fn resyncs(&self) -> u64 {
        self.resyncs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_tick_never_sleeps_however_large_it_is() {
        // The regression this exists for: the tick counter belongs to the Toolbox
        // and has been running through Initialize and Blank, so the first tick a
        // player paces is never zero. Anchoring on zero made the first call sleep
        // `tick/60` seconds — with the window's only input path held shut — which
        // read as the whole application freezing.
        for first in [0u32, 300, 30_000, 3_000_000] {
            let mut p = Pacer::new();
            let before = Instant::now();
            p.wait_for_tick(first);
            let waited = before.elapsed();
            assert!(
                waited < Duration::from_millis(20),
                "first tick {first} slept {waited:?}"
            );
            assert_eq!(p.slept(), 0);
        }
    }

    #[test]
    fn a_future_tick_sleeps_until_it_is_due() {
        let mut p = Pacer::new();
        p.wait_for_tick(1_000); // anchor somewhere realistic
        let before = Instant::now();
        // Six ticks past the anchor is 100 ms.
        p.wait_for_tick(1_006);
        let waited = before.elapsed();
        assert!(
            waited >= Duration::from_millis(90),
            "returned after only {waited:?}"
        );
        assert_eq!(p.slept(), 1);
    }

    /// A tick far in the future must not hold the input path shut.
    ///
    /// The reported symptom: dying in Lunatic Fringe froze the application for a
    /// long stretch with the spinning wheel up, the high-score screen appeared,
    /// and typing a name froze it for good. One cause. The emulated clock comes
    /// from executed cycles, so the burst of work at the end of a game advances
    /// it by seconds in a single step; this then slept off the whole difference
    /// inside the present hook, which is the only place keys are read. Name entry
    /// made it permanent — the game waits for a key that cannot arrive until this
    /// returns, and spins, which advances the clock further.
    #[test]
    fn a_tick_far_in_the_future_does_not_hold_the_window_shut() {
        let mut p = Pacer::new();
        p.wait_for_tick(1_000);

        let before = Instant::now();
        // Ten emulated seconds in one step.
        p.wait_for_tick(1_600);
        let waited = before.elapsed();
        assert!(
            waited <= MAX_SLEEP + Duration::from_millis(60),
            "held the input path shut for {waited:?}"
        );

        // Clamped, not abandoned. Restarting the schedule here would leave the
        // module unpaced for the whole stretch it was ahead, and its own timeouts
        // run on this clock: Lunatic Fringe's high-score entry expired instantly
        // and bounced back to the title screen before a name could be typed.
        assert_eq!(p.resyncs(), 0, "the schedule must survive a forward jump");
        assert!(p.slept() >= 1);

        // Still ahead, so the next call blocks again — the remainder is slept off
        // across calls, with the window serviced in between, rather than in one
        // go with the window shut.
        let before = Instant::now();
        p.wait_for_tick(1_600);
        assert!(
            before.elapsed() >= Duration::from_millis(50),
            "the backlog must still be being paced, not skipped"
        );
    }

    /// The cap is above an ordinary frame by a wide margin, so normal pacing
    /// never trips it: one tick is ~16.7 ms.
    #[test]
    fn the_cap_leaves_ordinary_pacing_untouched() {
        assert!(
            MAX_SLEEP >= Duration::from_nanos(3 * TICK_NANOS),
            "a few frames of catch-up must still be slept in one call"
        );
    }

    #[test]
    fn the_schedule_is_absolute_so_it_cannot_drift_slow() {
        let mut p = Pacer::new();
        p.wait_for_tick(1_000);
        let before = Instant::now();
        // Ticks in sequence must take the same ~100 ms as jumping straight to the
        // end: each target comes from the epoch, not from the previous return.
        for t in 1_001..=1_006 {
            p.wait_for_tick(t);
        }
        let waited = before.elapsed();
        assert!(
            waited >= Duration::from_millis(90) && waited < Duration::from_millis(200),
            "six ticks took {waited:?}, expected about 100 ms"
        );
    }

    #[test]
    fn a_long_stall_abandons_the_backlog_instead_of_racing() {
        let mut p = Pacer::new();
        p.wait_for_tick(0);
        p.wait_for_tick(1);
        // Pretend the process was suspended: the epoch is far in the past, so
        // the next tick is overdue by more than MAX_LAG.
        p.epoch = Instant::now() - Duration::from_secs(30);
        let before = Instant::now();
        p.wait_for_tick(2);
        assert!(before.elapsed() < Duration::from_millis(20), "must not sleep");
        assert_eq!(p.resyncs(), 1, "the backlog must be dropped, not worked off");
        // And the schedule now runs from now, so the next tick sleeps again.
        let before = Instant::now();
        p.wait_for_tick(8);
        assert!(before.elapsed() >= Duration::from_millis(90));
    }

    #[test]
    fn restarting_a_sequence_resyncs_rather_than_running_free() {
        let mut p = Pacer::new();
        p.wait_for_tick(600);
        p.wait_for_tick(601);
        // A second module in the same process starting again from a low tick.
        p.wait_for_tick(3);
        assert_eq!(p.resyncs(), 1);
        let before = Instant::now();
        p.wait_for_tick(9);
        assert!(
            before.elapsed() >= Duration::from_millis(90),
            "after a resync the schedule must pace again"
        );
    }
}
