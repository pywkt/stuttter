//! Starter patterns: ready-made patterns a pattern can be filled with from
//! its right-click menu, to start from instead of an empty grid.
//!
//! A fill keeps the pattern's **rate and length** — the grid is the user's —
//! and replaces what is drawn on it: the blocks, the Volume row and the
//! filter, the way Clear does before drawing. Each starter is written in
//! beats and laid onto whatever grid it lands on ([`Starter::fill`]):
//!
//! - **Per bar**, repeated across the pattern: Reverse snares, Half-time.
//!   Written with the drums of a 4/4 bar in mind — the kick on beats 1 and
//!   3, the snare on 2 and 4.
//! - **Across the whole pattern**: Stutter build and Filter sweep, which
//!   stretch to its length, and Tape stop ending, on its last beat.
//! - **Per step**: Gate, every other step of the grid.
//!
//! A bar that does not fit whole at the end of the pattern gets what fits of
//! it. Pure, like the other core modules.

use crate::actions::SpeedPreset;
use crate::pattern::{Action, Block, Filter, Lane, MAX_SOURCE_PASSES, NoteValue, Pattern, Source};

/// One starter pattern.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Starter {
    /// Repeats that get faster across the pattern.
    StutterBuild,
    /// Plays normally, then a tape stop over the last beat.
    TapeStopEnding,
    /// The two snare beats of every bar played backwards.
    ReverseSnares,
    /// Every half bar plays its first beat at half speed.
    HalfTime,
    /// The volume chopping on and off every other step.
    Gate,
    /// A low-pass closing down across the pattern.
    FilterSweep,
}

impl Starter {
    /// Every starter, in the order the menu lists them.
    pub const ALL: [Starter; 6] = [
        Starter::StutterBuild,
        Starter::TapeStopEnding,
        Starter::ReverseSnares,
        Starter::HalfTime,
        Starter::Gate,
        Starter::FilterSweep,
    ];

    /// The starter's name, as the menu shows it.
    pub fn label(self) -> &'static str {
        match self {
            Starter::StutterBuild => "Stutter build",
            Starter::TapeStopEnding => "Tape stop ending",
            Starter::ReverseSnares => "Reverse snares",
            Starter::HalfTime => "Half-time",
            Starter::Gate => "Gate",
            Starter::FilterSweep => "Filter sweep",
        }
    }

    /// What the starter does, for the menu item's hover text.
    pub fn description(self) -> &'static str {
        match self {
            Starter::StutterBuild => {
                "Repeats that get faster across the whole pattern: the first beat over the \
                 first half, then repeats of 1/8, 1/16 and 1/32."
            }
            Starter::TapeStopEnding => {
                "Plays the pattern untouched, then a tape stop slows its last beat to a halt."
            }
            Starter::ReverseSnares => {
                "Beats 2 and 4 of every bar, where the snares usually are, played backwards \
                 from the bar before."
            }
            Starter::HalfTime => {
                "Every half bar plays its first beat at half speed, an octave down: the groove \
                 at half the pace."
            }
            Starter::Gate => "The volume chopping on and off every other step.",
            Starter::FilterSweep => {
                "A low-pass filter closing from fully open to about 220 Hz across the whole \
                 pattern, with a little resonance."
            }
        }
    }

    /// What `grid` becomes when filled with this starter: its rate and its
    /// length, with this starter drawn on them and nothing else.
    pub fn fill(self, grid: &Pattern) -> Pattern {
        let mut pattern = Pattern::new();
        pattern.set_rate(grid.rate());
        pattern.set_length(grid.length());

        let length = pattern.length();
        let beat = steps_per_beat(&pattern);
        let bar = BEATS_PER_BAR * beat;

        match self {
            Starter::StutterBuild => {
                // Half the pattern, a quarter, an eighth and an eighth.
                let loops = [
                    NoteValue::Quarter,
                    NoteValue::Eighth,
                    NoteValue::Sixteenth,
                    NoteValue::ThirtySecond,
                ];
                let at = |eighths: u16| (length * eighths + 4) / 8;
                let edges = [0, at(4), at(6), at(7), length];
                let blocks = loops.iter().enumerate().filter_map(|(index, &looping)| {
                    let (start, end) = (edges[index], edges[index + 1]);
                    (end > start).then(|| Block::new(start, end - start, Action::repeat(looping)))
                });
                pattern.set_blocks(blocks);
            }
            Starter::TapeStopEnding => {
                let start = length.saturating_sub(beat);
                let stop = Action::from_preset(SpeedPreset::TapeStop);
                pattern.set_blocks([Block::new(start, length - start, stop)]);
            }
            Starter::ReverseSnares => {
                // Each block reads backwards from the end of its own beat one
                // bar earlier: the bar before's snare, reversed in its place.
                let reverse = Action::from_preset(SpeedPreset::Reverse);
                let starts = (0..length).step_by(usize::from(bar)).flat_map(|bar_start| {
                    [1, 3].map(|snare| bar_start + snare * beat)
                });
                // Skipped rather than built when past the end, since
                // `Block::new` would clamp it back onto the last step.
                let blocks = starts.filter(|&start| start < length).map(|start| {
                    let source = spot_back(start, bar - beat, length);
                    Block::new(start, beat, reverse.with_source(source))
                });
                pattern.set_blocks(blocks);
            }
            Starter::HalfTime => {
                let half = Action::from_preset(SpeedPreset::HalfSpeed);
                let span = 2 * beat;
                let starts = (0..length).step_by(usize::from(span));
                pattern.set_blocks(starts.map(|start| Block::new(start, span, half)));
            }
            Starter::Gate => {
                let open = |step: u16| if step.is_multiple_of(2) { 1.0 } else { 0.0 };
                pattern.set_lane(Lane::Volume, (0..length).map(open));
            }
            Starter::FilterSweep => {
                pattern.set_filter(Filter::LowPass);
                let last = f32::from(length.saturating_sub(1).max(1));
                let sweep = (0..length).map(|step| 1.0 - SWEEP_DEPTH * f32::from(step) / last);
                pattern.set_lane(Lane::Cutoff, sweep);
                pattern.set_lane(Lane::Resonance, (0..length).map(|_| SWEEP_RESONANCE));
            }
        }

        pattern
    }
}

/// The beats in a bar: the starters are written for 4/4.
const BEATS_PER_BAR: u16 = 4;

/// How far the filter sweep closes, as a share of the Cutoff row: from the
/// top (20 kHz) down to 0.35 of it, about 220 Hz.
const SWEEP_DEPTH: f32 = 0.65;

/// The filter sweep's resonance on every step: a little, so the sweep can be
/// heard moving rather than only dulling.
const SWEEP_RESONANCE: f32 = 0.35;

/// How many of `pattern`'s steps make a beat: 2, 3, 4, 6 or 8, since every
/// rate divides a beat a whole number of times.
fn steps_per_beat(pattern: &Pattern) -> u16 {
    (1.0 / pattern.rate().step_beats()).round().clamp(1.0, 8.0) as u16
}

/// The fixed spot `back` steps before step `start` of a `length`-step
/// pattern, as a [`Source::Spot`] names it: a step, and how many whole passes
/// further back than the last time that step played at or before `start`.
fn spot_back(start: u16, back: u16, length: u16) -> Source {
    let length = i32::from(length.max(1));
    let time = i32::from(start) - i32::from(back);
    let (pass, step) = (time.div_euclid(length), time.rem_euclid(length));

    // The pass `Spot { step, passes: 0 }` means: this one if the step is at
    // or before the start, else the one before.
    let nearest = if step <= i32::from(start) { 0 } else { -1 };
    let passes = (nearest - pass).clamp(0, i32::from(MAX_SOURCE_PASSES));

    // Both are in range by construction: `step` below `length`, a `u16`, and
    // `passes` clamped above.
    Source::Spot { step: step as u16, passes: passes as u8 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pattern::{Rate, cutoff_hz};

    fn grid(rate: Rate, length: u16) -> Pattern {
        let mut pattern = Pattern::new();
        pattern.set_rate(rate);
        pattern.set_length(length);
        pattern
    }

    /// The steps a starter's blocks cover, as `(start, len)`.
    fn spans(pattern: &Pattern) -> Vec<(u16, u16)> {
        pattern.blocks().iter().map(|block| (block.start(), block.len())).collect()
    }

    /// Where a spot source starts reading, in steps from the start of the
    /// pass its block is in — the inverse of [`spot_back`].
    fn spot_time(start: u16, source: Source, length: u16) -> i32 {
        let Source::Spot { step, passes } = source else { panic!("not a spot: {source:?}") };
        let nearest = if step <= start { 0 } else { -1 };
        i32::from(step) + (nearest - i32::from(passes)) * i32::from(length)
    }

    #[test]
    fn a_fill_keeps_the_rate_and_length_on_every_grid() {
        for rate in Rate::ALL {
            for length in 1..=64 {
                for starter in Starter::ALL {
                    let filled = starter.fill(&grid(rate, length));
                    assert_eq!(filled.rate(), rate, "{starter:?} {rate} {length}");
                    assert_eq!(filled.length(), length, "{starter:?} {rate} {length}");
                    for block in filled.blocks() {
                        assert!(block.end() <= length, "{starter:?} {rate} {length}");
                    }
                }
            }
        }
    }

    #[test]
    fn every_starter_has_a_name_and_draws_something_different() {
        let bar = grid(Rate::Sixteenth, 16);
        for (index, starter) in Starter::ALL.iter().enumerate() {
            assert!(!starter.label().is_empty() && !starter.description().is_empty());
            for other in &Starter::ALL[index + 1..] {
                assert_ne!(starter.label(), other.label());
                assert_ne!(starter.fill(&bar), other.fill(&bar), "{starter:?} and {other:?}");
            }
        }
    }

    /// What is drawn before goes: a fill is a clear and then the starter.
    #[test]
    fn a_fill_replaces_what_was_drawn() {
        let mut drawn = grid(Rate::Eighth, 32);
        drawn.add_block(20, 4, Action::repeat(NoteValue::Half));
        drawn.set_level(Lane::Volume, 3, 0.25);
        drawn.set_filter(Filter::HighPass);

        let filled = Starter::TapeStopEnding.fill(&drawn);
        assert_eq!(spans(&filled), [(30, 2)], "only the starter's block");
        assert_eq!(filled.level(Lane::Volume, 3), 1.0);
        assert_eq!(filled.filter(), Filter::Off);
    }

    /// One bar of 1/16: the layout the starters were written as.
    #[test]
    fn on_one_bar_of_sixteenths() {
        let bar = grid(Rate::Sixteenth, 16);
        let fill = |starter: Starter| spans(&starter.fill(&bar));

        assert_eq!(fill(Starter::StutterBuild), [(0, 8), (8, 4), (12, 2), (14, 2)]);
        assert_eq!(fill(Starter::TapeStopEnding), [(12, 4)]);
        assert_eq!(fill(Starter::ReverseSnares), [(4, 4), (12, 4)]);
        assert_eq!(fill(Starter::HalfTime), [(0, 8), (8, 8)]);
        assert_eq!(fill(Starter::Gate), []);
        assert_eq!(fill(Starter::FilterSweep), []);
    }

    /// The case that asked for this: 32 steps of 1/8, four bars.
    #[test]
    fn on_four_bars_of_eighths() {
        let bars = grid(Rate::Eighth, 32);
        let fill = |starter: Starter| spans(&starter.fill(&bars));

        assert_eq!(fill(Starter::StutterBuild), [(0, 16), (16, 8), (24, 4), (28, 4)]);
        assert_eq!(fill(Starter::TapeStopEnding), [(30, 2)], "the last beat");
        let snares: Vec<_> = [2, 6, 10, 14, 18, 22, 26, 30].map(|start| (start, 2)).into();
        assert_eq!(fill(Starter::ReverseSnares), snares, "beats 2 and 4 of every bar");
        let halves: Vec<_> = (0..8).map(|half| (half * 4, 4)).collect();
        assert_eq!(fill(Starter::HalfTime), halves);

        let sweep = Starter::FilterSweep.fill(&bars);
        assert_eq!(sweep.level(Lane::Cutoff, 0), 1.0);
        assert!((150.0..300.0).contains(&cutoff_hz(sweep.level(Lane::Cutoff, 31))));
    }

    #[test]
    fn the_stutter_build_gets_faster() {
        let filled = Starter::StutterBuild.fill(&grid(Rate::Sixteenth, 16));
        let loops: Vec<_> = filled.blocks().iter().map(|block| block.action().looping).collect();
        let expected = [NoteValue::Quarter, NoteValue::Eighth, NoteValue::Sixteenth]
            .into_iter()
            .chain([NoteValue::ThirtySecond])
            .map(Some)
            .collect::<Vec<_>>();
        assert_eq!(loops, expected);
    }

    /// Every reversed snare starts reading at the end of its own beat one
    /// bar earlier, wherever that falls — this pass, the pass before, or
    /// further back on a pattern shorter than a bar.
    #[test]
    fn the_reverse_snares_read_the_bar_before() {
        for rate in Rate::ALL {
            for length in 1..=64 {
                let filled = Starter::ReverseSnares.fill(&grid(rate, length));
                let beat = steps_per_beat(&filled);
                for block in filled.blocks() {
                    assert_eq!(block.action().preset(), Some(SpeedPreset::Reverse));
                    let want = i32::from(block.start()) + i32::from(beat) - 4 * i32::from(beat);
                    let source = block.action().source;
                    let got = spot_time(block.start(), source, length);
                    let reachable = -i32::from(MAX_SOURCE_PASSES + 1) * i32::from(length);
                    if want >= reachable {
                        assert_eq!(got, want, "{rate} {length}: block at {}", block.start());
                    }
                }
            }
        }
    }

    #[test]
    fn the_gate_alternates_and_the_rest_leave_the_volume_alone() {
        let bars = grid(Rate::Eighth, 32);
        let gate = Starter::Gate.fill(&bars);
        for step in 0..32_u16 {
            let expected = if step.is_multiple_of(2) { 1.0 } else { 0.0 };
            assert_eq!(gate.level(Lane::Volume, step), expected, "step {step}");
        }

        for starter in Starter::ALL.into_iter().filter(|&starter| starter != Starter::Gate) {
            let filled = starter.fill(&bars);
            for step in 0..32 {
                assert_eq!(filled.level(Lane::Volume, step), 1.0, "{starter:?} step {step}");
            }
        }
    }

    #[test]
    fn the_filter_sweep_closes_and_only_it_has_a_filter() {
        let sweep = Starter::FilterSweep.fill(&grid(Rate::Sixteenth, 16));
        assert_eq!(sweep.filter(), Filter::LowPass);
        assert_eq!(sweep.level(Lane::Cutoff, 0), 1.0, "it starts fully open");
        for step in 1..16 {
            assert!(sweep.level(Lane::Cutoff, step) < sweep.level(Lane::Cutoff, step - 1));
        }
        let closed = cutoff_hz(sweep.level(Lane::Cutoff, 15));
        assert!((150.0..300.0).contains(&closed), "it closes to about 220 Hz, not {closed}");

        let one = Starter::FilterSweep.fill(&grid(Rate::Sixteenth, 1));
        assert_eq!(one.level(Lane::Cutoff, 0), 1.0, "one step is just open, not NaN");

        for starter in Starter::ALL.into_iter().filter(|&starter| starter != Starter::FilterSweep) {
            assert_eq!(starter.fill(&grid(Rate::Sixteenth, 16)).filter(), Filter::Off);
        }
    }
}
