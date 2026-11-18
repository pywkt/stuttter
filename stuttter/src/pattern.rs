//! What a pattern is: a rate, a length, blocks on the Time row and a volume
//! per step.
//!
//! The sixteen patterns the plugin switches between are sixteen of
//! [`Pattern`]. One of them is a grid of `length` steps, each step `rate`
//! long, with two rows drawn over it: the **Time row**, a sorted run of
//! non-overlapping [`Block`]s each carrying an [`Action`], and the **Volume
//! row**, one level per step. A step with no block over it plays live; a
//! step with a block over it reads the past the way that block's action says
//! (`crate::actions`).
//!
//! # The invariants
//!
//! This module exists to keep four statements true of every `Pattern` that
//! can be built, however it was built:
//!
//! 1. `1 ≤ length ≤ MAX_STEPS`.
//! 2. Blocks are **sorted by `start`** and **never overlap**, every one of
//!    them `len ≥ 1` and inside `0..length`, and there are at most
//!    [`MAX_BLOCKS`] of them.
//! 3. Every level — Volume, Cutoff and Res rows ([`Lane`]) — is a real
//!    number in `0.0..=1.0`.
//! 4. Every speed endpoint is inside ±[`crate::actions::MAX_SPEED`].
//!
//! They hold by construction: the fields are private, every setter clamps
//! its argument and re-establishes the lot, and the two bulk setters
//! ([`Pattern::set_blocks`], [`Pattern::set_volumes`]) are written for the
//! hostile input a loaded snapshot is. Nothing here returns an error for
//! out-of-range input and nothing panics on it: a GUI drag and a corrupt
//! project file both get the nearest thing that is legal — the clamped
//! value, or nothing at all where there is no room.
//!
//! Which is why the audio thread can read a published pattern without
//! checking anything: the blocks it binary-searches are sorted, the volume
//! it multiplies by is finite, and the slice it repeats is a positive number
//! of beats.
//!
//! # What is here
//!
//! The types: [`Rate`], [`NoteValue`], [`Action`], [`Block`] and [`Pattern`]
//! with the named limits and the writer surface the model needs — set rate,
//! set length, set step volume, add / resize / move / re-action / duplicate
//! / delete block, clear. A block's [`Action`] is one recipe for its read
//! head — a [`Source`], an optional loop and a speed ramp — solved into a
//! [`crate::actions::BlockPath`] by [`Pattern::block_path`].
//!
//! And the beat maths over them: [`NoteValue::beats`] and
//! [`Rate::step_beats`] for how long a note and a step are, triplets
//! included; [`Pattern::span_beats`] for the `P` a pattern repeats at; and
//! [`Pattern::position_at`], which turns a project beat into the step under
//! it, the block over that step and the phases [`crate::actions`] is written
//! in terms of — a [`Position`].
//!
//! The sixteen patterns behind a writer mutex are `model.rs`'s. Copying a
//! pattern over another is `Clone`, which the model's copy-pattern writer is
//! built from.

use std::fmt;

use crate::actions::{BlockPath, SpeedPreset, clamp_speed};

/// How many patterns the plugin holds, which is how many positions the
/// `pattern` parameter has above Off.
pub const PATTERN_COUNT: usize = 16;

/// The longest a pattern can be, in steps — and so the size of its volume
/// row.
///
/// A bound rather than a `Vec` length because a hostile snapshot must not be
/// able to make the plugin allocate without limit, and because 64 `f32`s per
/// pattern is 4 KB for all sixteen: cheaper to keep than to grow.
pub const MAX_STEPS: usize = 64;

/// The most blocks one pattern can carry.
///
/// 64 is `MAX_STEPS`: a one-step block on every step of the longest pattern
/// is the densest Time row that can be drawn, so this bound costs the GUI
/// nothing and keeps a loaded snapshot's block list finite.
pub const MAX_BLOCKS: usize = 64;

/// A note value, from a whole bar down to a 1/64th, straight or triplet —
/// what a loop length and a "before" distance are measured in.
///
/// The discriminant is `base × 2 + triplet`, bases longest first, so
/// `ALL[d] == d` for every variant and the stored byte is a table lookup
/// both ways.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum NoteValue {
    /// 1/1 — a whole 4/4 bar, four beats.
    Whole = 0,
    /// 1/1T — two thirds of a bar.
    WholeTriplet = 1,
    /// 1/2 — two beats.
    Half = 2,
    /// 1/2T.
    HalfTriplet = 3,
    /// 1/4 — one beat.
    Quarter = 4,
    /// 1/4T — two thirds of a beat.
    QuarterTriplet = 5,
    /// 1/8.
    Eighth = 6,
    /// 1/8T.
    EighthTriplet = 7,
    /// 1/16, the default loop and the default step.
    #[default]
    Sixteenth = 8,
    /// 1/16T.
    SixteenthTriplet = 9,
    /// 1/32.
    ThirtySecond = 10,
    /// 1/32T.
    ThirtySecondTriplet = 11,
    /// 1/64.
    SixtyFourth = 12,
    /// 1/64T, the shortest.
    SixtyFourthTriplet = 13,
}

impl NoteValue {
    /// What a new loop is when nothing says otherwise, and what an unknown
    /// stored discriminant decodes to.
    pub const DEFAULT: NoteValue = NoteValue::Sixteenth;

    /// Every variant, longest first — the order the GUI lists them in.
    /// `ALL[d as usize] == d`, which is what makes [`NoteValue::from_u8`] a
    /// lookup.
    pub const ALL: [NoteValue; 14] = [
        NoteValue::Whole,
        NoteValue::WholeTriplet,
        NoteValue::Half,
        NoteValue::HalfTriplet,
        NoteValue::Quarter,
        NoteValue::QuarterTriplet,
        NoteValue::Eighth,
        NoteValue::EighthTriplet,
        NoteValue::Sixteenth,
        NoteValue::SixteenthTriplet,
        NoteValue::ThirtySecond,
        NoteValue::ThirtySecondTriplet,
        NoteValue::SixtyFourth,
        NoteValue::SixtyFourthTriplet,
    ];

    /// The `n` in the "1/n" name: 1, 2, 4 … 64 — the same for a triplet as
    /// for the straight note it modifies (1/16T is "a 1/16 triplet").
    pub const fn denominator(self) -> u32 {
        // Two variants per base, bases longest first: 1 << 0 … 1 << 6.
        1 << (self as u32 / 2)
    }

    /// Whether this is the triplet of its base — the ×2/3
    /// [`NoteValue::beats`] applies.
    pub const fn is_triplet(self) -> bool {
        self as u8 % 2 == 1
    }

    /// How long this note is in beats, a quarter note being one beat:
    /// 1/1 = 4.0 halving down to 1/64 = 0.0625, and a triplet two thirds of
    /// its base — 1/4T = 2/3, 1/16T = 1/6. Always positive and finite.
    ///
    /// The straight notes are powers of two, so scaling `4.0` by the
    /// denominator is lossless, and the ×2/3 doubles first — one rounded
    /// division and no more, which is why **three triplets fill exactly two
    /// of their base note** and a 12-step 1/8T pattern spans exactly `4.0`
    /// beats.
    pub const fn beats(self) -> f64 {
        let base = 4.0 / self.denominator() as f64;

        // `× 2 / 3` in that order: the doubling is exact, so the division is
        // the only rounding step.
        if self.is_triplet() { base * 2.0 / 3.0 } else { base }
    }

    /// The discriminant a snapshot stores.
    pub const fn to_u8(self) -> u8 {
        self as u8
    }

    /// Decodes a stored discriminant. Anything out of range becomes
    /// [`NoteValue::DEFAULT`] rather than panicking.
    pub const fn from_u8(raw: u8) -> NoteValue {
        if (raw as usize) < NoteValue::ALL.len() {
            NoteValue::ALL[raw as usize]
        } else {
            NoteValue::DEFAULT
        }
    }
}

impl fmt::Display for NoteValue {
    /// "1/16", "1/16T", "1/1" — the label the GUI puts on a loop.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "1/{}{}", self.denominator(), if self.is_triplet() { "T" } else { "" })
    }
}

/// A pattern's step length: 1/8, 1/16 or 1/32, straight or — for the two
/// longer ones — triplet.
///
/// Five rates and not the twenty-one of a step sequencer, because a
/// stutter grid is read at a glance and a pattern that is already sixteen
/// steps of a sixteenth spans a bar: the useful range is one zoom level
/// either side of that, plus the two triplets that put the grid in
/// three-against-four. Anything longer is a longer block at a shorter rate.
///
/// Like [`NoteValue`] the discriminant is the wire format, lowest rate
/// first, triplet after its base.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum Rate {
    /// 1/8 steps — a 16-step pattern spans two 4/4 bars.
    Eighth = 0,
    /// 1/8T steps.
    EighthTriplet = 1,
    /// 1/16 steps, the default: 16 of them span one 4/4 bar exactly.
    #[default]
    Sixteenth = 2,
    /// 1/16T steps.
    SixteenthTriplet = 3,
    /// 1/32 steps — the finest grid, half a bar over 16 steps.
    ThirtySecond = 4,
}

impl Rate {
    /// A new pattern's rate, and what an unknown stored discriminant decodes
    /// to: 1/16.
    pub const DEFAULT: Rate = Rate::Sixteenth;

    /// Every rate, in the order the GUI's rate dropdown lists them.
    /// `ALL[d as usize] == d`.
    pub const ALL: [Rate; 5] = [
        Rate::Eighth,
        Rate::EighthTriplet,
        Rate::Sixteenth,
        Rate::SixteenthTriplet,
        Rate::ThirtySecond,
    ];

    /// One step of this rate, as a note value — the slice a new block gets
    /// ("slice = one step of the pattern's rate") and the bridge between the
    /// two enums' discriminants.
    pub const fn note_value(self) -> NoteValue {
        match self {
            Rate::Eighth => NoteValue::Eighth,
            Rate::EighthTriplet => NoteValue::EighthTriplet,
            Rate::Sixteenth => NoteValue::Sixteenth,
            Rate::SixteenthTriplet => NoteValue::SixteenthTriplet,
            Rate::ThirtySecond => NoteValue::ThirtySecond,
        }
    }

    /// The `n` in the "1/n" name: 8, 16 or 32.
    pub const fn denominator(self) -> u32 {
        self.note_value().denominator()
    }

    /// Whether the step is the triplet of its base — the ×2/3
    /// [`Rate::step_beats`] applies.
    pub const fn is_triplet(self) -> bool {
        self.note_value().is_triplet()
    }

    /// One step of this rate in beats — the step length `L`: 1/8 = 0.5,
    /// 1/16 = 0.25, 1/32 = 0.125, 1/8T = 1/3, 1/16T = 1/6.
    ///
    /// [`NoteValue::beats`] of the step's note value, so the exactness
    /// argument there is this one's. Always positive and finite, which is
    /// what lets [`Pattern::position_at`] divide by it without a guard.
    pub const fn step_beats(self) -> f64 {
        self.note_value().beats()
    }

    /// The discriminant a snapshot stores.
    pub const fn to_u8(self) -> u8 {
        self as u8
    }

    /// Decodes a stored discriminant, out-of-range bytes included:
    /// [`Rate::DEFAULT`] rather than a panic.
    pub const fn from_u8(raw: u8) -> Rate {
        if (raw as usize) < Rate::ALL.len() {
            Rate::ALL[raw as usize]
        } else {
            Rate::DEFAULT
        }
    }
}

impl fmt::Display for Rate {
    /// "1/16", "1/8T" — the rate dropdown's labels.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.note_value())
    }
}

/// The most extra passes back a [`Source::Spot`] may reach. The recording
/// is 64 s long, so at slow tempos even fewer passes are still there; a
/// read past the recording fades out rather than clicking.
pub const MAX_SOURCE_PASSES: u8 = 8;

/// Where a block's read head starts.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum Source {
    /// At the block's own start: the first loop (or the start of a ramp)
    /// plays what would have played live.
    #[default]
    BlockStart,
    /// A fixed spot in the pattern: step `step` (counted from 0) the last
    /// time it played at or before the block's start, plus `passes` more
    /// whole passes of the pattern back. A spot after the block's own start
    /// is therefore in the previous pass. Moving the block leaves the spot
    /// where it is, so the block keeps playing the same audio.
    Spot {
        /// The step the read head starts on.
        step: u16,
        /// Extra whole passes back, up to [`MAX_SOURCE_PASSES`].
        passes: u8,
    },
    /// Wherever the block right before this one left its read head — so a
    /// tape stop can slow down the beat a repeat was looping. With no block
    /// ending exactly where this one starts it behaves as
    /// [`Source::BlockStart`].
    Continue,
}

/// What a block does with the past it reads: one recipe for a read head
/// (`crate::actions`).
///
/// - `source`: where the head starts.
/// - `looping`: `Some(length)` sends the head back to its start every
///   `length` — a repeat. `None` lets it run.
/// - `from` → `to`: the read speed at the block's start and end, ramped
///   linearly, inside ±[`crate::actions::MAX_SPEED`]. 1 is live speed; pitch
///   follows speed.
///
/// The old fixed actions are settings of this: a Repeat is a loop at speed
/// 1, and the five [`SpeedPreset`]s are unlooped ramps from the block start.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Action {
    /// Where the read head starts.
    pub source: Source,
    /// The loop length, or `None` for no loop.
    pub looping: Option<NoteValue>,
    /// The read speed at the block's start.
    pub from: f32,
    /// The read speed at the block's end.
    pub to: f32,
}

impl Action {
    /// What a new block gets at this rate: the classic stutter — a loop of
    /// one step from the block's start, at live speed.
    pub const fn repeat_at(rate: Rate) -> Action {
        Action::repeat(rate.note_value())
    }

    /// A loop of `length` from the block's start, at live speed.
    pub const fn repeat(length: NoteValue) -> Action {
        Action { source: Source::BlockStart, looping: Some(length), from: 1.0, to: 1.0 }
    }

    /// One of the five named ramps: unlooped, from the block's start.
    pub fn from_preset(preset: SpeedPreset) -> Action {
        let (from, to) = preset.speeds();
        Action { source: Source::BlockStart, looping: None, from: from as f32, to: to as f32 }
    }

    /// An unlooped ramp between two speeds from the block's start, clamped
    /// to ±[`crate::actions::MAX_SPEED`].
    pub fn ramp(from: f32, to: f32) -> Action {
        Action {
            source: Source::BlockStart,
            looping: None,
            from: clamp_speed_f32(from),
            to: clamp_speed_f32(to),
        }
    }

    /// This action reading from `source` instead.
    pub const fn with_source(self, source: Source) -> Action {
        Action { source, ..self }
    }

    /// This action with its speeds clamped (invariant 4).
    pub fn sanitised(self) -> Action {
        let source = match self.source {
            Source::Spot { step, passes } => Source::Spot {
                step: step.min(MAX_STEPS as u16 - 1),
                passes: passes.min(MAX_SOURCE_PASSES),
            },
            other => other,
        };
        Action { source, from: clamp_speed_f32(self.from), to: clamp_speed_f32(self.to), ..self }
    }

    /// The two speeds in `f64`, which is what the maths runs in.
    pub fn speeds(self) -> (f64, f64) {
        (f64::from(self.from), f64::from(self.to))
    }

    /// Whether the read head loops — a repeat.
    pub fn is_repeat(self) -> bool {
        self.looping.is_some()
    }

    /// The named speed shape, for an unlooped block whose speeds are exactly
    /// one of the five pairs.
    pub fn preset(self) -> Option<SpeedPreset> {
        if self.looping.is_some() {
            return None;
        }
        let (from, to) = self.sanitised().speeds();
        SpeedPreset::of_speeds(from, to)
    }

}

impl Default for Action {
    /// A loop of a 1/16 from the start of the block — the default action at
    /// the default rate.
    fn default() -> Action {
        Action::repeat_at(Rate::DEFAULT)
    }
}

/// A ramp endpoint as the pattern stores it, brought inside
/// ±[`crate::actions::MAX_SPEED`].
///
/// `crate::actions::clamp_speed` in `f32`, with its NaN rule: the bound and
/// every preset endpoint are exact in both precisions, so widening, clamping
/// and narrowing again changes nothing an in-range speed would notice.
pub fn clamp_speed_f32(speed: f32) -> f32 {
    clamp_speed(f64::from(speed)) as f32
}

/// A step volume, brought inside `0.0..=1.0`.
///
/// `max`-then-`min` in that order, which is the NaN guard: `f32::max` hands
/// back the non-NaN operand, so a NaN lands on `0.0` — silence for that
/// step, the quiet reading of nonsense, rather than a NaN multiplied into
/// the output.
pub fn clamp_volume(volume: f32) -> f32 {
    // The lint wants `clamp`, which propagates a NaN instead of guarding it.
    #[allow(clippy::manual_clamp)]
    volume.max(0.0).min(1.0)
}

/// The filter a pattern runs its effect signal through, per pattern.
///
/// Its cutoff and resonance are per step, on the pattern's Cutoff and Res rows
/// ([`Lane`]); this is the shape of it. [`Filter::Off`] is the default and a
/// true bypass, so a project from before the filter existed sounds the same.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Filter {
    /// No filter.
    #[default]
    Off,
    /// Cuts above the cutoff.
    LowPass,
    /// Cuts below the cutoff.
    HighPass,
    /// Keeps a band around the cutoff.
    BandPass,
}

impl Filter {
    /// All four, in the order the window lists them.
    pub const ALL: [Filter; 4] = [Filter::Off, Filter::LowPass, Filter::HighPass, Filter::BandPass];

    /// The stored discriminant.
    pub fn to_u8(self) -> u8 {
        match self {
            Filter::Off => 0,
            Filter::LowPass => 1,
            Filter::HighPass => 2,
            Filter::BandPass => 3,
        }
    }

    /// A stored discriminant read back; anything unknown is [`Filter::Off`].
    pub fn from_u8(value: u8) -> Filter {
        match value {
            1 => Filter::LowPass,
            2 => Filter::HighPass,
            3 => Filter::BandPass,
            _ => Filter::Off,
        }
    }

    /// The Cutoff row level where this filter does least: the top for a
    /// low-pass (and for Off), the bottom for a high-pass, the middle for a
    /// band-pass, which has no open setting. What a double-click on a Cutoff
    /// bar puts back, and what an untouched row is moved to when the filter
    /// changes ([`Pattern::choose_filter`]).
    pub fn open_cutoff(self) -> f32 {
        match self {
            Filter::Off | Filter::LowPass => 1.0,
            Filter::HighPass => 0.0,
            Filter::BandPass => 0.5,
        }
    }
}

/// One of a pattern's rows of per-step levels, every level in `0.0..=1.0`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Lane {
    /// The Volume row: the gain on the effect signal.
    #[default]
    Volume,
    /// The Cutoff row: the filter's cutoff, from 20 Hz at the bottom to
    /// 20 kHz at the top on a log scale ([`cutoff_hz`]).
    Cutoff,
    /// The Res row: the filter's resonance, none at the bottom.
    Resonance,
}

impl Lane {
    /// All three, in the order the window lists them.
    pub const ALL: [Lane; 3] = [Lane::Volume, Lane::Cutoff, Lane::Resonance];

    /// What a new pattern's row holds at every step, and what a snapshot
    /// with the row missing pads it with.
    pub fn default_level(self) -> f32 {
        match self {
            Lane::Volume => Pattern::DEFAULT_VOLUME,
            Lane::Cutoff => Filter::Off.open_cutoff(),
            Lane::Resonance => 0.0,
        }
    }
}

/// The lowest cutoff, at the bottom of the Cutoff row.
pub const CUTOFF_MIN_HZ: f32 = 20.0;
/// The highest cutoff, at the top of the Cutoff row: a thousand times the
/// lowest, so the row covers the ten octaves people hear.
pub const CUTOFF_MAX_HZ: f32 = 20_000.0;

/// The frequency a Cutoff row `level` means: 20 Hz at `0.0`, 20 kHz at
/// `1.0`, evenly spaced in octaves between.
pub fn cutoff_hz(level: f32) -> f32 {
    CUTOFF_MIN_HZ * (CUTOFF_MAX_HZ / CUTOFF_MIN_HZ).powf(clamp_volume(level))
}

/// The resonance a Res row `level` means, as the filter's Q: 0.707 (none,
/// a flat corner) at `0.0`, rising evenly in octaves to 8.5 at `1.0` —
/// a strong peak that stays well short of a whistle.
pub fn resonance_q(level: f32) -> f32 {
    std::f32::consts::FRAC_1_SQRT_2 * 12.0f32.powf(clamp_volume(level))
}

/// One block on the Time row: `len` steps of [`Action`] starting at step
/// `start`.
///
/// The span is private because what it may be depends on the pattern around
/// it — the pattern's length, and the neighbours it must not overlap — so
/// [`Pattern`] is what moves and resizes a block ([`Pattern::move_block`],
/// [`Pattern::resize_block`], [`Pattern::set_block_span`]). A `Block` on its
/// own guarantees only the part it can: `len ≥ 1`, a span that fits inside
/// [`MAX_STEPS`], and a sanitised action.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Block {
    start: u16,
    len: u16,
    action: Action,
}

impl Block {
    /// A block of `len` steps at `start`, clamped to what a block can be
    /// anywhere: at least one step long, and inside the first
    /// [`MAX_STEPS`] steps. [`Pattern`] narrows it further to the pattern it
    /// goes into.
    pub fn new(start: u16, len: u16, action: Action) -> Block {
        let start = start.min(MAX_STEPS as u16 - 1);
        let len = len.max(1).min(MAX_STEPS as u16 - start);

        Block { start, len, action: action.sanitised() }
    }

    /// The step the block starts on.
    pub fn start(&self) -> u16 {
        self.start
    }

    /// How many steps it covers — never zero.
    // The lint wants an `is_empty` beside this; a block is at least one step
    // long by construction, so that method could only ever return `false`.
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> u16 {
        self.len
    }

    /// One past the last step it covers: the `end` of the half-open range
    /// `[start, end)` the whole codebase is written in.
    pub fn end(&self) -> u16 {
        self.start + self.len
    }

    /// Whether `step` is inside `[start, end)`.
    pub fn covers(&self, step: u16) -> bool {
        step >= self.start && step < self.end()
    }

    /// Where the block begins in beats, measured from the start of the
    /// pattern cycle it is in — the block start `s` of the offset maths — at
    /// a step length of `step_beats` ([`Pattern::step_beats`]).
    pub fn start_beats(&self, step_beats: f64) -> f64 {
        self.start as f64 * step_beats
    }

    /// How long the block lasts in beats — the `B` the offset maths measures
    /// a phase against — at a step length of `step_beats`. Never zero: a
    /// block is at least one step.
    pub fn beats(&self, step_beats: f64) -> f64 {
        self.len as f64 * step_beats
    }

    /// What the block does.
    pub fn action(&self) -> Action {
        self.action
    }

    /// Gives the block a new action, sanitised.
    pub fn set_action(&mut self, action: Action) {
        self.action = action.sanitised();
    }
}

/// One pattern: a rate, a length in steps, the blocks of its Time row and
/// the levels of its Volume row.
///
/// Every field is private and every setter re-establishes
/// [the invariants](self#the-invariants), which is the whole of this type's
/// job. Sixteen of these are what `model.rs` publishes to the audio thread,
/// and `Clone` is what the model's copy-pattern writer is built from.
#[derive(Clone, Debug, PartialEq)]
pub struct Pattern {
    rate: Rate,
    length: u16,
    blocks: Vec<Block>,
    volume: [f32; MAX_STEPS],
    filter: Filter,
    cutoff: [f32; MAX_STEPS],
    resonance: [f32; MAX_STEPS],
}

impl Pattern {
    /// A new pattern's length: 16 steps, which at the default rate is one
    /// 4/4 bar.
    pub const DEFAULT_LENGTH: u16 = 16;

    /// The longest a pattern may be, as the `u16` its length is stored in.
    pub const MAX_LENGTH: u16 = MAX_STEPS as u16;

    /// What an untouched step plays at: unity, the Volume row full.
    pub const DEFAULT_VOLUME: f32 = 1.0;

    /// An empty 16-step 1/16 pattern at full volume — what all sixteen
    /// patterns are in a fresh project, and what a corrupt snapshot loads
    /// as.
    pub fn new() -> Pattern {
        Pattern {
            rate: Rate::DEFAULT,
            length: Pattern::DEFAULT_LENGTH,
            blocks: Vec::new(),
            volume: [Pattern::DEFAULT_VOLUME; MAX_STEPS],
            filter: Filter::Off,
            cutoff: [Lane::Cutoff.default_level(); MAX_STEPS],
            resonance: [Lane::Resonance.default_level(); MAX_STEPS],
        }
    }

    /// How long one step is.
    pub fn rate(&self) -> Rate {
        self.rate
    }

    /// Sets the step length. Nothing else moves: the blocks keep their step
    /// positions, so changing the rate stretches or squeezes the pattern in
    /// time rather than rearranging it.
    pub fn set_rate(&mut self, rate: Rate) {
        self.rate = rate;
    }

    /// One step of this pattern in beats — [`Rate::step_beats`] of its
    /// rate, the `L` the Time row's steps and blocks are measured in.
    pub const fn step_beats(&self) -> f64 {
        self.rate.step_beats()
    }

    /// How long the whole pattern lasts in beats: `P = N × L`, which is the
    /// period [`Pattern::position_at`] folds the project beat into.
    ///
    /// Always positive and finite — `1..=64` steps of a positive step
    /// length. A 16-step 1/16 pattern spans exactly `4.0`, one 4/4 bar, as
    /// following Bitwig requires; so does a 12-step 1/8T
    /// one, three steps to the beat.
    pub const fn span_beats(&self) -> f64 {
        self.length as f64 * self.step_beats()
    }

    /// How many steps the pattern is, always `1..=MAX_LENGTH`.
    pub fn length(&self) -> u16 {
        self.length
    }

    /// Sets the length, clamped to `1..=MAX_LENGTH`, and brings the Time row
    /// back inside it: a block that now starts past the end is **dropped**,
    /// one that merely overhangs it is **truncated** (never below one step —
    /// a block starting on the last step keeps that step).
    ///
    /// The Volume row is left alone. Levels on steps past the end are simply
    /// unused, so shrinking a pattern and growing it again gives back the
    /// row that was drawn — which is what a length drag in the GUI needs.
    pub fn set_length(&mut self, length: u16) {
        // An integer, so `clamp` is safe here — the `max`-then-`min` order
        // is the NaN guard the float clamps above need, not this one.
        self.length = length.clamp(1, Pattern::MAX_LENGTH);

        let length = self.length;
        self.blocks.retain(|block| block.start < length);
        for block in &mut self.blocks {
            if block.end() > length {
                block.len = length - block.start;
            }
        }
    }

    /// The level of one step, in `0.0..=1.0`.
    ///
    /// A step index past the Volume row reads as [`Pattern::DEFAULT_VOLUME`]
    /// rather than panicking: the engine indexes this with a step it
    /// computed from the host's transport, and unity is the level that
    /// changes nothing.
    pub fn volume(&self, step: u16) -> f32 {
        match self.volume.get(step as usize) {
            Some(&volume) => volume,
            None => Pattern::DEFAULT_VOLUME,
        }
    }

    /// The whole Volume row, every entry in `0.0..=1.0`. Entries from
    /// [`Pattern::length`] on are unused.
    pub fn volumes(&self) -> &[f32; MAX_STEPS] {
        &self.volume
    }

    /// Sets one step's level, clamped by [`clamp_volume`]. A step index
    /// outside the row is ignored.
    pub fn set_volume(&mut self, step: u16, volume: f32) {
        if let Some(slot) = self.volume.get_mut(step as usize) {
            *slot = clamp_volume(volume);
        }
    }

    /// Replaces the whole Volume row from an iterator of any length — the
    /// shape a loaded snapshot arrives in (volumes truncated or padded to
    /// `MAX_STEPS` and clamped to 0.0–1.0).
    ///
    /// Extra entries are dropped, missing ones are
    /// [`Pattern::DEFAULT_VOLUME`], and every entry taken is clamped.
    pub fn set_volumes(&mut self, volumes: impl IntoIterator<Item = f32>) {
        self.volume = [Pattern::DEFAULT_VOLUME; MAX_STEPS];
        for (slot, volume) in self.volume.iter_mut().zip(volumes) {
            *slot = clamp_volume(volume);
        }
    }

    /// One of the rows of per-step levels, whole, every entry in
    /// `0.0..=1.0`. Entries from [`Pattern::length`] on are unused.
    pub fn lane(&self, lane: Lane) -> &[f32; MAX_STEPS] {
        match lane {
            Lane::Volume => &self.volume,
            Lane::Cutoff => &self.cutoff,
            Lane::Resonance => &self.resonance,
        }
    }

    /// One step's level on `lane`; a step past the row reads as the lane's
    /// default ([`Lane::default_level`]), as [`Pattern::volume`] does.
    pub fn level(&self, lane: Lane, step: u16) -> f32 {
        match self.lane(lane).get(step as usize) {
            Some(&level) => level,
            None => lane.default_level(),
        }
    }

    /// Replaces a whole row from an iterator of any length: extra entries
    /// are dropped, missing ones are the lane's default, and every entry is
    /// clamped ([`clamp_volume`]).
    pub fn set_lane(&mut self, lane: Lane, levels: impl IntoIterator<Item = f32>) {
        let row = match lane {
            Lane::Volume => &mut self.volume,
            Lane::Cutoff => &mut self.cutoff,
            Lane::Resonance => &mut self.resonance,
        };
        *row = [lane.default_level(); MAX_STEPS];
        for (slot, level) in row.iter_mut().zip(levels) {
            *slot = clamp_volume(level);
        }
    }

    /// Sets one step's level on `lane`, clamped; a step outside the row is
    /// ignored.
    pub fn set_level(&mut self, lane: Lane, step: u16, level: f32) {
        let row = match lane {
            Lane::Volume => &mut self.volume,
            Lane::Cutoff => &mut self.cutoff,
            Lane::Resonance => &mut self.resonance,
        };
        if let Some(slot) = row.get_mut(step as usize) {
            *slot = clamp_volume(level);
        }
    }

    /// The filter's shape ([`Filter`]).
    pub fn filter(&self) -> Filter {
        self.filter
    }

    /// Sets the filter's shape and nothing else — what a snapshot load does.
    pub fn set_filter(&mut self, filter: Filter) {
        self.filter = filter;
    }

    /// The filter picked in the window: sets it, and if the Cutoff row is
    /// still everywhere at the old filter's open setting — untouched — moves
    /// it to the new one's ([`Filter::open_cutoff`]), so that picking
    /// High-pass on a fresh pattern does not cut everything until each bar
    /// is drawn down.
    pub fn choose_filter(&mut self, filter: Filter) {
        let open = self.filter.open_cutoff();
        if self.cutoff.iter().all(|&level| level == open) {
            self.cutoff = [filter.open_cutoff(); MAX_STEPS];
        }
        self.filter = filter;
    }

    /// The Time row: sorted by `start`, non-overlapping, all inside
    /// `0..length`.
    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }

    /// Replaces the whole Time row from an iterator of any order — the other
    /// half of the snapshot door (blocks truncated to `MAX_BLOCKS`,
    /// re-sorted, overlaps dropped keeping the earlier block, out-of-range
    /// starts and lengths clamped into `0..length`).
    ///
    /// In that order: blocks starting past the end are dropped, the rest are
    /// sorted by `start` (stably, so the first of two blocks written on the
    /// same step is the one that survives), one starting inside the block
    /// kept before it is dropped, each is truncated to the end of the
    /// pattern, and the row is cut to [`MAX_BLOCKS`].
    ///
    /// Overlaps are resolved by keeping the **earlier** block whole rather
    /// than by squeezing both, because a snapshot's overlap is damage, not a
    /// gesture: the first block is the one the user can see where it was.
    pub fn set_blocks(&mut self, blocks: impl IntoIterator<Item = Block>) {
        let length = self.length;

        self.blocks.clear();
        let mut incoming: Vec<Block> =
            blocks.into_iter().filter(|block| block.start < length).collect();
        incoming.sort_by_key(|block| block.start);

        // `cursor` is the first step not yet covered: a block starting
        // before it overlaps the one kept last and is dropped.
        let mut cursor = 0;
        for mut block in incoming {
            if self.blocks.len() >= MAX_BLOCKS {
                break;
            }
            if block.start < cursor {
                continue;
            }

            block.len = block.len.max(1).min(length - block.start);
            block.action = block.action.sanitised();
            cursor = block.end();
            self.blocks.push(block);
        }
    }

    /// The index of the block covering `step`, if any — `O(log n)`, which is
    /// what [`Pattern::position_at`] looks a step up with on the audio
    /// thread.
    pub fn block_at(&self, step: u16) -> Option<usize> {
        // The blocks are sorted and disjoint, so the only candidate is the
        // last one starting at or before `step`.
        let candidate = self.blocks.partition_point(|block| block.start <= step).checked_sub(1)?;

        self.blocks[candidate].covers(step).then_some(candidate)
    }

    /// Where project beat `beat` falls in one cycle of this pattern, in
    /// beats: `b mod P`, which is [`Position::pattern_phase`] and what every
    /// position below is measured from. `None` for a beat that is not a
    /// finite number, as [`Pattern::position_at`].
    ///
    /// Its own method because the scope needs it on its own (it indexes its
    /// columns by `(b mod P) / P`) and two foldings of a beat into a cycle
    /// that could ever disagree would be one too many.
    pub fn phase_at(&self, beat: f64) -> Option<f64> {
        if !beat.is_finite() {
            return None;
        }

        let span = self.span_beats();

        // The span is positive and finite by the invariants, so the only
        // edge left is `rem_euclid`'s own rounding: for a beat a hair below
        // zero it hands back `span` itself, and a cycle is half-open, so
        // that beat belongs to the start of the next one.
        let phase = beat.rem_euclid(span);

        Some(if phase < span { phase } else { 0.0 })
    }

    /// Where project beat `beat` falls in this pattern — the step under it,
    /// the block over that step and the phases — or `None` for a beat that
    /// is not a finite number.
    ///
    /// The pattern is laid over the project **from beat 0**, never from
    /// wherever it was switched on: the position is `b mod P` and nothing
    /// else (patterns do not have their own phase; a pattern change joins at
    /// the current place in the bar). So this is a function of the beat
    /// alone — no state, no start beat, the same answer however the
    /// transport reached it, which is what makes a loop jump nothing
    /// special. A count-in's negative beats wrap the same way
    /// (`rem_euclid`): beat −0.25 of a 16-step 1/16 pattern is its last
    /// step.
    ///
    /// `None` means the host reported a beat that is not a number, and live
    /// audio is the engine's answer to it. Nothing here allocates, panics or
    /// divides by zero — the step length is positive by construction and the
    /// lookup is a binary search over the pattern's own sorted row — so it
    /// is callable per sample from the audio thread.
    pub fn position_at(&self, beat: f64) -> Option<Position> {
        let pattern_phase = self.phase_at(beat)?;
        let step_beats = self.step_beats();

        // The quotient is non-negative and below `length` but for the
        // rounding of one division, which a triplet step length can push up
        // to `length` exactly at the end of a cycle: the `min` is that
        // rounding's guard, and the cast saturates rather than wrapping.
        let step = ((pattern_phase / step_beats).floor() as u16).min(self.length - 1);
        let step_phase = clamp_phase(pattern_phase - step as f64 * step_beats, step_beats);

        let (block, block_phase, block_beats) = match self.block_at(step) {
            Some(index) => {
                let block = self.blocks[index];
                let beats = block.beats(step_beats);
                let phase = pattern_phase - block.start_beats(step_beats);

                (Some(index), clamp_phase(phase, beats), beats)
            }
            None => (None, 0.0, 0.0),
        };

        Some(Position { pattern_phase, step, step_phase, block, block_phase, block_beats })
    }

    /// Adds a block of up to `len` steps at `start`, returning its index in
    /// the Time row.
    ///
    /// The clicked step is what the gesture is about (a click on an empty
    /// step), so the start is clamped into the pattern and the length is cut
    /// to the gap in front of it — a four-step block dropped two steps
    /// before its neighbour becomes a two-step block. `None`, and no block
    /// added, only where the gesture has nowhere to go: the step is already
    /// covered, or the row already holds [`MAX_BLOCKS`].
    pub fn add_block(&mut self, start: u16, len: u16, action: Action) -> Option<usize> {
        if self.blocks.len() >= MAX_BLOCKS {
            return None;
        }

        let start = start.min(self.length - 1);
        if self.block_at(start).is_some() {
            return None;
        }

        let index = self.blocks.partition_point(|block| block.start <= start);
        let len = len.max(1).min(self.next_start(index) - start);
        self.blocks.insert(index, Block { start, len, action: action.sanitised() });

        Some(index)
    }

    /// Moves a block to `start` keeping its length, returning the step it
    /// actually landed on.
    ///
    /// The block stops against its neighbours and against the ends of the
    /// pattern instead of pushing through them — the clamp a drag needs. Its
    /// index never changes, because a block that cannot pass a neighbour
    /// cannot reorder the row either. `None` for an index that does not
    /// exist.
    pub fn move_block(&mut self, index: usize, start: u16) -> Option<u16> {
        let len = self.blocks.get(index)?.len;
        let (first, last) = (self.gap_start(index), self.next_start(index + 1) - len);

        // `last >= first`: the block is in that gap at this length already.
        let start = start.max(first).min(last);
        self.blocks[index].start = start;

        Some(start)
    }

    /// Resizes a block from its right edge, returning the length it actually
    /// got: at least one step, at most the gap to the next block or to the
    /// end of the pattern.
    pub fn resize_block(&mut self, index: usize, len: u16) -> Option<u16> {
        let start = self.blocks.get(index)?.start;
        let len = len.max(1).min(self.next_start(index + 1) - start);
        self.blocks[index].len = len;

        Some(len)
    }

    /// Sets both edges at once — the left-edge drag, where the start moves
    /// and the length follows it — returning the span the block actually
    /// got.
    ///
    /// The start is clamped into the gap the block lives in (it may not pass
    /// a neighbour, and it leaves itself at least one step), then the length
    /// is clamped to what is left of that gap. Unlike
    /// [`Pattern::move_block`], a start pushed up against the next block
    /// **shrinks** this one rather than stopping it, which is what a dragged
    /// edge should do.
    pub fn set_block_span(&mut self, index: usize, start: u16, len: u16) -> Option<(u16, u16)> {
        let action = self.blocks.get(index)?.action;
        let (first, end) = (self.gap_start(index), self.next_start(index + 1));

        let start = start.max(first).min(end - 1);
        let len = len.max(1).min(end - start);
        self.blocks[index] = Block { start, len, action };

        Some((start, len))
    }

    /// Gives a block a new action, sanitised. `false` for an index that does
    /// not exist.
    pub fn set_block_action(&mut self, index: usize, action: Action) -> bool {
        match self.blocks.get_mut(index) {
            Some(block) => {
                block.set_action(action);
                true
            }
            None => false,
        }
    }

    /// Copies a block into the space immediately after it, returning the
    /// copy's index.
    ///
    /// Same length and same action, cut to the gap if the gap is shorter —
    /// so duplicating a two-step block with one step of room gives a
    /// one-step copy. `None` where there is no room at all: the block ends
    /// at the end of the pattern, its neighbour is adjacent, or the row is
    /// full.
    pub fn duplicate_block(&mut self, index: usize) -> Option<usize> {
        let block = *self.blocks.get(index)?;
        if block.end() >= self.length {
            return None;
        }

        self.add_block(block.end(), block.len, block.action)
    }

    /// Removes a block. `false` for an index that does not exist.
    pub fn delete_block(&mut self, index: usize) -> bool {
        if index >= self.blocks.len() {
            return false;
        }

        self.blocks.remove(index);
        true
    }

    /// Empties the pattern: no blocks, the Volume row back to unity, the
    /// filter off with its two rows back to their defaults.
    ///
    /// The rate and the length stay, because clearing is "take what I drew
    /// off this grid", not "give me a different grid".
    pub fn clear(&mut self) {
        self.blocks.clear();
        self.volume = [Pattern::DEFAULT_VOLUME; MAX_STEPS];
        self.filter = Filter::Off;
        self.cutoff = [Lane::Cutoff.default_level(); MAX_STEPS];
        self.resonance = [Lane::Resonance.default_level(); MAX_STEPS];
    }

    /// The read head of the block at `index`, solved: its speeds, length and
    /// loop in beats, and its anchor — which for [`Source::Continue`] is
    /// where the block before it left off.
    ///
    /// A run of `Continue` blocks is followed back to the first block that
    /// is not one (or has no block ending exactly where it starts), then
    /// solved forwards. The block on step 0 continues the pattern's last
    /// block when that one ends on the pattern's last step. A run that comes
    /// all the way round to `index` starts from the block after it with its
    /// anchor at its own start. At most [`MAX_BLOCKS`] blocks are visited, so
    /// this is bounded, and it allocates nothing.
    pub fn block_path(&self, index: usize) -> Option<BlockPath> {
        self.blocks.get(index)?;

        let mut first = index;
        for _ in 0..MAX_BLOCKS {
            if self.blocks[first].action.source != Source::Continue {
                break;
            }
            match self.previous_adjacent(first) {
                Some(previous) if previous != index => first = previous,
                _ => break,
            }
        }

        let mut current = first;
        let mut lag = self.source_lag(first);
        loop {
            let path = self.solve(current, lag);
            if current == index {
                return Some(path);
            }
            lag = path.end_offset();
            current = (current + 1) % self.blocks.len();
        }
    }

    /// The block a [`Source::Continue`] block at `index` carries on from:
    /// the one ending exactly where it starts (round the cycle for step 0),
    /// or `None` when it has none — or is not set to continue — and so reads
    /// from its own start.
    pub fn continues_from(&self, index: usize) -> Option<usize> {
        let block = self.blocks.get(index)?;
        if block.action.source != Source::Continue {
            return None;
        }
        self.previous_adjacent(index).filter(|&previous| previous != index)
    }

    /// How many steps before the block at `index` its spot is: the spot's
    /// step the last time it played at or before the block's start, plus
    /// its extra passes. `None` for a block whose source is not a spot. A
    /// spot past the pattern's end (it was shortened) reads its last step.
    pub fn spot_steps_back(&self, index: usize) -> Option<u32> {
        let block = self.blocks.get(index)?;
        let Source::Spot { step, passes } = block.action.source else {
            return None;
        };
        let length = u32::from(self.length);
        let step = u32::from(step).min(length - 1);
        let back = (u32::from(block.start) + length - step) % length;

        Some(back + u32::from(passes) * length)
    }

    /// The anchor's lag behind the start of the block at `index`, in beats,
    /// for a source that does not depend on the blocks around it (a
    /// `Continue` reads 0 here; [`Pattern::block_path`] resolves it).
    fn source_lag(&self, index: usize) -> f64 {
        self.spot_steps_back(index).map_or(0.0, |steps| f64::from(steps) * self.step_beats())
    }

    /// One block's path with its anchor `lag` beats before its start.
    fn solve(&self, index: usize, lag: f64) -> BlockPath {
        let block = self.blocks[index];
        let action = block.action;
        let (from, to) = action.speeds();
        let looping = action.looping.map(NoteValue::beats);

        BlockPath::new(from, to, block.beats(self.step_beats()), looping, lag)
    }

    /// The block ending exactly where the block at `index` starts — the one
    /// before it in the row, or, for a block on step 0, the last block when
    /// it ends on the pattern's last step (the previous cycle).
    fn previous_adjacent(&self, index: usize) -> Option<usize> {
        let block = self.blocks.get(index)?;
        if index > 0 {
            let previous = index - 1;
            return (self.blocks[previous].end() == block.start).then_some(previous);
        }

        let last = self.blocks.len() - 1;
        (block.start == 0 && self.blocks[last].end() == self.length).then_some(last)
    }

    /// The first step the block at `index` may start on: one past the end of
    /// the block before it, or 0.
    ///
    /// Answers for `index == self.blocks.len()` too, which is what
    /// [`Pattern::add_block`] asks it at an insertion point.
    fn gap_start(&self, index: usize) -> u16 {
        match index.checked_sub(1) {
            Some(previous) => self.blocks[previous].end(),
            None => 0,
        }
    }

    /// Where the block at `index` begins, or — past the end of the row — the
    /// end of the pattern: the first step the block *before* it may not
    /// cover.
    ///
    /// So the gap around an existing block `i` is
    /// `gap_start(i)..next_start(i + 1)`, and the gap at an insertion point
    /// `i` is `gap_start(i)..next_start(i)`.
    fn next_start(&self, index: usize) -> u16 {
        match self.blocks.get(index) {
            Some(next) => next.start,
            None => self.length,
        }
    }
}

impl Default for Pattern {
    fn default() -> Pattern {
        Pattern::new()
    }
}

/// Where one project beat falls in a pattern: which step is playing, which
/// block is over it, and how far into each the beat is.
///
/// What [`Pattern::position_at`] answers, and all the engine needs to know
/// about the grid for one sample: [`Position::step`] indexes the Volume row,
/// [`Position::block`] indexes [`Pattern::blocks`], and
/// [`Position::block_phase`] with [`Position::block_beats`] are the `p` and
/// `B` of the offset maths — exactly what [`crate::actions`] takes.
///
/// Note what is *not* here: a project beat. A position has already been
/// folded into one pattern cycle, so nothing downstream of it has to know
/// where the transport is, only where in the pattern it is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Position {
    pattern_phase: f64,
    step: u16,
    step_phase: f64,
    block: Option<usize>,
    block_phase: f64,
    block_beats: f64,
}

impl Position {
    /// Beats since the start of the pattern cycle this beat is in: the
    /// `b mod P`, inside `0.0..`[`Pattern::span_beats`]. What the GUI draws
    /// its playhead from.
    pub const fn pattern_phase(&self) -> f64 {
        self.pattern_phase
    }

    /// Which step is playing, always inside `0..`[`Pattern::length`] — the
    /// index the Volume row is read at.
    pub const fn step(&self) -> u16 {
        self.step
    }

    /// Beats since that step began, inside `0.0..=`[`Pattern::step_beats`]
    /// (the top end only by the sub-ULP rounding `position_at` clamps).
    pub const fn step_phase(&self) -> f64 {
        self.step_phase
    }

    /// The block over that step, as an index into [`Pattern::blocks`], or
    /// `None` where the step is bare — which is the live case, `d = 0`.
    pub const fn block(&self) -> Option<usize> {
        self.block
    }

    /// Whether the step plays live: nothing drawn over it.
    pub fn is_live(&self) -> bool {
        self.block.is_none()
    }

    /// Beats since that block began — the block phase `p`, inside
    /// `0.0..=`[`Position::block_beats`]. `0.0` where there is no block.
    pub const fn block_phase(&self) -> f64 {
        self.block_phase
    }

    /// How long that block lasts in beats — the block length `B`.
    /// `0.0` where there is no block, which is the one case a caller must
    /// not divide by: there is no ramp to divide for.
    pub const fn block_beats(&self) -> f64 {
        self.block_beats
    }
}

/// A phase brought inside `0.0..=high`.
///
/// `max`-then-`min`, the house NaN order, though no NaN reaches this one —
/// [`Pattern::position_at`] turns those away at the door. What it is really
/// for is the sub-ULP overshoot a triplet step length leaves at the very end
/// of a cycle, where a phase can land a hair past the step or block it was
/// measured in.
fn clamp_phase(phase: f64, high: f64) -> f64 {
    // The lint wants `clamp`, which propagates a NaN instead of guarding it.
    #[allow(clippy::manual_clamp)]
    phase.max(0.0).min(high)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [The invariants](self#the-invariants), checked on a pattern however
    /// it got that way: every test that touches a setter ends here, because
    /// the point of this module is not what one setter returns but that no
    /// sequence of them can leave a pattern the audio thread cannot trust.
    fn check_invariants(pattern: &Pattern) {
        let length = pattern.length();
        assert!((1..=Pattern::MAX_LENGTH).contains(&length), "length {length} is out of range");
        assert!(pattern.blocks().len() <= MAX_BLOCKS, "{} blocks", pattern.blocks().len());

        let mut cursor = 0;
        for (index, block) in pattern.blocks().iter().enumerate() {
            assert!(block.len() >= 1, "block {index} is empty");
            assert!(block.start() >= cursor, "block {index} is out of order or overlapping");
            assert!(block.end() <= length, "block {index} ends past step {length}");
            cursor = block.end();

            let action = block.action();
            for speed in [action.from, action.to] {
                assert!(speed.is_finite(), "block {index} has a {speed} speed");
                assert!(speed.abs() <= 2.0, "block {index} runs at {speed}");
            }
        }

        for lane in Lane::ALL {
            for (step, &level) in pattern.lane(lane).iter().enumerate() {
                assert!((0.0..=1.0).contains(&level), "{lane:?} step {step} is at {level}");
            }
        }
    }

    /// A pattern with `starts` as one-step blocks on it, for the tests that
    /// care about the neighbours rather than about the actions.
    fn with_blocks(starts: &[u16]) -> Pattern {
        let mut pattern = Pattern::new();
        for &start in starts {
            assert!(pattern.add_block(start, 1, Action::default()).is_some(), "step {start}");
        }
        check_invariants(&pattern);

        pattern
    }

    /// The named limits, which the rest of the plugin sizes itself from.
    #[test]
    fn the_limits_are_the_spec_s_limits() {
        assert_eq!(PATTERN_COUNT, 16);
        assert_eq!(MAX_STEPS, 64);
        assert_eq!(MAX_BLOCKS, 64);
        assert_eq!(Pattern::MAX_LENGTH as usize, MAX_STEPS);
    }

    /// A fresh pattern is the default one: 1/16, 16 steps, nothing drawn,
    /// the Volume row full.
    #[test]
    fn a_new_pattern_is_sixteen_empty_steps_of_one_sixteenth_at_unity() {
        let pattern = Pattern::new();

        assert_eq!(pattern.rate(), Rate::Sixteenth);
        assert_eq!(pattern.length(), 16);
        assert!(pattern.blocks().is_empty());
        assert!(pattern.volumes().iter().all(|&volume| volume == 1.0));
        assert_eq!(pattern, Pattern::default());
        check_invariants(&pattern);
    }

    /// The note values are a whole bar down to a 1/64, with triplets, with
    /// the denominators and triplet flags the beat maths reads them by.
    #[test]
    fn note_values_are_a_bar_down_to_a_sixty_fourth_with_triplets() {
        let names: Vec<String> = NoteValue::ALL.iter().map(|value| value.to_string()).collect();
        assert_eq!(names, [
            "1/1", "1/1T", "1/2", "1/2T", "1/4", "1/4T", "1/8", "1/8T", "1/16", "1/16T", "1/32",
            "1/32T", "1/64", "1/64T"
        ]);

        for (index, value) in NoteValue::ALL.into_iter().enumerate() {
            assert_eq!(value.to_u8() as usize, index, "{value} is not at its own discriminant");
            assert_eq!(NoteValue::from_u8(value.to_u8()), value);
            assert_eq!(value.is_triplet(), index % 2 == 1);
            assert_eq!(value.denominator(), 1 << (index / 2));
        }

        assert_eq!(NoteValue::default(), NoteValue::DEFAULT);
        assert_eq!(NoteValue::DEFAULT, NoteValue::Sixteenth);
    }

    /// The five rates, and the slice one step of each is — the mapping a new
    /// block's default slice comes from.
    #[test]
    fn rates_are_the_five_of_the_spec_and_name_their_own_step_as_a_slice() {
        let names: Vec<String> = Rate::ALL.iter().map(|rate| rate.to_string()).collect();
        assert_eq!(names, ["1/8", "1/8T", "1/16", "1/16T", "1/32"]);

        for (index, rate) in Rate::ALL.into_iter().enumerate() {
            assert_eq!(rate.to_u8() as usize, index, "{rate} is not at its own discriminant");
            assert_eq!(Rate::from_u8(rate.to_u8()), rate);
            assert_eq!(rate.note_value().to_string(), rate.to_string());
            assert_eq!(rate.denominator(), rate.note_value().denominator());
            assert_eq!(rate.is_triplet(), rate.note_value().is_triplet());
        }

        assert_eq!(Rate::default(), Rate::DEFAULT);
        assert_eq!(Rate::DEFAULT, Rate::Sixteenth);
    }

    /// A stored discriminant from a corrupt or newer snapshot decodes to the
    /// default instead of panicking — the enum half of "a corrupt snapshot
    /// loads as defaults".
    #[test]
    fn an_unknown_stored_discriminant_decodes_to_the_default() {
        for raw in [14, 21, 64, 200, u8::MAX] {
            assert_eq!(NoteValue::from_u8(raw), NoteValue::DEFAULT, "note value {raw}");
        }
        for raw in [5, 12, 64, 200, u8::MAX] {
            assert_eq!(Rate::from_u8(raw), Rate::DEFAULT, "rate {raw}");
        }
    }

    /// The default action is the classic stutter at the pattern's own rate,
    /// and a preset action carries the endpoints its name promises.
    #[test]
    fn a_new_block_repeats_one_step_from_the_block_start() {
        assert_eq!(Action::default(), Action::repeat(NoteValue::Sixteenth));
        let triplet = Action::repeat(NoteValue::EighthTriplet);
        assert_eq!(Action::repeat_at(Rate::EighthTriplet), triplet);

        for preset in SpeedPreset::ALL {
            let action = Action::from_preset(preset);
            assert_eq!(action.sanitised(), action, "{preset:?} is not already clean");
            assert_eq!(action.speeds(), preset.speeds(), "{preset:?}");
            assert_eq!(action.preset(), Some(preset), "{preset:?}");
            assert_eq!(action.source, Source::BlockStart);
        }

        let repeat = Action::repeat_at(Rate::Sixteenth);
        assert!(repeat.is_repeat());
        assert_eq!((repeat.speeds(), repeat.preset()), ((1.0, 1.0), None));
    }

    /// Out-of-range speeds reaching an action — from the GUI or from a
    /// snapshot — come back inside ±2, NaN included
    /// (`crate::actions::clamp_speed`'s rule: nonsense is live).
    #[test]
    fn an_action_s_speeds_are_clamped_however_they_arrive() {
        assert_eq!(Action::ramp(9.0, -9.0).speeds(), (2.0, -2.0));
        assert_eq!(Action::ramp(f32::INFINITY, f32::NEG_INFINITY).speeds(), (2.0, -2.0));
        assert_eq!(Action::ramp(f32::NAN, 0.5).speeds(), (1.0, 0.5));

        let hostile = Action { from: 40.0, to: f32::NAN, ..Action::default() };
        assert_eq!(hostile.sanitised(), Action { from: 2.0, to: 1.0, ..Action::default() });

        // An in-range ramp is left exactly alone, including its sign.
        let drawn = Action::ramp(-0.25, 1.75);
        assert_eq!(drawn.sanitised(), drawn);
        assert_eq!(drawn.speeds(), (-0.25, 1.75));
    }

    /// A preset is named by its speeds alone, on an unlooped block, from
    /// any source; a loop is never a preset.
    #[test]
    fn a_preset_is_named_by_its_speeds() {
        assert_eq!(Action::ramp(0.3, 0.7).preset(), None);
        assert_eq!(Action::ramp(1.0, 0.0).preset(), Some(SpeedPreset::TapeStop));
        assert_eq!(Action::ramp(-9.0, -1.0).preset(), None, "-9 clamps to -2, not -1");

        let continued = Action::ramp(1.0, 0.0).with_source(Source::Continue);
        assert_eq!(continued.preset(), Some(SpeedPreset::TapeStop));

        let looped = Action { looping: Some(NoteValue::Quarter), ..Action::ramp(1.0, 0.0) };
        assert_eq!(looped.preset(), None);
    }

    /// A spot is counted back from the block's start to the last time its
    /// step played: earlier in this pass, or in the previous pass for a spot
    /// at or after the block — plus any extra passes. Other sources have no
    /// spot.
    #[test]
    fn a_spot_is_counted_back_to_the_last_time_its_step_played() {
        let spot = |step, passes| Action::default().with_source(Source::Spot { step, passes });
        let pattern = row_of(&[(0, 1, spot(0, 0)), (8, 2, spot(2, 0)), (12, 1, spot(14, 1))]);

        assert_eq!(pattern.spot_steps_back(0), Some(0), "its own step: no lag");
        assert_eq!(pattern.spot_steps_back(1), Some(6));
        assert_eq!(pattern.spot_steps_back(2), Some(14 + 16), "the pass before, and one more");
        assert_eq!(pattern.spot_steps_back(3), None);

        let others = row_of(&[(0, 1, Action::default()), (1, 1, continuing(Action::default()))]);
        assert_eq!(others.spot_steps_back(0), None);
        assert_eq!(others.spot_steps_back(1), None);

        // Shortened under a spot, the spot reads the last step there is.
        let mut short = row_of(&[(2, 1, spot(14, 0))]);
        short.set_length(8);
        assert_eq!(short.spot_steps_back(0), Some(2 + 8 - 7));
    }

    /// A spot past the longest pattern or too many passes back is clamped.
    #[test]
    fn a_spot_is_clamped_to_what_a_pattern_can_hold() {
        let wild = Action::default().with_source(Source::Spot { step: 900, passes: 200 });
        let source = Source::Spot { step: MAX_STEPS as u16 - 1, passes: MAX_SOURCE_PASSES };
        assert_eq!(wild.sanitised().source, source);
    }

    /// A block clamps what it can on its own: a zero length becomes one
    /// step, a span past [`MAX_STEPS`] is cut back, and the action it is
    /// handed is sanitised.
    #[test]
    fn a_block_is_at_least_one_step_and_fits_inside_the_longest_pattern() {
        let block = Block::new(3, 0, Action::default());
        assert_eq!((block.start(), block.len(), block.end()), (3, 1, 4));

        let clamped = Block::new(9_000, 9_000, Action::default());
        assert_eq!(clamped.start(), MAX_STEPS as u16 - 1);
        assert_eq!(clamped.end(), MAX_STEPS as u16);

        let long = Block::new(60, 9_000, Action::default());
        assert_eq!((long.start(), long.len()), (60, 4));

        let dirty = Block::new(0, 1, Action { from: 7.0, to: 7.0, ..Action::default() });
        assert_eq!(dirty.action().speeds(), (2.0, 2.0));

        let mut block = Block::new(0, 2, Action::default());
        block.set_action(Action { from: -7.0, to: 0.5, ..Action::default() });
        assert_eq!(block.action(), Action { from: -2.0, to: 0.5, ..Action::default() });

        assert!(block.covers(0) && block.covers(1));
        assert!(!block.covers(2));
    }

    /// The length clamps to `1..=64`, and the Time row comes back inside it:
    /// a block past the end is dropped, one overhanging it is truncated.
    #[test]
    fn setting_the_length_clamps_it_and_brings_the_blocks_inside() {
        let mut pattern = Pattern::new();
        pattern.set_length(0);
        assert_eq!(pattern.length(), 1);
        check_invariants(&pattern);

        pattern.set_length(u16::MAX);
        assert_eq!(pattern.length(), 64);
        check_invariants(&pattern);

        assert_eq!(pattern.add_block(0, 4, Action::default()), Some(0));
        assert_eq!(pattern.add_block(10, 4, Action::default()), Some(1));
        assert_eq!(pattern.add_block(40, 4, Action::default()), Some(2));

        pattern.set_length(12);
        assert_eq!(pattern.blocks().len(), 2);
        assert_eq!((pattern.blocks()[0].start(), pattern.blocks()[0].len()), (0, 4));
        assert_eq!((pattern.blocks()[1].start(), pattern.blocks()[1].len()), (10, 2));
        check_invariants(&pattern);

        // A block starting on the last step keeps that step, never zero.
        pattern.set_length(11);
        assert_eq!((pattern.blocks()[1].start(), pattern.blocks()[1].len()), (10, 1));
        check_invariants(&pattern);
    }

    /// Shrinking and growing the pattern gives back the Volume row that was
    /// drawn: levels past the end are unused, not erased.
    #[test]
    fn the_volume_row_survives_a_shorter_pattern() {
        let mut pattern = Pattern::new();
        pattern.set_volume(20, 0.25);
        pattern.set_length(4);
        assert_eq!(pattern.volume(20), 0.25);

        pattern.set_length(32);
        assert_eq!(pattern.volume(20), 0.25);
        check_invariants(&pattern);
    }

    /// Volumes clamp into `0.0..=1.0` with the NaN rule above, and a
    /// step outside the row is neither written nor a panic.
    #[test]
    fn setting_a_volume_clamps_it_and_ignores_a_step_outside_the_row() {
        let mut pattern = Pattern::new();

        pattern.set_volume(0, 4.0);
        pattern.set_volume(1, -4.0);
        pattern.set_volume(2, f32::NAN);
        pattern.set_volume(3, f32::INFINITY);
        pattern.set_volume(4, f32::NEG_INFINITY);
        pattern.set_volume(5, 0.375);

        assert_eq!(pattern.volume(0), 1.0);
        assert_eq!(pattern.volume(1), 0.0);
        assert_eq!(pattern.volume(2), 0.0);
        assert_eq!(pattern.volume(3), 1.0);
        assert_eq!(pattern.volume(4), 0.0);
        assert_eq!(pattern.volume(5), 0.375);

        pattern.set_volume(MAX_STEPS as u16, 0.5);
        pattern.set_volume(u16::MAX, 0.5);
        assert_eq!(pattern.volume(MAX_STEPS as u16), Pattern::DEFAULT_VOLUME);
        assert_eq!(pattern.volume(u16::MAX), Pattern::DEFAULT_VOLUME);
        check_invariants(&pattern);
    }

    /// The snapshot door for the Volume row: any number of levels, any
    /// values, padded and truncated to the row and clamped on the way in.
    #[test]
    fn a_volume_row_from_a_snapshot_is_padded_truncated_and_clamped() {
        let mut pattern = Pattern::new();
        pattern.set_volumes([0.5, -1.0, f32::NAN, 2.0]);

        assert_eq!(pattern.volumes()[..4], [0.5, 0.0, 0.0, 1.0]);
        assert!(pattern.volumes()[4..].iter().all(|&volume| volume == 1.0));
        check_invariants(&pattern);

        pattern.set_volumes(std::iter::repeat_n(0.25, MAX_STEPS * 4));
        assert!(pattern.volumes().iter().all(|&volume| volume == 0.25));
        check_invariants(&pattern);

        pattern.set_volumes(std::iter::empty());
        assert!(pattern.volumes().iter().all(|&volume| volume == 1.0));
        check_invariants(&pattern);
    }

    /// Adding a block: the start is clamped into the pattern, the length to
    /// the gap in front of it, and the row stays sorted whatever order the
    /// blocks are drawn in.
    #[test]
    fn adding_a_block_clamps_it_into_the_gap_it_is_dropped_in() {
        let mut pattern = Pattern::new();

        assert_eq!(pattern.add_block(12, 4, Action::default()), Some(0));
        assert_eq!(pattern.add_block(4, 0, Action::default()), Some(0));
        assert_eq!(pattern.add_block(8, 9_000, Action::default()), Some(1));
        check_invariants(&pattern);

        let spans: Vec<(u16, u16)> =
            pattern.blocks().iter().map(|block| (block.start(), block.len())).collect();
        assert_eq!(spans, [(4, 1), (8, 4), (12, 4)]);

        // A start past the end of the pattern lands on the last step, where
        // there is room for one step of block and no more.
        let mut edge = Pattern::new();
        assert_eq!(edge.add_block(u16::MAX, 4, Action::default()), Some(0));
        assert_eq!((edge.blocks()[0].start(), edge.blocks()[0].len()), (15, 1));
        check_invariants(&edge);
    }

    /// A gesture with nowhere to go adds nothing: a step some block already
    /// covers. Filling the longest pattern with one-step blocks reaches
    /// [`MAX_BLOCKS`] exactly as it covers the last step, which is why that
    /// bound is the same number as [`MAX_STEPS`].
    #[test]
    fn adding_a_block_on_a_covered_step_or_a_full_row_does_nothing() {
        let mut pattern = Pattern::new();
        assert_eq!(pattern.add_block(4, 4, Action::default()), Some(0));

        for start in 4..8 {
            assert_eq!(pattern.add_block(start, 1, Action::default()), None, "step {start}");
        }
        assert_eq!(pattern.blocks().len(), 1);
        check_invariants(&pattern);

        let mut full = Pattern::new();
        full.set_length(Pattern::MAX_LENGTH);
        for start in 0..MAX_STEPS as u16 {
            assert!(full.add_block(start, 1, Action::default()).is_some(), "step {start}");
        }
        assert_eq!(full.blocks().len(), MAX_BLOCKS);

        full.set_length(32);
        assert_eq!(full.blocks().len(), 32);
        check_invariants(&full);
    }

    /// Moving a block stops it against both neighbours and both ends of the
    /// pattern, keeps its length, and leaves its index alone.
    #[test]
    fn moving_a_block_stops_against_its_neighbours_and_the_pattern_ends() {
        let mut pattern = Pattern::new();
        assert_eq!(pattern.add_block(0, 2, Action::default()), Some(0));
        assert_eq!(pattern.add_block(6, 2, Action::default()), Some(1));
        assert_eq!(pattern.add_block(12, 2, Action::default()), Some(2));

        // Left, it stops on block 0's end; right, before block 2's start.
        assert_eq!(pattern.move_block(1, 0), Some(2));
        assert_eq!(pattern.move_block(1, u16::MAX), Some(10));
        assert_eq!(pattern.blocks()[1].len(), 2);
        check_invariants(&pattern);

        // And at the ends of the pattern: block 2 stops at step 14 of 16.
        assert_eq!(pattern.move_block(2, 9_000), Some(14));
        assert_eq!(pattern.move_block(0, 9_000), Some(8));
        assert_eq!(pattern.move_block(0, 0), Some(0));
        assert_eq!(pattern.move_block(0, 9_000), Some(8));
        check_invariants(&pattern);

        // The row is still in the order it was, blocks 0..2 left to right.
        let starts: Vec<u16> = pattern.blocks().iter().map(Block::start).collect();
        assert_eq!(starts, [8, 10, 14]);

        assert_eq!(pattern.move_block(3, 0), None);
        assert_eq!(pattern.move_block(usize::MAX, 0), None);
        check_invariants(&pattern);
    }

    /// Resizing from the right edge: never below one step, never into the
    /// next block or past the end of the pattern.
    #[test]
    fn resizing_a_block_clamps_to_one_step_and_to_the_gap_in_front() {
        let mut pattern = Pattern::new();
        assert_eq!(pattern.add_block(2, 1, Action::default()), Some(0));
        assert_eq!(pattern.add_block(8, 1, Action::default()), Some(1));

        assert_eq!(pattern.resize_block(0, 0), Some(1));
        assert_eq!(pattern.resize_block(0, 9_000), Some(6));
        assert_eq!(pattern.blocks()[0].end(), 8);
        check_invariants(&pattern);

        assert_eq!(pattern.resize_block(1, u16::MAX), Some(8));
        assert_eq!(pattern.blocks()[1].end(), 16);
        check_invariants(&pattern);

        assert_eq!(pattern.resize_block(2, 4), None);
        check_invariants(&pattern);
    }

    /// The left-edge drag: both edges at once, the start clamped into the
    /// block's own gap and the length to what is left of it.
    #[test]
    fn setting_a_span_clamps_the_start_into_the_gap_and_shrinks_to_fit() {
        let mut pattern = Pattern::new();
        assert_eq!(pattern.add_block(2, 2, Action::default()), Some(0));
        assert_eq!(pattern.add_block(8, 4, Action::default()), Some(1));

        // Dragging block 1's left edge out to step 4 keeps its right edge.
        assert_eq!(pattern.set_block_span(1, 4, 8), Some((4, 8)));
        check_invariants(&pattern);

        // Further left it stops on block 0's end, and the length follows.
        assert_eq!(pattern.set_block_span(1, 0, 12), Some((4, 12)));
        assert_eq!(pattern.blocks()[1].start(), 4);
        check_invariants(&pattern);

        // Dragging the left edge past the right one leaves one step.
        assert_eq!(pattern.set_block_span(1, u16::MAX, 0), Some((15, 1)));
        check_invariants(&pattern);

        assert_eq!(pattern.set_block_span(7, 0, 1), None);
        check_invariants(&pattern);
    }

    /// Duplicating puts a copy in the space after the block, cut to the room
    /// there, and refuses where there is none.
    #[test]
    fn duplicating_a_block_fills_the_space_after_it_or_refuses() {
        let mut pattern = Pattern::new();
        assert_eq!(pattern.add_block(0, 2, Action::from_preset(SpeedPreset::TapeStop)), Some(0));

        assert_eq!(pattern.duplicate_block(0), Some(1));
        assert_eq!((pattern.blocks()[1].start(), pattern.blocks()[1].len()), (2, 2));
        assert_eq!(pattern.blocks()[1].action(), Action::from_preset(SpeedPreset::TapeStop));
        check_invariants(&pattern);

        // One step of room gives a one-step copy.
        assert_eq!(pattern.add_block(5, 1, Action::default()), Some(2));
        assert_eq!(pattern.duplicate_block(1), Some(2));
        assert_eq!((pattern.blocks()[2].start(), pattern.blocks()[2].len()), (4, 1));
        check_invariants(&pattern);

        // No room: adjacent neighbour, and the end of the pattern.
        assert_eq!(pattern.duplicate_block(2), None);
        assert_eq!(pattern.add_block(15, 1, Action::default()), Some(4));
        assert_eq!(pattern.duplicate_block(4), None);
        assert_eq!(pattern.duplicate_block(9), None);
        check_invariants(&pattern);
    }

    /// Deleting, and the index that is not there.
    #[test]
    fn deleting_a_block_removes_one_and_an_unknown_index_removes_none() {
        let mut pattern = with_blocks(&[1, 5, 9]);

        assert!(pattern.delete_block(1));
        assert_eq!(pattern.blocks().iter().map(Block::start).collect::<Vec<_>>(), [1, 9]);

        assert!(!pattern.delete_block(2));
        assert!(!pattern.delete_block(usize::MAX));
        assert_eq!(pattern.blocks().len(), 2);
        check_invariants(&pattern);
    }

    /// A new action on a block is sanitised like any other, and an unknown
    /// index changes nothing.
    #[test]
    fn setting_a_block_action_sanitises_it() {
        let mut pattern = with_blocks(&[0]);

        let lying = Action { from: 99.0, to: -99.0, ..Action::ramp(1.0, 1.0) };
        assert!(pattern.set_block_action(0, lying));
        assert_eq!(pattern.blocks()[0].action(), Action::ramp(2.0, -2.0));

        assert!(!pattern.set_block_action(1, Action::default()));
        assert_eq!(pattern.blocks().len(), 1);
        check_invariants(&pattern);
    }

    /// The snapshot door for the Time row: unsorted, overlapping,
    /// out-of-range, over-long and dirty blocks all at once — sorted,
    /// dropped, clamped and cut to [`MAX_BLOCKS`].
    #[test]
    fn a_time_row_from_a_snapshot_is_sorted_clamped_and_de_overlapped() {
        let mut pattern = Pattern::new();
        pattern.set_blocks([
            Block::new(9, 2, Action::default()),           // kept, out of order
            Block::new(2, 3, Action::default()),           // kept, first
            Block::new(3, 9_000, Action::default()),       // overlaps block at 2 → dropped
            Block::new(40, 1, Action::default()),          // past length 16 → dropped
            Block::new(14, 9_000, Action::ramp(9.0, 9.0)), // kept, truncated and clamped
        ]);

        let spans: Vec<(u16, u16)> =
            pattern.blocks().iter().map(|block| (block.start(), block.len())).collect();
        assert_eq!(spans, [(2, 3), (9, 2), (14, 2)]);
        assert_eq!(pattern.blocks()[2].action().speeds(), (2.0, 2.0));
        check_invariants(&pattern);

        // Two blocks written on the same step: the first one in survives.
        let mut same = Pattern::new();
        same.set_blocks([
            Block::new(4, 1, Action::from_preset(SpeedPreset::Reverse)),
            Block::new(4, 1, Action::default()),
        ]);
        assert_eq!(same.blocks().len(), 1);
        assert_eq!(same.blocks()[0].action(), Action::from_preset(SpeedPreset::Reverse));
        check_invariants(&same);

        // More blocks than the row can hold, all of them legal.
        let mut crowded = Pattern::new();
        crowded.set_length(Pattern::MAX_LENGTH);
        crowded.set_blocks((0..400u16).map(|start| Block::new(start % 64, 1, Action::default())));
        assert_eq!(crowded.blocks().len(), MAX_BLOCKS);
        check_invariants(&crowded);
    }

    /// Looking a step up: the block covering it, and nothing for the gaps,
    /// the steps past the end and the far edge of `u16`.
    #[test]
    fn a_step_finds_the_block_over_it_and_only_that_block() {
        let mut pattern = Pattern::new();
        assert_eq!(pattern.add_block(2, 3, Action::default()), Some(0));
        assert_eq!(pattern.add_block(8, 1, Action::default()), Some(1));

        let found: Vec<Option<usize>> = (0..12).map(|step| pattern.block_at(step)).collect();
        assert_eq!(found, [
            None,
            None,
            Some(0),
            Some(0),
            Some(0),
            None,
            None,
            None,
            Some(1),
            None,
            None,
            None
        ]);

        assert_eq!(pattern.block_at(u16::MAX), None);
        assert_eq!(Pattern::new().block_at(0), None);
    }

    /// Clearing takes the drawing off the grid and leaves the grid: no
    /// blocks, the Volume row at unity, the rate and length as they were.
    #[test]
    fn clearing_keeps_the_rate_and_the_length() {
        let mut pattern = with_blocks(&[0, 4, 8]);
        pattern.set_rate(Rate::ThirtySecond);
        pattern.set_length(9);
        pattern.set_volume(3, 0.0);
        pattern.choose_filter(Filter::HighPass);
        pattern.set_lane(Lane::Resonance, [0.5]);

        pattern.clear();

        assert!(pattern.blocks().is_empty());
        assert!(pattern.volumes().iter().all(|&volume| volume == 1.0));
        assert_eq!(pattern.filter(), Filter::Off);
        assert_eq!(pattern.lane(Lane::Cutoff), Pattern::new().lane(Lane::Cutoff));
        assert_eq!(pattern.lane(Lane::Resonance), Pattern::new().lane(Lane::Resonance));
        assert_eq!(pattern.rate(), Rate::ThirtySecond);
        assert_eq!(pattern.length(), 9);
        check_invariants(&pattern);
    }

    /// Setting the rate moves nothing: the Time row keeps its step
    /// positions, so a rate change stretches the pattern in time.
    #[test]
    fn setting_the_rate_leaves_the_rows_alone() {
        let mut pattern = with_blocks(&[1, 7]);
        let before = pattern.blocks().to_vec();

        for rate in Rate::ALL {
            pattern.set_rate(rate);
            assert_eq!(pattern.rate(), rate);
            assert_eq!(pattern.blocks(), before.as_slice());
            check_invariants(&pattern);
        }
    }

    /// Every note value's length in beats, and the exactness the ×2/3 order
    /// buys: three triplets fill exactly two of their base note, with no
    /// epsilon anywhere.
    #[test]
    fn note_value_lengths_are_the_spec_s_and_the_triplets_are_exact() {
        let beats: Vec<f64> = NoteValue::ALL.iter().map(|value| value.beats()).collect();
        assert_eq!(beats, [
            4.0,
            8.0 / 3.0,
            2.0,
            4.0 / 3.0,
            1.0,
            2.0 / 3.0,
            0.5,
            1.0 / 3.0,
            0.25,
            1.0 / 6.0,
            0.125,
            1.0 / 12.0,
            0.0625,
            1.0 / 24.0,
        ]);

        for value in NoteValue::ALL {
            let base = 4.0 / f64::from(value.denominator());
            if value.is_triplet() {
                assert_eq!(3.0 * value.beats(), 2.0 * base, "three {value}s");
            } else {
                assert_eq!(value.beats(), base, "{value}");
            }
            assert!(value.beats() > 0.0 && value.beats().is_finite(), "{value}");
        }
    }

    /// The step length per rate and the span it adds up to: 16 steps of
    /// 1/16 are one 4/4 bar **exactly**, and so is every other rate's bar.
    #[test]
    fn a_sixteen_step_sixteenth_pattern_spans_one_bar_exactly() {
        let steps: Vec<f64> = Rate::ALL.iter().map(|rate| rate.step_beats()).collect();
        assert_eq!(steps, [0.5, 1.0 / 3.0, 0.25, 1.0 / 6.0, 0.125]);

        let fresh = Pattern::new();
        assert_eq!(fresh.step_beats(), 0.25);
        assert_eq!(fresh.span_beats(), 4.0);

        let mut pattern = Pattern::new();
        for (rate, bar) in [
            (Rate::Eighth, 8),
            (Rate::EighthTriplet, 12),
            (Rate::Sixteenth, 16),
            (Rate::SixteenthTriplet, 24),
            (Rate::ThirtySecond, 32),
        ] {
            pattern.set_rate(rate);
            pattern.set_length(bar);
            assert_eq!(pattern.step_beats(), rate.step_beats(), "{rate}");
            assert_eq!(pattern.span_beats(), 4.0, "{bar} steps of {rate}");
        }

        // The default 16 steps at each rate: two bars down to half of one,
        // the triplets a third of a beat at a time.
        for (rate, span) in [
            (Rate::Eighth, 8.0),
            (Rate::EighthTriplet, 16.0 / 3.0),
            (Rate::Sixteenth, 4.0),
            (Rate::SixteenthTriplet, 8.0 / 3.0),
            (Rate::ThirtySecond, 2.0),
        ] {
            pattern.set_rate(rate);
            pattern.set_length(Pattern::DEFAULT_LENGTH);
            assert_eq!(pattern.span_beats(), span, "16 steps of {rate}");
        }
    }

    /// A project beat becomes the step it is in and the phase inside it,
    /// measured from beat 0 of the project and wrapping at the span — a
    /// count-in's negative beats included.
    #[test]
    fn a_project_beat_becomes_the_step_it_is_in_and_the_phase_inside_it() {
        let pattern = Pattern::new();

        for (beat, step, phase) in [
            (0.0, 0, 0.0),
            (0.125, 0, 0.125),
            (0.25, 1, 0.0),
            (0.375, 1, 0.125),
            (3.75, 15, 0.0),
            (3.9375, 15, 0.1875),
            // The next bar is the same bar: a pattern has no phase of its
            // own, so bar 2 and bar 26 read like bar 1.
            (4.0, 0, 0.0),
            (4.25, 1, 0.0),
            (100.5, 2, 0.0),
            // A count-in runs it backwards from the end.
            (-0.25, 15, 0.0),
            (-0.125, 15, 0.125),
            (-4.0, 0, 0.0),
            (-4.25, 15, 0.0),
        ] {
            let position = pattern.position_at(beat).expect("a finite beat");

            assert_eq!(position.step(), step, "beat {beat}");
            assert_eq!(position.step_phase(), phase, "beat {beat}");
            assert_eq!(position.pattern_phase(), f64::from(step) * 0.25 + phase, "beat {beat}");

            // Nothing is drawn on this pattern, so every step plays live.
            assert!(position.is_live(), "beat {beat}");
            assert_eq!(position.block(), None, "beat {beat}");
            assert_eq!((position.block_phase(), position.block_beats()), (0.0, 0.0), "{beat}");
        }
    }

    /// A position carries the block over its step with the `p` and `B` the
    /// offset maths is written in terms of, measured from that block's start
    /// in the cycle the beat is in.
    #[test]
    fn a_position_carries_the_block_over_it_with_its_phase_and_span() {
        let mut pattern = Pattern::new();
        assert_eq!(pattern.add_block(4, 4, Action::default()), Some(0));
        assert_eq!(pattern.add_block(12, 1, Action::from_preset(SpeedPreset::Reverse)), Some(1));

        // Block 0 is steps 4..8, a beat long from beat 1.0; block 1 is step
        // 12 alone, a quarter note long from beat 3.0.
        for (beat, block, phase, beats) in [
            (0.0, None, 0.0, 0.0),
            (0.75, None, 0.0, 0.0),
            (1.0, Some(0), 0.0, 1.0),
            (1.5, Some(0), 0.5, 1.0),
            (1.9375, Some(0), 0.9375, 1.0),
            (2.0, None, 0.0, 0.0),
            (3.0, Some(1), 0.0, 0.25),
            (3.125, Some(1), 0.125, 0.25),
            (3.25, None, 0.0, 0.0),
            // The row repeats with the cycle, phases and all.
            (5.5, Some(0), 0.5, 1.0),
            (-3.0, Some(0), 0.0, 1.0),
            (-1.0, Some(1), 0.0, 0.25),
        ] {
            let position = pattern.position_at(beat).expect("a finite beat");

            assert_eq!(position.block(), block, "beat {beat}");
            assert_eq!(position.block_phase(), phase, "beat {beat}");
            assert_eq!(position.block_beats(), beats, "beat {beat}");
            assert_eq!(position.is_live(), block.is_none(), "beat {beat}");

            if let Some(index) = position.block() {
                assert!(pattern.blocks()[index].covers(position.step()), "beat {beat}");
            }
        }
    }

    /// A beat that is not a finite number has no position at all — the host
    /// is reporting nonsense, and the engine plays live rather than reading
    /// a NaN position out of the history buffer.
    #[test]
    fn a_beat_that_is_not_a_finite_number_has_no_position() {
        let pattern = Pattern::new();

        for beat in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(pattern.position_at(beat).is_none(), "beat {beat}");
        }
    }

    /// [`Pattern::phase_at`] is the fold [`Pattern::position_at`] uses, not
    /// a second one beside it: the scope indexes its columns by the phase
    /// alone and the two must be the same number to the bit, or a column
    /// would summarise audio from a step the grid drew somewhere else.
    #[test]
    fn the_phase_on_its_own_is_the_one_the_position_is_measured_from() {
        for rate in Rate::ALL {
            for length in [1, 3, 16, 64] {
                let mut pattern = with_blocks(&[0]);
                pattern.set_rate(rate);
                pattern.set_length(length);

                for step in 0..length {
                    for offset in [0.0, 0.5, 0.9999] {
                        let beat = (f64::from(step) + offset) * pattern.step_beats();
                        let position = pattern.position_at(beat).expect("a finite beat");

                        assert_eq!(pattern.phase_at(beat), Some(position.pattern_phase()));

                        // And a count-in's negative beats fold *into* the
                        // cycle like any other — to within the rounding of
                        // the subtraction, which is why this is the one
                        // assertion here that is not an equality.
                        let span = pattern.span_beats();
                        let before = pattern.phase_at(beat - 4.0 * span).expect("a finite beat");
                        assert!((0.0..span).contains(&before), "beat {beat}, phase {before}");
                    }
                }

                assert_eq!(pattern.phase_at(f64::NAN), None);
                assert_eq!(pattern.phase_at(f64::INFINITY), None);
            }
        }
    }

    /// The two float edges of the fold, both of them real at a triplet step
    /// length: a beat a hair below the span, where the division rounds the
    /// step up to `length`, and a beat a hair below zero, where
    /// `rem_euclid` hands back the span itself.
    #[test]
    fn the_edges_of_a_cycle_land_inside_the_pattern() {
        let mut pattern = Pattern::new();
        pattern.set_rate(Rate::EighthTriplet);
        pattern.set_length(12);
        assert_eq!(pattern.span_beats(), 4.0);

        let last = pattern.position_at(pattern.span_beats().next_down()).expect("a finite beat");
        assert_eq!(last.step(), 11);
        assert!(last.step_phase() <= pattern.step_beats(), "{}", last.step_phase());

        let wrapped = pattern.position_at(-1e-300).expect("a finite beat");
        assert_eq!(wrapped.pattern_phase(), 0.0);
        assert_eq!((wrapped.step(), wrapped.step_phase()), (0, 0.0));
    }

    /// Whatever the rate, the length and the row, a position is one the
    /// audio thread can use without checking it: a step inside the pattern,
    /// both phases inside their spans, and a block index that really covers
    /// the step it came with.
    #[test]
    fn every_position_is_one_the_audio_thread_can_use() {
        for rate in Rate::ALL {
            for length in [1, 3, 7, 16, 17, 64] {
                let mut pattern = with_blocks(&[0, 3, 4, 11]);
                pattern.set_rate(rate);
                pattern.set_length(length);
                assert!(pattern.resize_block(0, 3).is_some());
                check_invariants(&pattern);

                let (span, step_beats) = (pattern.span_beats(), pattern.step_beats());
                for tick in -500..500 {
                    // A stride that divides neither the span nor a step, so
                    // the walk lands inside steps, on their edges and a hair
                    // either side of a cycle's.
                    let beat = f64::from(tick) * span / 97.0;
                    let position = pattern.position_at(beat).expect("a finite beat");
                    let where_ = format!("beat {beat} on {length} steps of {rate}");

                    assert!(position.step() < length, "{where_}");
                    assert!((0.0..span).contains(&position.pattern_phase()), "{where_}");
                    assert!((0.0..=step_beats).contains(&position.step_phase()), "{where_}");

                    match position.block() {
                        Some(index) => {
                            let block = pattern.blocks()[index];
                            assert!(block.covers(position.step()), "{where_}");
                            assert_eq!(position.block_beats(), block.beats(step_beats), "{where_}");
                            assert!(
                                (0.0..=position.block_beats()).contains(&position.block_phase()),
                                "{where_}"
                            );
                            assert!(!position.is_live(), "{where_}");
                        }
                        None => {
                            assert!(position.is_live(), "{where_}");
                            assert_eq!(position.block_phase(), 0.0, "{where_}");
                            assert_eq!(position.block_beats(), 0.0, "{where_}");
                        }
                    }
                }
            }
        }
    }

    /// The point of the module, as one test: a long deterministic run of
    /// hostile calls to **every** setter, in every order it can reach, with
    /// the invariants checked after each one. No sequence of gestures and no
    /// loaded snapshot can leave a pattern the audio thread cannot trust.
    #[test]
    fn no_sequence_of_hostile_setter_calls_can_break_the_invariants() {
        let mut pattern = Pattern::new();
        // A tiny deterministic generator, so the walk is the same every run.
        let mut state = 0x9E37_79B9_u32;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state
        };

        for _ in 0..4_000 {
            let roll = next();
            // Deliberately wild: steps and lengths well past MAX_STEPS,
            // indices past the row, speeds past ±2, volumes past 1.
            let step = (roll % 200) as u16;
            let other = (roll >> 12) as u16 % 200;
            let index = (roll >> 8) as usize % 70;
            let number = (roll >> 16) as f32 / 1_000.0 - 20.0;

            match roll % 11 {
                0 => pattern.set_rate(Rate::from_u8((roll >> 4) as u8)),
                1 => pattern.set_length(step),
                2 => pattern.set_volume(step, number),
                3 => pattern.set_volumes((0..step).map(|i| number + f32::from(i))),
                4 => {
                    let action = Action::ramp(number, -number);
                    let _ = pattern.add_block(step, other, action);
                }
                5 => {
                    let _ = pattern.move_block(index, step);
                }
                6 => {
                    let _ = pattern.resize_block(index, step);
                }
                7 => {
                    let _ = pattern.set_block_span(index, step, other);
                }
                8 => {
                    let looping = NoteValue::ALL.get(index % 16).copied();
                    let source = [Source::BlockStart, Source::Continue][index % 2];
                    let hostile = Action { source, looping, from: number, to: f32::NAN };
                    pattern.set_block_action(index, hostile);
                    let _ = pattern.block_path(index % 70);
                }
                9 => {
                    let _ = pattern.duplicate_block(index);
                }
                _ => {
                    pattern.delete_block(index);
                }
            }

            check_invariants(&pattern);
        }

        // The walk actually drew something, rather than refusing everything.
        assert!(!pattern.blocks().is_empty());
    }

    // ---- block paths: sources and continuing ----

    /// A sixteen-step 1/16 pattern with these blocks.
    fn row_of(blocks: &[(u16, u16, Action)]) -> Pattern {
        let mut pattern = Pattern::new();
        for &(start, len, action) in blocks {
            assert!(pattern.add_block(start, len, action).is_some(), "block at {start}");
        }
        pattern
    }

    fn continuing(action: Action) -> Action {
        action.with_source(Source::Continue)
    }

    #[test]
    fn a_block_from_its_own_start_has_no_lag_and_one_from_a_spot_lags_back_to_it() {
        let reverse = Action::from_preset(SpeedPreset::Reverse);
        let before = reverse.with_source(Source::Spot { step: 0, passes: 0 });
        let pattern = row_of(&[(0, 4, reverse), (8, 4, before)]);

        assert_eq!(pattern.block_path(0).unwrap().offset(0.0), 0.0);
        assert_eq!(pattern.block_path(1).unwrap().offset(0.0), 2.0);
        assert_eq!(pattern.block_path(1).unwrap().offset(0.5), 3.0, "reversing from there");
        assert_eq!(pattern.block_path(2), None);
    }

    #[test]
    fn a_continue_picks_up_where_the_block_before_left_off() {
        // Three repeats of one beat (twelve 1/16 steps), then a stop: the
        // stop opens three beats back — on the beat that was looping.
        let stop = continuing(Action::from_preset(SpeedPreset::TapeStop));
        let pattern = row_of(&[(0, 12, Action::repeat(NoteValue::Quarter)), (12, 4, stop)]);

        let repeat = pattern.block_path(0).unwrap();
        let tail = pattern.block_path(1).unwrap();
        assert_eq!(repeat.end_offset(), 3.0);
        assert_eq!(tail.offset(0.0), 3.0);
        assert_eq!(tail.end_offset(), 3.0 + 0.5, "a one-beat tape stop falls half a beat behind");
    }

    #[test]
    fn a_continue_with_a_gap_before_it_reads_from_its_own_start() {
        let stop = continuing(Action::from_preset(SpeedPreset::TapeStop));
        let pattern = row_of(&[(0, 4, Action::repeat(NoteValue::Quarter)), (6, 2, stop)]);

        assert_eq!(pattern.block_path(1).unwrap().offset(0.0), 0.0);
    }

    #[test]
    fn a_chain_of_continues_carries_the_lag_all_the_way() {
        let stop = Action::from_preset(SpeedPreset::TapeStop);
        let half = continuing(Action::from_preset(SpeedPreset::HalfSpeed));
        let pattern = row_of(&[(0, 4, stop), (4, 4, half), (8, 4, half)]);

        // A one-beat stop ends half a beat behind; each half-speed beat after
        // it falls another half beat behind.
        assert_eq!(pattern.block_path(1).unwrap().offset(0.0), 0.5);
        assert_eq!(pattern.block_path(2).unwrap().offset(0.0), 1.0);
        assert_eq!(pattern.block_path(2).unwrap().end_offset(), 1.5);
    }

    #[test]
    fn the_first_block_continues_the_last_one_round_the_cycle() {
        let stop = continuing(Action::from_preset(SpeedPreset::TapeStop));
        let pattern = row_of(&[(0, 4, stop), (12, 4, Action::repeat(NoteValue::Quarter))]);

        assert_eq!(pattern.block_path(0).unwrap().offset(0.0), 1.0, "one loop of a beat");

        // Not when the last block stops short of the pattern's end.
        let short = row_of(&[(0, 4, stop), (11, 4, Action::repeat(NoteValue::Quarter))]);
        assert_eq!(short.block_path(0).unwrap().offset(0.0), 0.0);
    }

    #[test]
    fn a_ring_of_continues_is_resolved_without_running_forever() {
        let half = continuing(Action::from_preset(SpeedPreset::HalfSpeed));
        let ring = row_of(&[(0, 8, half), (8, 8, half)]);
        for index in 0..2 {
            let path = ring.block_path(index).unwrap();
            assert!(path.offset(0.0).is_finite() && path.offset(0.0) >= 0.0, "block {index}");
        }

        let alone = row_of(&[(0, 16, half)]);
        assert_eq!(alone.block_path(0).unwrap().offset(0.0), 0.0, "it cannot continue itself");
    }

    /// A new pattern has the filter off, the Cutoff row at the top and the
    /// Res row at the bottom; the rows clamp like the Volume row, pad with
    /// their own default, and read their default past the end.
    #[test]
    fn the_filter_rows_start_open_and_clamp_like_the_volume_row() {
        let mut pattern = Pattern::new();
        assert_eq!(pattern.filter(), Filter::Off);
        assert!(pattern.lane(Lane::Cutoff).iter().all(|&level| level == 1.0));
        assert!(pattern.lane(Lane::Resonance).iter().all(|&level| level == 0.0));
        assert_eq!(pattern.lane(Lane::Volume), pattern.volumes());

        pattern.set_lane(Lane::Cutoff, [0.25, 4.0, -1.0, f32::NAN]);
        assert_eq!(&pattern.lane(Lane::Cutoff)[..5], &[0.25, 1.0, 0.0, 0.0, 1.0]);
        pattern.set_lane(Lane::Resonance, [0.5]);
        assert_eq!(&pattern.lane(Lane::Resonance)[..2], &[0.5, 0.0]);
        assert_eq!(pattern.level(Lane::Resonance, 0), 0.5);
        assert_eq!(pattern.level(Lane::Resonance, u16::MAX), 0.0);
        assert_eq!(pattern.level(Lane::Cutoff, u16::MAX), 1.0);
        check_invariants(&pattern);
    }

    /// Picking a filter moves an untouched Cutoff row to where that filter
    /// does least, and leaves a drawn one alone.
    #[test]
    fn choosing_a_filter_opens_an_untouched_cutoff_row() {
        let mut pattern = Pattern::new();
        pattern.choose_filter(Filter::LowPass);
        assert!(pattern.lane(Lane::Cutoff).iter().all(|&level| level == 1.0));
        pattern.choose_filter(Filter::HighPass);
        assert!(pattern.lane(Lane::Cutoff).iter().all(|&level| level == 0.0));
        pattern.choose_filter(Filter::BandPass);
        assert!(pattern.lane(Lane::Cutoff).iter().all(|&level| level == 0.5));

        pattern.set_lane(Lane::Cutoff, [0.3]);
        let drawn = *pattern.lane(Lane::Cutoff);
        pattern.choose_filter(Filter::LowPass);
        assert_eq!(pattern.lane(Lane::Cutoff), &drawn, "a drawn row is the user's");
        assert_eq!(pattern.filter(), Filter::LowPass);

        // A load sets the shape alone.
        pattern.set_filter(Filter::HighPass);
        assert_eq!(pattern.lane(Lane::Cutoff), &drawn);
    }

    /// The filter's stored number round-trips, and nonsense loads as Off;
    /// the row levels map onto 20 Hz–20 kHz and a sane Q.
    #[test]
    fn the_filter_stores_as_a_number_and_its_rows_map_to_hz_and_q() {
        for filter in Filter::ALL {
            assert_eq!(Filter::from_u8(filter.to_u8()), filter);
        }
        assert_eq!(Filter::from_u8(200), Filter::Off);

        assert!((cutoff_hz(0.0) - 20.0).abs() < 1e-3);
        assert!((cutoff_hz(1.0) - 20_000.0).abs() < 1.0);
        assert!((cutoff_hz(0.5) - 632.5).abs() < 1.0, "the middle is the middle in octaves");
        assert_eq!(cutoff_hz(f32::NAN), cutoff_hz(0.0));
        assert!((resonance_q(0.0) - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
        assert!(resonance_q(1.0) > 8.0 && resonance_q(1.0) < 9.0);
    }
}
