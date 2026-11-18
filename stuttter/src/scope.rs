//! The waveform summaries the audio thread writes and the GUI draws.
//!
//! One [`Scope`] per plugin instance, shared between the two threads as an
//! `Arc` — the audio thread writes it a frame at a time from inside the
//! engine's walk ([`Scope::write`]), the editor reads whatever is there when
//! it paints ([`Scope::input`], [`Scope::output`], [`Scope::age`],
//! [`Scope::playhead`]). Every field is an atomic and every access is
//! relaxed, so neither side allocates, locks or blocks: the audio thread may
//! not and the editor has nothing to gain from making it.
//!
//! # What is in it
//!
//! [`COLUMNS`] columns of the **input** and [`COLUMNS`] of the **output**,
//! each column the lowest and highest sample that landed in it ([`Column`]),
//! indexed by where in the pattern cycle it sits — `(b mod P) / P × 512`,
//! which arrives here as a phase in `0.0..1.0` and becomes a column index
//! through [`Scope::column_of`]. Columns are **overwritten in place** as the
//! playhead moves: a column the playhead has just entered holds that one
//! frame, and widens as the frames after it arrive.
//!
//! Which cycle each column was last written in is a [`Scope::generation`],
//! counted up whenever the playhead wraps back to an earlier column, so the
//! GUI can dim a column that is a pattern-length old ([`Scope::age`]) — the
//! columns ahead of the playhead are the previous cycle's and are *meant* to
//! still be there, because a waveform that erased itself as it went would
//! leave three quarters of the view empty.
//!
//! And the playhead itself ([`Scope::playhead`]), as the same phase, for the
//! cursor the GUI draws over those columns.
//!
//! # What is not in it
//!
//! **The cycle the summaries are of**: a phase is all the audio thread
//! sends, so the columns say *where in the pattern* a shape sat and never
//! how long that pattern was. That is the GUI's own business — it draws the
//! view the width of the grid above it, whatever rate and length the pattern
//! has.
//!
//! **Anything from a frame with no pattern over it.** The summaries are "of
//! the current pattern", and a frame the engine plays live — pattern Off, a
//! stopped transport, a transport that cannot be followed — has no position
//! in a cycle to index by. The engine simply does not call [`Scope::write`]
//! for those, so the view holds still and ages rather than filling with a
//! waveform drawn against a grid it has no relation to.
//!
//! # The one-writer contract
//!
//! [`Scope::write`] reads columns back to widen them, so it is a
//! read-modify-write and there must be exactly **one** writer: the audio
//! thread of the one engine this scope belongs to
//! ([`crate::engine::Engine::new`] takes it and nothing else writes it). The
//! reader side is any number of threads.
//!
//! A reader can see half an update — the `min` of one frame beside the `max`
//! of the frame before it, or a column whose generation has moved on while
//! its samples have not — because the atomics are independent and relaxed.
//! That is a pixel's worth of waveform a frame early or late, which is why
//! relaxed is enough; nothing downstream of a read may *depend* on two of
//! these numbers agreeing.

use std::sync::atomic::{AtomicU32, Ordering::Relaxed};

/// How many columns one pattern cycle is summarised into, for the input and
/// for the output alike — a fixed array of 512.
///
/// Fixed, because the audio thread may not allocate and the GUI may not make
/// it wait: a column is a slot that is overwritten, never a sample that is
/// pushed. At a 16-step 1/16 pattern and 120 BPM one column is about 4 ms,
/// which is finer than the view is ever drawn.
pub const COLUMNS: usize = 512;

/// The generation a scope starts at, and the one its first cycle is written
/// in.
///
/// One rather than zero so that a column **nothing has written yet** is
/// already a generation old ([`Scope::age`] of 1): an untouched scope draws
/// as dimmed silence instead of as a current reading of a signal that has
/// not arrived.
pub const FIRST_GENERATION: u32 = 1;

/// The [`Scope::column_of`] marker for "the playhead has not entered a
/// column yet", which is a stored column index of one more than itself.
const NO_COLUMN: u32 = 0;

/// What one column of a summary says: the lowest and the highest sample that
/// landed in it.
///
/// Both channels of the frame go into the same pair — the view is one
/// waveform per source, not a stereo pair — so `min` is the lower of the two
/// channels' lowest and `max` the higher of their highest. A column the
/// playhead has only just entered holds that single frame, so `min` and
/// `max` can be equal, and are both `0.0` where nothing has been written
/// ([`Column::SILENT`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Column {
    /// The lowest sample in the column, over both channels.
    pub min: f32,
    /// The highest sample in the column, over both channels.
    pub max: f32,
}

impl Column {
    /// Nothing in the column: a flat line at zero, which is what an
    /// untouched scope is full of and what [`Scope::clear`] puts back.
    pub const SILENT: Column = Column { min: 0.0, max: 0.0 };

    /// The column one stereo frame alone makes.
    ///
    /// Not widened from [`Column::SILENT`] but taken from the frame itself,
    /// so a column of audio sitting entirely above zero says so rather than
    /// being stretched down to the centre line.
    fn of(left: f32, right: f32) -> Column {
        let (left, right) = (finite(left), finite(right));

        Column { min: left.min(right), max: left.max(right) }
    }

    /// This column widened to hold one more stereo frame.
    fn widened(self, left: f32, right: f32) -> Column {
        let frame = Column::of(left, right);

        Column { min: self.min.min(frame.min), max: self.max.max(frame.max) }
    }
}

/// `sample` if it is a real number, and silence if it is not.
///
/// A NaN or an infinity in the audio is a bug upstream rather than anything
/// this plugin can draw — and `f32::min`/`f32::max` would carry a NaN into
/// the column it met, where it would sit in the summary for a whole cycle.
/// So the summary says silence for that frame, which is the one honest
/// answer a waveform has for a sample that is not a number.
fn finite(sample: f32) -> f32 {
    match sample.is_finite() {
        true => sample,
        false => 0.0,
    }
}

/// One column as it is stored: the two halves of a [`Column`] as
/// `f32::to_bits` patterns in relaxed `AtomicU32`s.
struct AtomicColumn {
    /// [`Column::min`]'s bits.
    min: AtomicU32,
    /// [`Column::max`]'s bits.
    max: AtomicU32,
}

impl AtomicColumn {
    /// [`Column::SILENT`], whose two halves are both all-zero bits.
    ///
    /// A `const fn` rather than a `const`, because a `const` holding
    /// atomics is a value every use *copies* — which is exactly what the
    /// array initialisers below want, but is a trap everywhere else, and
    /// clippy is right to refuse to tell the two apart.
    const fn silent() -> AtomicColumn {
        AtomicColumn { min: AtomicU32::new(0), max: AtomicU32::new(0) }
    }

    /// The column as it stands. Two independent relaxed loads — see the
    /// module's one-writer contract for what that means for a reader.
    fn load(&self) -> Column {
        Column {
            min: f32::from_bits(self.min.load(Relaxed)),
            max: f32::from_bits(self.max.load(Relaxed)),
        }
    }

    /// Writes `column` over whatever is there.
    fn store(&self, column: Column) {
        self.min.store(column.min.to_bits(), Relaxed);
        self.max.store(column.max.to_bits(), Relaxed);
    }
}

/// The summaries themselves: input and output columns, the generation each
/// column was written in, and the playhead.
///
/// Built once per plugin instance and shared as an `Arc` — see the module
/// documentation for the whole of what it holds and who may write it.
pub struct Scope {
    /// The input's columns, indexed by position in the pattern cycle.
    input: [AtomicColumn; COLUMNS],
    /// The output's columns, the same way: what the plugin played, where the
    /// input alongside it is what it was handed.
    output: [AtomicColumn; COLUMNS],
    /// The generation each column was last written in, or `0` for a column
    /// nothing has written ([`FIRST_GENERATION`]).
    written: [AtomicU32; COLUMNS],
    /// The cycle being written now, counted up from [`FIRST_GENERATION`]
    /// every time the playhead wraps.
    generation: AtomicU32,
    /// Where the playhead is, as a phase in `0.0..=1.0` — an `f32`'s bits,
    /// like the columns.
    playhead: AtomicU32,
    /// The column the playhead is in, **plus one**, or [`NO_COLUMN`] before
    /// the first frame is written.
    ///
    /// The writer's own state, kept here rather than in the engine so that
    /// a scope is one thing to clear and one thing to reason about: it is
    /// what tells a frame that continues the column it is in (widen) from
    /// one that has moved on (overwrite), and what the wrap that counts the
    /// generation up is noticed against.
    cursor: AtomicU32,
}

impl Scope {
    /// An empty scope: silence in every column, nothing written, the
    /// playhead at the start of the cycle and the first cycle about to be
    /// written as [`FIRST_GENERATION`].
    ///
    /// The one place a scope is made. It is ~10 kB of atomics, so it is made
    /// where allocation is allowed — with the plugin instance, never in
    /// `process()`.
    pub const fn new() -> Scope {
        Scope {
            input: [const { AtomicColumn::silent() }; COLUMNS],
            output: [const { AtomicColumn::silent() }; COLUMNS],
            written: [const { AtomicU32::new(0) }; COLUMNS],
            generation: AtomicU32::new(FIRST_GENERATION),
            playhead: AtomicU32::new(0),
            cursor: AtomicU32::new(NO_COLUMN),
        }
    }

    /// Puts the scope back as [`Scope::new`] left it.
    ///
    /// What a discontinuity in the audio asks for: the engine's `reset()`
    /// forgets the recorded past, and the summaries of that past are no
    /// longer summaries of anything. Allocation-free and lock-free — a few
    /// thousand relaxed stores — because it is called from the audio thread
    /// as well as after `activate()`.
    pub fn clear(&self) {
        for (input, output) in self.input.iter().zip(self.output.iter()) {
            input.store(Column::SILENT);
            output.store(Column::SILENT);
        }
        for written in &self.written {
            written.store(0, Relaxed);
        }
        self.generation.store(FIRST_GENERATION, Relaxed);
        self.playhead.store(0, Relaxed);
        self.cursor.store(NO_COLUMN, Relaxed);
    }

    /// Summarises one frame at `phase` of the pattern cycle: the frame the
    /// plugin was handed (`input`) and the frame it played (`output`), each
    /// as `(left, right)`.
    ///
    /// The audio thread's only call here, made once per output frame from
    /// inside the engine's walk — and only for a frame that **has** a phase,
    /// which is the module's "what is not in it".
    ///
    /// Three things happen to the column the phase names
    /// ([`Scope::column_of`]):
    ///
    /// - A frame that **continues** the column the last one was in widens it
    ///   to hold this frame too, which is how a column comes to span the
    ///   lowest and highest of the hundred-odd frames in it.
    /// - A frame that has **moved on** to another column overwrites that
    ///   column with itself alone and stamps it with the generation being
    ///   written — "overwritten in place as the playhead moves", so what was
    ///   there from the previous cycle goes rather than being merged into a
    ///   shape neither cycle had.
    /// - A move **backwards** over the columns is the playhead wrapping, and
    ///   counts the generation up first. Backwards covers a loop jump and a
    ///   relocation as well as the end of a cycle, which is right: either
    ///   way the columns ahead are from an older pass and the GUI should dim
    ///   them.
    ///
    /// Allocates nothing, locks nothing, waits for nothing: a handful of
    /// relaxed loads and stores on fixed arrays. Any `phase` is safe,
    /// including a NaN — it is folded into the cycle on the way in, so the
    /// index is always a column that exists.
    pub fn write(&self, phase: f64, input: (f32, f32), output: (f32, f32)) {
        let phase = clamp_phase(phase);
        let column = Scope::column_of(phase);
        self.playhead.store((phase as f32).to_bits(), Relaxed);

        // The cursor stores the column plus one, so that `NO_COLUMN` can
        // mean "no column at all" without a second atomic to say so.
        // The cursor stores the column plus one, so that `NO_COLUMN` can
        // mean "no column at all" without a second atomic to say so.
        let marker = column as u32 + 1;
        let previous = self.cursor.swap(marker, Relaxed);
        let continuing = previous == marker;
        if !continuing {
            // A move to an earlier column is the playhead wrapping. The
            // comparison is this way round because [`NO_COLUMN`] is below
            // every marker, and the first frame ever written is not a wrap.
            if previous > marker {
                let generation = self.generation.load(Relaxed).wrapping_add(1);
                self.generation.store(generation, Relaxed);
            }
            self.written[column].store(self.generation.load(Relaxed), Relaxed);
        }

        let (summary_in, summary_out) = match continuing {
            true => (
                self.input[column].load().widened(input.0, input.1),
                self.output[column].load().widened(output.0, output.1),
            ),
            false => (Column::of(input.0, input.1), Column::of(output.0, output.1)),
        };

        self.input[column].store(summary_in);
        self.output[column].store(summary_out);
    }

    /// Which column a phase in the cycle belongs to: `phase × COLUMNS`,
    /// rounded down and folded into the columns that exist.
    ///
    /// The `(b mod P) / P × 512` once the division is done. A phase
    /// of exactly `1.0` is the last column rather than one past the end —
    /// the cycle is half-open so no real phase reaches it, and the fold is
    /// what makes that true of a hostile number too. NaN-safe in the
    /// project's `max`-then-`min` order, which sends a NaN to column 0.
    pub fn column_of(phase: f64) -> usize {
        let scaled = (clamp_phase(phase) * COLUMNS as f64).floor();

        // The clamp above bounds this either side, so the cast cannot
        // saturate; the `min` is the guard for the one rounding left, a
        // phase a hair below 1.0 scaling to `COLUMNS` exactly.
        (scaled as usize).min(COLUMNS - 1)
    }

    /// The input's summary for `column`, or [`Column::SILENT`] for a column
    /// outside the array.
    pub fn input(&self, column: usize) -> Column {
        self.input.get(column).map_or(Column::SILENT, AtomicColumn::load)
    }

    /// The output's summary for `column`, or [`Column::SILENT`] for a column
    /// outside the array.
    pub fn output(&self, column: usize) -> Column {
        self.output.get(column).map_or(Column::SILENT, AtomicColumn::load)
    }

    /// The cycle being written now, counted up from [`FIRST_GENERATION`] on
    /// every wrap.
    pub fn generation(&self) -> u32 {
        self.generation.load(Relaxed)
    }

    /// The generation `column` was last written in — `0` for a column
    /// nothing has written, and for one outside the array.
    pub fn written_in(&self, column: usize) -> u32 {
        self.written.get(column).map_or(0, |written| written.load(Relaxed))
    }

    /// How many cycles old `column`'s summary is: `0` for a column written
    /// in the cycle being written now, `1` for one left over from the cycle
    /// before it, and more for a column the playhead has skipped over.
    ///
    /// This is the window's dimming rule: columns a generation old draw
    /// dimmer. A `wrapping_sub`, so a scope left open for the four billion
    /// cycles it takes to overflow the counter keeps answering rather than
    /// panicking in a debug build.
    pub fn age(&self, column: usize) -> u32 {
        self.generation().wrapping_sub(self.written_in(column))
    }

    /// Where the playhead is, as a phase in `0.0..=1.0` of the pattern
    /// cycle — the GUI's cursor.
    ///
    /// `0.0` before anything is written, which is the start of the cycle:
    /// the cursor has to be somewhere, and a scope with nothing in it draws
    /// nothing for it to be wrong about.
    pub fn playhead(&self) -> f32 {
        f32::from_bits(self.playhead.load(Relaxed))
    }

    /// The column the playhead is in, or `None` before the first frame is
    /// written — the column whose summary is still being widened, and so the
    /// one the GUI may want to draw as "filling".
    pub fn column(&self) -> Option<usize> {
        match self.cursor.load(Relaxed) {
            NO_COLUMN => None,
            marker => Some(marker as usize - 1),
        }
    }
}

/// An empty scope, which is [`Scope::new`].
impl Default for Scope {
    fn default() -> Scope {
        Scope::new()
    }
}

/// `phase` folded into one cycle: `0.0..=1.0`, with a NaN landing at the
/// start of it (`max` before `min`, the project's NaN-safe order).
///
/// The engine hands over a phase that is already in range by construction;
/// this is the boundary clamp that makes every index below safe anyway, for
/// the same reason the rest of the plugin clamps what the host says.
fn clamp_phase(phase: f64) -> f64 {
    // The lint wants `clamp`; the order here *is* the guard, see above.
    #[allow(clippy::manual_clamp)]
    phase.max(0.0).min(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A phase a few frames into column `column` of 512 — far enough in
    /// that the rounding of the division cannot put it in a neighbour.
    fn phase_in(column: usize) -> f64 {
        (column as f64 + 0.5) / COLUMNS as f64
    }

    /// A fresh scope reads as dimmed silence everywhere: no samples, no
    /// column written, the playhead at the start, and every column already a
    /// generation old so that the GUI draws none of it as current.
    #[test]
    fn a_fresh_scope_is_silent_and_a_generation_old() {
        let scope = Scope::new();

        assert_eq!(scope.generation(), FIRST_GENERATION);
        assert_eq!(scope.playhead(), 0.0);
        assert_eq!(scope.column(), None);
        for column in 0..COLUMNS {
            assert_eq!(scope.input(column), Column::SILENT, "input column {column}");
            assert_eq!(scope.output(column), Column::SILENT, "output column {column}");
            assert_eq!(scope.written_in(column), 0, "column {column}");
            assert_eq!(scope.age(column), 1, "column {column}");
        }
    }

    /// The phase-to-column mapping, including everything a hostile number
    /// can be: the cycle is half-open, so the whole of `0.0..1.0` lands
    /// inside the array and nothing lands outside it.
    #[test]
    fn the_cycle_maps_onto_the_columns_and_nothing_lands_outside_them() {
        assert_eq!(Scope::column_of(0.0), 0);
        assert_eq!(Scope::column_of(0.5), COLUMNS / 2);
        assert_eq!(Scope::column_of(1.0 - f64::EPSILON), COLUMNS - 1);
        for column in 0..COLUMNS {
            assert_eq!(Scope::column_of(phase_in(column)), column, "column {column}");
        }

        // And outside the cycle, which the engine never sends but a clamp is
        // cheaper than trusting it: the ends, and the three numbers that are
        // not numbers.
        assert_eq!(Scope::column_of(1.0), COLUMNS - 1);
        assert_eq!(Scope::column_of(2.0), COLUMNS - 1);
        assert_eq!(Scope::column_of(f64::INFINITY), COLUMNS - 1);
        assert_eq!(Scope::column_of(-0.5), 0);
        assert_eq!(Scope::column_of(f64::NEG_INFINITY), 0);
        assert_eq!(Scope::column_of(f64::NAN), 0);
    }

    /// A column holds the lowest and highest sample that landed in it, over
    /// both channels and over every frame of it — the input's and the
    /// output's separately, because telling what came in from what went out
    /// is the whole point of the view.
    #[test]
    fn a_column_spans_every_frame_that_lands_in_it() {
        let scope = Scope::new();
        let phase = phase_in(100);

        scope.write(phase, (0.5, -0.25), (1.0, 1.0));
        assert_eq!(scope.input(100), Column { min: -0.25, max: 0.5 });
        assert_eq!(scope.output(100), Column { min: 1.0, max: 1.0 });

        // Widened by the frames after it, and only in the directions they
        // reach: the input's low end and the output's high end stand.
        scope.write(phase, (0.75, 0.0), (0.5, 0.25));
        scope.write(phase, (0.1, -0.1), (0.5, -2.0));
        assert_eq!(scope.input(100), Column { min: -0.25, max: 0.75 });
        assert_eq!(scope.output(100), Column { min: -2.0, max: 1.0 });

        // One column and no other: the neighbours are untouched.
        assert_eq!(scope.input(99), Column::SILENT);
        assert_eq!(scope.input(101), Column::SILENT);
        assert_eq!(scope.column(), Some(100));
        assert_eq!(scope.playhead(), phase as f32);
    }

    /// A column entirely above zero says so: a fresh column is the frame
    /// itself, not silence widened by it, so a DC-offset signal does not
    /// draw as one stretched to the centre line.
    #[test]
    fn a_fresh_column_is_the_frame_and_not_silence_widened() {
        let scope = Scope::new();

        scope.write(phase_in(7), (0.25, 0.75), (0.5, 0.5));

        assert_eq!(scope.input(7), Column { min: 0.25, max: 0.75 });
        assert_eq!(scope.output(7), Column { min: 0.5, max: 0.5 });
    }

    /// Moving on overwrites the column moved into rather than widening what
    /// the last cycle left there — "overwritten in place as the playhead
    /// moves". A view that merged the two cycles would draw a shape neither
    /// of them had.
    #[test]
    fn moving_on_overwrites_the_column_rather_than_widening_it() {
        let scope = Scope::new();

        // A loud first cycle through three columns...
        for column in 0..3 {
            scope.write(phase_in(column), (-1.0, 1.0), (-1.0, 1.0));
        }
        assert_eq!(scope.input(1), Column { min: -1.0, max: 1.0 });

        // ...and a quiet second one over the top of it.
        for column in 0..3 {
            scope.write(phase_in(column), (-0.1, 0.1), (-0.1, 0.1));
        }

        for column in 0..3 {
            assert_eq!(scope.input(column), Column { min: -0.1, max: 0.1 }, "column {column}");
            assert_eq!(scope.output(column), Column { min: -0.1, max: 0.1 }, "column {column}");
        }
    }

    /// The generation counts up when the playhead wraps, and every column
    /// carries the generation it was written in — so the columns ahead of the
    /// playhead read as a cycle old and the ones behind it as current, which
    /// is the dimming rule.
    #[test]
    fn the_generation_bumps_when_the_playhead_wraps() {
        let scope = Scope::new();

        // One whole cycle: every column written in the first generation, and
        // none of them old.
        for column in 0..COLUMNS {
            scope.write(phase_in(column), (-0.5, 0.5), (-0.5, 0.5));
        }
        assert_eq!(scope.generation(), FIRST_GENERATION);
        for column in 0..COLUMNS {
            assert_eq!(scope.written_in(column), FIRST_GENERATION, "column {column}");
            assert_eq!(scope.age(column), 0, "column {column}");
        }

        // A quarter of the way into the next one. The wrap is the one write
        // that counts the generation up — not the 127 after it.
        for column in 0..COLUMNS / 4 {
            scope.write(phase_in(column), (-0.5, 0.5), (-0.5, 0.5));
        }
        assert_eq!(scope.generation(), FIRST_GENERATION + 1);

        for column in 0..COLUMNS {
            let fresh = column < COLUMNS / 4;
            assert_eq!(scope.age(column), u32::from(!fresh), "column {column}");
        }
    }

    /// A column the playhead skips over keeps the generation it was last
    /// written in, so it goes on ageing: at a tempo and pattern length where
    /// a column is less than a frame wide, the view dims the gaps instead of
    /// claiming them as current.
    #[test]
    fn a_skipped_column_keeps_ageing() {
        let scope = Scope::new();

        // Every fourth column, three cycles over.
        for cycle in 0..3 {
            for column in (0..COLUMNS).step_by(4) {
                scope.write(phase_in(column), (cycle as f32, cycle as f32), (0.0, 0.0));
            }
        }

        assert_eq!(scope.generation(), FIRST_GENERATION + 2);
        assert_eq!(scope.age(0), 0, "a column the playhead wrote this cycle");
        assert_eq!(scope.age(4), 0);
        assert_eq!(scope.age(1), 3, "a column nothing has ever written");
        assert_eq!(scope.input(0), Column { min: 2.0, max: 2.0 }, "the newest cycle's sample");
        assert_eq!(scope.input(1), Column::SILENT);
    }

    /// A jump backwards is a wrap: a loop that sends the song to an earlier
    /// place in the pattern starts another pass over the columns, and the
    /// ones ahead of where it lands are from an older one.
    #[test]
    fn a_jump_backwards_counts_as_a_wrap() {
        let scope = Scope::new();

        for column in 0..100 {
            scope.write(phase_in(column), (0.0, 0.0), (0.0, 0.0));
        }
        assert_eq!(scope.generation(), FIRST_GENERATION);

        scope.write(phase_in(50), (0.0, 0.0), (0.0, 0.0));

        assert_eq!(scope.generation(), FIRST_GENERATION + 1);
        assert_eq!(scope.age(50), 0, "the column the jump landed in");
        assert_eq!(scope.age(51), 1, "the columns it left behind are a cycle old");
        assert_eq!(scope.column(), Some(50));
    }

    /// The playhead is the phase it was last written at, which is what the
    /// GUI draws the cursor from — and it holds still while no frame has a
    /// phase, because that is what a transport that is not moving looks
    /// like.
    #[test]
    fn the_playhead_follows_the_phase() {
        let scope = Scope::new();

        for phase in [0.0, 0.25, 0.5, 0.75, 0.999] {
            scope.write(phase, (0.0, 0.0), (0.0, 0.0));
            assert_eq!(scope.playhead(), phase as f32, "phase {phase}");
            assert_eq!(scope.column(), Some(Scope::column_of(phase)));
        }

        // And a phase from outside the cycle is folded into it rather than
        // sending the cursor off the view.
        scope.write(f64::NAN, (0.0, 0.0), (0.0, 0.0));
        assert_eq!(scope.playhead(), 0.0);
        scope.write(7.5, (0.0, 0.0), (0.0, 0.0));
        assert_eq!(scope.playhead(), 1.0);
    }

    /// A sample that is not a number never reaches a summary: it would sit
    /// in the column for a whole cycle and draw as a hole in the waveform,
    /// where silence is at least true of the one frame it came from.
    #[test]
    fn a_sample_that_is_not_a_number_summarises_as_silence() {
        let scope = Scope::new();

        scope.write(phase_in(3), (f32::NAN, f32::INFINITY), (f32::NEG_INFINITY, f32::NAN));
        assert_eq!(scope.input(3), Column::SILENT);
        assert_eq!(scope.output(3), Column::SILENT);

        // And it does not poison a column that holds real audio either.
        scope.write(phase_in(4), (-0.5, 0.5), (-0.5, 0.5));
        scope.write(phase_in(4), (f32::NAN, f32::NAN), (f32::NAN, f32::NAN));
        assert_eq!(scope.input(4), Column { min: -0.5, max: 0.5 });
        assert_eq!(scope.output(4), Column { min: -0.5, max: 0.5 });
    }

    /// A column outside the array answers as silence and as old, rather than
    /// panicking: the GUI's own column count is the one thing here it could
    /// get wrong, and a view drawn one column too wide is not worth a crash
    /// in the host.
    #[test]
    fn a_column_outside_the_array_reads_as_silent_and_old() {
        let scope = Scope::new();
        scope.write(phase_in(0), (1.0, 1.0), (1.0, 1.0));

        assert_eq!(scope.input(COLUMNS), Column::SILENT);
        assert_eq!(scope.output(COLUMNS), Column::SILENT);
        assert_eq!(scope.written_in(COLUMNS), 0);
        assert_eq!(scope.age(COLUMNS), FIRST_GENERATION);
    }

    /// Clearing puts the scope back as it was made — what the engine's reset
    /// asks for, because the summaries of a past that has been forgotten are
    /// summaries of nothing.
    #[test]
    fn clearing_puts_it_back_as_new() {
        let scope = Scope::new();
        for column in 0..COLUMNS {
            scope.write(phase_in(column), (-1.0, 1.0), (-1.0, 1.0));
        }
        scope.write(phase_in(0), (-1.0, 1.0), (-1.0, 1.0));
        assert_eq!(scope.generation(), FIRST_GENERATION + 1);

        scope.clear();

        assert_eq!(scope.generation(), FIRST_GENERATION);
        assert_eq!(scope.playhead(), 0.0);
        assert_eq!(scope.column(), None);
        for column in 0..COLUMNS {
            assert_eq!(scope.input(column), Column::SILENT, "input column {column}");
            assert_eq!(scope.output(column), Column::SILENT, "output column {column}");
            assert_eq!(scope.age(column), 1, "column {column}");
        }

        // And it writes from scratch afterwards, rather than carrying a
        // cursor into a column the clear emptied.
        scope.write(phase_in(0), (0.25, 0.25), (0.25, 0.25));
        assert_eq!(scope.input(0), Column { min: 0.25, max: 0.25 });
        assert_eq!(scope.generation(), FIRST_GENERATION, "the first write after a clear wrapped");
    }
}
