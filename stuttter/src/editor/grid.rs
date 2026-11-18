//! The Time row's step grid: everything about it that can be worked out
//! without drawing it.
//!
//! The row is one hand-painted surface — there is no egui widget per step —
//! so every cell rectangle, every block rectangle and every pointer lookup
//! is arithmetic, and that arithmetic is here, where it can be tested on a
//! machine with no display. The egui glue over it is [`ui_time_row`], which
//! is the only thing in this file that is not pure.
//!
//! Three things live here: [`StepGrid`], the geometry and the hit-testing;
//! [`BlockKind`], which action a block is and therefore what colour it
//! wears; and [`block_label`], the short label written on it. The figures are
//! all [`theme`]'s, never numbers written here.
//!
//! # A long pattern is scaled, not scrolled
//!
//! One layout decision was left to this module: the window is a fixed size
//! wide enough for [`super::STEPS_SHOWN`] cells at [`theme::CELL_WIDTH`], and
//! a pattern may be up to [`crate::pattern::MAX_STEPS`] steps long. **The
//! grid scales**: whatever the length, the whole pattern is laid across the
//! row's full width, so the cell pitch is the width divided by the length
//! rather than a constant.
//!
//! Scrolling was the alternative, and this is why it lost:
//!
//! * The window **cannot be resized** ([`super::create`]), so a scroll bar
//!   would spend a strip of the only width there is
//!   ([`theme::SCROLL_BAR_STRIP`]) on a pattern that is nearly always short
//!   enough not to need one.
//! * The three bands under the top bar are **one x-axis**: a step's cell, its
//!   volume bar and its slice of the waveform are at the same place across the
//!   window, and the playhead crosses all three at once. A scrolled Time row
//!   would have to drag the other two with it, or stop lining up with them.
//! * A stutter pattern is read **at a glance** — that is what a grid of
//!   blocks is for instead of drawn curves — and the whole of it is always
//!   on screen this way, with no state to remember, no offset to clamp per
//!   frame and no follow-the-playhead scrolling to write.
//!
//! What it costs is cell width, and the arithmetic is kind about it: the
//! row's width plus one [`theme::CELL_GAP`] is exactly
//! [`super::STEPS_SHOWN`] × [`theme::CELL_PITCH`] = 512 points, so the pitch
//! is 512/length — 32 points at the default sixteen steps, 16 at thirty-two
//! and 8 at the longest pattern there is, whose cells are therefore 6 points
//! wide and still plainly cells. Shorter patterns get *wider* cells, which
//! is the same rule read the other way and is what keeps the row full.
//!
//! # What the pointer lands on
//!
//! [`StepGrid::hit`] is the whole of it: a step with nothing over it, a
//! block's body, or one of a block's two edges ([`GridHit`]). Columns are
//! the full pitch, so the gap after a cell belongs to that cell and the row
//! has no dead pixels a press can fall into and do nothing — and the edge
//! handles are measured on those columns too, so the gap just past a block's
//! last cell resizes it rather than moving it. Every gesture over the row
//! starts from that lookup.
//!
//! # The gestures over it
//!
//! A press on a step with nothing over it adds a one-step block of the
//! **last-used action** and takes hold of its end, so a press-and-drag draws a
//! longer one — the quickest way from an empty pattern to a stutter, and the
//! only way to make a block at all.
//! What "last-used" means, and why it is a pref saved with the project rather
//! than something this window holds, is [`action_to_add`].
//!
//! A press on a block and a pointer that moves drags it ([`TimeRowDrag`]): by
//! its middle the block moves, by either of its ends it stretches, and either
//! way it stops against its neighbours and the ends of the pattern rather than
//! pushing through them ([`BlockDrag::target`]). The whole drag is one undo
//! step, taken when the hand lets go ([`TimeRowDrag::settle`]).
//!
//! A press on a block also selects it, and everything *else* about it is the
//! block panel's ([`super::panel`], over [`super::block`]): its preset, loop,
//! source and speeds, and a copy of it. A right-click removes a block, as
//! does Delete for the selected one. The panel is the only thing that
//! *chooses* an action, which is what writes the memory a click reads
//! ([`remember_action`]).

use std::fmt;

use egui::{Color32, Pos2, Rect, pos2};

use super::layout::sane;
use super::theme;
use crate::actions::SpeedPreset;
use crate::model::StutterModel;
use crate::pattern::{Action, Block, MAX_STEPS, NoteValue, Pattern, Rate, Source};
use crate::state::{Prefs, PrefsCell};

/// What a pointer position over the Time row lands on.
///
/// An edge wins over the body it is part of: it is the narrower target, and
/// it is the one gesture that would otherwise be impossible to start.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GridHit {
    /// A step with no block over it. A click here adds one
    /// ([`add_block_at`]).
    Step {
        /// The step, `0..length`.
        step: u16,
    },
    /// The middle of a block — what a move gesture grabs ([`TimeRowDrag`]).
    Body {
        /// Which of the pattern's blocks, as an index into
        /// [`Pattern::blocks`].
        index: usize,
        /// The step of it under the pointer, which is what a move has to
        /// hold on to.
        step: u16,
    },
    /// One end of a block — what a stretch gesture grabs ([`TimeRowDrag`]).
    Edge {
        /// Which of the pattern's blocks.
        index: usize,
        /// Which of its two ends.
        edge: BlockEdge,
    },
}

/// Which end of a block a gesture has hold of.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlockEdge {
    /// Its first step's left edge: dragging this moves the start and keeps
    /// the end.
    Start,
    /// Its last step's right edge: dragging this moves the end and keeps the
    /// start.
    End,
}

/// One frame's step grid: the row it is drawn in, and how many steps are
/// laid across it.
///
/// Cheap to build and small to copy, so a frame builds one per band that
/// needs it rather than passing one around; it holds no pattern, only the
/// length, because the Volume row and the waveform lay out over the same
/// columns and should ask the same arithmetic for them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StepGrid {
    row: Rect,
    steps: u16,
}

impl StepGrid {
    /// The grid of `steps` columns filling `row`.
    ///
    /// `steps` is clamped to `1..=`[`MAX_STEPS`] — a zero-step grid would
    /// divide by zero in every lookup below, and the length it comes from is
    /// already clamped by [`Pattern::set_length`], so this is the boundary
    /// saying so rather than a second opinion. `row` is squared up the way
    /// [`super::layout::EditorLayout`] squares up the window: a reversed,
    /// infinite or NaN rect produces an empty grid that contains no point,
    /// never rects with an infinity in them.
    pub fn new(row: Rect, steps: u16) -> StepGrid {
        let left = sane(row.min.x, f32::MIN, f32::MAX);
        let top = sane(row.min.y, f32::MIN, f32::MAX);
        let width = sane(row.width(), 0.0, f32::MAX);
        let height = sane(row.height(), 0.0, f32::MAX);

        StepGrid {
            row: Rect::from_min_max(pos2(left, top), pos2(left + width, top + height)),
            steps: steps.clamp(1, MAX_STEPS as u16),
        }
    }

    /// The grid `pattern` is drawn on in `row`: its length, laid across the
    /// whole width.
    pub fn for_pattern(row: Rect, pattern: &Pattern) -> StepGrid {
        StepGrid::new(row, pattern.length())
    }

    /// The row the grid fills, squared up.
    pub fn row(&self) -> Rect {
        self.row
    }

    /// How many columns there are — the pattern's length, clamped.
    pub fn steps(&self) -> u16 {
        self.steps
    }

    /// Distance from one column to the next: the row's width plus one gap,
    /// shared out over the steps (the module's §A long pattern is scaled).
    ///
    /// Always finite and never negative; zero only for a row with no width
    /// at all, which is a window squeezed below its own size and a grid that
    /// draws nothing.
    pub fn pitch(&self) -> f32 {
        (self.row.width() + theme::CELL_GAP) / f32::from(self.steps)
    }

    /// The gap drawn between neighbouring cells: [`theme::CELL_GAP`], unless
    /// the pitch is so small that the gap would leave no cell — then it is
    /// whatever the pitch can spare over [`theme::MIN_CELL_WIDTH`].
    ///
    /// At every pitch the fixed window actually produces (8 points at the
    /// longest pattern) this is simply [`theme::CELL_GAP`]; the floor is for
    /// a host that ignored the fixed-size hint.
    pub fn gap(&self) -> f32 {
        sane(self.pitch() - theme::MIN_CELL_WIDTH, 0.0, theme::CELL_GAP)
    }

    /// How wide one cell is drawn: the pitch less the gap after it. 30
    /// points at the default sixteen steps, 6 at the longest pattern.
    pub fn cell_width(&self) -> f32 {
        sane(self.pitch() - self.gap(), 0.0, f32::MAX)
    }

    /// The left edge of column `step`, fractional steps included — so the
    /// playhead and a block's end both have an `x` ([`BlockDrag`]).
    ///
    /// Clamped to `0..=steps`, which is the grid's own span: `step_x(steps)`
    /// is one gap past the row's right edge, because the last cell ends
    /// *on* that edge and the trailing gap is not drawn.
    pub fn step_x(&self, step: f32) -> f32 {
        let step = sane(step, 0.0, f32::from(self.steps));
        self.row.min.x + step * self.pitch()
    }

    /// One step's drawn cell: the gaps either side of it excluded, the row's
    /// full height.
    ///
    /// A step past the last one is drawn as the last one rather than off the
    /// end of the row — the pattern's length is what says how many there
    /// are, and it is read from the same snapshot in the same frame.
    pub fn cell_rect(&self, step: u16) -> Rect {
        let left = self.cell_left(step);

        self.span(left, left + self.cell_width())
    }

    /// One step's column: its cell plus the gap it owns, so the columns tile
    /// the row with no dead pixels in between.
    ///
    /// This is the region [`StepGrid::step_at`] answers with, and the region
    /// a playhead wash should cover so that the lit column reads as
    /// continuous.
    pub fn column_rect(&self, step: u16) -> Rect {
        let step = self.clamp_step(step);

        self.span(self.cell_left(step), self.step_x(f32::from(step) + 1.0))
    }

    /// A block's drawn rectangle: from the left edge of its first cell to
    /// the right edge of its last, in one piece with no gaps inside it.
    ///
    /// `len` is treated as at least one step (a block is never shorter) and
    /// never reaches past the last column.
    pub fn block_rect(&self, start: u16, len: u16) -> Rect {
        let first = self.clamp_step(start);
        let last = self.clamp_step(first.saturating_add(len.max(1) - 1));

        self.cell_rect(first).union(self.cell_rect(last))
    }

    /// A block's pointer region: the columns of all its steps, so the gap
    /// after its last cell belongs to it rather than to nothing.
    pub fn block_columns(&self, start: u16, len: u16) -> Rect {
        let first = self.clamp_step(start);
        let last = self.clamp_step(first.saturating_add(len.max(1) - 1));

        self.column_rect(first).union(self.column_rect(last))
    }

    /// The grab region of one of a block's edges, inside its columns:
    /// [`theme::BLOCK_EDGE_GRAB`] wide, but never more than
    /// [`theme::BLOCK_EDGE_SHARE`] of the block.
    ///
    /// That share is what leaves a one-step block a middle to be moved by:
    /// the two handles together can never take more than two thirds of it,
    /// however narrow the cells have been scaled.
    pub fn edge_rect(&self, start: u16, len: u16, edge: BlockEdge) -> Rect {
        let rect = self.block_columns(start, len);
        let width = sane(theme::BLOCK_EDGE_GRAB, 0.0, rect.width() * theme::BLOCK_EDGE_SHARE);

        match edge {
            BlockEdge::Start => Rect::from_min_max(rect.min, pos2(rect.min.x + width, rect.max.y)),
            BlockEdge::End => Rect::from_min_max(pos2(rect.max.x - width, rect.min.y), rect.max),
        }
    }

    /// Which column `pos` is in, or `None` when it is not over the row at
    /// all.
    ///
    /// Both axes are read: the Time row is one of four bands and a position
    /// above or below it belongs to another one (`super::layout`), not to
    /// whichever column happens to be under it. A running gesture that wants only the column
    /// — a drag whose pointer has wandered out of the row — asks
    /// [`StepGrid::step_at_x`] instead ([`BlockDrag::target`]).
    pub fn step_at(&self, pos: Pos2) -> Option<u16> {
        (self.row.is_positive() && self.row.contains(pos)).then(|| self.step_at_x(pos.x))
    }

    /// Which column screen `x` is in, clamped onto the grid: a gesture that
    /// has been dragged off either end stops at the first or last column
    /// rather than stopping dead.
    ///
    /// The boundaries are walked rather than divided through, because the two
    /// have to agree exactly: the pitch is a division, so `row.min.x + pitch`
    /// is not always `pitch` away from `row.min.x` once rounded, and a
    /// division would put a point on a cell's own left edge in the column
    /// before it — the cell the user is looking at would not be the step they
    /// got. Walking [`StepGrid::step_x`], the lookup is the exact inverse of
    /// [`StepGrid::column_rect`] by construction, at the cost of at most
    /// [`MAX_STEPS`] comparisons on a pointer event.
    pub fn step_at_x(&self, x: f32) -> u16 {
        // A NaN compares false against every bound, so it would otherwise
        // fall out of the walk as the *last* column; the first is the quieter
        // reading of nonsense, as it is everywhere else in this plugin.
        if x.is_nan() {
            return 0;
        }

        for step in 0..self.steps - 1 {
            if x < self.step_x(f32::from(step) + 1.0) {
                return step;
            }
        }

        self.steps - 1
    }

    /// What `pos` lands on, given the pattern's blocks (`blocks` as
    /// [`Pattern::blocks`] holds them: sorted, non-overlapping, inside the
    /// length).
    ///
    /// `None` only when `pos` is not over the row.
    pub fn hit(&self, pos: Pos2, blocks: &[Block]) -> Option<GridHit> {
        let step = self.step_at(pos)?;
        let Some(index) = blocks.iter().position(|block| block.covers(step)) else {
            return Some(GridHit::Step { step });
        };

        let (start, len) = (blocks[index].start(), blocks[index].len());
        for edge in [BlockEdge::Start, BlockEdge::End] {
            if self.edge_rect(start, len, edge).contains(pos) {
                return Some(GridHit::Edge { index, edge });
            }
        }

        Some(GridHit::Body { index, step })
    }

    /// `step`, brought inside the columns this grid has.
    fn clamp_step(&self, step: u16) -> u16 {
        step.min(self.steps - 1)
    }

    /// The left edge of a column, held inside the row.
    fn cell_left(&self, step: u16) -> f32 {
        sane(self.step_x(f32::from(self.clamp_step(step))), self.row.min.x, self.row.max.x)
    }

    /// `left..right` of the row, as a rect the row's own height and never
    /// reaching past either of its edges.
    ///
    /// The right edge is where the clamping earns its keep: the pitch is a
    /// division, so `steps` of it only sums back to the row's width to
    /// within a float, and the last cell of a three-step pattern would
    /// otherwise end sixteen thousandths of a point outside the band it is
    /// drawn in. Clamped, it ends exactly on it.
    fn span(&self, left: f32, right: f32) -> Rect {
        let left = sane(left, self.row.min.x, self.row.max.x);
        let right = sane(right, left, self.row.max.x);

        Rect::from_min_max(pos2(left, self.row.min.y), pos2(right, self.row.max.y))
    }
}

/// How many steps of `rate` make one beat: 2, 3, 4, 6 or 8 — every rate
/// there is divides a beat a whole number of times.
///
/// At least one whatever arrives, so [`beat_shaded`] can divide by it.
pub fn steps_per_beat(rate: Rate) -> u16 {
    let steps = (1.0 / rate.step_beats()).round();

    sane(steps as f32, 1.0, MAX_STEPS as f32) as u16
}

/// Whether `step` falls in a shaded column ([`theme::CELL_SHADED`]), which
/// `theme` leaves to this module to decide.
///
/// Alternate **beats** are shaded, not alternate steps: what the eye needs
/// from the grid is where the beat is, and that is the one thing a step
/// count cannot say on its own once the rate is a triplet or the pattern is
/// sixty-four steps long. The first beat of a pattern is unshaded, so step 1
/// always reads as the start.
pub fn beat_shaded(step: u16, rate: Rate) -> bool {
    (step / steps_per_beat(rate)) % 2 == 1
}

/// The colour an empty step's cell is drawn in.
pub fn cell_colour(step: u16, rate: Rate) -> Color32 {
    if beat_shaded(step, rate) { theme::CELL_SHADED } else { theme::CELL }
}

/// Which of the seven coloured kinds of block an action is.
///
/// The discriminant is the index into [`theme::BLOCK_COLOURS`], which is why
/// the order here is that list's order and not [`SpeedPreset::ALL`]'s.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum BlockKind {
    /// A Repeat, whatever its slice and source.
    Repeat = 0,
    /// The −1 → −1 ramp.
    Reverse = 1,
    /// The 0.5 → 0.5 ramp.
    HalfSpeed = 2,
    /// The 2 → 2 ramp.
    DoubleSpeed = 3,
    /// The 1 → 0 ramp.
    TapeStop = 4,
    /// The 0 → 1 ramp.
    TapeStart = 5,
    /// Any other ramp — one the user drew.
    Ramp = 6,
}

impl BlockKind {
    /// Every kind, in [`theme::BLOCK_COLOURS`]'s order.
    pub const ALL: [BlockKind; 7] = [
        BlockKind::Repeat,
        BlockKind::Reverse,
        BlockKind::HalfSpeed,
        BlockKind::DoubleSpeed,
        BlockKind::TapeStop,
        BlockKind::TapeStart,
        BlockKind::Ramp,
    ];

    /// What kind of block `action` makes: any loop is a Repeat; otherwise
    /// the speeds name a preset or make a free ramp. The source does not
    /// change the kind (a tape stop that continues the block before it is
    /// still a tape stop).
    pub fn of(action: Action) -> BlockKind {
        let action = action.sanitised();
        if action.is_repeat() {
            return BlockKind::Repeat;
        }
        match action.preset() {
            None => BlockKind::Ramp,
            Some(SpeedPreset::Reverse) => BlockKind::Reverse,
            Some(SpeedPreset::HalfSpeed) => BlockKind::HalfSpeed,
            Some(SpeedPreset::DoubleSpeed) => BlockKind::DoubleSpeed,
            Some(SpeedPreset::TapeStop) => BlockKind::TapeStop,
            Some(SpeedPreset::TapeStart) => BlockKind::TapeStart,
        }
    }

    /// This kind's palette entry — the colour its label is written in and
    /// its body washed with ([`theme::BLOCK_BODY_FACTOR`]).
    pub fn colour(self) -> Color32 {
        theme::BLOCK_COLOURS[self as usize]
    }
}

/// What a Repeat block's label says, one per [`NoteValue`] in
/// [`NoteValue::ALL`]'s order: `Rpt 1/16`.
///
/// Written out rather than formatted per frame, as the top bar's labels are
/// ([`super::top_bar::RATE_LABELS`]): the row is redrawn on every repaint of
/// a playing transport, and a `&'static str` costs nothing to hand egui. A
/// test holds these against [`NoteValue`]'s own `Display`, so the two cannot
/// drift.
pub const REPEAT_LABELS: [&str; NoteValue::ALL.len()] = [
    "Rpt 1/1",
    "Rpt 1/1T",
    "Rpt 1/2",
    "Rpt 1/2T",
    "Rpt 1/4",
    "Rpt 1/4T",
    "Rpt 1/8",
    "Rpt 1/8T",
    "Rpt 1/16",
    "Rpt 1/16T",
    "Rpt 1/32",
    "Rpt 1/32T",
    "Rpt 1/64",
    "Rpt 1/64T",
];

/// What every Repeat label starts with — the word "Repeat" shortened to fit
/// a cell, and the prefix the test above builds the table from.
///
/// Compiled for the tests alone: [`REPEAT_LABELS`] spells the prefix out
/// sixteen times because a `const` table cannot be concatenated together,
/// and this is what holds those sixteen (and the menu's own labels) to one
/// word.
#[cfg(test)]
pub const REPEAT_PREFIX: &str = "Rpt ";

/// The five named ramps' labels, in [`SpeedPreset::ALL`]'s order: `Rev`,
/// `½×`, `2×`, `Stop`, `Start`.
///
/// A symbol where there is one — `½×` and `2×` say "half speed" and "double
/// speed" in two characters, which is what fits a one-step block — and the
/// shortest unambiguous word otherwise.
pub const PRESET_LABELS: [&str; SpeedPreset::ALL.len()] = ["½×", "2×", "Rev", "Stop", "Start"];

/// What a free ramp's two speeds are joined by: `0.5→1.5`.
pub const RAMP_ARROW: &str = "→";

/// A block's short label: an optional source marker, then what it does.
///
/// A struct of `&'static str`s and numbers rather than a `String`, so the
/// pure half of this module allocates nothing; [`fmt::Display`] writes it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BlockLabel {
    /// `""` from the block start, [`SPOT_MARK`] for a fixed spot,
    /// [`CONTINUE_MARK`] for one that continues the previous block.
    pub mark: &'static str,
    /// What the block does.
    pub body: LabelBody,
}

/// The part of a [`BlockLabel`] that says what the block does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LabelBody {
    /// A word: a preset, or a loop at live speed (`Rpt 1/16`).
    Fixed(&'static str),
    /// A free ramp's endpoints, written `0.5→1.5`.
    Ramp {
        /// The speed the block starts at.
        from: f32,
        /// The speed it ends at.
        to: f32,
    },
    /// A loop at another speed: `Rpt 1/16 ×0.5`, or `Rpt 1/16 1→2` when the
    /// speed ramps.
    Repeat {
        /// The loop's label, `Rpt 1/16`.
        name: &'static str,
        /// The speed at the block's start.
        from: f32,
        /// The speed at its end.
        to: f32,
    },
}

/// Written before a block that reads from a fixed spot in the pattern.
pub const SPOT_MARK: &str = "‹";
/// Written before a block that continues where the previous one left off.
pub const CONTINUE_MARK: &str = "…";

impl fmt::Display for BlockLabel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.mark)?;
        match self.body {
            LabelBody::Fixed(text) => f.write_str(text),
            LabelBody::Ramp { from, to } => write_ramp(f, from, to),
            LabelBody::Repeat { name, from, to } if from == to => {
                write!(f, "{name} ×")?;
                write_speed(f, from)
            }
            LabelBody::Repeat { name, from, to } => {
                write!(f, "{name} ")?;
                write_ramp(f, from, to)
            }
        }
    }
}

/// `from→to`.
fn write_ramp(f: &mut fmt::Formatter<'_>, from: f32, to: f32) -> fmt::Result {
    write_speed(f, from)?;
    f.write_str(RAMP_ARROW)?;
    write_speed(f, to)
}

/// One ramp endpoint, as short as it can be written: `1`, `0.5`, `-1.25`.
///
/// Rounded to [`SPEED_DECIMALS`] places and then left to `f64`'s own
/// `Display`, which writes the shortest form that reads back — so a whole
/// speed loses its `.0` and the label stays inside a cell. A non-finite
/// speed cannot come out of the model, which clamps, but this is a label on
/// a block that may have been *loaded*, so it is written as `0` rather than
/// as `NaN`.
fn write_speed(f: &mut fmt::Formatter<'_>, speed: f32) -> fmt::Result {
    let speed = if speed.is_finite() { f64::from(speed) } else { 0.0 };
    let scale = 10f64.powi(SPEED_DECIMALS);

    write!(f, "{}", (speed * scale).round() / scale)
}

/// How many decimal places a ramp endpoint's label keeps.
///
/// Two: the speeds are knob positions between −2 and +2, so two places tell
/// a 1.25 from a 1.3, and a third would not fit beside the arrow.
const SPEED_DECIMALS: i32 = 2;

/// The short label `action`'s block wears.
pub fn block_label(action: Action) -> BlockLabel {
    let action = action.sanitised();
    let mark = match action.source {
        Source::BlockStart => "",
        Source::Spot { .. } => SPOT_MARK,
        Source::Continue => CONTINUE_MARK,
    };
    let (from, to) = (action.from, action.to);

    let body = match (action.looping, action.preset()) {
        (Some(length), _) => {
            let name = REPEAT_LABELS[length.to_u8() as usize];
            match from == 1.0 && to == 1.0 {
                true => LabelBody::Fixed(name),
                false => LabelBody::Repeat { name, from, to },
            }
        }
        (None, Some(preset)) => LabelBody::Fixed(PRESET_LABELS[preset_index(preset)]),
        (None, None) => LabelBody::Ramp { from, to },
    };

    BlockLabel { mark, body }
}

/// A preset's index in [`SpeedPreset::ALL`], and so in [`PRESET_LABELS`].
///
/// Written out rather than taken from a discriminant, because
/// [`SpeedPreset`] is `actions.rs`'s type and its order is that module's to
/// change; a test holds this against [`SpeedPreset::ALL`].
fn preset_index(preset: SpeedPreset) -> usize {
    match preset {
        SpeedPreset::HalfSpeed => 0,
        SpeedPreset::DoubleSpeed => 1,
        SpeedPreset::Reverse => 2,
        SpeedPreset::TapeStop => 3,
        SpeedPreset::TapeStart => 4,
    }
}

// ---- the add gesture ----

/// How many steps a click on an empty step adds: one.
///
/// A click is the short way in — it puts a block where the pointer is, and
/// the drag that follows is what makes it the length it wants
/// ([`TimeRowDrag`]) — so
/// the block it adds is as small as a block can be and covers the step that
/// was clicked and nothing else.
pub const ADDED_LEN: u16 = 1;

/// The action a click on an empty step of a pattern at `rate` adds: the
/// last-used one, remembered across clicks and saved with the project
/// ([`Prefs::last_action`]).
///
/// The one wrinkle is the first click of an instance that has never been
/// clicked, where two rules meet. The memory starts at "Repeat, one-step
/// slice, source BlockStart", and a new block's slice is "one step of the
/// pattern's rate". Both are true of [`Prefs::default`] on a pattern at
/// [`Rate::DEFAULT`], and they part company on a 1/32 pattern, where the
/// remembered 1/16 would loop two steps of the one-step block it was put
/// on — which is not the classic stutter either rule is describing.
///
/// So an **untouched** memory is read as "the classic stutter at whatever rate
/// this pattern runs at", and a memory a hand has put there is obeyed exactly:
/// a user who picks `Rpt 1/16` deliberately gets `Rpt 1/16` on every pattern
/// afterwards, whatever its rate, because that is what the memory is for.
///
/// The two cases are told apart by the value alone — an untouched memory is
/// [`Action::default`] — and that leaves one corner: a user who *chooses*
/// `Rpt 1/16`, which is the default's own value, is choosing something this
/// cannot tell from never having chosen, so a 1/32 pattern clicked
/// afterwards gets `Rpt 1/32`. A flag saying "chosen" would close it, at the
/// cost of a third pref in the saved snapshot for a case whose two answers
/// are "a Repeat of one step" and "a Repeat of two steps" on one kind of
/// pattern. The rate-derived answer is the better of the two anyway, which
/// is why the corner is left as it is rather than paid for.
pub fn action_to_add(prefs: Prefs, rate: Rate) -> Action {
    if prefs.last_action == Action::default() {
        return Action::repeat_at(rate);
    }

    // A spot only from a project saved by hand: see [`remember_action`].
    match prefs.last_action.sanitised() {
        action @ Action { source: Source::Spot { .. }, .. } => {
            action.with_source(Source::BlockStart)
        }
        action => action,
    }
}

/// Remembers `action` as the one the next click adds ([`action_to_add`]).
///
/// [`Action::sanitised`] on the way in, as everything that reaches the model
/// is: the pref is saved with the project and loaded back from a file that
/// may say anything, and a remembered action that disagreed with itself
/// would be labelled one thing and added as another.
///
/// The only place the pref is written, and what writes it is a *choice*: the
/// block panel calls this with the settings the user picked for a block.
/// Adding a block does not — adding is not a choice about which action to
/// add.
///
/// A fixed spot is remembered as the block start: a spot is a place in one
/// pattern, and a new block somewhere else reading it would be a surprise.
pub fn remember_action(prefs: &PrefsCell, action: Action) {
    let action = action.sanitised();
    let action = match action.source {
        Source::Spot { .. } => action.with_source(Source::BlockStart),
        _ => action,
    };
    prefs.set_last_action(action);
}

/// A click on empty `step` of pattern `pattern`, which runs at `rate`: a
/// one-step block of the last-used action ([`action_to_add`]), added through
/// the writer API.
///
/// Returns the new block's index on the row, or `None` where the click had
/// nowhere to go — the step is covered already, the row is full, or the index
/// is not one of the sixteen ([`StutterModel::add_block`]) — in which case
/// nothing is published and there is nothing to undo.
///
/// Committed on the spot, because a click is a whole gesture — there is no
/// half-added block the way there is a half-finished drag — so one click is
/// one undo step.
///
/// # Adding a block does not change the memory
///
/// This **reads** the pref and never writes it, which is deliberate: a click
/// does not choose an action, it applies the one already chosen, and writing
/// back what it applied would be a no-op in every case but the one where it
/// would be wrong. That case is the untouched memory [`action_to_add`]
/// re-derives from the rate — a first click on a 1/32 pattern would make
/// `Rpt 1/32` the memory, and the user's 1/16 patterns would then get
/// one-step blocks looping two steps for having been clicked on second.
/// Left alone, an untouched memory keeps meaning "the classic stutter at this
/// pattern's rate" for every pattern until a hand picks something
/// ([`remember_action`]).
///
/// The pref is not in the undo history either, and that is the same
/// reasoning read backwards: it is what the hand last chose, not part of any
/// pattern, so stepping back over a block leaves the next click adding the
/// same thing.
pub fn add_block_at(
    model: &StutterModel,
    prefs: &PrefsCell,
    pattern: usize,
    rate: Rate,
    step: u16,
) -> Option<usize> {
    let index = model.add_block(pattern, step, ADDED_LEN, action_to_add(prefs.get(), rate))?;
    model.commit_edits();

    Some(index)
}

// ---- the drag gestures ----

/// What a drag of a block is doing to it.
///
/// The two gestures — "drag a block's edges to stretch/shrink, drag its middle
/// to move" — and which of the two it is was settled by where the press landed
/// ([`BlockDrag::begin`]) and never changes while the hand is down: a move
/// stopped against a neighbour does not become a stretch because the pointer
/// kept going.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DragKind {
    /// Moving the whole block, which keeps its length.
    Move {
        /// Which step of the block was grabbed, counted from its first: the
        /// step that stays under the pointer, so a four-step block picked up
        /// by its last cell does not jump three steps forward to meet the
        /// pointer.
        grab: u16,
    },
    /// Stretching one of its ends, which leaves the other end where it is.
    Stretch {
        /// The end the hand has hold of.
        edge: BlockEdge,
    },
}

/// One block being dragged: which block of the row, how it was when the press
/// landed, and what the drag is doing to it.
///
/// # Measured from the press, never from the last frame
///
/// The gesture keeps the block **as the press found it** and resolves every
/// later pointer position against that, rather than against the block as the
/// frame before left it. That is what makes a drag an absolute mapping from
/// the pointer, and it is what the clamping needs: a move held against a
/// neighbour has written that same clamped start on every frame since, and a
/// relative gesture would have thrown away the travel the pointer made while
/// it was stuck there — dragging back would then start from the clamp instead
/// of from where the hand really is. Nothing accumulates either, so a drag
/// that wanders off and comes back leaves the block exactly as it was, and
/// therefore leaves no undo step at all ([`StutterModel::commit_edits`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BlockDrag {
    /// Which block of the row it has hold of, as an index into
    /// [`Pattern::blocks`].
    ///
    /// Stable for the whole gesture, because neither thing a drag can do
    /// reorders the row: a block that cannot pass a neighbour cannot change
    /// places with one either ([`crate::pattern::Pattern::move_block`]).
    index: usize,
    /// The block as the press found it (the type's §Measured from the
    /// press).
    from: Block,
    kind: DragKind,
}

impl BlockDrag {
    /// The drag a press on `hit` starts, with `blocks` the row the frame that
    /// was pressed drew ([`Pattern::blocks`]).
    ///
    /// `None` for a press that has no block under it — a press on an empty
    /// step belongs to the add gesture ([`add_block_at`]) — and for a hit
    /// taken against some other row than `blocks`, which the editor never
    /// does (it reads both from one snapshot in one frame).
    pub fn begin(hit: GridHit, blocks: &[Block]) -> Option<BlockDrag> {
        let (index, from, kind) = match hit {
            GridHit::Step { .. } => return None,
            GridHit::Body { index, step } => {
                let from = *blocks.get(index)?;
                // Held inside the block: a body hit is one of its own steps,
                // so this only bites on a hit from another row.
                let grab = step.saturating_sub(from.start()).min(from.len() - 1);

                (index, from, DragKind::Move { grab })
            }
            GridHit::Edge { index, edge } => {
                (index, *blocks.get(index)?, DragKind::Stretch { edge })
            }
        };

        Some(BlockDrag { index, from, kind })
    }

    /// What the drag is doing to it.
    pub fn kind(&self) -> DragKind {
        self.kind
    }

    /// The block this drag holds, by its index on the row.
    pub fn index(&self) -> usize {
        self.index
    }

    /// A hold on the end of block `index`, which is `from` — what a press on
    /// an empty step takes of the block it has just added there.
    pub fn stretching(index: usize, from: Block) -> BlockDrag {
        BlockDrag { index, from, kind: DragKind::Stretch { edge: BlockEdge::End } }
    }

    /// The span `(start, len)` the block wants with the pointer at `pos`,
    /// clamped against its neighbours in `blocks` and the ends of the
    /// pattern.
    ///
    /// The step the pointer is *in* is what it follows — [`StepGrid::hit`]'s
    /// own columns, read with [`StepGrid::step_at_x`], so a pointer dragged
    /// off either end of the row keeps pulling the block to the last column
    /// instead of stopping dead, and the cell under the hand is the cell the
    /// block takes. A move holds its grabbed step under the pointer; a
    /// stretched end moves to the column under it and the other end stays
    /// where the press found it, never closer than the one step a block is at
    /// least.
    ///
    /// `None` when `blocks` is not the row this drag began on — the block is
    /// gone, or the row disagrees with the gesture (`block_gap` below) — where
    /// there is no honest answer and the drag had better do nothing.
    pub fn target(&self, grid: &StepGrid, blocks: &[Block], pos: Pos2) -> Option<(u16, u16)> {
        let (first, limit) = block_gap(blocks, self.index, grid.steps())?;
        let under = grid.step_at_x(pos.x);

        // `first < limit` and the block fits between them, so every clamp
        // below has a lower bound at or under its upper one. Written
        // `max`-then-`min` as everywhere else in this plugin.
        Some(match self.kind {
            DragKind::Move { grab } => {
                let len = self.from.len().max(1).min(limit - first);
                let start = under.saturating_sub(grab).max(first).min(limit - len);

                (start, len)
            }
            DragKind::Stretch { edge: BlockEdge::End } => {
                let start = self.from.start().max(first).min(limit - 1);
                let len = (under.saturating_sub(start) + 1).min(limit - start);

                (start, len)
            }
            DragKind::Stretch { edge: BlockEdge::Start } => {
                let end = self.from.end().max(first + 1).min(limit);
                let start = under.max(first).min(end - 1);

                (start, end - start)
            }
        })
    }

    /// Writes the span the pointer asks for through the writer API, and
    /// deliberately does **not** commit: a drag is one undo step, taken when
    /// the hand lets go ([`TimeRowDrag::settle`]).
    ///
    /// Each kind of drag goes through the writer method that means it, so the
    /// model clamps it a second time and the two can only agree: a move
    /// through [`StutterModel::move_block`], which stops a block against its
    /// neighbours; a stretched right edge through
    /// [`StutterModel::resize_block`]; and a stretched left edge through
    /// [`StutterModel::set_block_span`], which is the one write that moves
    /// the start and the length together.
    ///
    /// Returns the span the block has afterwards — which is the span it
    /// already had when the pointer has not left the step it was on, since
    /// nothing is written then. `None` where there was nowhere to write: the
    /// row is not the one the drag began on, or `pattern` is not one of the
    /// sixteen.
    pub fn apply(
        &self,
        model: &StutterModel,
        grid: &StepGrid,
        pattern: usize,
        blocks: &[Block],
        pos: Pos2,
    ) -> Option<(u16, u16)> {
        let (start, len) = self.target(grid, blocks, pos)?;
        let now = blocks.get(self.index)?;

        // Most frames of a drag ask for the span the block already has — a
        // pointer moving across one cell is thirty of them — and writing that
        // back would publish a snapshot per frame for nothing, with the
        // collector behind it (`top_bar::choose_rate` has the same guard for
        // the same reason).
        if (now.start(), now.len()) == (start, len) {
            return Some((start, len));
        }

        match self.kind {
            DragKind::Move { .. } => {
                model.move_block(pattern, self.index, start).map(|start| (start, len))
            }
            DragKind::Stretch { edge: BlockEdge::End } => {
                model.resize_block(pattern, self.index, len).map(|len| (start, len))
            }
            DragKind::Stretch { edge: BlockEdge::Start } => {
                model.set_block_span(pattern, self.index, start, len)
            }
        }
    }
}

/// The room the block at `index` has to itself: the first step it may start
/// on, and one past the last step it may reach — its neighbours' facing edges
/// in `blocks`, or the ends of a `steps`-long pattern.
///
/// This is [`crate::pattern::Pattern::move_block`]'s own clamp, worked out
/// again from the published row instead of taken on trust, so that a drag is
/// resolved to the span it will really get before anything is written: the
/// gesture decides, the model agrees, and the block under the pointer is the
/// block the next frame draws.
///
/// `None` for a row this grid was not drawn from — the index is past its end,
/// or the block does not fit in the room its neighbours leave it, which a
/// sorted, disjoint row inside the pattern's length never does. A half-made
/// drag can see one: an undo taken while the hand is still down replaces
/// the row under it.
fn block_gap(blocks: &[Block], index: usize, steps: u16) -> Option<(u16, u16)> {
    let block = blocks.get(index)?;
    let first = index.checked_sub(1).map_or(0, |before| blocks[before].end());
    let limit = blocks.get(index + 1).map_or(steps, |after| after.start());

    (first <= block.start() && block.end() <= limit).then_some((first, limit))
}

/// The Time row's half-made gesture, as the window holds it between frames.
///
/// Per opened window, like the top bar's own gestures
/// ([`super::top_bar::LengthBox`]) and for the same reason: it is a hand over
/// this row, not anything the instance or the project holds, so it is built
/// afresh in `NiceEguiApp::build` — nice-plug-egui 0.6 never calls
/// `editor_closed`, and a window that reopened still holding a drag would
/// write to a block the hand let go of long ago.
///
/// There are three states and not two, because a press that landed on no
/// block has to be told from no press at all: without [`TimeRowDrag::Missed`]
/// a hand that pressed an empty step and slid onto a block would pick that
/// block up half-way through its own gesture and drag it from wherever the
/// pointer happened to be.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum TimeRowDrag {
    /// No press on the row.
    #[default]
    Idle,
    /// A press that landed on no block: an empty step, which is the add
    /// gesture's ([`add_block_at`], applied on release), or a row the hit
    /// does not fit. It stays this way until the hand lets go.
    Missed,
    /// A press with hold of a block, and what it is doing to it.
    Held(BlockDrag),
}

impl TimeRowDrag {
    /// The press: what a press at `pos` takes hold of, `true` if that is a
    /// block.
    ///
    /// Does nothing once a press is open, which is what the three states are
    /// for (see the type) — the drawing calls this on every frame the button
    /// is down, and only the first of them is the press.
    pub fn press(&mut self, grid: &StepGrid, blocks: &[Block], pos: Pos2) -> bool {
        if self.open() {
            return self.held().is_some();
        }

        *self = match grid.hit(pos, blocks).and_then(|hit| BlockDrag::begin(hit, blocks)) {
            Some(drag) => TimeRowDrag::Held(drag),
            None => TimeRowDrag::Missed,
        };

        self.held().is_some()
    }

    /// Whether a press is open at all, block or no block.
    ///
    /// What holds the window on the pattern the gesture started on
    /// ([`super::top_bar::edited_this_frame`]): with follow on, a host that
    /// changed pattern between two frames would otherwise move the write out
    /// from under the pointer, and the drag would finish by moving a block of
    /// a pattern the user never touched.
    pub fn open(&self) -> bool {
        !matches!(self, TimeRowDrag::Idle)
    }

    /// The block the gesture has hold of, if it has one.
    pub fn held(&self) -> Option<BlockDrag> {
        match self {
            TimeRowDrag::Held(drag) => Some(*drag),
            TimeRowDrag::Idle | TimeRowDrag::Missed => None,
        }
    }

    /// The pointer has moved with the button still down: the block follows it
    /// ([`BlockDrag::apply`]), written through and not committed.
    ///
    /// Nothing at all for a press that has no block, which is every frame of
    /// a press on an empty step.
    pub fn drag_to(
        &self,
        model: &StutterModel,
        grid: &StepGrid,
        pattern: usize,
        blocks: &[Block],
        pos: Pos2,
    ) -> Option<(u16, u16)> {
        self.held()?.apply(model, grid, pattern, blocks, pos)
    }

    /// The hand let go: whatever the drag wrote becomes one undo step, and
    /// the row is idle again. `true` if a block had been dragged.
    ///
    /// Cheap to call on every frame with no press on the row, which is how
    /// the drawing calls it: an idle row is a `matches!` and nothing else,
    /// and a drag that ended where it started still records no step
    /// ([`StutterModel::commit_edits`] compares the patterns).
    pub fn settle(&mut self, model: &StutterModel) -> bool {
        if std::mem::replace(self, TimeRowDrag::Idle).held().is_none() {
            return false;
        }

        model.commit_edits();
        true
    }
}

/// What one open window holds between frames for the Time row: the hand on a
/// block ([`TimeRowDrag`]) and the selected block with its speed gesture
/// ([`super::block::BlockEditor`]), which the block panel edits.
///
/// Both are built afresh in `NiceEguiApp::build` (nice-plug-egui 0.6 never
/// calls `editor_closed`). A gesture open in either holds the window on the
/// pattern it started over ([`TimeRowHands::busy`]); a selection alone does
/// not.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TimeRowHands {
    /// The hand on a block, if one is held.
    pub drag: TimeRowDrag,
    /// The selected block and the speed boxes' gesture.
    pub editor: super::block::BlockEditor,
}

impl TimeRowHands {
    /// No hand on anything and nothing selected.
    pub fn idle() -> TimeRowHands {
        TimeRowHands::default()
    }

    /// Whether a gesture is half made: a block held, or a speed box being
    /// dragged or typed into.
    pub fn busy(&self) -> bool {
        self.drag.open() || self.editor.gesture_open()
    }
}

/// The pointer's shape over the Time row: what the drag under way is doing,
/// or what a press where the pointer is would start.
///
/// `None` leaves egui's own arrow, which is the answer over an empty step
/// (there is nothing there to take hold of, and the hover text already says
/// what a click would add) and over a press that landed on one.
pub fn drag_cursor(drag: TimeRowDrag, hit: Option<GridHit>) -> Option<egui::CursorIcon> {
    if drag.open() {
        return drag.held().map(|held| match held.kind() {
            DragKind::Move { .. } => egui::CursorIcon::Grabbing,
            DragKind::Stretch { .. } => egui::CursorIcon::ResizeHorizontal,
        });
    }

    match hit {
        Some(GridHit::Edge { .. }) => Some(egui::CursorIcon::ResizeHorizontal),
        Some(GridHit::Body { .. }) => Some(egui::CursorIcon::Grab),
        Some(GridHit::Step { .. }) | None => None,
    }
}

/// What a block is and where it sits: `Rpt 1/16 on step 1.`, `Stop on steps
/// 5 to 8.`
///
pub fn block_span_text(block: &Block) -> String {
    let (first, last) = (block.start() + 1, block.end());
    let label = block_label(block.action());

    match first == last {
        true => format!("{label} on step {first}."),
        false => format!("{label} on steps {first} to {last}."),
    }
}

fn time_row_id() -> egui::Id {
    egui::Id::unique("stuttter-time-row")
}

/// The x of every loop restart inside block `block`, for the repeat ticks —
/// none when there is no loop, or when they would be closer together than
/// [`theme::REPEAT_TICK_MIN_SPACING`].
pub fn repeat_ticks(grid: &StepGrid, pattern: &Pattern, block: &Block) -> Vec<f32> {
    let Some(length) = block.action().looping else {
        return Vec::new();
    };
    let loop_steps = length.beats() / pattern.step_beats();
    if loop_steps * f64::from(grid.pitch()) < f64::from(theme::REPEAT_TICK_MIN_SPACING) {
        return Vec::new();
    }

    let end = f64::from(block.len());
    (1..)
        .map(|lap| f64::from(lap) * loop_steps)
        .take_while(|&steps| steps < end - 1e-9)
        .map(|steps| grid.step_x((f64::from(block.start()) + steps) as f32) - grid.gap() * 0.5)
        .collect()
}

/// Paints the Time row and handles its gestures: a press on an empty step
/// adds a block there (drag on to make it longer), a press on a block
/// selects it and moves or stretches it, a right-click removes one, Delete
/// removes the selected block and Escape deselects.
///
/// Returns the block under the pointer, if any, so the waveform can light up
/// where it reads from.
pub(super) fn ui_time_row(
    ui: &mut egui::Ui,
    row: Rect,
    model: &StutterModel,
    prefs: &PrefsCell,
    index: usize,
    pattern: &Pattern,
    hands: &mut TimeRowHands,
) -> Option<usize> {
    let TimeRowHands { drag, editor } = hands;
    let grid = StepGrid::for_pattern(row, pattern);
    let rate = pattern.rate();
    let selected = editor.selected(index, pattern);
    let painter = ui.painter();

    // The empty grid first, which is also the empty state.
    for step in 0..grid.steps() {
        painter.rect_filled(grid.cell_rect(step), theme::CELL_CORNER, cell_colour(step, rate));
    }
    for (slot, block) in pattern.blocks().iter().enumerate() {
        let kind = BlockKind::of(block.action());
        let colour = kind.colour();
        let rect = grid.block_rect(block.start(), block.len());
        painter.rect(
            rect,
            theme::CELL_CORNER,
            theme::dim(colour, theme::BLOCK_BODY_FACTOR),
            egui::Stroke::new(theme::BLOCK_OUTLINE_WIDTH, colour),
            egui::StrokeKind::Inside,
        );
        // A tick at every loop restart, so the repeats can be counted.
        let tick_colour = theme::dim(colour, theme::REPEAT_TICK_FACTOR);
        let tick = egui::Stroke::new(theme::HAIRLINE, tick_colour);
        let inset = rect.height() * 0.2;
        for x in repeat_ticks(&grid, pattern, block) {
            painter.vline(x, (rect.min.y + inset)..=(rect.max.y - inset), tick);
        }
        if selected == Some(slot) {
            painter.rect_stroke(
                rect,
                theme::CELL_CORNER,
                egui::Stroke::new(theme::SELECTED_OUTLINE_WIDTH, theme::SELECTED_OUTLINE),
                egui::StrokeKind::Outside,
            );
        }
        // The label, if the block is wide enough to hold it.
        let text = block_label(block.action()).to_string();
        let font = egui::FontId::proportional(theme::BLOCK_LABEL_SIZE);
        let galley = painter.layout_no_wrap(text, font, colour);
        if galley.size().x + 2.0 * theme::BLOCK_LABEL_PADDING <= rect.width() {
            let pos = rect.center() - galley.size() * 0.5;
            painter.galley(pos, galley, theme::TEXT);
        }
    }

    // A fixed id, hashed from a name: the row is painted into a rect rather
    // than laid out, so the `Ui`'s auto counter is not a stable thing to hang
    // a frame's hover state on.
    let response = ui.interact(grid.row(), time_row_id(), egui::Sense::click_and_drag());
    // A drag is the primary button, from the moment it goes down: not
    // `dragged`, which egui holds back until a click has been ruled out, so a
    // block follows the pointer from the first cell it reaches.
    let primary = ui.ctx().input(|input| input.pointer.button_down(egui::PointerButton::Primary));
    let holding = response.is_pointer_button_down_on() && primary;
    match response.interact_pointer_pos().filter(|_| holding) {
        Some(pos) if !drag.open() => {
            // The press. On an empty step it adds a block of the last-used
            // recipe and takes hold of its end, so a press-and-drag draws a
            // longer block; the whole gesture is one undo step, committed on
            // release. The block is not in this frame's snapshot yet, so the
            // drag itself starts next frame.
            match grid.hit(pos, pattern.blocks()) {
                Some(GridHit::Step { step }) => {
                    let action = action_to_add(prefs.get(), rate);
                    *drag = match model.add_block(index, step, ADDED_LEN, action) {
                        Some(added) => {
                            editor.select(model, index, Some(added));
                            TimeRowDrag::Held(BlockDrag::stretching(
                                added,
                                Block::new(step, ADDED_LEN, action),
                            ))
                        }
                        None => TimeRowDrag::Missed,
                    };
                }
                _ => {
                    if drag.press(&grid, pattern.blocks(), pos)
                        && let Some(held) = drag.held()
                    {
                        editor.select(model, index, Some(held.index()));
                    }
                }
            }
        }
        Some(pos) => {
            drag.drag_to(model, &grid, index, pattern.blocks(), pos);
        }
        // Every way a press can end leaves the button no longer down on the
        // row, so this is also where a finished gesture becomes its undo step.
        None => {
            drag.settle(model);
        }
    }

    let hit = response.hover_pos().and_then(|pos| grid.hit(pos, pattern.blocks()));

    // The other button removes the block it lands on.
    if response.secondary_clicked()
        && !drag.open()
        && let Some(pos) = response.interact_pointer_pos()
        && let Some(target) = super::block::hovered_block(grid.hit(pos, pattern.blocks()))
    {
        super::block::remove(model, editor, index, target);
    }

    // Delete removes the selected block (or, with none selected, the one
    // under the pointer); Escape deselects. Not while a number is being
    // typed — those keys are the box's then — and not mid-drag.
    let typing = ui.ctx().text_edit_focused();
    if !drag.open() && !typing {
        let (delete, escape) = ui.ctx().input(|input| {
            (input.key_pressed(egui::Key::Delete), input.key_pressed(egui::Key::Escape))
        });
        if delete
            && let Some(target) = selected.or_else(|| super::block::hovered_block(hit))
        {
            super::block::remove(model, editor, index, target);
        }
        if escape {
            editor.clear(model);
        }
    }

    if let Some(cursor) = drag_cursor(*drag, hit)
        && (response.hovered() || drag.open())
    {
        ui.ctx().set_cursor_icon(cursor);
    }

    super::block::hovered_block(hit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::StutterData;
    use crate::pattern::PATTERN_COUNT;
    use crate::editor::STEPS_SHOWN;
    use egui::vec2;

    /// A row sixteen bitstep cells wide — the rect the geometry below is
    /// measured against, so the figures are whole cells. The window's own
    /// Time row is whatever width the window has; the maths is the same.
    const LANE_WIDTH: f32 = STEPS_SHOWN * theme::CELL_PITCH - theme::CELL_GAP;

    fn time_row() -> Rect {
        Rect::from_min_size(pos2(23.0, 88.0), vec2(LANE_WIDTH, theme::CELL_HEIGHT))
    }

    /// A grid of `steps` columns in that row.
    fn grid(steps: u16) -> StepGrid {
        StepGrid::new(time_row(), steps)
    }

    /// At the default sixteen steps the cells are the theme's own — which is
    /// the arithmetic the window's width was chosen by, read back
    /// (`super::super::LANE_WIDTH`).
    #[test]
    fn sixteen_steps_are_drawn_at_the_themes_cell_size() {
        let grid = grid(16);

        assert_eq!(grid.row().width(), LANE_WIDTH);
        assert_eq!(grid.pitch(), theme::CELL_PITCH);
        assert_eq!(grid.gap(), theme::CELL_GAP);
        assert_eq!(grid.cell_width(), theme::CELL_WIDTH);

        // Cell 0 starts on the row's left edge and cell 15 ends on its
        // right one: nothing is left over at either end.
        assert_eq!(grid.cell_rect(0).min.x, grid.row().min.x);
        assert_eq!(grid.cell_rect(15).max.x, grid.row().max.x);
        for step in 0..16 {
            let cell = grid.cell_rect(step);
            assert_eq!(cell.width(), theme::CELL_WIDTH, "cell {step}");
            assert_eq!(cell.height(), grid.row().height(), "cell {step}");
            assert_eq!(cell.min.x, grid.row().min.x + f32::from(step) * theme::CELL_PITCH);
        }
    }

    /// A pattern longer than the window fits into the same width by scaling
    /// — the decision this module made (the module's §A long pattern is
    /// scaled) — and the figures come out as that section promises: 512
    /// points of pitch shared out, so 32 at sixteen steps, 16 at
    /// thirty-two and 8 at the longest pattern there is.
    #[test]
    fn a_longer_pattern_scales_into_the_same_width() {
        let span = LANE_WIDTH + theme::CELL_GAP;
        assert_eq!(span, STEPS_SHOWN * theme::CELL_PITCH, "the width is sixteen whole cells");

        for (steps, pitch) in [(1_u16, 512.0), (8, 64.0), (16, 32.0), (32, 16.0), (64, 8.0)] {
            let grid = grid(steps);

            assert_eq!(grid.pitch(), pitch, "{steps} steps");
            assert_eq!(grid.pitch(), span / f32::from(steps), "{steps} steps");
            assert_eq!(grid.gap(), theme::CELL_GAP, "{steps} steps keep the theme's gap");
            assert_eq!(grid.cell_width(), pitch - theme::CELL_GAP, "{steps} steps");

            // However many there are, they fill the row exactly: the first
            // on its left edge, the last on its right.
            assert_eq!(grid.cell_rect(0).min.x, grid.row().min.x, "{steps} steps");
            assert_eq!(grid.cell_rect(steps - 1).max.x, grid.row().max.x, "{steps} steps");
        }

        // The longest pattern's cells are still plainly cells.
        assert_eq!(grid(MAX_STEPS as u16).cell_width(), 6.0);
    }

    /// Columns tile the row with nothing in between: every point of the row
    /// is in exactly one column, which is what makes a press over the Time
    /// row always land on a step.
    #[test]
    fn the_columns_tile_the_row_with_no_dead_pixels() {
        for steps in [1_u16, 3, 16, 17, 32, 64] {
            let grid = grid(steps);

            let mut previous: Option<Rect> = None;
            for step in 0..steps {
                let column = grid.column_rect(step);
                assert!(grid.row().contains_rect(column), "{steps}: column {step}");
                assert!(
                    column.contains_rect(grid.cell_rect(step)),
                    "{steps}: column {step} does not hold its cell"
                );

                if let Some(before) = previous {
                    assert_eq!(column.min.x, before.max.x, "{steps}: gap before column {step}");
                }
                previous = Some(column);
            }

            assert_eq!(grid.column_rect(0).min.x, grid.row().min.x, "{steps}");
            assert_eq!(previous.expect("a column").max.x, grid.row().max.x, "{steps}");
        }
    }

    /// Pixel → step, at every scale: the left of a cell, its middle, the gap
    /// after it, and both ends of the row.
    #[test]
    fn a_position_maps_to_the_step_under_it() {
        for steps in [1_u16, 16, 17, 64] {
            let grid = grid(steps);
            let y = grid.row().center().y;

            for step in 0..steps {
                let column = grid.column_rect(step);
                let cell = grid.cell_rect(step);
                // The cell's left edge, its middle, and — where the pitch
                // leaves one — a point in the gap it owns.
                let mut probes = vec![cell.min.x, cell.center().x];
                if column.max.x > cell.max.x {
                    probes.push((cell.max.x + column.max.x) * 0.5);
                }

                for x in probes {
                    assert_eq!(grid.step_at(pos2(x, y)), Some(step), "{steps} steps at x {x}");
                }
            }

            // Both ends of the row, and just outside it.
            assert_eq!(grid.step_at(grid.row().left_center()), Some(0), "{steps}");
            assert_eq!(grid.step_at(pos2(grid.row().max.x - 0.5, y)), Some(steps - 1), "{steps}");
            assert_eq!(grid.step_at(pos2(grid.row().min.x - 1.0, y)), None, "{steps}");
            assert_eq!(grid.step_at(pos2(grid.row().max.x + 1.0, y)), None, "{steps}");
            assert_eq!(grid.step_at(pos2(grid.row().center().x, grid.row().min.y - 1.0)), None);
            assert_eq!(grid.step_at(pos2(grid.row().center().x, grid.row().max.y + 1.0)), None);

            // Dragged off either end, the column is clamped rather than lost
            // — what a stretch gesture needs ([`BlockDrag::target`]).
            assert_eq!(grid.step_at_x(grid.row().min.x - 1000.0), 0, "{steps}");
            assert_eq!(grid.step_at_x(grid.row().max.x + 1000.0), steps - 1, "{steps}");
            assert_eq!(grid.step_at_x(f32::NAN), 0, "{steps}");
        }
    }

    /// A block is drawn in one piece from its first cell to its last, and
    /// its pointer region is the columns under it — one gap wider at the
    /// end, so the space just past it resizes it rather than doing nothing.
    #[test]
    fn a_block_is_one_piece_over_the_steps_it_covers() {
        let grid = grid(16);

        let rect = grid.block_rect(2, 3);
        assert_eq!(rect.min.x, grid.cell_rect(2).min.x);
        assert_eq!(rect.max.x, grid.cell_rect(4).max.x);
        assert_eq!(rect.width(), 3.0 * theme::CELL_PITCH - theme::CELL_GAP);

        let columns = grid.block_columns(2, 3);
        assert_eq!(columns.min.x, rect.min.x);
        assert_eq!(columns.max.x, rect.max.x + theme::CELL_GAP);
        assert!(columns.contains_rect(rect));

        // A one-step block is one cell, and a zero-length one (which the
        // model never holds) is read as one step rather than as an empty
        // rect.
        assert_eq!(grid.block_rect(5, 1), grid.cell_rect(5));
        assert_eq!(grid.block_rect(5, 0), grid.cell_rect(5));

        // And one that would reach past the last column stops at it.
        assert_eq!(grid.block_rect(14, 9).max.x, grid.cell_rect(15).max.x);
    }

    /// Both edge handles are inside the block, never overlapping, and always
    /// leave a middle to grab — down to a one-step block on the narrowest
    /// grid there is.
    #[test]
    fn the_edge_handles_always_leave_a_middle() {
        for steps in [16_u16, 64] {
            let grid = grid(steps);

            for len in [1_u16, 2, 5] {
                let columns = grid.block_columns(1, len);
                let start = grid.edge_rect(1, len, BlockEdge::Start);
                let end = grid.edge_rect(1, len, BlockEdge::End);

                assert!(columns.contains_rect(start), "{steps}/{len}: start handle");
                assert!(columns.contains_rect(end), "{steps}/{len}: end handle");
                assert_eq!(start.min.x, columns.min.x, "{steps}/{len}");
                assert_eq!(end.max.x, columns.max.x, "{steps}/{len}");
                assert!(start.max.x < end.min.x, "{steps}/{len}: the handles meet");
                assert!(
                    start.width() <= columns.width() * theme::BLOCK_EDGE_SHARE + 0.001,
                    "{steps}/{len}: the start handle takes more than its share"
                );
                assert_eq!(start.width(), end.width(), "{steps}/{len}");
            }

            // On a wide block the handle is simply the theme's grab width.
            assert_eq!(grid.edge_rect(0, 8, BlockEdge::Start).width(), theme::BLOCK_EDGE_GRAB);
        }
    }

    /// The pattern used for hit-testing: a one-step Repeat on step 0, a
    /// four-step tape stop on steps 4 to 7, and empty steps either side.
    fn hit_pattern(length: u16) -> Pattern {
        let mut pattern = Pattern::new();
        pattern.set_length(length);
        pattern.set_blocks([
            Block::new(0, 1, Action::default()),
            Block::new(4, 4, Action::from_preset(SpeedPreset::TapeStop)),
        ]);

        assert_eq!(pattern.blocks().len(), 2, "the fixture's blocks both fit");
        pattern
    }

    /// Pixel → step, block or edge, at the default sixteen steps.
    #[test]
    fn hit_testing_tells_a_step_from_a_block_from_an_edge() {
        let pattern = hit_pattern(16);
        let grid = StepGrid::for_pattern(time_row(), &pattern);
        let blocks = pattern.blocks();
        let y = grid.row().center().y;
        let at = |x: f32| grid.hit(pos2(x, y), blocks);

        // An empty step is a step, whichever of them it is.
        for step in [1_u16, 2, 3, 8, 15] {
            let x = grid.cell_rect(step).center().x;
            assert_eq!(at(x), Some(GridHit::Step { step }), "step {step}");
        }

        // The four-step block: its middle is its body, and the body knows
        // which of its steps was grabbed.
        for step in 5..7 {
            let x = grid.cell_rect(step).center().x;
            assert_eq!(at(x), Some(GridHit::Body { index: 1, step }), "step {step}");
        }

        // Its two ends are its handles, and so is the gap just past it.
        let columns = grid.block_columns(4, 4);
        assert_eq!(
            at(columns.min.x + 1.0),
            Some(GridHit::Edge { index: 1, edge: BlockEdge::Start })
        );
        assert_eq!(
            at(columns.max.x - 1.0),
            Some(GridHit::Edge { index: 1, edge: BlockEdge::End })
        );
        assert_eq!(
            at(grid.cell_rect(7).max.x + theme::CELL_GAP * 0.5),
            Some(GridHit::Edge { index: 1, edge: BlockEdge::End }),
            "the gap past a block belongs to its end handle"
        );

        // A one-step block is all handle but the middle third, and the
        // middle third is still its body.
        assert_eq!(at(grid.cell_rect(0).center().x), Some(GridHit::Body { index: 0, step: 0 }));
        assert_eq!(
            at(grid.cell_rect(0).min.x),
            Some(GridHit::Edge { index: 0, edge: BlockEdge::Start })
        );

        // And nothing at all outside the row.
        assert_eq!(grid.hit(pos2(grid.row().min.x - 1.0, y), blocks), None);
        assert_eq!(grid.hit(pos2(grid.row().center().x, grid.row().max.y + 1.0), blocks), None);
    }

    /// The same lookups on a pattern longer than the window shows, which is
    /// the mapping this module decided (the scaled grid): the cells are a
    /// quarter of their usual width and every step is still reachable, with
    /// the blocks and their handles where the scaling puts them.
    #[test]
    fn hit_testing_follows_the_scaling_past_sixteen_steps() {
        let pattern = hit_pattern(MAX_STEPS as u16);
        let grid = StepGrid::for_pattern(time_row(), &pattern);
        let blocks = pattern.blocks();
        let y = grid.row().center().y;

        assert_eq!(grid.steps(), 64);
        assert_eq!(grid.cell_width(), 6.0, "a quarter of the theme's pitch, less the gap");

        // Every one of the sixty-four steps is reachable, each maps to
        // itself, and a step a block covers names that block — as its body
        // or as one of its edges, which is what the scaling decides.
        for step in 0..64_u16 {
            let hit = grid.hit(pos2(grid.cell_rect(step).center().x, y), blocks);
            match step {
                0 => assert_eq!(hit, Some(GridHit::Body { index: 0, step }), "step {step}"),
                4..=7 => match hit {
                    Some(GridHit::Body { index, step: grabbed }) => {
                        assert_eq!((index, grabbed), (1, step), "step {step}");
                    }
                    Some(GridHit::Edge { index, .. }) => assert_eq!(index, 1, "step {step}"),
                    other => panic!("step {step} of the block is {other:?}"),
                },
                _ => assert_eq!(hit, Some(GridHit::Step { step }), "step {step}"),
            }
        }

        // The handles of a four-step block at this scale: thirty-two points
        // of columns, so each handle is the theme's 7 rather than a third —
        // which is most of the first and last of its four 6-point cells, so
        // the two middle steps are what is left to move it by.
        let columns = grid.block_columns(4, 4);
        assert_eq!(grid.edge_rect(4, 4, BlockEdge::Start).width(), theme::BLOCK_EDGE_GRAB);
        for (step, edge) in [(4_u16, BlockEdge::Start), (7, BlockEdge::End)] {
            let x = grid.cell_rect(step).center().x;
            assert_eq!(grid.hit(pos2(x, y), blocks), Some(GridHit::Edge { index: 1, edge }));
        }
        for step in 5..7_u16 {
            let x = grid.cell_rect(step).center().x;
            let hit = Some(GridHit::Body { index: 1, step });
            assert_eq!(grid.hit(pos2(x, y), blocks), hit, "the middle is still grabbable");
        }
        assert_eq!(
            grid.hit(pos2(columns.min.x, y), blocks),
            Some(GridHit::Edge { index: 1, edge: BlockEdge::Start })
        );
        assert_eq!(
            grid.hit(pos2(columns.max.x - 0.5, y), blocks),
            Some(GridHit::Edge { index: 1, edge: BlockEdge::End })
        );

        // The one-step block on step 0 is 8 points of column at this scale,
        // so its handles are a third of it each ([`theme::BLOCK_EDGE_SHARE`])
        // and its middle third is still reachable — the share is what buys
        // that at a scale where the grab width alone would have taken the
        // whole cell.
        let share = grid.edge_rect(0, 1, BlockEdge::Start).width();
        assert!((share - 8.0 / 3.0).abs() < 0.001, "a third of an eight-point column, not {share}");
        assert_eq!(
            grid.hit(pos2(grid.cell_rect(0).center().x, y), blocks),
            Some(GridHit::Body { index: 0, step: 0 })
        );
    }

    /// Every action has a label, and it is the short one the row is meant
    /// to show.
    #[test]
    fn every_action_has_its_short_label() {
        let text = |action| block_label(action).to_string();

        // The five named ramps.
        assert_eq!(text(Action::from_preset(SpeedPreset::Reverse)), "Rev");
        assert_eq!(text(Action::from_preset(SpeedPreset::HalfSpeed)), "½×");
        assert_eq!(text(Action::from_preset(SpeedPreset::DoubleSpeed)), "2×");
        assert_eq!(text(Action::from_preset(SpeedPreset::TapeStop)), "Stop");
        assert_eq!(text(Action::from_preset(SpeedPreset::TapeStart)), "Start");

        // A loop at live speed carries its length.
        assert_eq!(text(Action::default()), "Rpt 1/16");
        for slice in NoteValue::ALL {
            assert_eq!(text(Action::repeat(slice)), format!("{REPEAT_PREFIX}{slice}"));
        }
        // At another speed it says so: one speed, or the ramp.
        let slow = Action { from: 0.5, to: 0.5, ..Action::repeat(NoteValue::Eighth) };
        assert_eq!(text(slow), "Rpt 1/8 ×0.5");
        let rising = Action { from: 1.0, to: 2.0, ..Action::repeat(NoteValue::Sixteenth) };
        assert_eq!(text(rising), "Rpt 1/16 1→2");

        // The source is marked in front: a fixed spot, or continuing.
        let before = Action::default().with_source(Source::Spot { step: 3, passes: 1 });
        assert_eq!(text(before), "‹Rpt 1/16");
        let stop = Action::from_preset(SpeedPreset::TapeStop).with_source(Source::Continue);
        assert_eq!(text(stop), "…Stop");

        // A free ramp, which is its two speeds: whole numbers without a
        // trailing `.0`, fractions as short as they read.
        assert_eq!(text(Action::ramp(0.5, 1.5)), "0.5→1.5");
        assert_eq!(text(Action::ramp(1.0, -2.0)), "1→-2");
        assert_eq!(text(Action::ramp(-1.25, 0.0)), "-1.25→0");
        assert_eq!(text(Action::ramp(0.333, 0.666)), "0.33→0.67");

        // Clamped before it is written, so a ramp out of range still reads
        // as a ramp inside it.
        assert_eq!(text(Action::ramp(9.0, -9.0)), "2→-2");
        // A NaN endpoint is the model's "normal speed", not this module's
        // guess ([`crate::actions::clamp_speed`]), so the label says what
        // the block will actually play.
        assert_eq!(text(Action { from: f32::NAN, to: 0.5, ..Action::ramp(1.0, 1.0) }), "1→0.5");
    }

    /// The Repeat labels are the note values' own spelling with one prefix,
    /// so the written-out table cannot drift from [`NoteValue`]'s `Display`.
    #[test]
    fn the_repeat_labels_mirror_the_note_values() {
        assert_eq!(REPEAT_LABELS.len(), NoteValue::ALL.len());

        for (index, slice) in NoteValue::ALL.into_iter().enumerate() {
            assert_eq!(REPEAT_LABELS[index], format!("{REPEAT_PREFIX}{slice}"), "slice {index}");
            assert_eq!(slice.to_u8() as usize, index, "the discriminant is the index");
        }
    }

    /// The seven kinds of block map one-to-one onto the seven palette
    /// entries, and a lying preset name is read as the free ramp it is.
    #[test]
    fn every_action_wears_its_own_colour() {
        assert_eq!(BlockKind::ALL.len(), theme::BLOCK_COLOURS.len());
        assert_eq!(PRESET_LABELS.len(), SpeedPreset::ALL.len());

        for (index, kind) in BlockKind::ALL.into_iter().enumerate() {
            assert_eq!(kind as usize, index, "the discriminant is the colour's index");
            assert_eq!(kind.colour(), theme::BLOCK_COLOURS[index], "kind {index}");
        }

        assert_eq!(BlockKind::of(Action::default()), BlockKind::Repeat);
        assert_eq!(BlockKind::of(Action::default()).colour(), theme::BLOCK_REPEAT);
        for (preset, kind) in [
            (SpeedPreset::Reverse, BlockKind::Reverse),
            (SpeedPreset::HalfSpeed, BlockKind::HalfSpeed),
            (SpeedPreset::DoubleSpeed, BlockKind::DoubleSpeed),
            (SpeedPreset::TapeStop, BlockKind::TapeStop),
            (SpeedPreset::TapeStart, BlockKind::TapeStart),
        ] {
            assert_eq!(BlockKind::of(Action::from_preset(preset)), kind, "{preset:?}");

            let label = block_label(Action::from_preset(preset)).to_string();
            assert_eq!(PRESET_LABELS[preset_index(preset)], label, "{preset:?}");
        }

        assert_eq!(BlockKind::of(Action::ramp(0.3, 0.7)), BlockKind::Ramp);
        let rising = Action { from: 1.0, to: 2.0, ..Action::default() };
        assert_eq!(BlockKind::of(rising), BlockKind::Repeat, "any loop is a repeat");
    }

    /// Alternate beats are shaded, not alternate steps — so the grid says
    /// where the beat is at every rate, triplets included.
    #[test]
    fn the_grid_shades_alternate_beats() {
        for (rate, per_beat) in [
            (Rate::Eighth, 2_u16),
            (Rate::EighthTriplet, 3),
            (Rate::Sixteenth, 4),
            (Rate::SixteenthTriplet, 6),
            (Rate::ThirtySecond, 8),
        ] {
            assert_eq!(steps_per_beat(rate), per_beat, "{rate}");
            assert_eq!(f64::from(per_beat) * rate.step_beats(), 1.0, "{rate} divides a beat");

            for step in 0..MAX_STEPS as u16 {
                let beat = step / per_beat;
                assert_eq!(beat_shaded(step, rate), beat % 2 == 1, "{rate} step {step}");
            }

            // The first beat is the unshaded one, so step 1 reads as the
            // start of the pattern.
            assert!(!beat_shaded(0, rate), "{rate}");
            assert_eq!(cell_colour(0, rate), theme::CELL, "{rate}");
            assert_eq!(cell_colour(per_beat, rate), theme::CELL_SHADED, "{rate}");
        }
    }

    /// A block's sentence names its label and its steps.
    #[test]
    fn a_block_names_its_label_and_steps() {
        let pattern = hit_pattern(16);
        let blocks = pattern.blocks();

        assert_eq!(block_span_text(&blocks[0]), "Rpt 1/16 on step 1.");
        assert_eq!(block_span_text(&blocks[1]), "Stop on steps 5 to 8.");
    }

    /// A looping block has a tick at every loop restart inside it, and none
    /// where they would crowd together or where the block does not loop.
    #[test]
    fn a_loop_has_a_tick_at_every_restart() {
        let mut pattern = Pattern::new();
        pattern.set_blocks([
            Block::new(0, 8, Action::repeat(NoteValue::Eighth)),
            Block::new(8, 4, Action::from_preset(SpeedPreset::TapeStop)),
            Block::new(12, 4, Action::repeat(NoteValue::SixtyFourth)),
        ]);
        let grid = StepGrid::new(Rect::from_min_size(pos2(0.0, 0.0), vec2(LANE_WIDTH, 40.0)), 16);
        let blocks = pattern.blocks();

        // A 1/8 loop is two 1/16 steps: three restarts inside eight steps.
        let ticks = repeat_ticks(&grid, &pattern, &blocks[0]);
        assert_eq!(ticks.len(), 3);
        let pitch = grid.pitch();
        for (lap, x) in ticks.iter().enumerate() {
            let expected = grid.step_x(2.0 * (lap as f32 + 1.0)) - grid.gap() * 0.5;
            assert!((x - expected).abs() < 1e-3, "lap {lap}: {x} vs {expected}");
        }
        assert!(ticks.windows(2).all(|pair| (pair[1] - pair[0] - 2.0 * pitch).abs() < 1e-3));

        assert!(repeat_ticks(&grid, &pattern, &blocks[1]).is_empty(), "no loop, no ticks");
        // A 1/64 loop is a quarter of a step: drawn at this pitch, but too
        // close together to draw on a 64-step grid of the same width.
        assert_eq!(repeat_ticks(&grid, &pattern, &blocks[2]).len(), 15);
        let fine = StepGrid::new(grid.row(), 64);
        assert!(fine.pitch() * 0.25 < theme::REPEAT_TICK_MIN_SPACING);
        assert!(repeat_ticks(&fine, &pattern, &blocks[2]).is_empty());
    }

    // ---- the add gesture ----

    /// A model with a committed starting point, as the editor leaves it
    /// after its first frame: sixteen default patterns and an empty history,
    /// so any step this gesture makes is its own.
    fn committed_model() -> StutterModel {
        let model = StutterModel::default();
        model.commit_edits();
        assert!(!model.can_undo() && !model.can_redo(), "nothing to step into yet");

        model
    }

    /// The blocks the audio thread would read for pattern `index` — what the
    /// gesture actually published, not what it was handed.
    fn published(model: &StutterModel, index: usize) -> Pattern {
        model.shared().pattern(index).expect("a pattern of the sixteen").clone()
    }

    /// A click on an empty step of the pattern being edited, as
    /// [`ui_time_row`] makes it: the rate read from this frame's snapshot,
    /// the step from the hit.
    fn click(model: &StutterModel, prefs: &PrefsCell, index: usize, step: u16) -> Option<usize> {
        let rate = published(model, index).rate();

        add_block_at(model, prefs, index, rate, step)
    }

    /// The shape of the whole gesture: one click, one one-step block of the
    /// default action on the step that was clicked, published through the
    /// writer API and undoable as one step.
    #[test]
    fn a_click_on_an_empty_step_adds_a_one_step_block() {
        let model = committed_model();
        let prefs = PrefsCell::default();

        assert_eq!(click(&model, &prefs, 0, 3), Some(0), "the row's first block");

        let pattern = published(&model, 0);
        assert_eq!(pattern.blocks().len(), 1);
        let block = pattern.blocks()[0];
        assert_eq!((block.start(), block.len()), (3, ADDED_LEN));
        assert_eq!(block.len(), 1, "a click adds the smallest block there is");
        assert_eq!(block.action(), Action::default(), "the classic stutter");

        // One click is one undo step, committed on the spot: there is no
        // half-added block to settle first.
        assert!(model.can_undo(), "the click is a step");
        assert!(model.undo());
        assert_eq!(published(&model, 0).blocks().len(), 0, "and the step is the whole click");
    }

    /// Clicks land where they are aimed, each in its own pattern and each
    /// its own undo step — and a second click on a step already covered adds
    /// nothing at all.
    #[test]
    fn each_click_adds_its_own_block_and_its_own_step() {
        let model = committed_model();
        let prefs = PrefsCell::default();

        for (index, step) in [(0_usize, 0_u16), (0, 15), (0, 7), (5, 2)] {
            assert!(click(&model, &prefs, index, step).is_some(), "pattern {index} step {step}");
        }

        // Sorted by start, as the model keeps them, so the index a click
        // returns is where the block landed on the row and not when it was
        // made.
        let steps: Vec<u16> = published(&model, 0).blocks().iter().map(Block::start).collect();
        assert_eq!(steps, [0, 7, 15]);
        assert_eq!(published(&model, 5).blocks().len(), 1, "the other pattern got its own");
        assert_eq!(published(&model, 1).blocks().len(), 0, "and nothing was added anywhere else");

        // A click on a step a block already covers is not a gesture: nothing
        // published, nothing to undo but the four clicks before it.
        assert_eq!(click(&model, &prefs, 0, 7), None, "the step is taken");
        assert_eq!(click(&model, &prefs, 0, 15), None);

        let mut back = 0;
        while model.undo() {
            back += 1;
        }
        assert_eq!(back, 4, "one step per click that added something");
        assert_eq!(published(&model, 0).blocks().len(), 0);
        assert_eq!(published(&model, 5).blocks().len(), 0);
    }

    /// A click that has nowhere to go leaves everything exactly as it was —
    /// the patterns, the history and the memory.
    #[test]
    fn a_click_with_nowhere_to_go_changes_nothing() {
        let model = committed_model();
        let prefs = PrefsCell::new(Prefs {
            keep_on_top: false,
            last_action: Action::from_preset(SpeedPreset::TapeStop),
            notes: None,
            latch: false,
        });
        let before = prefs.get();

        // A one-step pattern with its one step already covered: the row is
        // as full as a row can be. (A pattern with all sixty-four blocks on
        // it cannot have an empty step left to click — sixty-four blocks of
        // at least a step each cover every step there can be — so this is
        // the whole of the "nowhere to go" case.)
        assert_eq!(model.set_length(0, 1), Some(1));
        assert_eq!(click(&model, &prefs, 0, 0), Some(0));
        model.commit_edits();

        assert_eq!(click(&model, &prefs, 0, 0), None, "the step is covered");
        assert_eq!(click(&model, &prefs, 0, 9), None, "and a step past the end is that step");
        assert_eq!(
            add_block_at(&model, &prefs, 99, Rate::DEFAULT, 0),
            None,
            "a pattern index that is not one of the sixteen — which the window cannot show"
        );

        assert_eq!(published(&model, 0).blocks().len(), 1, "nothing was added");
        assert!(!model.can_redo(), "and no edit threw the future away");
        assert_eq!(prefs.get(), before, "and the memory is a click's to read, never to write");

        // The one block it did add comes back as the one step there was.
        assert!(model.undo());
        assert_eq!(published(&model, 0).blocks().len(), 0);
        assert!(!model.undo(), "the three refused clicks were not steps");
    }

    /// The memory: the action a click adds is the one last chosen, it is used
    /// by every click afterwards on every pattern, and it is not undone with
    /// the block it made.
    #[test]
    fn the_action_a_click_adds_is_the_last_one_chosen() {
        let model = committed_model();
        let prefs = PrefsCell::default();

        // Nothing chosen yet, so the first click is the classic stutter —
        // and adding it chooses nothing, so the memory is still untouched.
        assert_eq!(click(&model, &prefs, 0, 0), Some(0));
        assert_eq!(prefs.get(), Prefs::default(), "a click uses the memory, it does not set it");

        // Something chosen — which is what the block panel does — and
        // every click afterwards adds it, in this pattern and in the next.
        let reverse = Action::from_preset(SpeedPreset::Reverse);
        remember_action(&prefs, reverse);
        assert_eq!(click(&model, &prefs, 0, 4), Some(1));
        assert_eq!(click(&model, &prefs, 2, 4), Some(0));

        assert_eq!(published(&model, 0).blocks()[0].action(), Action::default(), "step 1 is older");
        assert_eq!(published(&model, 0).blocks()[1].action(), reverse);
        assert_eq!(published(&model, 2).blocks()[0].action(), reverse);
        assert_eq!(prefs.get().last_action, reverse, "and it is still what the next click adds");

        // The memory is not part of the undo history: it is what the hand
        // last did, not part of any pattern, so stepping back over the block
        // leaves the next click adding the same thing.
        while model.undo() {}
        assert_eq!(published(&model, 0).blocks().len(), 0);
        assert_eq!(prefs.get().last_action, reverse, "the memory is not undone with the block");
        assert_eq!(click(&model, &prefs, 0, 1), Some(0));
        assert_eq!(published(&model, 0).blocks()[0].action(), reverse);

        // And it is the keep-on-top pref's neighbour, not its owner: a
        // remembered action leaves the switch where it was.
        let pinned = PrefsCell::new(Prefs { keep_on_top: false, ..Prefs::default() });
        remember_action(&pinned, reverse);
        let expected = Prefs { keep_on_top: false, last_action: reverse, ..Prefs::default() };
        assert_eq!(pinned.get(), expected);
    }

    /// An untouched memory means "the classic stutter at *this* pattern's
    /// rate" ([`action_to_add`]): the reading that satisfies both the
    /// memory's "one-step slice" and a new block's "slice = one step of the
    /// pattern's rate".
    #[test]
    fn an_untouched_memory_follows_the_patterns_rate() {
        let fresh = Prefs::default();
        for rate in Rate::ALL {
            let action = action_to_add(fresh, rate);

            assert_eq!(action, Action::repeat_at(rate), "{rate}");
            assert_eq!(
                block_label(action).to_string(),
                format!("{REPEAT_PREFIX}{}", rate.note_value()),
                "{rate} is labelled with its own slice"
            );
        }

        // Through the gesture: a 1/32 pattern's first block loops one of its
        // steps and not two of them.
        let model = committed_model();
        let prefs = PrefsCell::default();
        assert!(model.set_rate(0, Rate::ThirtySecond));
        model.commit_edits();

        assert_eq!(click(&model, &prefs, 0, 0), Some(0));
        let added = published(&model, 0).blocks()[0].action();
        assert_eq!(added, Action::repeat_at(Rate::ThirtySecond));
        assert_eq!(block_label(added).to_string(), "Rpt 1/32");

        // And the slice it derived is *not* remembered, which is the whole
        // point of deriving it: the next pattern's clicks follow that
        // pattern's own rate rather than the rate of whichever pattern
        // happened to be clicked on first (`add_block_at`).
        assert_eq!(prefs.get(), Prefs::default(), "the memory is still untouched");
        assert_eq!(click(&model, &prefs, 1, 0), Some(0));
        assert_eq!(published(&model, 1).rate(), Rate::DEFAULT, "a pattern at the default rate");
        assert_eq!(
            published(&model, 1).blocks()[0].action(),
            Action::repeat_at(Rate::DEFAULT),
            "so its own rate is what its first block loops"
        );
    }

    /// A memory a hand put there is obeyed exactly, whatever it is — and one
    /// that arrived from a project file is sanitised first, so the block that
    /// is added is the block the hover text promised.
    #[test]
    fn a_remembered_action_is_obeyed_and_sanitised() {
        for action in [
            Action::default(),
            Action::repeat(NoteValue::Quarter).with_source(Source::Continue),
            Action::from_preset(SpeedPreset::TapeStart),
            Action::ramp(0.25, 1.75),
        ] {
            let prefs = PrefsCell::new(Prefs { last_action: action, ..Prefs::default() });
            let model = committed_model();

            // The default is the one that follows the rate, by design; every
            // other memory is used as it stands.
            let expected =
                if action == Action::default() { Action::repeat_at(Rate::DEFAULT) } else { action };
            assert_eq!(action_to_add(prefs.get(), Rate::DEFAULT), expected, "{action:?}");
            assert_eq!(click(&model, &prefs, 0, 2), Some(0), "{action:?}");
            assert_eq!(published(&model, 0).blocks()[0].action(), expected, "{action:?}");
        }

        // A ramp out of range and a preset label that disagrees with its own
        // endpoints are both things a cell handed a hostile value may hold:
        // the block gets the sanitised form, so what plays is what the hover
        // text promised.
        let lying = Action { from: 9.0, to: -9.0, ..Action::ramp(1.0, 1.0) };
        let prefs = PrefsCell::new(Prefs { last_action: lying, ..Prefs::default() });
        let model = committed_model();

        assert_eq!(action_to_add(prefs.get(), Rate::DEFAULT), Action::ramp(2.0, -2.0));
        assert_eq!(click(&model, &prefs, 0, 0), Some(0));
        assert_eq!(published(&model, 0).blocks()[0].action(), Action::ramp(2.0, -2.0));

        // And sanitised on the way *in* as well, so the one place that
        // writes the pref cannot store a value this would have to clean up
        // on every frame's hover text.
        remember_action(&prefs, lying);
        assert_eq!(prefs.get().last_action, Action::ramp(2.0, -2.0), "sanitised on the way in");
    }

    /// The memory is a *persisted* pref: it is saved with the project and
    /// comes back with it, which is what makes it survive closing the window
    /// rather than only the frame.
    #[test]
    fn the_memory_is_saved_with_the_project() {
        use crate::state::Snapshot;

        let model = committed_model();
        let prefs = PrefsCell::default();
        let eighth = NoteValue::Eighth;
        let chosen = Action::repeat(eighth).with_source(Source::Continue);
        remember_action(&prefs, chosen);
        assert_eq!(click(&model, &prefs, 0, 0), Some(0));

        let json = serde_json::to_string(&Snapshot::of(&model.data(), prefs.get()))
            .expect("the snapshot serialises");
        let loaded: Snapshot = serde_json::from_str(&json).expect("and reads back");

        assert_eq!(loaded.prefs().last_action, chosen, "the next click adds it again");

        // And a reopened window adds the same block as the one that was
        // closed, which is the whole point of persisting it.
        let reopened = StutterModel::new(loaded.data());
        let cell = PrefsCell::new(loaded.prefs());
        assert_eq!(click(&reopened, &cell, 0, 4), Some(1));
        assert_eq!(published(&reopened, 0).blocks()[1].action(), chosen);
    }

    /// A gesture publishes and reclaims, like every other write through the
    /// writer API: an open editor clicking around does not pile up retired
    /// snapshots.
    #[test]
    fn a_click_publishes_and_reclaims() {
        let model = committed_model();
        let prefs = PrefsCell::default();

        for step in 0..8 {
            assert!(click(&model, &prefs, 0, step).is_some());
        }
        assert!(model.undo());
        model.collect();

        assert_eq!(model.live_allocs(), 1, "only the published snapshot is left alive");
    }

    /// The length is clamped where it arrives, as every boundary in this
    /// plugin is: a zero-step grid (which would divide by zero) and one
    /// longer than any pattern both come out inside `1..=MAX_STEPS`.
    #[test]
    fn the_step_count_is_clamped() {
        assert_eq!(grid(0).steps(), 1);
        assert_eq!(grid(1).steps(), 1);
        assert_eq!(grid(u16::MAX).steps(), MAX_STEPS as u16);
        assert_eq!(StepGrid::for_pattern(time_row(), &Pattern::new()).steps(), 16);
    }

    /// A row that is not a rectangle — a host that squeezed the window, or
    /// egui's own `Rect::EVERYTHING` before a frame has been laid out — is
    /// squared up rather than carried into every cell.
    #[test]
    fn a_nonsense_row_is_squared_up() {
        let reversed = Rect::from_min_max(pos2(500.0, 400.0), pos2(100.0, 50.0));
        let nan = Rect::from_min_size(pos2(f32::NAN, f32::NAN), vec2(f32::NAN, f32::NAN));
        let blocks = [Block::new(0, 2, Action::default())];

        for row in [reversed, Rect::EVERYTHING, Rect::NOTHING, nan, time_row()] {
            for steps in [1_u16, 16, 64] {
                let grid = StepGrid::new(row, steps);
                let label = format!("{row:?} over {steps} steps");

                assert!(grid.row().width() >= 0.0, "{label}");
                assert!(grid.pitch().is_finite() && grid.pitch() >= 0.0, "{label}");
                assert!(grid.gap() >= 0.0 && grid.cell_width() >= 0.0, "{label}");

                for step in [0, steps / 2, steps - 1] {
                    for rect in [
                        grid.cell_rect(step),
                        grid.column_rect(step),
                        grid.block_rect(step, 2),
                        grid.block_columns(step, 2),
                        grid.edge_rect(step, 2, BlockEdge::Start),
                        grid.edge_rect(step, 2, BlockEdge::End),
                    ] {
                        assert!(rect.min.x.is_finite() && rect.max.x.is_finite(), "{label}");
                        assert!(rect.width() >= 0.0, "{label}");
                        assert!(grid.row().contains_rect(rect), "{label}: {rect:?}");
                    }
                }

                // And the lookups answer, with no panic and no step outside
                // the grid.
                for pos in [grid.row().min, grid.row().center(), grid.row().max] {
                    if let Some(step) = grid.step_at(pos) {
                        assert!(step < grid.steps(), "{label}");
                    }
                    assert!(grid.step_at_x(pos.x) < grid.steps(), "{label}");
                    assert_eq!(grid.hit(pos, &blocks).is_some(), grid.step_at(pos).is_some());
                }
            }
        }
    }

    // ---- the drag gestures ----

    /// The middle of step `step`'s column, which is where a press on that
    /// step lands.
    fn at(grid: &StepGrid, step: u16) -> Pos2 {
        grid.column_rect(step).center()
    }

    /// The row's blocks as `(start, len)` pairs — the whole of what a drag is
    /// about, without the actions it never touches.
    fn spans(pattern: &Pattern) -> Vec<(u16, u16)> {
        pattern.blocks().iter().map(|block| (block.start(), block.len())).collect()
    }

    /// A committed model whose pattern 0 holds [`hit_pattern`]'s row: a
    /// one-step block on step 1 and a four-step tape stop on steps 5 to 8 of
    /// a sixteen-step pattern, with nothing to undo yet.
    fn dragging_model() -> StutterModel {
        let model = StutterModel::new(StutterData::from_patterns([hit_pattern(16)]));
        model.commit_edits();
        assert_eq!(spans(&published(&model, 0)), [(0, 1), (4, 4)]);
        assert!(!model.can_undo(), "the row it starts with is not a step to undo");

        model
    }

    /// One whole drag over the Time row of pattern `index`, as
    /// [`ui_time_row`] makes it: a press at `press`, a frame at each position
    /// of `path`, and the hand let go. Hands back the row the model published.
    ///
    /// Every frame reads the pattern back out of the model, as the editor
    /// does — one snapshot per frame — so the clamping is measured against
    /// the neighbours as this drag has left them, not as they were.
    fn drag_along(model: &StutterModel, index: usize, press: Pos2, path: &[Pos2]) -> Pattern {
        let start = published(model, index);
        let grid = StepGrid::for_pattern(time_row(), &start);
        let mut drag = TimeRowDrag::default();

        assert!(drag.press(&grid, start.blocks(), press), "the press landed on a block");
        for &pos in path {
            let now = published(model, index);
            drag.drag_to(model, &grid, index, now.blocks(), pos);
        }
        assert!(drag.settle(model), "the gesture closed");
        assert_eq!(drag, TimeRowDrag::Idle, "and left the row idle");

        published(model, index)
    }

    /// What a press takes hold of: the block under it and which gesture, or
    /// nothing at all — and whichever it is, it holds for the whole press.
    #[test]
    fn a_press_takes_hold_of_a_block_and_a_press_on_an_empty_step_does_not() {
        let pattern = hit_pattern(16);
        let blocks = pattern.blocks();
        let grid = StepGrid::for_pattern(time_row(), &pattern);

        let mut drag = TimeRowDrag::default();
        assert!(!drag.open() && drag.held().is_none(), "nothing is pressed");

        // The middle of the four-step block: a move, holding the step it was
        // grabbed by (its third, so `grab` is 2).
        assert!(drag.press(&grid, blocks, at(&grid, 6)));
        let held = drag.held().expect("the block under the press");
        assert_eq!((held.index, held.from), (1, blocks[1]));
        assert_eq!(held.kind(), DragKind::Move { grab: 2 });
        assert!(drag.open());

        // And a press while one is open changes nothing: a hand that slid
        // onto another block does not let go of this one to pick that up.
        assert!(drag.press(&grid, blocks, at(&grid, 0)));
        assert_eq!(drag.held().map(|held| held.index), Some(1));

        // An end of a block is a stretch of that end instead.
        for edge in [BlockEdge::Start, BlockEdge::End] {
            let mut drag = TimeRowDrag::default();
            assert!(drag.press(&grid, blocks, grid.edge_rect(4, 4, edge).center()), "{edge:?}");
            assert_eq!(drag.held().map(|held| held.kind()), Some(DragKind::Stretch { edge }));
            assert_eq!(drag.held().map(|held| held.from), Some(blocks[1]));
        }

        // A press on a step with nothing over it, or off the row entirely,
        // holds nothing — and stays that way for the rest of the press, even
        // once the pointer has slid onto a block.
        for press in [at(&grid, 2), pos2(grid.row().center().x, grid.row().min.y - 20.0)] {
            let mut drag = TimeRowDrag::default();
            assert!(!drag.press(&grid, blocks, press));
            assert_eq!(drag, TimeRowDrag::Missed);
            assert!(drag.open(), "a press is open; it just has no block");
            assert!(!drag.press(&grid, blocks, at(&grid, 6)), "slid onto a block mid-press");
            assert_eq!(drag.held(), None);
        }

        // Neither is a hit that belongs to another row (the editor takes both
        // from one snapshot, so this is the hostile case).
        assert_eq!(BlockDrag::begin(GridHit::Body { index: 9, step: 0 }, blocks), None);
        assert_eq!(BlockDrag::begin(GridHit::Step { step: 3 }, blocks), None);
    }

    /// A move: the grabbed step stays under the pointer, and the block stops
    /// against its neighbour and against both ends of the pattern.
    #[test]
    fn a_moved_block_keeps_the_grabbed_step_under_the_pointer() {
        let pattern = hit_pattern(16);
        let blocks = pattern.blocks();
        let grid = StepGrid::for_pattern(time_row(), &pattern);
        let moving =
            BlockDrag::begin(GridHit::Body { index: 1, step: 6 }, blocks).expect("the block");

        assert_eq!(moving.target(&grid, blocks, at(&grid, 6)), Some((4, 4)), "the press alone");
        assert_eq!(moving.target(&grid, blocks, at(&grid, 8)), Some((6, 4)), "two steps along");
        assert_eq!(moving.target(&grid, blocks, at(&grid, 2)), Some((1, 4)), "and back the other");

        // Against the block before it, which ends on step 1, and against the
        // end of the pattern, where all four of its steps still have to fit.
        assert_eq!(moving.target(&grid, blocks, at(&grid, 0)), Some((1, 4)));
        assert_eq!(moving.target(&grid, blocks, at(&grid, 15)), Some((12, 4)));
        // Dragged off either end of the row, it keeps pulling to the column
        // it can reach rather than stopping where the row does.
        assert_eq!(moving.target(&grid, blocks, pos2(1e9, 0.0)), Some((12, 4)));
        assert_eq!(moving.target(&grid, blocks, pos2(-1e9, 0.0)), Some((1, 4)));
        // A pointer that is not a number reads as the first column
        // ([`StepGrid::step_at_x`]), as nonsense does everywhere here.
        assert_eq!(moving.target(&grid, blocks, pos2(f32::NAN, 0.0)), Some((1, 4)));

        // The same block grabbed by its first step is the same gesture with
        // nothing held back: the pointer's own column is the new start.
        let by_the_nose =
            BlockDrag::begin(GridHit::Body { index: 1, step: 4 }, blocks).expect("the block");
        assert_eq!(by_the_nose.kind(), DragKind::Move { grab: 0 });
        assert_eq!(by_the_nose.target(&grid, blocks, at(&grid, 6)), Some((6, 4)));
        // A one-step block can only ever be grabbed by its own step.
        let short = BlockDrag::begin(GridHit::Body { index: 0, step: 0 }, blocks).expect("block");
        assert_eq!(short.kind(), DragKind::Move { grab: 0 });
        assert_eq!(short.target(&grid, blocks, at(&grid, 9)), Some((3, 1)), "up to its neighbour");
    }

    /// A stretch of the right edge: the end follows the pointer's column, the
    /// start stays, and the block never shrinks past the one step it is at
    /// least.
    #[test]
    fn a_stretched_end_follows_the_pointer_and_never_shrinks_past_one_step() {
        let pattern = hit_pattern(16);
        let blocks = pattern.blocks();
        let grid = StepGrid::for_pattern(time_row(), &pattern);
        let press = grid.edge_rect(4, 4, BlockEdge::End).center();
        let stretching = BlockDrag::begin(GridHit::Edge { index: 1, edge: BlockEdge::End }, blocks)
            .expect("the block");

        assert_eq!(stretching.target(&grid, blocks, press), Some((4, 4)), "the press alone");
        assert_eq!(stretching.target(&grid, blocks, at(&grid, 9)), Some((4, 6)));
        assert_eq!(stretching.target(&grid, blocks, at(&grid, 15)), Some((4, 12)), "to the end");
        assert_eq!(stretching.target(&grid, blocks, pos2(1e9, 0.0)), Some((4, 12)));
        // Pulled back over its own start, and past it: one step, still there.
        assert_eq!(stretching.target(&grid, blocks, at(&grid, 4)), Some((4, 1)));
        assert_eq!(stretching.target(&grid, blocks, at(&grid, 0)), Some((4, 1)));
        assert_eq!(stretching.target(&grid, blocks, pos2(f32::NAN, 0.0)), Some((4, 1)));

        // And the one-step block's right edge stops against the block after
        // it rather than growing through it.
        let first = BlockDrag::begin(GridHit::Edge { index: 0, edge: BlockEdge::End }, blocks)
            .expect("the block");
        assert_eq!(first.target(&grid, blocks, at(&grid, 2)), Some((0, 3)));
        assert_eq!(first.target(&grid, blocks, at(&grid, 9)), Some((0, 4)), "up to its neighbour");
    }

    /// A stretch of the left edge: the start follows the pointer, the far end
    /// stays where the press found it, and neither passes the other.
    #[test]
    fn a_stretched_start_keeps_the_far_end_where_it_was() {
        let pattern = hit_pattern(16);
        let blocks = pattern.blocks();
        let grid = StepGrid::for_pattern(time_row(), &pattern);
        let press = grid.edge_rect(4, 4, BlockEdge::Start).center();
        let stretching =
            BlockDrag::begin(GridHit::Edge { index: 1, edge: BlockEdge::Start }, blocks)
                .expect("the block");

        assert_eq!(stretching.target(&grid, blocks, press), Some((4, 4)), "the press alone");
        assert_eq!(stretching.target(&grid, blocks, at(&grid, 2)), Some((2, 6)), "end still at 8");
        // Against the block before it, and never past its own far end.
        assert_eq!(stretching.target(&grid, blocks, at(&grid, 0)), Some((1, 7)));
        assert_eq!(stretching.target(&grid, blocks, pos2(-1e9, 0.0)), Some((1, 7)));
        assert_eq!(stretching.target(&grid, blocks, at(&grid, 7)), Some((7, 1)));
        assert_eq!(stretching.target(&grid, blocks, at(&grid, 15)), Some((7, 1)));
        assert_eq!(stretching.target(&grid, blocks, pos2(1e9, 0.0)), Some((7, 1)));
    }

    /// A block with no room either side cannot be moved at all — the clamp
    /// is what a crowded row is made of — but its ends still shrink it.
    #[test]
    fn a_block_boxed_in_by_its_neighbours_cannot_be_moved() {
        let mut pattern = Pattern::new();
        pattern.set_length(8);
        pattern.set_blocks([
            Block::new(0, 4, Action::default()),
            Block::new(4, 4, Action::from_preset(SpeedPreset::Reverse)),
        ]);
        let blocks = pattern.blocks();
        let grid = StepGrid::for_pattern(time_row(), &pattern);

        let boxed_in =
            BlockDrag::begin(GridHit::Body { index: 1, step: 5 }, blocks).expect("the block");
        for step in 0..grid.steps() {
            let label = format!("dragged to step {step}");
            assert_eq!(boxed_in.target(&grid, blocks, at(&grid, step)), Some((4, 4)), "{label}");
        }

        // Its two ends are the one thing left to do with it.
        let start = BlockDrag::begin(GridHit::Edge { index: 1, edge: BlockEdge::Start }, blocks)
            .expect("the block");
        assert_eq!(start.target(&grid, blocks, at(&grid, 6)), Some((6, 2)));
        let end = BlockDrag::begin(GridHit::Edge { index: 1, edge: BlockEdge::End }, blocks)
            .expect("the block");
        assert_eq!(end.target(&grid, blocks, at(&grid, 5)), Some((4, 2)));
    }

    /// The whole of a move through the writer API: every frame of the drag
    /// writes, the row the audio thread reads follows the pointer, and the
    /// lot is **one** undo step.
    #[test]
    fn a_whole_drag_is_one_undo_step() {
        let model = dragging_model();
        let grid = StepGrid::for_pattern(time_row(), &published(&model, 0));

        // Picked up by its third step and dragged four steps along, through
        // every column in between.
        let path: Vec<Pos2> = (7..=10).map(|step| at(&grid, step)).collect();
        let row = drag_along(&model, 0, at(&grid, 6), &path);

        assert_eq!(spans(&row), [(0, 1), (8, 4)], "four steps along, its length kept");
        assert_eq!(row.blocks()[1].action(), Action::from_preset(SpeedPreset::TapeStop));
        assert!(model.can_undo() && !model.can_redo());

        assert!(model.undo(), "the whole drag comes back at once");
        assert_eq!(spans(&published(&model, 0)), [(0, 1), (4, 4)]);
        assert!(!model.can_undo(), "one gesture, one step");
        assert!(model.redo());
        assert_eq!(spans(&published(&model, 0)), [(0, 1), (8, 4)]);
    }

    /// A drag that wandered off and came back leaves no step behind it: the
    /// patterns are what the history compares, not the writes.
    #[test]
    fn a_drag_that_ended_where_it_started_is_no_step_at_all() {
        let model = dragging_model();
        let grid = StepGrid::for_pattern(time_row(), &published(&model, 0));

        let path = [at(&grid, 9), at(&grid, 15), at(&grid, 1), at(&grid, 6)];
        let row = drag_along(&model, 0, at(&grid, 6), &path);

        assert_eq!(spans(&row), [(0, 1), (4, 4)], "back where it came from");
        assert!(!model.can_undo(), "so there is nothing to undo");
    }

    /// Both ends of one block, dragged through the model: each writes through
    /// the method that means it, each keeps the other end, and each is its
    /// own undo step.
    #[test]
    fn a_stretch_gesture_writes_through_the_edge_it_has_hold_of() {
        let model = dragging_model();
        let grid = StepGrid::for_pattern(time_row(), &published(&model, 0));

        // The right edge out to the end of the pattern.
        let press = grid.edge_rect(4, 4, BlockEdge::End).center();
        let row = drag_along(&model, 0, press, &[at(&grid, 11), at(&grid, 15)]);
        assert_eq!(spans(&row), [(0, 1), (4, 12)]);

        // Then the left edge back as far as it goes, which keeps the right
        // one where the first gesture left it and stops against the block
        // before it.
        let press = grid.edge_rect(4, 12, BlockEdge::Start).center();
        let row = drag_along(&model, 0, press, &[at(&grid, 2), at(&grid, 0)]);
        assert_eq!(spans(&row), [(0, 1), (1, 15)]);
        assert_eq!(row.blocks()[1].action(), Action::from_preset(SpeedPreset::TapeStop));

        assert!(model.undo() && model.undo(), "two gestures, two steps");
        assert_eq!(spans(&published(&model, 0)), [(0, 1), (4, 4)]);
        assert!(!model.can_undo());
    }

    /// The clamping, measured on what the model published rather than on what
    /// the gesture asked for: a block dragged hard either way stops where its
    /// neighbour or the pattern does.
    #[test]
    fn a_dragged_block_stops_against_its_neighbour_and_the_rows_ends() {
        let model = dragging_model();
        let grid = StepGrid::for_pattern(time_row(), &published(&model, 0));
        let off_the_left = pos2(grid.row().min.x - 500.0, grid.row().center().y);
        let off_the_right = pos2(grid.row().max.x + 500.0, grid.row().center().y);

        let row = drag_along(&model, 0, at(&grid, 5), &[at(&grid, 2), off_the_left]);
        assert_eq!(spans(&row), [(0, 1), (1, 4)], "the step after its neighbour's end");

        let row = drag_along(&model, 0, at(&grid, 2), &[off_the_right]);
        assert_eq!(spans(&row), [(0, 1), (12, 4)], "the last four steps of the pattern");

        assert!(model.undo() && model.undo());
        assert_eq!(spans(&published(&model, 0)), [(0, 1), (4, 4)]);
    }

    /// A frame whose pointer has not left its column writes nothing: the
    /// model's edit counter is what says so.
    #[test]
    fn a_drag_writes_only_when_the_span_it_asks_for_changes() {
        let model = dragging_model();
        let row = published(&model, 0);
        let grid = StepGrid::for_pattern(time_row(), &row);
        let mut drag = TimeRowDrag::default();

        assert!(drag.press(&grid, row.blocks(), at(&grid, 6)));
        let before = model.edit_count();

        // Thirty frames with the pointer in the cell it was pressed in.
        for _ in 0..30 {
            let now = published(&model, 0);
            assert_eq!(drag.drag_to(&model, &grid, 0, now.blocks(), at(&grid, 6)), Some((4, 4)));
        }
        assert_eq!(model.edit_count(), before, "a pointer that stayed put published nothing");

        // One column along is one write, and holding it there is no more.
        let now = published(&model, 0);
        assert_eq!(drag.drag_to(&model, &grid, 0, now.blocks(), at(&grid, 7)), Some((5, 4)));
        assert_eq!(model.edit_count(), before + 1);
        for _ in 0..5 {
            let now = published(&model, 0);
            assert_eq!(drag.drag_to(&model, &grid, 0, now.blocks(), at(&grid, 7)), Some((5, 4)));
        }
        assert_eq!(model.edit_count(), before + 1, "one write for one step of travel");
        assert!(drag.settle(&model));
        assert!(model.can_undo());
    }

    /// A press that landed on no block writes nothing, commits nothing and
    /// leaves no step behind it — the add gesture has already committed its
    /// own click ([`add_block_at`]).
    #[test]
    fn a_press_on_an_empty_step_drags_nothing() {
        let model = dragging_model();
        let row = published(&model, 0);
        let grid = StepGrid::for_pattern(time_row(), &row);
        let before = model.edit_count();
        let mut drag = TimeRowDrag::default();

        assert!(!drag.press(&grid, row.blocks(), at(&grid, 2)));
        assert_eq!(drag.drag_to(&model, &grid, 0, row.blocks(), at(&grid, 12)), None);
        assert!(!drag.settle(&model), "there was no drag to close");
        assert_eq!(drag, TimeRowDrag::Idle);

        assert_eq!(model.edit_count(), before, "nothing was written at all");
        assert_eq!(spans(&published(&model, 0)), [(0, 1), (4, 4)]);
        assert!(!model.can_undo());
    }

    /// A drag resolved against a row it did not begin on does nothing rather
    /// than guessing: an undo taken while the hand is still down,
    /// or a project the host loaded under it, replaces the row.
    #[test]
    fn a_drag_against_a_row_it_did_not_begin_on_does_nothing() {
        let model = dragging_model();
        let row = published(&model, 0);
        let grid = StepGrid::for_pattern(time_row(), &row);
        let moving =
            BlockDrag::begin(GridHit::Body { index: 1, step: 5 }, row.blocks()).expect("block");
        let to = at(&grid, 9);

        // The block it had hold of is gone.
        assert_eq!(moving.target(&grid, &[], to), None);
        assert_eq!(moving.apply(&model, &grid, 0, &[], to), None);
        // A row whose blocks overlap where this one sits leaves no honest gap
        // to clamp against either.
        let crossed = [Block::new(0, 6, Action::default()), Block::new(4, 4, Action::default())];
        assert_eq!(moving.target(&grid, &crossed, to), None);
        // And an index that is not one of the sixteen patterns publishes
        // nothing, so there is still no step to undo.
        assert_eq!(moving.apply(&model, &grid, PATTERN_COUNT, row.blocks(), to), None);
        model.commit_edits();
        assert_eq!(spans(&published(&model, 0)), [(0, 1), (4, 4)]);
        assert!(!model.can_undo());
    }

    /// The pointer's own shape over the row: what a press would start, or
    /// what the hand is already doing.
    #[test]
    fn the_pointer_shape_says_what_a_press_would_do() {
        let pattern = hit_pattern(16);
        let blocks = pattern.blocks();
        let grid = StepGrid::for_pattern(time_row(), &pattern);
        let idle = TimeRowDrag::default();

        assert_eq!(drag_cursor(idle, Some(GridHit::Step { step: 2 })), None, "nothing to grab");
        assert_eq!(drag_cursor(idle, None), None, "and nowhere near the row");
        assert_eq!(
            drag_cursor(idle, Some(GridHit::Body { index: 1, step: 5 })),
            Some(egui::CursorIcon::Grab)
        );
        for edge in [BlockEdge::Start, BlockEdge::End] {
            assert_eq!(
                drag_cursor(idle, Some(GridHit::Edge { index: 1, edge })),
                Some(egui::CursorIcon::ResizeHorizontal),
                "{edge:?}"
            );
        }

        // While a drag is under way it says what the hand is doing, wherever
        // the pointer has wandered off to.
        let mut moving = TimeRowDrag::default();
        assert!(moving.press(&grid, blocks, at(&grid, 5)));
        assert_eq!(drag_cursor(moving, None), Some(egui::CursorIcon::Grabbing));
        assert_eq!(
            drag_cursor(moving, Some(GridHit::Step { step: 0 })),
            Some(egui::CursorIcon::Grabbing)
        );

        let mut stretching = TimeRowDrag::default();
        let press = grid.edge_rect(4, 4, BlockEdge::End).center();
        assert!(stretching.press(&grid, blocks, press));
        assert_eq!(drag_cursor(stretching, None), Some(egui::CursorIcon::ResizeHorizontal));

        // A press that landed on no block takes no shape at all, even once
        // the pointer has slid over one: that press cannot drag it.
        let mut missed = TimeRowDrag::default();
        assert!(!missed.press(&grid, blocks, at(&grid, 2)));
        assert_eq!(drag_cursor(missed, Some(GridHit::Body { index: 1, step: 5 })), None);
    }
}
