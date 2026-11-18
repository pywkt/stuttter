//! Where to read: the offset `d`, in beats behind "now".
//!
//! Everything stuttter does reduces to recording the input (`history.rs`)
//! and, per output sample, choosing how far behind the playhead to read it.
//! This module is that choice for one block of the Time row.
//!
//! A block is a recipe for a read head ([`BlockPath`]):
//!
//! - an **anchor** — where in the past the head starts, given as the *lag*
//!   `a ≥ 0` in beats behind the block's start (0 = the block's own start);
//! - a **speed** ramped linearly from `from` to `to` across the block
//!   (tape style: pitch follows speed), clamped to ±[`MAX_SPEED`];
//! - optionally a **loop** of `ℓ` beats: every `ℓ` beats the head goes back
//!   to the anchor — that is a repeat.
//!
//! For a block starting at beat `s`, lasting `B` beats, at phase `p` in
//! `[0, B]`, the head reads beat `r = s + p − d(p)` with
//!
//! ```text
//! V(p) = from·p + (to − from)·p² / 2B   distance travelled since the block start
//! W(p) = V(p) − V(c)                     distance since the last loop restart,
//!        c = p − (p mod ℓ)               (c = 0 without a loop)
//! d(p) = p − W(p) + a + A
//! ```
//!
//! `A ≥ 0` is the smallest extra shift that keeps `d ≥ 0` — the plugin never
//! reads the future, which is why it adds no latency. It is solved once per
//! block in closed form: `W(p) − p` is a parabola on each loop cycle, and
//! with `|v| ≤ 2` only the first cycle can need a shift (on cycle `k ≥ 1`,
//! `W(p) − p ≤ 2(p − kℓ) − p < (1 − k)ℓ ≤ 0`), so its endpoints and vertex
//! are the only candidates.
//!
//! The old fixed actions are special cases, with identical results:
//! Repeat = loop at speed 1 (`a = 0` from the block start, `a = ℓ` before
//! it), and the five [`SpeedPreset`]s are unlooped ramps.
//!
//! Pure: no nice-plug types, no state, no allocation.

/// The fastest the read head may travel, either way, as a multiple of the
/// playhead's speed: double speed forwards (+2) to double speed backwards
/// (−2).
pub const MAX_SPEED: f64 = 2.0;

/// Folds any speed into `−MAX_SPEED..=MAX_SPEED`; NaN reads as 1 (live
/// speed), the one value that cannot surprise anyone.
pub fn clamp_speed(speed: f64) -> f64 {
    if speed.is_nan() {
        return 1.0;
    }

    // The lint wants `clamp`; a NaN never reaches here, see above.
    #[allow(clippy::manual_clamp)]
    speed.max(-MAX_SPEED).min(MAX_SPEED)
}

/// The five speed shapes worth a name. A block whose speeds are one of
/// these pairs (and that does not loop) is labelled with the name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpeedPreset {
    /// 0.5 → 0.5: an octave down, falling behind.
    HalfSpeed,
    /// 2 → 2: an octave up, landing back on live at the block's end.
    DoubleSpeed,
    /// −1 → −1: backwards at pitch through the audio before the block.
    Reverse,
    /// 1 → 0: from live speed to a standstill.
    TapeStop,
    /// 0 → 1: from a standstill up to live speed.
    TapeStart,
}

impl SpeedPreset {
    /// Every preset, in the order the GUI lists them.
    pub const ALL: [SpeedPreset; 5] = [
        SpeedPreset::HalfSpeed,
        SpeedPreset::DoubleSpeed,
        SpeedPreset::Reverse,
        SpeedPreset::TapeStop,
        SpeedPreset::TapeStart,
    ];

    /// The `(from, to)` speeds this preset names.
    pub fn speeds(self) -> (f64, f64) {
        match self {
            SpeedPreset::HalfSpeed => (0.5, 0.5),
            SpeedPreset::DoubleSpeed => (2.0, 2.0),
            SpeedPreset::Reverse => (-1.0, -1.0),
            SpeedPreset::TapeStop => (1.0, 0.0),
            SpeedPreset::TapeStart => (0.0, 1.0),
        }
    }

    /// The preset whose speeds are exactly `(from, to)`, if any.
    pub fn of_speeds(from: f64, to: f64) -> Option<SpeedPreset> {
        SpeedPreset::ALL.into_iter().find(|preset| preset.speeds() == (from, to))
    }
}

/// One block's read head, solved once when the walk enters the block and
/// then evaluated per sample with [`offset`](BlockPath::offset).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BlockPath {
    /// Speed at the block's start, clamped.
    from: f64,
    /// Speed at the block's end, clamped.
    to: f64,
    /// The block's length in beats, positive and finite.
    block: f64,
    /// The loop length in beats, positive and finite, or `None`.
    looping: Option<f64>,
    /// `a + A`: the anchor's lag behind the block start plus the shift that
    /// keeps the head out of the future.
    lead: f64,
}

impl BlockPath {
    /// The path of a block `block_beats` long reading at `from → to`,
    /// looping every `looping` beats if given, with its anchor `lag` beats
    /// before the block's start.
    ///
    /// Nonsense never panics: a block with no extent plays live, a loop that
    /// is not a positive finite length is no loop, and a lag that is not a
    /// non-negative finite number is 0.
    pub fn new(from: f64, to: f64, block_beats: f64, looping: Option<f64>, lag: f64) -> Self {
        if !block_beats.is_finite() || block_beats <= 0.0 {
            return BlockPath::live();
        }
        let looping = looping.filter(|length| length.is_finite() && *length > 0.0);
        let lag = if lag.is_finite() { lag.max(0.0) } else { 0.0 };

        // `lead` is not read by `travelled`, so solving for it against a
        // half-built path is sound.
        let mut path = BlockPath {
            from: clamp_speed(from),
            to: clamp_speed(to),
            block: block_beats,
            looping,
            lead: 0.0,
        };
        path.lead = lag + (path.peak_ahead() - lag).max(0.0);
        path
    }

    /// A path that reads live for its whole length: `d = 0`.
    pub fn live() -> Self {
        BlockPath { from: 1.0, to: 1.0, block: 1.0, looping: None, lead: 0.0 }
    }

    /// The offset `d ≥ 0` at `phase` beats into the block. Phases outside
    /// `[0, B]` are held at the nearer end.
    pub fn offset(&self, phase: f64) -> f64 {
        let phase = self.phase_in_block(phase);
        // `A` makes this non-negative over the whole block in exact
        // arithmetic; the `max` is for the last bit of the rounded one.
        (phase - self.since_restart(phase) + self.lead).max(0.0)
    }

    /// The offset at the block's very end, `p = B`, with a loop that fits the
    /// block exactly counted as having just restarted. This is the lag a
    /// following block set to continue from this one starts at.
    pub fn end_offset(&self) -> f64 {
        self.offset(self.block)
    }

    /// The read head's distance travelled since the block start, `V(p)`.
    pub fn travelled(&self, phase: f64) -> f64 {
        let phase = self.phase_in_block(phase);
        self.from * phase + (self.to - self.from) * phase * phase / (2.0 * self.block)
    }

    /// `W(p)`: the distance travelled since the loop last went back to the
    /// anchor (since the block start, without a loop).
    fn since_restart(&self, phase: f64) -> f64 {
        let Some(length) = self.looping else {
            return self.travelled(phase);
        };
        let into_cycle = phase % length;
        if self.from == self.to {
            // Constant speed: the same distance with one rounding step, so a
            // plain repeat's offset is exactly `p − (p mod ℓ)`.
            return self.from * into_cycle;
        }

        self.travelled(phase) - self.travelled(phase - into_cycle)
    }

    /// The most the head would get ahead of "now" with no shift at all:
    /// `max(W(p) − p)` over the first loop cycle (the whole block without a
    /// loop), never less than 0 (`p = 0` gives exactly 0).
    fn peak_ahead(&self) -> f64 {
        let span = match self.looping {
            Some(length) => length.min(self.block),
            None => self.block,
        };
        let mut peak = self.travelled(span) - span;

        if self.to != self.from {
            // Where the speed passes 1, `V(p) − p` turns.
            let vertex = self.block * (1.0 - self.from) / (self.to - self.from);
            if vertex > 0.0 && vertex < span {
                peak = peak.max(self.travelled(vertex) - vertex);
            }
        }

        peak.max(0.0)
    }

    /// `phase` held inside `[0, B]`, NaN at the start.
    fn phase_in_block(&self, phase: f64) -> f64 {
        // The lint wants `clamp`; `max`-then-`min` is what puts a NaN at the
        // start of the block rather than passing it into the read position.
        #[allow(clippy::manual_clamp)]
        phase.max(0.0).min(self.block)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIXTEENTH: f64 = 0.25;
    const EIGHTH: f64 = 0.5;

    /// A plain repeat: loop `slice` at speed 1, anchored `lag` before the
    /// block, over a block long enough never to matter.
    fn repeat(slice: f64, lag: f64) -> BlockPath {
        BlockPath::new(1.0, 1.0, 64.0, Some(slice), lag)
    }

    /// An unlooped ramp from the block start.
    fn ramp(from: f64, to: f64, block: f64) -> BlockPath {
        BlockPath::new(from, to, block, None, 0.0)
    }

    fn preset(preset: SpeedPreset, block: f64) -> BlockPath {
        let (from, to) = preset.speeds();
        ramp(from, to, block)
    }

    /// Dyadic phases across a block, so every assertion can be exact.
    fn phases(block: f64) -> impl Iterator<Item = f64> {
        (0..=64).map(move |step| block * f64::from(step) / 64.0)
    }

    #[test]
    fn the_first_slice_of_a_block_start_repeat_plays_live() {
        for phase in [0.0, 1.0e-9, 0.0625, 0.125, 0.2, 0.249, 0.2499999] {
            assert_eq!(repeat(SIXTEENTH, 0.0).offset(phase), 0.0, "phase {phase}");
        }
        assert_eq!(repeat(SIXTEENTH, 0.0).offset(SIXTEENTH), SIXTEENTH);
    }

    #[test]
    fn a_block_start_repeat_steps_back_one_slice_per_wrap() {
        let cases = [
            (0.0, 0.0),
            (0.125, 0.0),
            (0.25, 0.25),
            (0.375, 0.25),
            (0.5, 0.5),
            (0.6875, 0.5),
            (1.0, 1.0),
            (1.9375, 1.75),
        ];
        for (phase, expected) in cases {
            assert_eq!(repeat(SIXTEENTH, 0.0).offset(phase), expected, "phase {phase}");
        }
    }

    #[test]
    fn a_repeat_anchored_one_slice_back_is_the_old_before_block_repeat() {
        for slice in [SIXTEENTH, EIGHTH, 1.0] {
            for step in 0..32 {
                let phase = f64::from(step) * 0.0625;
                let from_start = repeat(slice, 0.0).offset(phase);
                let before = repeat(slice, slice).offset(phase);
                assert_eq!(before, from_start + slice, "slice {slice}, phase {phase}");
            }
        }
    }

    #[test]
    fn a_repeat_reads_the_same_beat_every_slice() {
        let start = 8.0;
        for lag in [0.0, SIXTEENTH, 1.0] {
            for slice in [SIXTEENTH, EIGHTH, 1.0] {
                let path = repeat(slice, lag);
                for step in 0..16 {
                    let phase = f64::from(step) * 0.03125;
                    let first = start + phase - path.offset(phase);
                    for lap in 1..=8 {
                        let later = phase + f64::from(lap) * slice;
                        assert_eq!(start + later - path.offset(later), first);
                    }
                }
            }
        }
    }

    #[test]
    fn a_triplet_slice_wraps_on_its_own_grid() {
        let slice = 1.0 / 6.0;
        let path = repeat(slice, 0.0);
        for lap in 0..12 {
            let restart = f64::from(lap) * slice;
            let offset = path.offset(restart + slice * 0.5);
            assert!((offset - restart).abs() < 1e-12, "lap {lap}: {offset}");
        }
    }

    #[test]
    fn a_loop_longer_than_the_block_never_restarts() {
        let path = BlockPath::new(1.0, 1.0, 1.0, Some(4.0), 0.0);
        for phase in phases(1.0) {
            assert_eq!(path.offset(phase), 0.0);
        }
    }

    #[test]
    fn half_speed_is_half_the_phase() {
        let path = preset(SpeedPreset::HalfSpeed, 2.0);
        for phase in phases(2.0) {
            assert_eq!(path.offset(phase), phase / 2.0, "phase {phase}");
        }
    }

    #[test]
    fn double_speed_ends_exactly_on_live() {
        let block = 2.0;
        let path = preset(SpeedPreset::DoubleSpeed, block);
        for phase in phases(block) {
            assert_eq!(path.offset(phase), block - phase, "phase {phase}");
        }
        assert_eq!(path.end_offset(), 0.0);
    }

    #[test]
    fn reverse_is_twice_the_phase() {
        let path = preset(SpeedPreset::Reverse, 1.0);
        for phase in phases(1.0) {
            assert_eq!(path.offset(phase), 2.0 * phase, "phase {phase}");
        }
    }

    #[test]
    fn tape_stop_and_start_follow_their_parabolas() {
        let block = 2.0;
        let stop = preset(SpeedPreset::TapeStop, block);
        let start = preset(SpeedPreset::TapeStart, block);
        for phase in phases(block) {
            let half_square = phase * phase / (2.0 * block);
            assert_eq!(stop.offset(phase), half_square, "phase {phase}");
            assert_eq!(start.offset(phase), phase - half_square, "phase {phase}");
        }
        assert_eq!(stop.end_offset(), block / 2.0);
        assert_eq!(start.end_offset(), block / 2.0);
    }

    #[test]
    fn a_ramp_with_its_vertex_inside_the_block_grazes_live_there() {
        let path = ramp(2.0, -2.0, 4.0);
        for phase in phases(4.0) {
            let from_vertex = phase - 1.0;
            assert_eq!(path.offset(phase), from_vertex * from_vertex * 0.5, "phase {phase}");
        }
        assert_eq!(path.offset(1.0), 0.0);
    }

    #[test]
    fn no_path_ever_reads_the_future() {
        let speeds = [-2.0, -1.0, -0.5, 0.0, 0.5, 1.0, 1.5, 2.0];
        for from in speeds {
            for to in speeds {
                for looping in [None, Some(0.25), Some(1.0), Some(1.0 / 3.0)] {
                    for lag in [0.0, 0.25, 3.0] {
                        let path = BlockPath::new(from, to, 2.0, looping, lag);
                        for phase in phases(2.0) {
                            let offset = path.offset(phase);
                            assert!(offset >= 0.0, "{from}->{to} {looping:?} {lag}: {offset}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn the_shift_is_the_smallest_one_that_works() {
        // Somewhere on each path the head comes within a sampling step of
        // live: the touch point can fall between the sampled phases, or at a
        // loop's end, which the head approaches but never reaches.
        let speeds = [-2.0, -1.0, 0.0, 0.5, 1.0, 1.5, 2.0];
        for from in speeds {
            for to in speeds {
                for looping in [None, Some(0.5)] {
                    let path = BlockPath::new(from, to, 2.0, looping, 0.0);
                    let lowest = phases(2.0)
                        .map(|phase| path.offset(phase))
                        .fold(f64::INFINITY, f64::min);
                    assert!(lowest < 0.05, "{from}->{to} {looping:?}: lowest {lowest}");
                }
            }
        }
    }

    #[test]
    fn a_rising_repeat_speeds_up_each_slice_and_stays_in_the_past() {
        // A 1/16 loop ramping 1 → 2 over two beats from the block start: the
        // first slices would outrun live, so the anchor is pulled back, and
        // every slice still reads from the one anchor.
        let path = BlockPath::new(1.0, 2.0, 2.0, Some(SIXTEENTH), 0.0);
        let start = 4.0;
        let anchor = start - path.offset(0.0);
        assert!(anchor < start, "a speed above 1 needs an earlier anchor");

        for lap in 0..8 {
            let restart = f64::from(lap) * SIXTEENTH;
            let read = start + restart - path.offset(restart);
            assert!((read - anchor).abs() < 1e-12, "lap {lap} restarts at the anchor");
        }
        // Later slices cover more material in the same time: higher pitch.
        let span = |lap: f64| {
            let restart = lap * SIXTEENTH;
            let end = restart + SIXTEENTH * 0.999;
            (end - path.offset(end)) - (restart - path.offset(restart))
        };
        assert!(span(7.0) > span(0.0));
    }

    #[test]
    fn a_lagged_anchor_needs_no_shift_until_the_head_would_catch_up() {
        // Double speed anchored a whole block back never reaches live early.
        let path = BlockPath::new(2.0, 2.0, 1.0, None, 1.0);
        assert_eq!(path.offset(0.0), 1.0);
        assert_eq!(path.end_offset(), 0.0);
        // A small lag is topped up to the shift double speed needs anyway.
        let short = BlockPath::new(2.0, 2.0, 1.0, None, 0.25);
        assert_eq!(short.offset(0.0), 1.0);
    }

    #[test]
    fn a_loop_that_fits_its_block_ends_back_on_its_anchor() {
        // Three repeats of one beat: the end offset is the block plus the
        // anchor's lag, so what follows starts on the repeated beat again.
        let path = BlockPath::new(1.0, 1.0, 3.0, Some(1.0), 0.0);
        assert_eq!(path.end_offset(), 3.0);
        let lagged = BlockPath::new(1.0, 1.0, 3.0, Some(1.0), 0.5);
        assert_eq!(lagged.end_offset(), 3.5);
    }

    #[test]
    fn a_phase_outside_the_block_is_held_at_its_ends() {
        let path = preset(SpeedPreset::Reverse, 1.0);
        assert_eq!(path.offset(-1.0), 0.0);
        assert_eq!(path.offset(f64::NAN), 0.0);
        assert_eq!(path.offset(5.0), 2.0);
    }

    #[test]
    fn speeds_are_clamped_to_the_limit() {
        assert_eq!(clamp_speed(3.0), MAX_SPEED);
        assert_eq!(clamp_speed(-3.0), -MAX_SPEED);
        assert_eq!(clamp_speed(f64::NAN), 1.0);
        assert_eq!(ramp(5.0, 5.0, 1.0), ramp(2.0, 2.0, 1.0));
    }

    #[test]
    fn nonsense_falls_back_to_something_safe() {
        assert_eq!(BlockPath::new(1.0, 1.0, 0.0, None, 0.0), BlockPath::live());
        assert_eq!(BlockPath::new(1.0, 1.0, f64::NAN, None, 0.0), BlockPath::live());
        let no_loop = BlockPath::new(1.0, 1.0, 1.0, Some(f64::NAN), f64::NAN);
        for phase in phases(1.0) {
            assert_eq!(no_loop.offset(phase), 0.0);
        }
        assert_eq!(BlockPath::new(-1.0, -1.0, 1.0, Some(-0.5), -3.0).offset(0.5), 1.0);
    }

    #[test]
    fn the_presets_are_found_by_their_speeds() {
        for preset in SpeedPreset::ALL {
            let (from, to) = preset.speeds();
            assert_eq!(SpeedPreset::of_speeds(from, to), Some(preset));
        }
        assert_eq!(SpeedPreset::of_speeds(1.0, 1.0), None);
        assert_eq!(SpeedPreset::of_speeds(1.0, 0.1), None);
    }
}
