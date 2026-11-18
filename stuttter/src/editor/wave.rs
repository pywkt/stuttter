//! The waveform view: what came in drawn dim, what went out drawn bright in
//! the colour of the block that played it, over one pattern cycle.
//!
//! The band under the two rows is the one part of this window that does not
//! show the pattern being edited — it shows the *audio*. The audio thread
//! summarises every frame it plays into [`Scope`]'s
//! [`COLUMNS`] min/max columns, indexed by where in the pattern cycle the
//! frame sat, and this module is the other end of that: the arithmetic that
//! turns a column index into a rectangle on the band ([`WaveView`]), the rule
//! that says how brightly it is drawn ([`Shade`]), and a thin piece of egui
//! glue that paints the two summaries and the playhead over them
//! ([`ui_waveform`]).
//!
//! The split is the one every band in this window has: the mapping and the
//! dimming rule are pure and tested on a machine with no display, and the
//! painting is `Verify: manual`.
//!
//! # Columns are phase, not steps
//!
//! The Time row and the Volume row are laid out over the *pattern's* steps
//! ([`super::grid::StepGrid`]); this band is laid out over the *cycle's*
//! phase, in the fixed [`COLUMNS`] the scope keeps whatever the pattern's
//! length is. Both span the same width, so the two mappings agree on where
//! the middle of the cycle is and a step, its level and its sound are in the
//! same place down the window — but this one has no gaps between its columns
//! and no cells: a waveform is continuous and is drawn that way
//! ([`WaveView::column_rect`] tiles the band).
//!
//! [`WaveView::phase_x`] and [`WaveView::column_x`] are therefore the same
//! arithmetic, and **exactly** so: a phase is multiplied by [`COLUMNS`] on
//! the way in, which is a power of two and so loses nothing, and the result
//! goes through the same division as a column index. That is what makes the
//! playhead land inside the column the scope says it is writing instead of a
//! rounding either side of it.
//!
//! # The span ahead of the playhead is the last time round
//!
//! Columns are overwritten in place as the playhead crosses them, so the
//! columns *ahead* of it still hold the previous cycle — deliberately, since
//! a view that erased itself as it went would be three quarters empty
//! (`crate::scope`). What tells the two apart is the scope's generation
//! counter: a column written in the cycle being written now is fresh, and
//! anything else is a cycle or more old and is drawn at
//! [`theme::WAVE_STALE_FACTOR`] of its colour ([`Shade`]). That is the
//! generation-based dimming, and it is why the view reads as a waveform being
//! drawn over its own last pass rather than as one long smear.
//!
//! # When the view is not of the pattern on screen
//!
//! The scope's columns are indexed by the cycle of the pattern that is
//! **playing**, and are written only while one is (`crate::scope` §What is
//! not in it: no writes for Off, for a stopped transport, or for a pattern
//! the model does not hold). The band, though, sits under a grid showing the
//! pattern being **edited**, and with the follow toggle off those are
//! routinely not the same one. What to draw then is decided here, as
//! [`Subject`]:
//!
//! - **The pattern on screen is the one playing** ([`Subject::OnScreen`]):
//!   the view is a picture of the grid above it, so the generation rule
//!   above is the whole of the brightness and the playhead marks where in
//!   that grid the song is.
//! - **Another pattern is playing, or none at all** ([`Subject::Elsewhere`]):
//!   the columns are of a cycle this window is not showing, so **every** one
//!   of them is drawn dim, however fresh it is. The view keeps moving if
//!   something is playing — it is still true about the audio — but it stops
//!   claiming to be about the grid, and the hover text says which pattern it
//!   is of and how to look at it.
//!
//! Dimmed and not hidden, and never cleared: a band that emptied itself would
//! read as a plugin that had stopped working, and this view is meant to show
//! the signal even when there is nothing in the pattern to see. The playhead is
//! the one thing that *is* withheld, and only when nothing is playing at all
//! ([`playhead`]) — a line marking "now" over a picture nothing is writing is
//! the one thing in the band that would be false rather than merely not
//! current.
//!
//! A stopped transport needs no case of its own: nice-plug's `reset()` is
//! called across a stop and a locate, and the engine clears the scope with
//! the recorded past it summarised (`Engine::reset`), so the band goes to
//! dimmed silence by itself. A host that does not reset leaves the last pass
//! held, which is what it was.
//!
//! # The sources over it
//!
//! Over the audio the band shows where the blocks read from
//! ([`SourceWash`]): the selected block's source washed full height and
//! captioned, the block under the pointer on the Time row washed more
//! lightly, and every other block's as a thin stripe along the foot. The
//! selected wash is the band's one control — dragging it moves the block's
//! fixed spot and dragging its right edge sets the loop — and a click on a
//! stripe selects that block. This module only reports those gestures
//! ([`WashInput`]); it needs no model and no writer, and the editor turns
//! them into edits.

use egui::{Color32, Rect, pos2};

use super::grid::BlockKind;
use super::layout::sane;
use super::theme;
use crate::pattern::Pattern;
use crate::scope::{COLUMNS, Column, Scope};

/// The sample value the view draws at the top and bottom of the band.
///
/// Full scale, and a clamp rather than a scaling: the input may well arrive
/// hotter than this (nothing stops a host sending ±4), and a view that
/// rescaled itself to whatever the loudest column happened to be would make
/// a quiet passage look like a loud one and would move under the eye every
/// cycle. A column past full scale is flat against the edge of the band,
/// which is what a clipping meter does and what the ear is hearing anyway.
pub const FULL_SCALE: f32 = 1.0;

/// How brightly one column is drawn: the generation-based dimming, as the
/// two cases it has.
///
/// The whole of the dimming rule, named so that it can be tested without a
/// colour: which of the two a column is, is [`Shade::of`], and what that does
/// to a colour is [`Shade::applied`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shade {
    /// Written in the cycle being written now: drawn in its own colour.
    Fresh,
    /// Left over from a cycle before this one — the span ahead of the
    /// playhead, and every column of a scope nothing has written yet: drawn
    /// at [`theme::WAVE_STALE_FACTOR`].
    Stale,
}

impl Shade {
    /// Which shade a column of [`Scope::age`] `age` is drawn in: `0` is this
    /// cycle and anything else is an older one.
    ///
    /// Two shades and not a fade per generation. A column the playhead
    /// *skipped* — a loop jump, a relocation — is several generations old and
    /// is still the most recent thing that was ever drawn there, so fading it
    /// towards nothing would empty the view out exactly when the song moved
    /// about the most. "Now" and "not now" is the distinction the eye needs,
    /// and it is the one this rule draws.
    pub fn of(age: u32) -> Shade {
        match age {
            0 => Shade::Fresh,
            _ => Shade::Stale,
        }
    }

    /// `colour` as this shade draws it: untouched for [`Shade::Fresh`], and
    /// taken down to [`theme::WAVE_STALE_FACTOR`] of itself for
    /// [`Shade::Stale`] — dimmer without being tinted and without being gone
    /// ([`theme::dim`]).
    pub fn applied(self, colour: Color32) -> Color32 {
        match self {
            Shade::Fresh => colour,
            Shade::Stale => theme::dim(colour, theme::WAVE_STALE_FACTOR),
        }
    }
}

/// What the view is a picture of: the pattern the window is showing, or a
/// cycle it is not (the module's §When the view is not of the pattern on
/// screen).
///
/// The second half of the brightness rule, and the half that is this
/// window's rather than the scope's: [`Shade`] says how old a column is,
/// this says whether the column is *ours*, and a column that is not is drawn
/// dim however fresh it is ([`Subject::shade`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Subject {
    /// The pattern being edited is the one playing, so the columns are of the
    /// grid drawn above them: the generation rule is the whole of the
    /// brightness.
    OnScreen,
    /// Another pattern is playing, or the `pattern` parameter is Off: the
    /// columns are of a cycle this window is not showing, and the whole view
    /// is dimmed to say so.
    Elsewhere,
}

impl Subject {
    /// Which of the two this frame is: `playing` is the pattern the host's
    /// parameter names (`None` for Off — `super::top_bar::playing_index`) and
    /// `edited` the one this window is showing.
    pub fn of(playing: Option<usize>, edited: usize) -> Subject {
        match playing {
            Some(playing) if playing == edited => Subject::OnScreen,
            _ => Subject::Elsewhere,
        }
    }

    /// How a column of [`Scope::age`] `age` is drawn: the generation rule
    /// when the view is of the pattern on screen, and [`Shade::Stale`]
    /// throughout when it is not.
    pub fn shade(self, age: u32) -> Shade {
        match self {
            Subject::OnScreen => Shade::of(age),
            Subject::Elsewhere => Shade::Stale,
        }
    }
}

/// The step of a `length`-step pattern that column `column` sits in: the one
/// under the column's middle, worked in whole numbers so that a step boundary
/// falls where the grid above draws it.
pub fn step_of_column(column: usize, length: u16) -> u16 {
    let length = usize::from(length.max(1));
    let step = (2 * column.min(COLUMNS - 1) + 1) * length / (2 * COLUMNS);
    // Below `length`, which is a `u16`, so the conversion cannot fail.
    u16::try_from(step.min(length - 1)).unwrap_or(0)
}

/// The colour the output is drawn in at column `column`: the colour of the
/// block playing there, so what the effect did reads apart from the grey
/// input behind it, and [`theme::WAVE_OUTPUT`]'s grey where no block is and
/// the input went through untouched.
///
/// `playing` is the pattern whose cycle the scope holds — the one playing,
/// which is not always the one on screen — and `None` (nothing playing)
/// draws the grey throughout, since there is no pattern to say which block
/// the columns came from.
pub fn output_colour(playing: Option<&Pattern>, column: usize) -> Color32 {
    let Some(pattern) = playing else { return theme::WAVE_OUTPUT };
    let step = step_of_column(column, pattern.length());

    match pattern.block_at(step) {
        Some(index) => BlockKind::of(pattern.blocks()[index].action()).colour(),
        None => theme::WAVE_OUTPUT,
    }
}

/// One frame's waveform view: the band it is drawn in, and the arithmetic
/// that puts a column of the scope into it.
///
/// Cheap to build and small to copy, like [`super::grid::StepGrid`], and it
/// holds no scope — a frame builds one from the band's rectangle and asks it
/// for rectangles, and the summaries are read straight out of the scope by
/// whoever is painting.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WaveView {
    view: Rect,
}

impl WaveView {
    /// The view filling `band`.
    ///
    /// `band` is squared up the way [`super::grid::StepGrid::new`] squares up
    /// a row and for the same reason: a reversed, infinite or NaN rectangle
    /// produces an empty view that contains no point and draws nothing,
    /// never rectangles with an infinity in them. A host that ignored the
    /// window's fixed-size hint is the only way to get one
    /// ([`super::layout::EditorLayout`] empties this band first).
    pub fn new(band: Rect) -> WaveView {
        let left = sane(band.min.x, f32::MIN, f32::MAX);
        let top = sane(band.min.y, f32::MIN, f32::MAX);
        let width = sane(band.width(), 0.0, f32::MAX);
        let height = sane(band.height(), 0.0, f32::MAX);

        WaveView { view: Rect::from_min_max(pos2(left, top), pos2(left + width, top + height)) }
    }

    /// The band the view fills, squared up.
    pub fn view(&self) -> Rect {
        self.view
    }

    /// Where the centre line is: the sample value `0.0`, which is what a
    /// column of silence is drawn on.
    pub fn zero(&self) -> f32 {
        self.view.min.y + self.half_height()
    }

    /// Half the band's height — what full scale is worth in points, up or
    /// down from [`WaveView::zero`].
    pub fn half_height(&self) -> f32 {
        self.view.height() * 0.5
    }

    /// How wide one column is drawn: the band's width shared out over
    /// [`COLUMNS`], with no gap anywhere.
    ///
    /// Just under a point at the window's own size — 512 columns across the
    /// 510-point grid — which is finer than the view can show and is the
    /// point of a summary being taken at all.
    pub fn column_width(&self) -> f32 {
        self.view.width() / COLUMNS as f32
    }

    /// The left edge of column `column`, fractional columns included — so the
    /// playhead and a column's edge are the same arithmetic
    /// ([`WaveView::phase_x`]).
    ///
    /// Clamped to `0..=`[`COLUMNS`], which is the view's own span:
    /// `column_x(COLUMNS)` is the band's right edge exactly, because the
    /// division and the multiplication are both by a power of two and cancel.
    pub fn column_x(&self, column: f32) -> f32 {
        let column = sane(column, 0.0, COLUMNS as f32);

        self.view.min.x + column * self.column_width()
    }

    /// Where in the band a phase of the cycle sits: `0.0` is the band's left
    /// edge and `1.0` its right.
    ///
    /// The scope's own mapping read as a position rather than as an index
    /// (`Scope::column_of` is the same phase floored to a column), which is
    /// why this is `× COLUMNS` and then [`WaveView::column_x`] rather than a
    /// second division: [`COLUMNS`] is a power of two, so the multiplication
    /// is exact and the playhead cannot land a rounding outside the column
    /// the scope says it is in.
    ///
    /// Clamped, and NaN-safe through [`WaveView::column_x`] — a phase is the
    /// audio thread's and is already in range by construction, so this is the
    /// boundary saying so.
    pub fn phase_x(&self, phase: f32) -> f32 {
        self.column_x(phase * COLUMNS as f32)
    }

    /// One column's region: its own slice of the band, full height.
    ///
    /// The columns **tile** the band — each one's right edge is the next
    /// one's left edge, exactly, because both are [`WaveView::column_x`] of
    /// the same number — so a waveform drawn column by column has no dead
    /// pixels down it. A column past the last is drawn as the last, the way
    /// a step past the pattern's length is ([`super::grid::StepGrid`]).
    pub fn column_rect(&self, column: usize) -> Rect {
        let column = column.min(COLUMNS - 1) as f32;

        Rect::from_min_max(
            pos2(self.column_x(column), self.view.min.y),
            pos2(self.column_x(column + 1.0), self.view.max.y),
        )
    }

    /// Where a sample value is drawn: `+`[`FULL_SCALE`] at the top of the
    /// band, `-`[`FULL_SCALE`] at the bottom, zero on
    /// [`WaveView::zero`].
    ///
    /// Clamped to the band both ways (the module's [`FULL_SCALE`]), and
    /// NaN-safe in the project's `max`-then-`min` order — a NaN cannot reach
    /// here through [`Scope`], which drops one as it writes, but a reader
    /// that clamps is a reader that cannot draw a rectangle with a NaN edge.
    pub fn sample_y(&self, sample: f32) -> f32 {
        let sample = sane(sample, -FULL_SCALE, FULL_SCALE);
        let y = self.zero() - sample * self.half_height();

        sane(y, self.view.min.y, self.view.max.y)
    }

    /// One column of a summary, drawn: its own slice of the band, from its
    /// highest sample down to its lowest.
    ///
    /// Never thinner than [`theme::WAVE_TRACE_MIN`], so a column of silence
    /// is a line on the centre rather than nothing at all — which is what
    /// makes an idle view read as a waveform display with no signal in it
    /// instead of as an empty surface. The minimum is taken about the pair's
    /// own midpoint and then slid inside the band, so a column pinned against
    /// either edge keeps its height rather than half of it.
    ///
    /// `summary`'s two halves are two independent relaxed loads and may be a
    /// frame out of step with each other (`crate::scope` §The one-writer
    /// contract), so the span is taken as the extent of the pair rather than
    /// trusting `min` to be the lower of them: the worst a torn read can draw
    /// is one column of one frame's blend of two cycles.
    pub fn trace_rect(&self, column: usize, summary: Column) -> Rect {
        let span = self.column_rect(column);
        let (first, second) = (self.sample_y(summary.max), self.sample_y(summary.min));
        let (top, bottom) = (first.min(second), first.max(second));

        // At least the minimum, and never more than the band itself — which
        // is also what keeps `view.max.y - height` above the band's top edge
        // for the clamp below, a band squeezed to nothing included.
        let height = sane(bottom - top, theme::WAVE_TRACE_MIN, self.view.height());
        let middle = sane((top + bottom) * 0.5, self.view.min.y, self.view.max.y);
        let top = sane(middle - 0.5 * height, self.view.min.y, self.view.max.y - height);

        Rect::from_min_max(pos2(span.min.x, top), pos2(span.max.x, top + height))
    }

    /// The playhead's line at `phase`: [`theme::PLAYHEAD_LINE_WIDTH`] wide,
    /// centred on where that phase sits ([`WaveView::phase_x`]), the band's
    /// full height.
    ///
    /// Slid inside the band rather than clipped by it, so the line at either
    /// end of the cycle is the same width as everywhere else — a playhead
    /// that thinned to nothing as it wrapped would be hardest to see exactly
    /// where it moves fastest.
    pub fn playhead_rect(&self, phase: f32) -> Rect {
        let width = sane(theme::PLAYHEAD_LINE_WIDTH, 0.0, self.view.width());
        let left =
            sane(self.phase_x(phase) - 0.5 * width, self.view.min.x, self.view.max.x - width);

        Rect::from_min_max(pos2(left, self.view.min.y), pos2(left + width, self.view.max.y))
    }
}

/// What the view says when it is showing the pattern on screen.
///
/// A display and not a control, so the text says what is being drawn rather
/// than what a gesture would do — this is the one band in this window with
/// nothing to do, and every band gets hover text all the same.
pub const LIVE_HINT: &str = "What came in, drawn dim, and what went out, drawn bright, over one \
    pass of this pattern.\n\
    What went out is in the colour of the block that played it, and light grey where the audio \
    went through untouched.\n\
    The line is where in the pattern the song is now. The part ahead of it is dimmer because it \
    is the last time round, still there until the line reaches it again.";

/// What the view says when nothing is playing: the `pattern` parameter is
/// Off, so there is no cycle being summarised and what is on the band is
/// whatever was last played.
pub const OFF_HINT: &str = "Pattern is Off, so nothing is being drawn: this is the last pass \
    stuttter played, dimmed and held as it was.";

/// What the view says when the pointer rests on it: what it is a picture of,
/// and — when that is not the pattern on screen — which pattern it is of and
/// how to watch that one instead ([`Subject`]).
///
/// `playing` and `edited` are the frame's own, as [`ui_waveform`] is given
/// them. Pattern numbers are the ones on the buttons, counted from one.
pub fn hover_text(playing: Option<usize>, edited: usize) -> String {
    match Subject::of(playing, edited) {
        Subject::OnScreen => LIVE_HINT.to_owned(),
        Subject::Elsewhere => match playing {
            Some(playing) => format!(
                "Pattern {} is playing, so this shows its passes, not pattern {}'s — dimmed \
                 to say so.\nClick pattern {} to watch what is playing.",
                playing + 1,
                edited + 1,
                playing + 1
            ),
            None => OFF_HINT.to_owned(),
        },
    }
}

/// Where the playhead's line is drawn, as a phase of the cycle, or `None`
/// for the frames that have no "now" to mark.
///
/// Two things have to be true for a line. Something has to be **playing**:
/// with the parameter Off the band is a held picture, and a line over it
/// would be the one thing in the view that was false rather than merely out
/// of date (the module's §When the view is not of the pattern on screen).
/// And the scope has to have been **written**: it reports a phase of zero
/// before the first frame because the cursor has to be somewhere
/// ([`Scope::playhead`]), which would otherwise draw a line against the left
/// edge of an empty view — the same `None` a host that cleared the scope on
/// a transport stop leaves behind (`Engine::reset`).
///
/// The line is drawn for [`Subject::Elsewhere`] as long as a pattern is
/// playing: the columns under it are dimmed because they are another
/// pattern's, but they are being written, and the line is where that writing
/// has got to.
pub fn playhead(playing: Option<usize>, scope: &Scope) -> Option<f32> {
    match playing.is_some() && scope.column().is_some() {
        true => Some(scope.playhead()),
        false => None,
    }
}

/// How a block's source is shown on the band.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WashLevel {
    /// The selected block: washed full height, outlined, captioned, and the
    /// one that can be dragged.
    Selected,
    /// The block under the pointer on the Time row: washed full height,
    /// lighter, with no caption.
    Hovered,
    /// Every other block: a thin stripe along the band's foot, which a click
    /// selects.
    Faint,
}

/// The stretch of the cycle a block reads from, for the view to wash over:
/// `from` and `to` are fractions of the pattern's cycle, negative where the
/// block reaches into an earlier pass.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SourceWash {
    /// The block's index on the Time row.
    pub block: usize,
    /// Where the read starts, as a fraction of the cycle.
    pub from: f64,
    /// Where it ends.
    pub to: f64,
    /// The block's colour.
    pub colour: egui::Color32,
    /// How it is drawn.
    pub level: WashLevel,
    /// Whether its right edge sets a loop length: a looping block only.
    pub stretch: bool,
}

/// `from..to` folded into the cycle's `0..1`: one piece, two where it wraps
/// round the cycle's end, or the whole cycle when it is a cycle long or more.
pub fn wrap_span(from: f64, to: f64) -> Vec<(f32, f32)> {
    if !(from.is_finite() && to.is_finite()) || to <= from {
        return Vec::new();
    }
    if to - from >= 1.0 {
        return vec![(0.0, 1.0)];
    }
    let start = from.rem_euclid(1.0);
    let end = start + (to - from);
    match end <= 1.0 {
        true => vec![(start as f32, end as f32)],
        false => vec![(start as f32, 1.0), (0.0, (end - 1.0) as f32)],
    }
}

/// The caption on the selected wash: how many passes back it reads.
pub fn wash_caption(wash: &SourceWash) -> String {
    let passes = (-wash.from).ceil().max(0.0) as u64;
    match passes {
        0 => "reads here".to_string(),
        1 => "reads here, one pass back".to_string(),
        n => format!("reads here, {n} passes back"),
    }
}

/// The hover text over the selected wash.
pub const WASH_HINT: &str = "Where the selected block reads from. Drag it to move it along the \
    pattern; drag its right edge to change the loop length.\n\
    More passes back are set in the block panel.";

/// The hover text over a faint stripe.
pub const STRIPE_HINT: &str = "Where another block reads from. Click to select that block.";

/// The rectangles `wash` covers on the band: full height for the selected
/// and hovered washes, a stripe along the foot for a faint one. Faint
/// stripes sit in one of three lanes by block, so two that read the same
/// audio can both be seen.
pub fn wash_rects(wave: &WaveView, wash: &SourceWash) -> Vec<Rect> {
    let view = wave.view();
    let (top, bottom) = match wash.level {
        WashLevel::Selected | WashLevel::Hovered => (view.min.y, view.max.y),
        WashLevel::Faint => {
            let lane = (wash.block % 3) as f32;
            let bottom = view.max.y - lane * (theme::SOURCE_STRIPE_HEIGHT + 1.0);
            (bottom - theme::SOURCE_STRIPE_HEIGHT, bottom)
        }
    };
    wrap_span(wash.from, wash.to)
        .into_iter()
        .map(|(start, end)| {
            let left = wave.phase_x(start);
            let right = wave.phase_x(end).max(left + 1.0);
            Rect::from_min_max(pos2(left, top.max(view.min.y)), pos2(right, bottom))
        })
        .collect()
}

/// What the pointer is over among the washes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WashHit {
    /// The selected wash's body: a drag moves it.
    Body,
    /// The selected wash's right edge: a drag sets the loop length.
    Edge,
    /// Block `index`'s faint stripe: a click selects it.
    Stripe(usize),
}

/// What is under `pos`: the selected wash (its right edge first, for a
/// looping block), else a faint stripe, else nothing.
pub fn hit_wash(wave: &WaveView, washes: &[SourceWash], pos: egui::Pos2) -> Option<WashHit> {
    let selected = washes.iter().find(|wash| wash.level == WashLevel::Selected);
    if let Some(wash) = selected {
        let rects = wash_rects(wave, wash);
        if wash.stretch
            && let Some(last) = rects.last()
        {
            let grip = theme::SOURCE_EDGE_GRIP;
            let edge = Rect::from_min_max(pos2(last.max.x - grip, last.min.y), last.max)
                .expand2(egui::vec2(grip * 0.5, 0.0));
            if edge.contains(pos) {
                return Some(WashHit::Edge);
            }
        }
        if rects.iter().any(|rect| rect.contains(pos)) {
            return Some(WashHit::Body);
        }
    }
    washes
        .iter()
        .filter(|wash| wash.level == WashLevel::Faint)
        .find(|wash| wash_rects(wave, wash).iter().any(|rect| rect.expand(1.0).contains(pos)))
        .map(|wash| WashHit::Stripe(wash.block))
}

/// What the hand did to the washes this frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WashInput {
    /// Nothing.
    Idle,
    /// A drag began on the selected wash's body, or on its edge.
    Picked { edge: bool },
    /// The drag is this many points to the right of where it began.
    Moved { dx: f32 },
    /// The drag let go.
    Dropped,
    /// A faint stripe was clicked: select its block.
    Select(usize),
}

/// The egui id the view's hover and the wash drag are held against: one of
/// this band per window, so the id is a constant rather than anything the
/// frame builds (`super::grid::ui_time_row` has the argument).
fn waveform_id() -> egui::Id {
    egui::Id::unique("stuttter-waveform")
}

/// The id the start of a wash drag is remembered under between frames.
fn picked_id() -> egui::Id {
    egui::Id::unique("stuttter-wash-picked")
}

/// The waveform view, drawn: the input's columns under the output's, each at
/// the brightness its generation and its subject call for, the blocks'
/// sources washed over them, and the playhead over the lot.
///
/// `scope` is the instance's own, the same one the engine is writing from the
/// audio thread (`crate::scope` §The one-writer contract): every read here is
/// relaxed and nothing waits for anything, so a frame drawn in the middle of
/// a block paints a column that is a frame out of date rather than holding
/// the audio up.
///
/// `playing` and `edited` are the frame's two pattern indices, the same pair
/// every other band is drawn from: together they say whether the summaries
/// are of the pattern on screen ([`Subject`]), which is what decides the
/// brightness of the whole band and whether there is a playhead to draw.
///
/// The one gesture here is on the washes: the selected one is dragged, a
/// faint one is clicked. What that means for the block is the caller's
/// business; this only reports it ([`WashInput`]).
pub(super) fn ui_waveform(
    ui: &mut egui::Ui,
    band: Rect,
    scope: &Scope,
    playing: Option<usize>,
    edited: usize,
    playing_pattern: Option<&Pattern>,
    washes: &[SourceWash],
) -> WashInput {
    let wave = WaveView::new(band);
    let painter = ui.painter();

    // The view's own surface: the window's darkest grey, so the band reads as
    // sunken into the panel rather than as a third row (`theme::WAVE_BACKGROUND`).
    painter.rect_filled(wave.view(), theme::CELL_CORNER, theme::WAVE_BACKGROUND);

    // Then the two signals, column by column: the input first and the output
    // over it, so a span the plugin passed straight through reads as one
    // waveform and a span it replayed shows the dim original behind what was
    // played instead. The output takes the colour of the block playing
    // there ([`output_colour`]), so the two never look alike where the effect
    // is doing something.
    let subject = Subject::of(playing, edited);
    for column in 0..COLUMNS {
        let shade = subject.shade(scope.age(column));

        for (summary, colour) in [
            (scope.input(column), theme::WAVE_INPUT),
            (scope.output(column), output_colour(playing_pattern, column)),
        ] {
            let trace = wave.trace_rect(column, summary);
            if trace.is_positive() {
                painter.rect_filled(trace, egui::CornerRadius::ZERO, shade.applied(colour));
            }
        }
    }

    // Where the blocks read from: the faint stripes first, then the hovered
    // wash, then the selected one on top with its caption.
    let order = [WashLevel::Faint, WashLevel::Hovered, WashLevel::Selected];
    for level in order {
        for wash in washes.iter().filter(|wash| wash.level == level) {
            paint_wash(painter, &wave, wash);
        }
    }

    // And the playhead, on the frames that have a "now" to mark at all
    // ([`playhead`] has the two conditions).
    if let Some(phase) = playhead(playing, scope) {
        let line = wave.playhead_rect(phase);
        painter.rect_filled(line, egui::CornerRadius::ZERO, theme::PLAYHEAD_LINE);
    }

    // A band the host squeezed to nothing is drawn and not interacted with,
    // as the rows above it are: an empty rect contains no point, and there is
    // no view to say anything about.
    if !wave.view().is_positive() {
        return WashInput::Idle;
    }

    let response = ui.interact(wave.view(), waveform_id(), egui::Sense::click_and_drag());
    let hovered = response.hover_pos().and_then(|pos| hit_wash(&wave, washes, pos));
    let picked = ui.data(|data| data.get_temp::<bool>(picked_id()));

    let input = if response.drag_started() {
        let origin = ui.input(|input| input.pointer.press_origin());
        match origin.and_then(|pos| hit_wash(&wave, washes, pos)) {
            Some(hit @ (WashHit::Body | WashHit::Edge)) => {
                let edge = hit == WashHit::Edge;
                ui.data_mut(|data| data.insert_temp(picked_id(), edge));
                WashInput::Picked { edge }
            }
            _ => WashInput::Idle,
        }
    } else if picked.is_some() && (response.drag_stopped() || !response.dragged()) {
        ui.data_mut(|data| data.remove::<bool>(picked_id()));
        WashInput::Dropped
    } else if picked.is_some() {
        WashInput::Moved { dx: response.total_drag_delta().map_or(0.0, |delta| delta.x) }
    } else if response.clicked()
        && let Some(WashHit::Stripe(block)) = hovered
    {
        WashInput::Select(block)
    } else {
        WashInput::Idle
    };

    // The cursor says what a press would do, and holds while it is doing it.
    let edge = match input {
        WashInput::Picked { edge } => Some(edge),
        WashInput::Moved { .. } => picked,
        _ => None,
    };
    let cursor = match (edge, hovered) {
        (Some(true), _) | (None, Some(WashHit::Edge)) => Some(egui::CursorIcon::ResizeHorizontal),
        (Some(false), _) => Some(egui::CursorIcon::Grabbing),
        (None, Some(WashHit::Body)) => Some(egui::CursorIcon::Grab),
        (None, Some(WashHit::Stripe(_))) => Some(egui::CursorIcon::PointingHand),
        (None, None) => None,
    };
    if let Some(cursor) = cursor {
        ui.ctx().set_cursor_icon(cursor);
    }

    // The text is built only for the frame it is shown on: the `Elsewhere`
    // case names two patterns and so is a `String`, and a window being
    // watched repaints every frame the transport moves.
    if response.hovered() && edge.is_none() {
        let text = match hovered {
            Some(WashHit::Body | WashHit::Edge) => WASH_HINT.to_owned(),
            Some(WashHit::Stripe(_)) => STRIPE_HINT.to_owned(),
            None => hover_text(playing, edited),
        };
        response.on_hover_text(text);
    }

    input
}

/// One wash, painted at its level.
fn paint_wash(painter: &egui::Painter, wave: &WaveView, wash: &SourceWash) {
    let rects = wash_rects(wave, wash);
    let (alpha, outline) = match wash.level {
        WashLevel::Selected => (theme::SOURCE_WASH_ALPHA, Some(theme::HAIRLINE)),
        WashLevel::Hovered => (theme::SOURCE_WASH_ALPHA * 0.6, Some(theme::HAIRLINE)),
        WashLevel::Faint => (theme::SOURCE_STRIPE_ALPHA, None),
    };
    let fill = wash.colour.gamma_multiply(alpha);
    for rect in &rects {
        painter.rect_filled(*rect, egui::CornerRadius::ZERO, fill);
        if let Some(width) = outline {
            let stroke = egui::Stroke::new(width, wash.colour);
            painter.rect_stroke(*rect, egui::CornerRadius::ZERO, stroke, egui::StrokeKind::Inside);
        }
    }
    if wash.level != WashLevel::Selected {
        return;
    }
    // The selected wash's grip: a bar on its right edge when it sets a loop.
    if wash.stretch
        && let Some(last) = rects.last()
    {
        let grip = Rect::from_min_max(
            pos2(last.max.x - theme::SOURCE_EDGE_BAR, last.min.y),
            pos2(last.max.x, last.max.y),
        );
        painter.rect_filled(grip, egui::CornerRadius::ZERO, wash.colour);
    }
    if let Some(rect) = rects.first() {
        painter.text(
            rect.left_top() + egui::Vec2::splat(theme::BLOCK_LABEL_PADDING),
            egui::Align2::LEFT_TOP,
            wash_caption(wash),
            egui::FontId::proportional(theme::HINT_SIZE),
            wash.colour,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::layout::EditorLayout;
    use crate::editor::{DEFAULT_HEIGHT, DEFAULT_WIDTH, GRID_WIDTH};
    use crate::scope::FIRST_GENERATION;
    use egui::vec2;

    /// The waveform band of the window at the size it opens at, taken from
    /// the layout rather than written out — the band the view is really drawn
    /// in.
    fn band() -> Rect {
        EditorLayout::new(Rect::from_min_size(pos2(17.0, 5.0), vec2(DEFAULT_WIDTH, DEFAULT_HEIGHT)))
            .waveform()
    }

    /// The view of that band.
    fn view() -> WaveView {
        WaveView::new(band())
    }

    /// A column holding one sample value: what the scope writes for a single
    /// frame.
    fn flat(sample: f32) -> Column {
        Column { min: sample, max: sample }
    }

    /// The columns tile the band exactly: the first starts on its left edge,
    /// the last ends on its right, and every one's right edge is the next
    /// one's left — the property that lets a waveform be drawn column by
    /// column with no dead pixels down it.
    #[test]
    fn the_columns_tile_the_band_with_no_gaps() {
        let wave = view();
        let band = wave.view();

        assert_eq!(wave.column_rect(0).min.x, band.min.x, "the first column starts on the edge");
        assert_eq!(wave.column_rect(COLUMNS - 1).max.x, band.max.x, "and the last ends on it");

        for column in 0..COLUMNS {
            let rect = wave.column_rect(column);

            assert_eq!(rect.min.y, band.min.y, "column {column} is the band's full height");
            assert_eq!(rect.max.y, band.max.y, "column {column}");
            assert_eq!(rect.width(), wave.column_width(), "column {column} is a column wide");
            assert!(band.contains_rect(rect), "column {column} is outside the band");

            if column + 1 < COLUMNS {
                assert_eq!(
                    rect.max.x,
                    wave.column_rect(column + 1).min.x,
                    "a gap after column {column}"
                );
            }
        }

        // And a column past the last is the last, rather than a rect off the
        // end of the band.
        for past in [COLUMNS, COLUMNS + 1, usize::MAX] {
            assert_eq!(wave.column_rect(past), wave.column_rect(COLUMNS - 1), "column {past}");
        }
    }

    /// At the window's own size the band is the grid wide and the cycle is
    /// summarised at about a point a column.
    #[test]
    fn the_default_window_draws_a_column_in_about_a_point() {
        let wave = view();

        assert_eq!(wave.view().width(), GRID_WIDTH);
        assert!(wave.view().height() >= theme::WAVEFORM_MIN_HEIGHT);
        assert_eq!(wave.column_width(), GRID_WIDTH / COLUMNS as f32);
        assert!(wave.column_width() > 0.9 && wave.column_width() < 1.5);
    }

    fn wash(block: usize, from: f64, to: f64, level: WashLevel) -> SourceWash {
        SourceWash { block, from, to, colour: Color32::WHITE, level, stretch: true }
    }

    /// The selected and hovered washes are full height; a faint one is a
    /// stripe along the foot, in a lane by block.
    #[test]
    fn washes_cover_the_band_and_stripes_sit_along_its_foot() {
        let wave = view();
        let band = wave.view();
        let full = wash_rects(&wave, &wash(0, 0.25, 0.5, WashLevel::Selected));
        assert_eq!(full.len(), 1);
        assert_eq!((full[0].min.y, full[0].max.y), (band.min.y, band.max.y));
        assert_eq!(full[0].min.x, wave.phase_x(0.25));
        assert_eq!(full[0].max.x, wave.phase_x(0.5));

        let stripe = wash_rects(&wave, &wash(0, 0.25, 0.5, WashLevel::Faint))[0];
        assert_eq!(stripe.max.y, band.max.y);
        assert_eq!(stripe.height(), theme::SOURCE_STRIPE_HEIGHT);
        let next = wash_rects(&wave, &wash(1, 0.25, 0.5, WashLevel::Faint))[0];
        assert!(next.max.y <= stripe.min.y, "the next block's stripe is above");

        let wrapped = wash_rects(&wave, &wash(0, -0.125, 0.125, WashLevel::Hovered));
        assert_eq!(wrapped.len(), 2, "a wash across the cycle's end is two pieces");
    }

    /// The pointer over the selected wash takes its body, or its right edge
    /// on a looping block; over a stripe it takes that block; elsewhere,
    /// nothing.
    #[test]
    fn the_pointer_finds_the_box_its_edge_and_the_stripes() {
        let wave = view();
        let band = wave.view();
        let selected = wash(0, 0.25, 0.5, WashLevel::Selected);
        let faint = wash(2, 0.6, 0.8, WashLevel::Faint);
        let washes = [selected, faint];
        let y = band.center().y;

        let middle = pos2(wave.phase_x(0.375), y);
        assert_eq!(hit_wash(&wave, &washes, middle), Some(WashHit::Body));
        let edge = pos2(wave.phase_x(0.5) - 1.0, y);
        assert_eq!(hit_wash(&wave, &washes, edge), Some(WashHit::Edge));
        let unlooped = [SourceWash { stretch: false, ..selected }];
        assert_eq!(hit_wash(&wave, &unlooped, edge), Some(WashHit::Body));

        let stripe = wash_rects(&wave, &faint)[0].center();
        assert_eq!(hit_wash(&wave, &washes, stripe), Some(WashHit::Stripe(2)));
        assert_eq!(hit_wash(&wave, &washes, pos2(wave.phase_x(0.7), y)), None);
        assert_eq!(hit_wash(&wave, &washes, pos2(wave.phase_x(0.1), y)), None);
    }

    /// The wash over the source folds into the cycle: one piece, two across
    /// the cycle's end, the whole width for a cycle or more, nothing for
    /// nonsense — and its caption says how many passes back it is.
    #[test]
    fn the_source_wash_folds_into_the_cycle() {
        assert_eq!(wrap_span(0.25, 0.5), vec![(0.25, 0.5)]);
        assert_eq!(wrap_span(-0.25, 0.0), vec![(0.75, 1.0)]);
        assert_eq!(wrap_span(-0.125, 0.125), vec![(0.875, 1.0), (0.0, 0.125)]);
        assert_eq!(wrap_span(-3.0, 0.0), vec![(0.0, 1.0)]);
        assert_eq!(wrap_span(0.5, 0.5), Vec::<(f32, f32)>::new());
        assert_eq!(wrap_span(f64::NAN, 1.0), Vec::<(f32, f32)>::new());

        let at = |from| wash(0, from, from + 0.1, WashLevel::Selected);
        assert_eq!(wash_caption(&at(0.2)), "reads here");
        assert_eq!(wash_caption(&at(-0.2)), "reads here, one pass back");
        assert_eq!(wash_caption(&at(-1.5)), "reads here, 2 passes back");
    }

    /// A phase and a column index are the same mapping: the point a phase
    /// sits at is inside the column the scope says that phase is in, for
    /// every column boundary and either side of it.
    ///
    /// This is what keeps the playhead over the column being written instead
    /// of a rounding to one side of it (the module's §Columns are phase, not
    /// steps).
    #[test]
    fn a_phase_lands_in_the_column_the_scope_puts_it_in() {
        let wave = view();

        assert_eq!(wave.phase_x(0.0), wave.view().min.x, "the cycle starts at the left edge");
        assert_eq!(wave.phase_x(1.0), wave.view().max.x, "and ends on the right one");
        assert_eq!(wave.phase_x(0.5), wave.column_x(COLUMNS as f32 * 0.5), "half way is half way");

        for column in [0usize, 1, 2, 171, COLUMNS / 2, COLUMNS - 2, COLUMNS - 1] {
            // The phase at the column's own start, and one a thousandth of a
            // column inside it: both belong to this column, and the point
            // they map to has to be in its rectangle.
            for inside in [0.0, 0.001, 0.5, 0.999] {
                let phase = (column as f32 + inside) / COLUMNS as f32;

                assert_eq!(Scope::column_of(f64::from(phase)), column, "phase {phase}");
                assert!(
                    wave.column_rect(column).x_range().contains(wave.phase_x(phase)),
                    "phase {phase} is drawn outside column {column}"
                );
            }
        }

        // A phase outside the cycle is brought into it rather than drawn off
        // the band, a NaN included (`column_x` clamps in the project's
        // `max`-then-`min` order).
        for phase in [-1.0, 2.0, f32::INFINITY, f32::NEG_INFINITY, f32::NAN] {
            let x = wave.phase_x(phase);

            assert!(x.is_finite(), "phase {phase}");
            assert!(wave.view().x_range().contains(x), "phase {phase} is drawn off the band");
        }
    }

    /// Full scale is the band's top and bottom edge, zero is the middle, and
    /// anything louder than full scale is flat against the edge rather than
    /// off it.
    #[test]
    fn a_sample_is_drawn_at_its_height_in_the_band() {
        let wave = view();
        let band = wave.view();

        assert_eq!(wave.sample_y(FULL_SCALE), band.min.y, "full scale is the top edge");
        assert_eq!(wave.sample_y(-FULL_SCALE), band.max.y, "and the bottom");
        assert_eq!(wave.sample_y(0.0), wave.zero(), "silence is the centre line");
        assert_eq!(wave.zero(), band.center().y);
        assert_eq!(wave.sample_y(0.5), wave.zero() - 0.5 * wave.half_height());
        assert_eq!(wave.sample_y(-0.5), wave.zero() + 0.5 * wave.half_height());

        // Louder than the view can show, and nonsense: clamped onto the band,
        // never outside it.
        for sample in [2.0, -2.0, 1e30, f32::INFINITY, f32::NEG_INFINITY, f32::NAN] {
            let y = wave.sample_y(sample);

            assert!(y.is_finite(), "sample {sample}");
            assert!(band.y_range().contains(y), "sample {sample} is drawn off the band");
        }
        assert_eq!(wave.sample_y(2.0), band.min.y, "a hot sample is flat against the top");
        assert_eq!(wave.sample_y(-2.0), band.max.y);
    }

    /// A column's trace spans its own summary: from its highest sample to its
    /// lowest, in its own slice of the band.
    #[test]
    fn a_trace_spans_the_columns_summary() {
        let wave = view();

        let full = wave.trace_rect(3, Column { min: -1.0, max: 1.0 });
        assert_eq!(full, wave.column_rect(3), "a column at full scale fills its slice");

        // A column entirely above zero is drawn where it sat and is not
        // stretched down to the centre line (`Column::of`).
        let high = wave.trace_rect(3, Column { min: 0.25, max: 0.75 });
        assert_eq!(high.min.y, wave.sample_y(0.75));
        assert_eq!(high.max.y, wave.sample_y(0.25));
        assert!(high.max.y < wave.zero(), "a column above zero stays above it");
        assert_eq!(high.x_range(), wave.column_rect(3).x_range(), "in its own slice");

        // A torn read — the two halves of a column are two relaxed loads —
        // is the extent of the pair, not a reversed rectangle.
        let torn = wave.trace_rect(3, Column { min: 0.75, max: 0.25 });
        assert_eq!(torn, high, "a torn pair draws the span it spans");

        // Every column of a hostile summary: finite, positive, inside the
        // band, in its own slice.
        for summary in [
            flat(0.0),
            flat(1.0),
            flat(-1.0),
            Column { min: -4.0, max: 9.0 },
            Column { min: f32::NAN, max: f32::NAN },
            Column { min: f32::NEG_INFINITY, max: f32::INFINITY },
        ] {
            for column in [0, COLUMNS / 2, COLUMNS - 1] {
                let trace = wave.trace_rect(column, summary);

                assert!(trace.is_positive(), "{summary:?} in column {column} draws nothing");
                assert!(
                    wave.view().contains_rect(trace),
                    "{summary:?} in column {column} is outside the band"
                );
                assert_eq!(trace.x_range(), wave.column_rect(column).x_range(), "column {column}");
            }
        }
    }

    /// A column of silence is still a line: the least a trace is drawn is
    /// [`theme::WAVE_TRACE_MIN`], on the centre for silence and at its own
    /// height for a column that sat still somewhere else.
    #[test]
    fn a_silent_column_draws_a_line_rather_than_nothing() {
        let wave = view();

        let silent = wave.trace_rect(0, Column::SILENT);
        assert_eq!(silent.height(), theme::WAVE_TRACE_MIN, "silence is a hairline");
        assert_eq!(silent.center().y, wave.zero(), "on the centre line");

        // One frame of a column, before the ones after it widen it: the same
        // line, at the height that frame sat at.
        let held = wave.trace_rect(0, flat(0.5));
        assert_eq!(held.height(), theme::WAVE_TRACE_MIN);
        assert_eq!(held.center().y, wave.sample_y(0.5));

        // And pinned against either edge it keeps its height instead of half
        // of it.
        for sample in [FULL_SCALE, -FULL_SCALE] {
            let edge = wave.trace_rect(0, flat(sample));

            assert_eq!(edge.height(), theme::WAVE_TRACE_MIN, "sample {sample}");
            assert!(wave.view().contains_rect(edge), "sample {sample}");
        }
    }

    /// The playhead is a line of the theme's width, centred on the phase it
    /// marks and never hanging off either end of the band.
    #[test]
    fn the_playhead_marks_its_phase_and_stays_in_the_band() {
        let wave = view();
        let band = wave.view();

        let middle = wave.playhead_rect(0.5);
        assert_eq!(middle.width(), theme::PLAYHEAD_LINE_WIDTH);
        assert_eq!(middle.center().x, wave.phase_x(0.5), "centred on the phase");
        assert_eq!(middle.min.y, band.min.y, "the band's full height");
        assert_eq!(middle.max.y, band.max.y);

        // At either end it is slid inside the band rather than thinned by it.
        for phase in [0.0, 1.0, -1.0, 2.0, f32::NAN] {
            let line = wave.playhead_rect(phase);

            assert_eq!(line.width(), theme::PLAYHEAD_LINE_WIDTH, "phase {phase}");
            assert!(band.contains_rect(line), "phase {phase} is drawn off the band");
        }
        assert_eq!(wave.playhead_rect(0.0).min.x, band.min.x, "the start of the cycle");
        assert_eq!(wave.playhead_rect(1.0).max.x, band.max.x, "and the end of it");
    }

    /// The dimming rule: the cycle being written now is drawn in its own
    /// colour and anything older is taken down to
    /// [`theme::WAVE_STALE_FACTOR`] of it — once, however old it is.
    #[test]
    fn this_cycle_is_bright_and_every_older_one_is_dimmed() {
        assert_eq!(Shade::of(0), Shade::Fresh, "the cycle being written now");
        for age in [1, 2, 3, 17, u32::MAX] {
            assert_eq!(Shade::of(age), Shade::Stale, "age {age}");
        }

        for colour in [theme::WAVE_INPUT, theme::WAVE_OUTPUT] {
            let fresh = Shade::Fresh.applied(colour);
            let stale = Shade::Stale.applied(colour);

            assert_eq!(fresh, colour, "a fresh column keeps its colour");
            assert_eq!(stale, theme::dim(colour, theme::WAVE_STALE_FACTOR));
            assert!(stale.intensity() < fresh.intensity(), "a cycle old has to read dimmer");
            assert!(stale.intensity() > theme::WAVE_BACKGROUND.intensity(), "and still read");
        }

        // The output stays the brighter of the two in either shade, which is
        // what the view is for: the dimming says *when*, the colour says
        // *which*.
        for shade in [Shade::Fresh, Shade::Stale] {
            let input = shade.applied(theme::WAVE_INPUT);
            let output = shade.applied(theme::WAVE_OUTPUT);

            assert!(output.intensity() > input.intensity(), "{shade:?}");
        }
    }

    /// Column → step agrees with the grid: the 512 columns split evenly over
    /// any length, the first and last columns land on the first and last
    /// steps, and every step gets at least one column.
    #[test]
    fn a_column_sits_in_the_step_drawn_above_it() {
        assert_eq!(step_of_column(0, 16), 0);
        assert_eq!(step_of_column(31, 16), 0);
        assert_eq!(step_of_column(32, 16), 1);
        assert_eq!(step_of_column(COLUMNS - 1, 16), 15);
        assert_eq!(step_of_column(COLUMNS * 4, 16), 15, "a column past the end is the last");
        assert_eq!(step_of_column(100, 0), 0, "a zero length is one step");

        for length in [1, 3, 12, 16, 47, 64] {
            let mut seen = vec![false; usize::from(length)];
            let mut last = 0;
            for column in 0..COLUMNS {
                let step = step_of_column(column, length);
                assert!(step < length && step >= last, "length {length}, column {column}");
                seen[usize::from(step)] = true;
                last = step;
            }
            assert!(seen.iter().all(|&seen| seen), "length {length} leaves a step out");
        }
    }

    /// The output takes the colour of the block playing at its column, and
    /// the plain grey where none is or nothing is playing.
    #[test]
    fn the_output_is_drawn_in_its_blocks_colour() {
        use crate::editor::grid::BlockKind;
        use crate::actions::SpeedPreset;
        use crate::pattern::{Action, Block};

        let mut pattern = Pattern::new();
        pattern.set_length(16);
        pattern.set_blocks([
            Block::new(0, 2, Action::default()),
            Block::new(8, 4, Action::from_preset(SpeedPreset::Reverse)),
        ]);
        let repeat = BlockKind::of(Action::default()).colour();
        let reverse = BlockKind::Reverse.colour();

        assert_eq!(output_colour(Some(&pattern), 0), repeat, "step 0");
        assert_eq!(output_colour(Some(&pattern), 63), repeat, "the last column of step 1");
        assert_eq!(output_colour(Some(&pattern), 64), theme::WAVE_OUTPUT, "step 2 plays live");
        assert_eq!(output_colour(Some(&pattern), 8 * 32), reverse, "step 8");
        assert_eq!(output_colour(Some(&pattern), 12 * 32 - 1), reverse, "step 11");
        assert_eq!(output_colour(Some(&pattern), 12 * 32), theme::WAVE_OUTPUT, "step 12");
        assert_eq!(output_colour(None, 0), theme::WAVE_OUTPUT, "nothing playing");

        // Every block colour stands out from the input behind it, fresh or
        // a cycle old, as the grey output does.
        for kind in BlockKind::ALL {
            for shade in [Shade::Fresh, Shade::Stale] {
                let input = shade.applied(theme::WAVE_INPUT);
                let output = shade.applied(kind.colour());
                assert!(output.intensity() > input.intensity(), "{kind:?} {shade:?}");
            }
        }
    }

    /// The band is a picture of the pattern on screen only when that is the
    /// one playing: with the host on another pattern, or on none at all,
    /// every column is dimmed however fresh it is (the module's §When the
    /// view is not of the pattern on screen).
    #[test]
    fn a_cycle_of_another_pattern_dims_the_whole_band() {
        assert_eq!(Subject::of(Some(0), 0), Subject::OnScreen, "the pattern on screen");
        assert_eq!(Subject::of(Some(7), 7), Subject::OnScreen);
        assert_eq!(Subject::of(Some(1), 0), Subject::Elsewhere, "another pattern is playing");
        assert_eq!(Subject::of(Some(0), 1), Subject::Elsewhere);
        assert_eq!(Subject::of(None, 0), Subject::Elsewhere, "the parameter is Off");

        // On screen, the generation rule is the whole of the brightness.
        assert_eq!(Subject::OnScreen.shade(0), Shade::Fresh);
        assert_eq!(Subject::OnScreen.shade(1), Shade::Stale);

        // Elsewhere, the freshest column there is still draws dim — the view
        // keeps moving, it just stops claiming to be about the grid.
        for age in [0, 1, 2, u32::MAX] {
            assert_eq!(Subject::Elsewhere.shade(age), Shade::Stale, "age {age}");
        }
    }

    /// The playhead needs something to be playing *and* something to have
    /// been written: a held picture gets no line, and neither does a scope
    /// that has never been written or has just been cleared.
    #[test]
    fn the_playhead_is_drawn_only_where_there_is_a_now_to_mark() {
        let scope = Scope::new();

        // Nothing written yet, whatever the parameter says.
        assert_eq!(playhead(Some(0), &scope), None, "nothing has been written");
        assert_eq!(playhead(None, &scope), None);

        scope.write(0.25, (0.5, 0.5), (0.5, 0.5));
        assert_eq!(playhead(Some(0), &scope), Some(0.25), "the phase the scope is at");
        // Another pattern's cycle is still being written, and the line is
        // where that writing has got to — the columns under it are what the
        // dimming says is not ours.
        assert_eq!(playhead(Some(3), &scope), Some(0.25), "another pattern is still playing");
        // Off is a held picture: no line at all.
        assert_eq!(playhead(None, &scope), None, "nothing is playing");

        // And a cleared scope — what the engine's `reset()` leaves behind
        // across a transport stop — is back to no line.
        scope.clear();
        assert_eq!(playhead(Some(0), &scope), None, "a cleared scope has no playhead");
    }

    /// The hover text says what the band is a picture of, and when that is
    /// not the pattern on screen it names the one it is of and how to watch
    /// it (every control gets hover text).
    #[test]
    fn the_hover_text_says_which_pattern_the_view_is_of() {
        assert_eq!(hover_text(Some(0), 0), LIVE_HINT, "the pattern on screen");
        assert_eq!(hover_text(None, 4), OFF_HINT, "the parameter is Off");

        // Pattern numbers are the ones on the buttons: counted from one.
        let elsewhere = hover_text(Some(2), 9);
        assert!(elsewhere.contains("Pattern 3 is playing"), "{elsewhere}");
        assert!(elsewhere.contains("pattern 10"), "{elsewhere}");
        assert!(elsewhere.contains("dimmed"), "the text has to explain the dimming: {elsewhere}");
        assert!(elsewhere.contains("Click pattern"), "and how to watch it instead: {elsewhere}");
    }

    /// The rule against a real scope: the columns the playhead has crossed in
    /// this cycle are fresh, the ones ahead of it are the last pass and draw
    /// dimmer, and a scope nothing has written is dimmed silence throughout.
    #[test]
    fn the_span_ahead_of_the_playhead_is_the_last_pass() {
        let scope = Scope::new();

        // Nothing written: every column is a generation old, so the whole
        // view is dimmed silence rather than a current reading of a signal
        // that has not arrived (`crate::scope::FIRST_GENERATION`).
        assert_eq!(scope.generation(), FIRST_GENERATION);
        assert_eq!(scope.column(), None, "and so no playhead to draw");
        for column in [0, COLUMNS / 2, COLUMNS - 1] {
            assert_eq!(Shade::of(scope.age(column)), Shade::Stale, "column {column}");
        }

        // One whole cycle, then a second one up to its middle: the columns
        // behind the playhead are this cycle's and the ones ahead are the
        // first cycle's.
        let crossed = COLUMNS / 2;
        for pass in 0..2 {
            let columns = match pass {
                0 => COLUMNS,
                _ => crossed,
            };
            for column in 0..columns {
                let phase = column as f64 / COLUMNS as f64;
                scope.write(phase, (0.5, 0.5), (0.25, 0.25));
            }
        }

        assert_eq!(scope.generation(), FIRST_GENERATION + 1, "the playhead wrapped once");
        assert_eq!(scope.column(), Some(crossed - 1));
        for column in 0..COLUMNS {
            let expected = match column < crossed {
                true => Shade::Fresh,
                false => Shade::Stale,
            };
            assert_eq!(Shade::of(scope.age(column)), expected, "column {column}");
        }

        // And the playhead is in the last column written, which is where the
        // bright span ends.
        let wave = view();
        let line = wave.playhead_rect(scope.playhead());
        let written = wave.column_rect(crossed - 1);
        assert!(line.x_range().contains(written.center().x), "the line is not on its column");
    }

    /// The view follows the band wherever the window is, and a band the host
    /// squeezed — or a nonsense rectangle — draws nothing rather than
    /// rectangles with an infinity in them (everything is clamped at the
    /// boundaries).
    #[test]
    fn a_nonsense_band_is_squared_up() {
        // The same view, offset, wherever the host opened the window.
        let here = view();
        let there = WaveView::new(band().translate(vec2(-400.0, 900.0)));
        for column in [0, COLUMNS / 2, COLUMNS - 1] {
            assert_eq!(
                there.column_rect(column),
                here.column_rect(column).translate(vec2(-400.0, 900.0)),
                "column {column}"
            );
        }

        let reversed = Rect::from_min_max(pos2(500.0, 400.0), pos2(100.0, 50.0));
        let nan = Rect::from_min_size(pos2(f32::NAN, f32::NAN), vec2(f32::NAN, f32::NAN));
        let flat_band = Rect::from_min_size(pos2(17.0, 5.0), vec2(DEFAULT_WIDTH, 0.0));

        for rect in [reversed, Rect::EVERYTHING, Rect::NOTHING, nan, flat_band] {
            let wave = WaveView::new(rect);
            let view = wave.view();

            assert!(view.width() >= 0.0 && view.height() >= 0.0, "{rect:?}");
            for column in [0, COLUMNS / 2, COLUMNS - 1] {
                for part in [
                    wave.column_rect(column),
                    wave.trace_rect(column, flat(0.5)),
                    wave.trace_rect(column, Column { min: -1.0, max: 1.0 }),
                    wave.playhead_rect(0.5),
                ] {
                    assert!(part.min.x.is_finite() && part.min.y.is_finite(), "{rect:?}");
                    assert!(part.max.x.is_finite() && part.max.y.is_finite(), "{rect:?}");
                    assert!(part.width() >= 0.0 && part.height() >= 0.0, "{rect:?}");
                    assert!(view.contains_rect(part), "{rect:?} column {column}");
                }
            }
        }
    }
}
