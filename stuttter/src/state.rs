//! What a project saves, and the colours a pattern is read by.
//!
//! The versioned serde snapshot of everything Bitwig stores in the project —
//! all sixteen patterns and the two GUI prefs — is [`Snapshot`], and the
//! conversions between it and the model's [`StutterData`] are the two halves
//! of persistence: **out** is exact, **in** clamps. The host
//! parameters are never in it: Pattern, Mix and Smoothing are parameters, and
//! the host saves those itself.
//!
//! # The two directions
//!
//! [`Snapshot::of`] reads the live patterns into the stored shape and loses
//! nothing — the whole Volume row travels, not just the steps inside the
//! current length, because shrinking a pattern and growing it again is
//! supposed to give back the row that was drawn ([`Pattern::set_length`]).
//! So `model → snapshot → model` is the identity for anything the model can
//! hold, which is what the round-trip test asserts.
//!
//! [`Snapshot::data`] and [`Snapshot::prefs`] go the other way, and that way
//! is a **boundary**: a snapshot arrives from a project file, which is to say
//! from anywhere. Nothing here validates and rejects; everything here clamps.
//! A length of sixty thousand becomes sixty-four, a block at step 900 is
//! dropped, two blocks written over each other lose the later one, a NaN
//! volume becomes silence, a ±40 speed becomes ±2, and a stored enum
//! discriminant from a newer build becomes the default. The work is done by
//! the model's own bulk setters ([`Pattern::set_blocks`],
//! [`Pattern::set_volumes`]) and by [`StutterData::from_patterns`], which
//! were written for exactly this input — this module's job is to hand them
//! the raw values rather than to re-derive their rules.
//!
//! What cannot be clamped is a blob that is not a snapshot at all: truncated
//! JSON, or a field of the wrong type. There is nothing to salvage, so it
//! loads as [`Snapshot::default`] — sixteen fresh patterns — and the plugin
//! comes up empty instead of refusing to open. The framework door does the
//! same by doing nothing: nice-plug's `deserialize_fields` logs a snapshot it
//! cannot parse and skips the field, which leaves the model at the defaults
//! it was built with.
//!
//! Which half of the instance a restore lands in: the patterns go through
//! [`crate::model::StutterModel`]'s writer API, and the two prefs go into
//! [`PrefsCell`], the small shared cell that is to them what the model is to
//! the patterns. The field that drives both of them from the host's blob is
//! `lib.rs`'s `StateField` ([`crate::StateField`]), because that is where
//! nice-plug is allowed to be imported.
//!
//! # Why the stored shape is its own set of types
//!
//! The model's types could have derived `Serialize` directly, and then every
//! field of them would be part of the file format: renaming a field or
//! changing an enum's order would silently change what projects mean. These
//! types are the file format instead, and they are deliberately dumber than
//! the model — plain `Vec`s rather than fixed arrays, `u8` discriminants
//! rather than enums, a tagged struct rather than [`Action`] — so that the
//! model stays free to change and the conversions stay the one place where
//! the two shapes meet.
//!
//! Every field carries `#[serde(default)]`, so a snapshot from a build that
//! had not invented that field yet loads with the default in its place, and
//! unknown fields are ignored rather than refused. [`SNAPSHOT_VERSION`] is
//! written so a future shape can be told apart from this one; a snapshot
//! whose version is *newer* than ours is still read field by field rather
//! than discarded, because every field is independently defaulted and
//! clamped and a user who opens their project in an older build should get
//! the patterns it can still understand. A snapshot *older* than version 2
//! (the fixed Repeat/Speed actions, before blocks became source + loop +
//! speed recipes) loads as empty patterns: no project worth converting was
//! ever saved in that shape.
//!
//! # [`LANE_PALETTE`]
//!
//! Also here, and the reason this file existed before the snapshot did: the
//! ten colours the window draws a block with. They live in this module and
//! not in the theme because a colour that is *stored* must be a fact about
//! the snapshot rather than about the window — `editor/theme.rs` converts
//! this list, it does not keep a second one.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Mutex, MutexGuard};

use serde::{Deserialize, Serialize};

use crate::model::StutterData;
use crate::notes;
use crate::pattern::{Action, Block, Filter, Lane, NoteValue, Pattern, Rate, Source};

/// The version written into every snapshot this build saves.
///
/// Bumped only when the shape changes in a way `#[serde(default)]` cannot
/// absorb on its own. Version 2 replaced the fixed actions with block
/// recipes; anything older loads as empty patterns ([`Snapshot::data`]).
pub const SNAPSHOT_VERSION: u32 = 2;

/// The oldest snapshot shape this build reads patterns from.
const OLDEST_READABLE_VERSION: u32 = 2;

/// The things the window remembers that are not a pattern.
///
/// Saved with the project rather than globally, because they belong to the
/// device: one instance pinned over Bitwig and another beside it is a
/// reasonable thing to want, and the action a click adds is part of how a
/// particular set of patterns is being edited.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Prefs {
    /// Whether the editor window asks X11 to stay above Bitwig's (the top
    /// bar's switch). Default **on**.
    pub keep_on_top: bool,
    /// The action a click on an empty step adds — the GUI's "last-used
    /// action".
    pub last_action: Action,
    /// Which key plays pattern 1 when notes play the patterns — the C of an
    /// octave ([`crate::notes::starts`]) — or `None` with notes ignored, the
    /// default ([`crate::notes`] says why).
    pub notes: Option<u8>,
    /// How a note plays its pattern: `false` (the default) for as long as
    /// the note is held, `true` latched — until the next note or the Stop
    /// key ([`crate::notes`]).
    pub latch: bool,
}

impl Default for Prefs {
    /// Pinned, and the classic stutter: what a first-time instance shows
    /// before anything has been clicked.
    fn default() -> Prefs {
        Prefs { keep_on_top: true, last_action: Action::default(), notes: None, latch: false }
    }
}

/// The one copy of the prefs an instance has, shared the way the patterns
/// are: the window reads and writes it, and a project load replaces it.
///
/// A cell and not a field of the editor because a restore has to work with
/// no editor open — the prefs come back whether or not there is a window to
/// show them, and the window that opens afterwards reads what the project
/// brought. The patterns have [`crate::model::StutterModel`] for that job;
/// this is the same idea for the values that are not patterns, and it is far
/// smaller because only one of them are read from the audio thread — the
/// Notes setting and Latch, which have lock-free copies of their own for
/// that ([`PrefsCell::notes`], [`PrefsCell::latch`]).
///
/// [`Prefs`] is `Copy`, so the cell hands out copies rather than guards and
/// no reader can hold the lock across a frame.
#[derive(Debug)]
pub struct PrefsCell {
    prefs: Mutex<Prefs>,
    /// [`Prefs::notes`] again, for `process()`: the key, or [`NOTES_OFF`].
    /// Every write goes through both, under the lock.
    notes: AtomicU8,
    /// [`Prefs::latch`] again, for `process()`, written the same way.
    latch: AtomicBool,
}

/// [`PrefsCell::notes`]'s stored value for notes ignored: no key is 255.
const NOTES_OFF: u8 = u8::MAX;

impl PrefsCell {
    /// A cell holding `prefs`.
    pub fn new(prefs: Prefs) -> PrefsCell {
        let cell = PrefsCell {
            prefs: Mutex::new(prefs),
            notes: AtomicU8::new(NOTES_OFF),
            latch: AtomicBool::new(false),
        };
        cell.set(prefs);
        cell
    }

    /// The Notes setting, without the lock: what `process()` reads once a
    /// block.
    pub fn notes(&self) -> Option<u8> {
        match self.notes.load(Ordering::Relaxed) {
            NOTES_OFF => None,
            start => Some(start),
        }
    }

    /// Sets the Notes setting, leaving the other prefs as they were.
    /// A start is squared to a legal C ([`crate::notes::clamp_start`]).
    pub fn set_notes(&self, start: Option<u8>) {
        let mut prefs = self.lock();
        prefs.notes = start.map(notes::clamp_start);
        self.notes.store(prefs.notes.unwrap_or(NOTES_OFF), Ordering::Relaxed);
    }

    /// Whether notes latch, without the lock: what `process()` reads once a
    /// block.
    pub fn latch(&self) -> bool {
        self.latch.load(Ordering::Relaxed)
    }

    /// Sets Latch, leaving the other prefs as they were.
    pub fn set_latch(&self, on: bool) {
        let mut prefs = self.lock();
        prefs.latch = on;
        self.latch.store(on, Ordering::Relaxed);
    }

    /// The prefs as they stand.
    pub fn get(&self) -> Prefs {
        *self.lock()
    }

    /// Replaces both of them — the window's keep-on-top switch, the action a
    /// click last added, and the prefs half of a project load.
    pub fn set(&self, prefs: Prefs) {
        let mut held = self.lock();
        *held = Prefs { notes: prefs.notes.map(notes::clamp_start), ..prefs };
        self.notes.store(held.notes.unwrap_or(NOTES_OFF), Ordering::Relaxed);
        self.latch.store(held.latch, Ordering::Relaxed);
    }

    /// Remembers the action a click on an empty step adds, leaving
    /// keep-on-top as it was ([`crate::editor::grid::remember_action`]).
    ///
    /// One field and not both, because the two prefs are two unrelated
    /// things one window writes: a read-modify-write through
    /// [`PrefsCell::get`] and [`PrefsCell::set`] would do the same job and
    /// would also be the one shape in this file that could lose the other
    /// pref if the two were ever written from two places.
    pub fn set_last_action(&self, action: Action) {
        self.lock().last_action = action;
    }

    /// Sets the window's keep-on-top switch, leaving the remembered action
    /// as it was ([`crate::editor::top_bar::toggle_keep_on_top`]).
    ///
    /// One field, for [`PrefsCell::set_last_action`]'s reason exactly — and
    /// here it is load-bearing in the other direction too: the switch is
    /// written by a click on the bar while the action is written by a choice
    /// in a block's menu, and either can happen while the other's value is
    /// the one the project just brought back.
    pub fn set_keep_on_top(&self, keep_on_top: bool) {
        self.lock().keep_on_top = keep_on_top;
    }

    /// Poisoning ignored, as in [`crate::model::StutterModel`]: a panicking
    /// writer must not wedge the window, and what is behind the lock is two
    /// values written whole, so there is no half-finished edit to find.
    fn lock(&self) -> MutexGuard<'_, Prefs> {
        self.prefs.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Default for PrefsCell {
    /// A first-time instance's prefs: pinned, the classic stutter.
    fn default() -> PrefsCell {
        PrefsCell::new(Prefs::default())
    }
}

/// One project's worth of saved state: the sixteen patterns and the prefs.
///
/// The `patterns` list is a `Vec` and not `[PatternSnapshot; PATTERN_COUNT]`
/// on purpose — a file may hold any number of them, and
/// [`StutterData::from_patterns`] is what turns any number into exactly
/// sixteen. Only the first sixteen are ever converted, so a file claiming ten
/// thousand patterns costs the parse and nothing more.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    /// The shape this was written by ([`SNAPSHOT_VERSION`]).
    #[serde(default = "written_version")]
    pub version: u32,
    /// The patterns, in `pattern` parameter order. Missing ones load fresh.
    #[serde(default)]
    pub patterns: Vec<PatternSnapshot>,
    /// [`Prefs::keep_on_top`]. Missing means **true**, the default — not
    /// `bool`'s own default, which is the opposite.
    #[serde(default = "pinned")]
    pub keep_on_top: bool,
    /// [`Prefs::notes`]: the key that plays pattern 1, or `null` for notes
    /// ignored. Missing (a project saved before the setting existed) means
    /// ignored.
    #[serde(default)]
    pub notes: Option<u8>,
    /// [`Prefs::latch`]. Missing means off: notes play while held.
    #[serde(default)]
    pub latch: bool,
    /// [`Prefs::last_action`].
    #[serde(default)]
    pub last_action: ActionSnapshot,
}

impl Snapshot {
    /// The live patterns and prefs as the host will store them — exact.
    pub fn of(data: &StutterData, prefs: Prefs) -> Snapshot {
        Snapshot {
            version: SNAPSHOT_VERSION,
            patterns: data.patterns().iter().map(PatternSnapshot::of).collect(),
            keep_on_top: prefs.keep_on_top,
            notes: prefs.notes,
            latch: prefs.latch,
            last_action: ActionSnapshot::of(prefs.last_action),
        }
    }

    /// The sixteen patterns this snapshot means, clamped into legality
    /// however hostile it is — the model half of a restore.
    ///
    /// Extra patterns are dropped and missing ones are [`Pattern::new`], so
    /// the result is always [`crate::pattern::PATTERN_COUNT`] patterns that
    /// satisfy every invariant of `pattern.rs`, and the audio thread can read
    /// one the instant it is published.
    ///
    /// A snapshot older than version 2 loads as sixteen empty patterns.
    pub fn data(&self) -> StutterData {
        if self.version < OLDEST_READABLE_VERSION {
            return StutterData::new();
        }
        StutterData::from_patterns(self.patterns.iter().map(PatternSnapshot::pattern))
    }

    /// The prefs this snapshot means, with the stored action sanitised. An
    /// old snapshot keeps its keep-on-top switch but not its action.
    pub fn prefs(&self) -> Prefs {
        let last_action = match self.version < OLDEST_READABLE_VERSION {
            true => Action::default(),
            false => self.last_action.action(),
        };
        let notes = self.notes.map(notes::clamp_start);
        Prefs { keep_on_top: self.keep_on_top, last_action, notes, latch: self.latch }
    }
}

impl Default for Snapshot {
    /// What a new instance saves and what a corrupt one loads as: sixteen
    /// fresh patterns, pinned window, the default action.
    fn default() -> Snapshot {
        Snapshot::of(&StutterData::new(), Prefs::default())
    }
}

/// One pattern as it is stored: its rate and length, the Time row and the
/// whole Volume row.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PatternSnapshot {
    /// [`Rate`]'s stored discriminant; an unknown one loads as
    /// [`Rate::DEFAULT`].
    #[serde(default = "default_rate")]
    pub rate: u8,
    /// Steps in the pattern, clamped into `1..=MAX_STEPS` on load.
    #[serde(default = "default_length")]
    pub length: u16,
    /// The Time row, in any order: sorted, de-overlapped and truncated on
    /// load by [`Pattern::set_blocks`].
    #[serde(default)]
    pub blocks: Vec<BlockSnapshot>,
    /// The Volume row — all `MAX_STEPS` of it, including the steps past
    /// `length` the GUI gives back when a pattern is grown again. Truncated,
    /// padded with unity and clamped on load by [`Pattern::set_volumes`].
    #[serde(default)]
    pub volumes: Vec<f32>,
    /// [`Filter`]'s stored discriminant. Missing (a project from before the
    /// filter) or unknown means [`Filter::Off`].
    #[serde(default)]
    pub filter: u8,
    /// The Cutoff row, all `MAX_STEPS` of it like `volumes`; missing entries
    /// load at the top (open).
    #[serde(default)]
    pub cutoff: Vec<f32>,
    /// The Res row, the same way; missing entries load at none.
    #[serde(default)]
    pub resonance: Vec<f32>,
}

impl PatternSnapshot {
    /// One live pattern as it is stored — exact, Volume row and all.
    pub fn of(pattern: &Pattern) -> PatternSnapshot {
        PatternSnapshot {
            rate: pattern.rate().to_u8(),
            length: pattern.length(),
            blocks: pattern.blocks().iter().map(BlockSnapshot::of).collect(),
            volumes: pattern.volumes().to_vec(),
            filter: pattern.filter().to_u8(),
            cutoff: pattern.lane(Lane::Cutoff).to_vec(),
            resonance: pattern.lane(Lane::Resonance).to_vec(),
        }
    }

    /// The pattern this means, every invariant re-established.
    ///
    /// The order matters: the length is set first, because it is what the
    /// Time row is clamped *into*, and the blocks last, so they are sorted
    /// and de-overlapped against the length this snapshot actually asked for
    /// rather than against the default sixteen.
    pub fn pattern(&self) -> Pattern {
        let mut pattern = Pattern::new();
        pattern.set_rate(Rate::from_u8(self.rate));
        pattern.set_length(self.length);
        pattern.set_volumes(self.volumes.iter().copied());
        pattern.set_filter(Filter::from_u8(self.filter));
        pattern.set_lane(Lane::Cutoff, self.cutoff.iter().copied());
        pattern.set_lane(Lane::Resonance, self.resonance.iter().copied());
        pattern.set_blocks(self.blocks.iter().map(BlockSnapshot::block));

        pattern
    }
}

impl Default for PatternSnapshot {
    /// A fresh pattern: 1/16, sixteen steps, nothing drawn, the row at unity.
    fn default() -> PatternSnapshot {
        PatternSnapshot::of(&Pattern::new())
    }
}

/// One block on the Time row as it is stored.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BlockSnapshot {
    /// The step it starts on.
    #[serde(default)]
    pub start: u16,
    /// How many steps it covers; at least one on load.
    #[serde(default = "one_step")]
    pub len: u16,
    /// What it does.
    #[serde(default)]
    pub action: ActionSnapshot,
}

impl BlockSnapshot {
    /// One live block as it is stored — exact.
    pub fn of(block: &Block) -> BlockSnapshot {
        BlockSnapshot {
            start: block.start(),
            len: block.len(),
            action: ActionSnapshot::of(block.action()),
        }
    }

    /// The block this means, with the span and the action brought inside what
    /// a [`Block`] may be. Where it sits relative to the pattern's length and
    /// its neighbours is [`Pattern::set_blocks`]'s business, not a block's.
    pub fn block(&self) -> Block {
        Block::new(self.start, self.len, self.action.action())
    }
}

impl Default for BlockSnapshot {
    /// A one-step block on step 0 running the default action.
    fn default() -> BlockSnapshot {
        BlockSnapshot { start: 0, len: 1, action: ActionSnapshot::default() }
    }
}

/// An [`Action`] as it is stored: the source, the loop and the two speeds,
/// each as a plain number so the format does not depend on enum layouts.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ActionSnapshot {
    /// 0 = block start, 2 = continue, 3 = a spot (at `step`, `passes`
    /// back). 1 was "a distance before the block", which is gone and loads
    /// as the block start, like anything else unknown.
    #[serde(default)]
    pub source: u8,
    /// [`Source::Spot`]'s step; clamped on load.
    #[serde(default)]
    pub step: u16,
    /// [`Source::Spot`]'s extra passes back; clamped on load.
    #[serde(default)]
    pub passes: u8,
    /// [`NoteValue`]'s stored discriminant for the loop, or `None` for no
    /// loop.
    #[serde(default)]
    pub looping: Option<u8>,
    /// The speed at the block's start; clamped to ±2 on load, NaN as 1.
    #[serde(default = "live_speed")]
    pub from: f32,
    /// The speed at the block's end, likewise.
    #[serde(default = "live_speed")]
    pub to: f32,
}

impl ActionSnapshot {
    /// An action as it will be stored — exact.
    pub fn of(action: Action) -> ActionSnapshot {
        let (source, step, passes) = match action.source {
            Source::BlockStart => (SOURCE_BLOCK_START, 0, 0),
            Source::Continue => (SOURCE_CONTINUE, 0, 0),
            Source::Spot { step, passes } => (SOURCE_SPOT, step, passes),
        };

        ActionSnapshot {
            source,
            step,
            passes,
            looping: action.looping.map(NoteValue::to_u8),
            from: action.from,
            to: action.to,
        }
    }

    /// The action this means, sanitised.
    pub fn action(self) -> Action {
        let source = match self.source {
            SOURCE_CONTINUE => Source::Continue,
            SOURCE_SPOT => Source::Spot { step: self.step, passes: self.passes },
            _ => Source::BlockStart,
        };

        Action {
            source,
            looping: self.looping.map(NoteValue::from_u8),
            from: self.from,
            to: self.to,
        }
        .sanitised()
    }
}

impl Default for ActionSnapshot {
    /// The default action, stored.
    fn default() -> ActionSnapshot {
        ActionSnapshot::of(Action::default())
    }
}

/// [`ActionSnapshot::source`]'s values.
const SOURCE_BLOCK_START: u8 = 0;
/// See [`SOURCE_BLOCK_START`]. (1 is retired: see [`ActionSnapshot::source`].)
const SOURCE_CONTINUE: u8 = 2;
/// See [`SOURCE_BLOCK_START`].
const SOURCE_SPOT: u8 = 3;

/// `serde(default)` for [`Snapshot::version`].
fn written_version() -> u32 {
    SNAPSHOT_VERSION
}

/// `serde(default)` for [`Snapshot::keep_on_top`] — on, as intended.
fn pinned() -> bool {
    true
}

/// `serde(default)` for [`PatternSnapshot::rate`]: the stored 1/16, which is
/// not the zero discriminant.
fn default_rate() -> u8 {
    Rate::DEFAULT.to_u8()
}

/// `serde(default)` for [`PatternSnapshot::length`].
fn default_length() -> u16 {
    Pattern::DEFAULT_LENGTH
}

/// `serde(default)` for [`BlockSnapshot::len`]: a block is at least a step.
fn one_step() -> u16 {
    1
}

/// `serde(default)` for a stored Speed's endpoint: unity, which reads live
/// rather than frozen.
fn live_speed() -> f32 {
    1.0
}

/// The ten colours a pattern is read by, as `0xRRGGBB`.
///
/// bitstep's lane palette unchanged, which is the point of it: the three
/// plugins are opened side by side in one project, and a block that named its
/// action in a red of its own would be the first thing to show. Red through
/// green, then cyan through pink — ten hues of one saturation and lightness,
/// so no entry shouts over its neighbours and every one of them reads over
/// the greys of `editor/theme.rs`.
///
/// Each of stuttter's seven block kinds is given a stable entry of this list
/// by `editor::theme`'s block colours; the list is longer than that so the
/// assignment has room to be chosen for how the hues sit next to each other
/// rather than for how many there are.
///
/// The top byte of every entry is zero: these are three channels and no
/// alpha, and the painter reads only the low 24 bits
/// ([`crate::editor::theme::rgb`]).
pub const LANE_PALETTE: [u32; 10] = [
    0xE2_6E_6E, 0xE2_9A_5E, 0xE2_C9_5E, 0xA8_C9_6E, 0x6E_C9_8A, // red … green
    0x6E_C2_C9, 0x6E_96_D6, 0x8E_7E_D6, 0xC2_7E_D6, 0xD6_7E_A8, // cyan … pink
];

#[cfg(test)]
mod tests {
    use super::*;

    use crate::actions::SpeedPreset;
    use crate::pattern::{MAX_BLOCKS, MAX_SOURCE_PASSES, MAX_STEPS, PATTERN_COUNT};

    // ---- the snapshot -----------------------------------------------------

    /// Sixteen patterns with nothing in common: every rate used, lengths from
    /// short to the longest, both repeat sources, every preset, a hand-drawn
    /// ramp, and a Volume row that is not flat. What a round trip has to
    /// carry, so that "lossless" means the whole model and not the parts a
    /// fresh pattern happens to share with it.
    fn populated() -> StutterData {
        let mut patterns = Vec::with_capacity(PATTERN_COUNT);

        for index in 0..PATTERN_COUNT {
            let mut pattern = Pattern::new();
            pattern.set_rate(Rate::ALL[index % Rate::ALL.len()]);
            pattern.set_length((4 + index as u16 * 4).min(Pattern::MAX_LENGTH));
            pattern.set_volumes((0..MAX_STEPS).map(|step| ((step + index) % 11) as f32 / 10.0));

            let mut start = (index % 3) as u16;
            let mut drawn = 0;
            while start + 2 <= pattern.length() {
                let action = match drawn % 4 {
                    0 => {
                        let note = NoteValue::ALL[(index + drawn) % NoteValue::ALL.len()];
                        let spot = Source::Spot { step: drawn as u16, passes: index as u8 % 3 };
                        Action::repeat(note).with_source(spot)
                    }
                    1 => Action::from_preset(
                        SpeedPreset::ALL[(index + drawn) % SpeedPreset::ALL.len()],
                    ),
                    2 => Action::ramp(-0.371_25, 1.937_5).with_source(Source::Continue),
                    _ => Action::repeat_at(pattern.rate()),
                };
                assert!(pattern.add_block(start, 2, action).is_some(), "{index} at {start}");

                start += 3;
                drawn += 1;
            }

            patterns.push(pattern);
        }

        let data = StutterData::from_patterns(patterns);
        assert_legal(&data);

        data
    }

    /// Every invariant of `pattern.rs` on every pattern of a loaded model —
    /// the assertion that matters for hostile input, because it is what the
    /// audio thread relies on without checking.
    fn assert_legal(data: &StutterData) {
        for (index, pattern) in data.patterns().iter().enumerate() {
            let length = pattern.length();
            assert!((1..=Pattern::MAX_LENGTH).contains(&length), "pattern {index}: {length}");
            assert!(pattern.blocks().len() <= MAX_BLOCKS, "pattern {index} has too many blocks");

            let mut cursor = 0;
            for (slot, block) in pattern.blocks().iter().enumerate() {
                assert!(block.len() >= 1, "pattern {index} block {slot} is empty");
                assert!(block.start() >= cursor, "pattern {index} block {slot} overlaps");
                assert!(block.end() <= length, "pattern {index} block {slot} ends past the end");
                cursor = block.end();

                let action = block.action();
                for speed in [action.from, action.to] {
                    assert!(speed.is_finite(), "pattern {index} block {slot}: {speed}");
                    assert!(speed.abs() <= 2.0, "pattern {index} block {slot}: {speed}");
                }
            }

            for (step, &volume) in pattern.volumes().iter().enumerate() {
                assert!((0.0..=1.0).contains(&volume), "pattern {index} step {step}: {volume}");
            }
        }
    }

    /// What a stored blob loads as: the snapshot it parses to, or the default
    /// one where it is not a snapshot at all — the rule nice-plug's
    /// `deserialize_fields` applies by skipping the field.
    fn reload(json: &str) -> Snapshot {
        serde_json::from_str::<Snapshot>(json).unwrap_or_default()
    }

    /// The round trip: a fully populated model survives model → JSON → model
    /// exactly, prefs and all.
    #[test]
    fn a_populated_model_survives_json_and_comes_back_identical() {
        let data = populated();

        for prefs in [
            Prefs::default(),
            Prefs {
                keep_on_top: false,
                last_action: Action::from_preset(SpeedPreset::Reverse),
                notes: Some(notes::SUGGESTED_START),
                latch: true,
            },
            Prefs { last_action: Action::ramp(-1.5, 0.125), notes: Some(0), ..Prefs::default() },
            Prefs {
                keep_on_top: false,
                notes: None,
                latch: false,
                last_action: Action {
                    source: Source::Spot { step: 63, passes: MAX_SOURCE_PASSES },
                    ..Action::repeat(NoteValue::ThirtySecondTriplet)
                },
            },
        ] {
            let snapshot = Snapshot::of(&data, prefs);
            let json = serde_json::to_string(&snapshot).expect("a snapshot serialises");
            let back = reload(&json);

            assert_eq!(back, snapshot, "the snapshot itself did not round trip");
            assert_eq!(back.version, SNAPSHOT_VERSION);
            assert_eq!(back.data(), data, "the model did not round trip");
            assert_eq!(back.prefs(), prefs, "the prefs did not round trip");
        }
    }

    /// The row past the end of a pattern travels too: shrinking a pattern and
    /// growing it again gives back the levels that were drawn, and saving the
    /// project in between may not quietly flatten them
    /// ([`Pattern::set_length`]).
    #[test]
    fn the_volume_row_past_the_end_of_a_pattern_is_saved_as_well() {
        let mut pattern = Pattern::new();
        pattern.set_volumes((0..MAX_STEPS).map(|step| step as f32 / 100.0));
        pattern.set_length(4);

        let data = StutterData::from_patterns([pattern.clone()]);
        let json = serde_json::to_string(&Snapshot::of(&data, Prefs::default())).unwrap();
        let back = reload(&json).data();

        assert_eq!(back.pattern(0), Some(&pattern));
        assert_eq!(back.pattern(0).unwrap().volume(60), 0.6);
    }

    /// An empty object is a snapshot with nothing in it, and that loads as a
    /// new instance: sixteen fresh patterns, the window pinned, the classic
    /// stutter under the next click.
    #[test]
    fn an_empty_object_loads_as_sixteen_fresh_patterns_and_the_default_prefs() {
        let loaded = reload("{}");

        assert_eq!(loaded.version, SNAPSHOT_VERSION, "a missing version is this one");
        assert_eq!(loaded.data(), StutterData::new());
        assert_eq!(loaded.prefs(), Prefs::default());
        assert!(loaded.prefs().keep_on_top, "keep-on-top defaults off, not on");
        assert_eq!(loaded.data(), Snapshot::default().data());
        assert_legal(&loaded.data());
    }

    /// Missing fields fall back to defaults field by field — including the
    /// two whose default is **not** the zero discriminant (the rate and the
    /// note values are 1/16, stored as 2 and 8) — and fields this build has
    /// never heard of are ignored rather than refused.
    #[test]
    fn missing_fields_default_and_unknown_fields_are_ignored() {
        let loaded = reload(
            r#"{
                "version": 2,
                "future_pref": {"nested": [1, 2, 3]},
                "patterns": [
                    {"blocks": [{"start": 2, "mystery": true}]},
                    {"length": 3, "volumes": [0.25, 0.5]}
                ]
            }"#,
        );
        let data = loaded.data();
        assert_legal(&data);

        let first = data.pattern(0).expect("pattern 0");
        assert_eq!(first.rate(), Rate::DEFAULT, "a missing rate is not the zero discriminant");
        assert_eq!(first.length(), Pattern::DEFAULT_LENGTH);
        assert_eq!(first.blocks().len(), 1);
        assert_eq!(first.blocks()[0].start(), 2);
        assert_eq!(first.blocks()[0].len(), 1, "a missing len is one step");
        assert_eq!(first.blocks()[0].action(), Action::default());
        assert!(first.volumes().iter().all(|&volume| volume == 1.0));

        let second = data.pattern(1).expect("pattern 1");
        assert_eq!(second.length(), 3);
        assert_eq!((second.volume(0), second.volume(1)), (0.25, 0.5));
        assert_eq!(second.volume(2), Pattern::DEFAULT_VOLUME, "a short row pads with unity");

        // The patterns the file never mentioned are fresh ones.
        for index in 2..PATTERN_COUNT {
            assert_eq!(data.pattern(index), Some(&Pattern::new()), "pattern {index}");
        }
    }

    /// An action with nothing stored in it reads live — no loop, speed 1,
    /// from the block start — rather than frozen, which is the harmless
    /// reading of a half-written block.
    #[test]
    fn an_action_with_nothing_stored_plays_live() {
        let loaded = reload(r#"{"patterns": [{"blocks": [{"action": {}}]}]}"#);
        let data = loaded.data();
        assert_legal(&data);

        let action = data.pattern(0).expect("pattern 0").blocks()[0].action();
        assert_eq!(action, Action::ramp(1.0, 1.0));
    }

    /// Oversize everything: a length of sixty thousand, forty patterns, two
    /// hundred blocks and five hundred volumes all come back at the limits.
    #[test]
    fn oversize_lengths_and_lists_are_cut_back_to_the_limits() {
        let blocks: Vec<String> =
            (0..200).map(|start| format!(r#"{{"start": {start}, "len": 1}}"#)).collect();
        let volumes: Vec<String> =
            (0..500).map(|step| format!("{}", (step % 7) as f32 - 2.0)).collect();
        let crowded = format!(
            r#"{{"length": 60000, "blocks": [{}], "volumes": [{}]}}"#,
            blocks.join(", "),
            volumes.join(", ")
        );
        let padding = vec!["{}"; 39].join(", ");
        let loaded = reload(&format!(r#"{{"patterns": [{crowded}, {padding}]}}"#));

        let data = loaded.data();
        assert_legal(&data);

        let first = data.pattern(0).expect("pattern 0");
        assert_eq!(first.length(), Pattern::MAX_LENGTH, "a 60000-step pattern");
        assert_eq!(first.blocks().len(), MAX_BLOCKS, "200 blocks over 64 steps");
        assert_eq!(first.blocks()[MAX_BLOCKS - 1].end(), Pattern::MAX_LENGTH);
        for step in 0..MAX_STEPS {
            // `clamp` rather than `clamp_volume`'s `max`-then-`min`: none of
            // these is a NaN, and the expected value may not be computed by
            // the function under test.
            let expected = ((step % 7) as f32 - 2.0).clamp(0.0, 1.0);
            assert_eq!(first.volume(step as u16), expected, "step {step}");
        }

        // Pattern 15 is the last one the file is allowed to have said
        // anything about; there is no seventeenth to leak into.
        assert_eq!(data.pattern(15), Some(&Pattern::new()));
        assert!(data.pattern(PATTERN_COUNT).is_none(), "a seventeenth pattern");
    }

    /// Blocks arrive in any order, overlapping, overhanging and off the end;
    /// they load sorted, disjoint and inside the length — keeping the
    /// **earlier** block where two collide.
    #[test]
    fn out_of_range_and_overlapping_blocks_load_sorted_and_disjoint() {
        let loaded = reload(
            r#"{"patterns": [{"length": 16, "blocks": [
                {"start": 9, "len": 3},
                {"start": 2, "len": 2},
                {"start": 3, "len": 1},
                {"start": 14, "len": 9},
                {"start": 900, "len": 1},
                {"start": 5, "len": 0}
            ]}]}"#,
        );
        let data = loaded.data();
        assert_legal(&data);

        let spans: Vec<(u16, u16)> = data
            .pattern(0)
            .expect("pattern 0")
            .blocks()
            .iter()
            .map(|block| (block.start(), block.len()))
            .collect();
        assert_eq!(spans, [(2, 2), (5, 1), (9, 3), (14, 2)]);
    }

    /// A block whose span overhangs a *short* pattern is truncated, and one
    /// that starts past the end is dropped outright.
    #[test]
    fn a_block_past_the_end_of_a_short_pattern_is_dropped_not_moved() {
        let loaded = reload(
            r#"{"patterns": [{"length": 4, "blocks": [
                {"start": 3, "len": 9},
                {"start": 6, "len": 1}
            ]}]}"#,
        );
        let data = loaded.data();
        assert_legal(&data);

        let blocks = data.pattern(0).expect("pattern 0").blocks();
        assert_eq!(blocks.len(), 1, "the block at step 6 of a four-step pattern survived");
        assert_eq!((blocks[0].start(), blocks[0].len()), (3, 1));
    }

    /// Volumes and speeds that are not numbers in their range: infinities
    /// through the file (JSON has no NaN literal), NaNs through the snapshot
    /// type itself, which is the door nice-plug hands us a parsed value at.
    #[test]
    fn nan_and_infinite_volumes_and_speeds_load_inside_their_ranges() {
        let loaded = reload(
            r#"{"patterns": [{"length": 4,
                "volumes": [1e40, -1e40, 2.5, -0.5],
                "blocks": [{"start": 0, "len": 2, "action":
                    {"from": 1e40, "to": -1e40}}]
            }]}"#,
        );
        let data = loaded.data();
        assert_legal(&data);

        let pattern = data.pattern(0).expect("pattern 0");
        assert_eq!(
            [pattern.volume(0), pattern.volume(1), pattern.volume(2), pattern.volume(3)],
            [1.0, 0.0, 1.0, 0.0],
        );
        assert_eq!(pattern.blocks()[0].action(), Action::ramp(2.0, -2.0));

        let hostile = Snapshot {
            version: SNAPSHOT_VERSION,
            patterns: vec![PatternSnapshot {
                rate: Rate::DEFAULT.to_u8(),
                length: 4,
                blocks: vec![BlockSnapshot {
                    start: 0,
                    len: 1,
                    action: ActionSnapshot {
                        from: f32::NAN,
                        to: f32::NEG_INFINITY,
                        ..ActionSnapshot::of(Action::ramp(1.0, 1.0))
                    },
                }],
                volumes: vec![f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 0.75],
                filter: 99,
                cutoff: vec![f32::NAN, 2.0],
                resonance: vec![-1.0, f32::INFINITY],
            }],
            keep_on_top: false,
            // Not a C, and too high for sixteen keys to fit under 127.
            notes: Some(125),
            latch: true,
            last_action: ActionSnapshot {
                from: f32::NAN,
                to: 9.0,
                ..ActionSnapshot::of(Action::ramp(1.0, 1.0))
            },
        };
        let data = hostile.data();
        assert_legal(&data);

        let pattern = data.pattern(0).expect("pattern 0");
        assert_eq!(
            [pattern.volume(0), pattern.volume(1), pattern.volume(2), pattern.volume(3)],
            [0.0, 1.0, 0.0, 0.75],
            "a NaN volume is silence, not a NaN multiplied into the output",
        );
        assert_eq!(pattern.filter(), Filter::Off, "an unknown filter loads as Off");
        assert_eq!(&pattern.lane(Lane::Cutoff)[..3], &[0.0, 1.0, 1.0]);
        assert_eq!(&pattern.lane(Lane::Resonance)[..3], &[0.0, 1.0, 0.0]);
        assert_eq!(pattern.blocks()[0].action(), Action::ramp(1.0, -2.0));
        assert_eq!(hostile.prefs().last_action, Action::ramp(1.0, 2.0));
        assert_eq!(hostile.prefs().notes, Some(notes::HIGHEST_START));
        let cell = PrefsCell::new(hostile.prefs());
        assert_eq!(cell.notes(), Some(notes::HIGHEST_START));
        assert!(cell.latch() && cell.get().latch, "the hostile snapshot latched");
        cell.set(Prefs { notes: Some(99), ..Prefs::default() });
        assert_eq!(cell.notes(), Some(96), "a start is squared to its C");
        cell.set_notes(None);
        assert_eq!((cell.notes(), cell.get().notes), (None, None));
        cell.set_latch(false);
        assert_eq!((cell.latch(), cell.get().latch), (false, false));
        assert!(!Prefs::default().latch, "notes play while held by default");
    }

    /// A source or a note value this build does not know loads as the
    /// default rather than failing the parse — one unreadable block may not
    /// cost a project its other fifteen patterns.
    #[test]
    fn unknown_sources_and_discriminants_load_as_the_defaults() {
        let loaded = reload(
            r#"{"last_action": {"source": 9, "freeze": 3},
                "patterns": [{"rate": 99, "length": 6, "blocks": [
                    {"start": 0, "len": 1, "action": {"source": 7, "looping": 200}},
                    {"start": 1, "len": 1, "action": {"source": 1, "before": 99}},
                    {"start": 2, "len": 1, "action": {"source": 3, "step": 900, "passes": 99}}
                ]}]}"#,
        );
        let data = loaded.data();
        assert_legal(&data);

        let pattern = data.pattern(0).expect("pattern 0");
        assert_eq!(pattern.rate(), Rate::DEFAULT, "an unknown rate");
        assert_eq!(pattern.blocks()[0].action(), Action::repeat(NoteValue::DEFAULT));
        assert_eq!(pattern.blocks()[1].action(), Action::ramp(1.0, 1.0), "the retired source 1");
        let spot = Source::Spot { step: MAX_STEPS as u16 - 1, passes: MAX_SOURCE_PASSES };
        assert_eq!(pattern.blocks()[2].action(), Action::ramp(1.0, 1.0).with_source(spot));
        assert_eq!(loaded.prefs().last_action, Action::ramp(1.0, 1.0));
    }

    /// A snapshot from before version 2 — the fixed Repeat/Speed actions —
    /// loads as empty patterns, but keeps its keep-on-top switch.
    #[test]
    fn a_version_one_snapshot_loads_as_empty_patterns() {
        let loaded = reload(
            r#"{"version": 1, "keep_on_top": false,
                "last_action": {"kind": "speed", "from": 1.0, "to": 0.0, "preset": 3},
                "patterns": [{"length": 8, "blocks": [
                    {"start": 0, "len": 2, "action": {"kind": "repeat", "slice": 4, "source": 1}}
                ]}]}"#,
        );

        assert_eq!(loaded.data(), StutterData::new());
        let expected = Prefs { keep_on_top: false, ..Prefs::default() };
        assert_eq!(loaded.prefs(), expected);
    }

    /// A snapshot from a build after this one is read field by field rather
    /// than thrown away: what this build understands of it survives.
    #[test]
    fn a_snapshot_from_a_newer_build_still_loads_what_it_can() {
        let loaded = reload(r#"{"version": 99, "patterns": [{"length": 8, "swing": 0.5}]}"#);

        assert_eq!(loaded.version, 99, "the stored version is not rewritten on load");
        assert_eq!(loaded.data().pattern(0).expect("pattern 0").length(), 8);
        assert_legal(&loaded.data());
    }

    /// A blob that is not a snapshot at all — truncated, wrongly typed, the
    /// wrong kind of JSON, empty — loads as the defaults without panicking.
    #[test]
    fn a_corrupt_blob_loads_as_the_defaults() {
        for blob in [
            "",
            "{",
            r#"{"patterns": [{"length": 16,"#,
            r#"{"patterns": [{"length": "sixteen"}]}"#,
            r#"{"patterns": {"first": {}}}"#,
            r#"{"patterns": [{"blocks": [{"start": -3}]}]}"#,
            r#"{"keep_on_top": "yes"}"#,
            r#"{"patterns": [{"volumes": ["loud"]}]}"#,
            "null",
            "0",
            "\u{0}\u{1}\u{2}",
        ] {
            assert!(serde_json::from_str::<Snapshot>(blob).is_err(), "{blob:?} parsed");

            let loaded = reload(blob);
            assert_eq!(loaded, Snapshot::default(), "{blob:?}");
            assert_eq!(loaded.data(), StutterData::new(), "{blob:?}");
            assert_eq!(loaded.prefs(), Prefs::default(), "{blob:?}");
            assert_legal(&loaded.data());
        }

        // An empty JSON array is the one blob that parses rather than
        // failing: serde reads a struct from a sequence as well as from a
        // map, and an empty sequence is every field missing. It carries no
        // patterns rather than sixteen default ones, which loads as the same
        // model — the only thing that matters on the way in.
        let empty = reload("[]");
        assert!(empty.patterns.is_empty());
        assert_eq!(empty.data(), StutterData::new());
        assert_eq!(empty.prefs(), Prefs::default());
    }

    /// The numbers and names that are in project files, pinned: a reordered
    /// menu list or a renamed tag has to break a test here rather than change
    /// what a saved project means.
    #[test]
    fn the_stored_enum_numbers_are_the_ones_in_project_files() {
        assert_eq!(Rate::DEFAULT.to_u8(), 2);
        assert_eq!(NoteValue::DEFAULT.to_u8(), 8);

        assert_eq!((SOURCE_BLOCK_START, SOURCE_CONTINUE, SOURCE_SPOT), (0, 2, 3));

        let repeat = ActionSnapshot::of(Action::default());
        assert_eq!(
            serde_json::to_string(&repeat).unwrap(),
            r#"{"source":0,"step":0,"passes":0,"looping":8,"from":1.0,"to":1.0}"#
        );

        let stop = Action::from_preset(SpeedPreset::TapeStop).with_source(Source::Continue);
        assert_eq!(
            serde_json::to_string(&ActionSnapshot::of(stop)).unwrap(),
            r#"{"source":2,"step":0,"passes":0,"looping":null,"from":1.0,"to":0.0}"#
        );

        let spot = Action::default().with_source(Source::Spot { step: 5, passes: 2 });
        assert_eq!(
            serde_json::to_string(&ActionSnapshot::of(spot)).unwrap(),
            r#"{"source":3,"step":5,"passes":2,"looping":8,"from":1.0,"to":1.0}"#
        );

        let empty = serde_json::to_string(&Snapshot::of(&StutterData::new(), Prefs::default()))
            .expect("a snapshot serialises");
        assert!(empty.starts_with(r#"{"version":2,"patterns":[{"rate":2,"length":16,"#), "{empty}");
        assert!(empty.contains(r#""keep_on_top":true"#), "{empty}");
        let default_action =
            r#""last_action":{"source":0,"step":0,"passes":0,"looping":8,"from":1.0,"to":1.0}}"#;
        assert!(empty.ends_with(default_action), "{empty}");
    }

    // ---- the palette ------------------------------------------------------


    /// The palette is ten colours, every one of them a plain `0xRRGGBB` with
    /// nothing in the top byte — what a snapshot stores and what the theme
    /// converts, which is the whole of the coupling between the two files.
    #[test]
    fn the_palette_is_ten_opaque_rgb_colours() {
        assert_eq!(LANE_PALETTE.len(), 10);

        for (index, &colour) in LANE_PALETTE.iter().enumerate() {
            assert_eq!(colour >> 24, 0, "entry {index} has something in its alpha byte");
            assert_ne!(colour, 0, "entry {index} is black");
        }
    }

    /// Ten distinguishable hues and not nine and a near-duplicate: a block's
    /// colour is what says which action it is before its label is read, so
    /// two actions may not be handed the same colour by accident.
    #[test]
    fn no_two_palette_entries_are_the_same_colour() {
        for (index, &colour) in LANE_PALETTE.iter().enumerate() {
            for (other_index, &other) in LANE_PALETTE.iter().enumerate().skip(index + 1) {
                assert_ne!(colour, other, "entries {index} and {other_index} are one colour");
            }
        }
    }
}
