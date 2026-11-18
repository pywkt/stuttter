//! The Volume row: a bar per step, drawn by dragging across it.
//!
//! The same row draws the pattern's other two rows of levels, Cutoff and Res
//! ([`Lane`]): the strip over it ([`ui_lane_bar`]) picks which one it shows
//! and holds the pattern's Filter dropdown. Everything below is said of the
//! Volume row and is true of all three — only the default a double-click
//! puts back, the colour and the words differ.
//!
//! The row is the Time row's twin — the same columns, the same hand-painted
//! surface, the same split between arithmetic that can be tested on a machine
//! with no display and a thin piece of egui glue over it ([`ui_volume_row`]).
//! It borrows the columns outright: [`StepGrid`] is built on this band's rect
//! and asked for the same cells, so a step's block and its level are at the
//! same place across the window and the playhead crosses both at once.
//!
//! What this module adds is the other axis. A step's level is a height inside
//! its cell, so the pointer's `y` is the value ([`level_at`]) and the fill is
//! that reading back ([`fill_rect`]) — the two are inverses, which is what
//! makes the bar end under the pointer that drew it.
//!
//! # Levels land on whole percent
//!
//! [`level_at`] rounds to [`LEVEL_STEPS`] — whole percent, the figure the
//! hover text says and the user guide's "silence to full" in the units
//! Bitwig shows a percentage in. The row is [`theme::CELL_HEIGHT`] tall, so a
//! point of travel is a third of a percent: the lattice is finer than the
//! pixels and no reachable level is unreachable because of it. What it buys
//! is that a pointer wobbling inside one point writes nothing — a write
//! publishes a snapshot with the collector behind it — and that a level is
//! the same number after a save and a reload as the one the bar was drawn at.
//!
//! # A fast drag leaves no gap
//!
//! A pointer can cross several columns between two frames, and bitstep's
//! paint drag has the same problem with the same answer: the gesture
//! remembers the column it last wrote and fills in everything between that
//! and where the pointer is now ([`Stroke`]). Here the level travels too, so
//! the columns it jumped get a straight ramp from the last level to the new
//! one rather than all of them getting the new one — which is what the hand
//! drew, and what makes a quick diagonal sweep across the row a ramp instead
//! of a staircase of whatever the frame rate happened to be.
//!
//! # One undo step per gesture, and the double-click
//!
//! The drag writes through the model on every frame that changes anything and
//! commits nothing; the release is the undo step ([`VolumeDraw::settle`]), as
//! a Time row drag is. A whole frame's stroke is **one** write — the row is
//! read out of the snapshot, painted into, and put back with
//! [`StutterModel::set_volumes`] — so a sweep across sixty-four columns
//! publishes one snapshot per frame and not one per step.
//!
//! A double-click puts a bar back to full ([`reset`]). The press that starts
//! it paints the bar at the pointer's height first, because there is no way
//! to know a second click is coming; both writes are in the frame's gesture,
//! so the row ends at full and that is what the window draws. A *single*
//! click is a gesture of its own and keeps its own undo step, so a bar that
//! was clicked to a level and then double-clicked back takes two presses of
//! Undo to return to where it started — one per thing the hand did.

use egui::{Pos2, Rect, pos2};

use super::grid::StepGrid;
use super::layout::sane;
use super::theme;
use crate::model::StutterModel;
use crate::pattern::{Filter, Lane, MAX_STEPS, Pattern, clamp_volume, cutoff_hz};

/// How many levels the row has: whole percent (the module's §Levels land on
/// whole percent).
///
/// Behaviour and not a look, which is why it is here and not in [`theme`]: it
/// says what a drag can write, not how tall the bar is drawn.
pub const LEVEL_STEPS: f32 = 100.0;

/// `level`, rounded onto the lattice of [`LEVEL_STEPS`] and brought inside
/// `0.0..=1.0`.
///
/// NaN-safe by [`clamp_volume`]'s `max`-then-`min`, and applied *after* the
/// rounding so that a nonsense level cannot escape through
/// `f32::round`'s own NaN.
pub fn quantise(level: f32) -> f32 {
    clamp_volume((level * LEVEL_STEPS).round() / LEVEL_STEPS)
}

/// The level the pointer at screen height `y` means: the row's bottom edge is
/// silence, its top edge is full, and everything between is the fraction of
/// the row's height above the bottom, quantised ([`quantise`]).
///
/// Clamped, not refused: a drag whose pointer has wandered above or below the
/// band keeps writing full or silence, which is the same kindness
/// [`StepGrid::step_at_x`] does the other axis — a sweep that drifts out of a
/// thirty-point-tall row should not stop drawing half way across it.
///
/// A row with no height at all, and a NaN `y`, read as
/// [`Pattern::DEFAULT_VOLUME`]: there is no level in either, and full is the
/// one answer that cannot silence a step by accident. Neither is reachable
/// from the window — a band squeezed to nothing is not interacted with at all
/// ([`ui_volume_row`]) — so this is the boundary saying so rather than a
/// behaviour anyone meets.
pub fn level_at(grid: &StepGrid, y: f32) -> f32 {
    let row = grid.row();
    let height = row.height();
    if !height.is_finite() || height <= 0.0 || y.is_nan() {
        return Pattern::DEFAULT_VOLUME;
    }

    quantise(sane((row.max.y - y) / height, 0.0, 1.0))
}

/// One bar's filled part: `level` of the way up [`StepGrid::cell_rect`],
/// measured from its bottom edge.
///
/// The inverse of [`level_at`] over the levels a drag can write, which is the
/// property the drawing needs: the top of the fill is the height the pointer
/// was at. A level of zero gives an empty rect — a silent step is the track
/// alone ([`theme::VOLUME_TRACK`]), with nothing drawn over it.
pub fn fill_rect(grid: &StepGrid, step: u16, level: f32) -> Rect {
    let cell = grid.cell_rect(step);
    let height = sane(cell.height() * clamp_volume(level), 0.0, cell.height());

    Rect::from_min_max(pos2(cell.min.x, cell.max.y - height), cell.max)
}

/// What one frame of a draw gesture paints: the column and level the gesture
/// was at, and the column and level the pointer has reached.
///
/// The pair and not a list, because the list is derivable and a gesture must
/// not allocate per frame to describe itself: [`Stroke::levels`] walks the
/// columns between the two ends and ramps the level across them (the module's
/// §A fast drag leaves no gap). On the press both ends are the same column,
/// and the stroke is that one bar.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stroke {
    /// Where the gesture was: the column it last wrote, and the level it
    /// gave it. Not painted again — the ramp starts at the column after it.
    from: (u16, f32),
    /// Where the pointer is: the column and level this stroke ends on, which
    /// it always writes.
    to: (u16, f32),
}

impl Stroke {
    /// The stroke from the column a gesture last wrote to the column the
    /// pointer is in now. `from == to` is the press: one bar.
    pub fn new(from: (u16, f32), to: (u16, f32)) -> Stroke {
        Stroke { from, to }
    }

    /// The `(step, level)` pairs this stroke writes, in the order the pointer
    /// crossed them: every column from the one after the stroke's `from` to
    /// its `to` inclusive, with the level ramped straight between the two.
    ///
    /// Always at least one pair, and the last of them is always exactly the
    /// `to` end — the column under the pointer gets the level under the
    /// pointer, whatever the arithmetic did to the ones in between. Right to
    /// left as happily as left to right: the ramp follows the hand.
    pub fn levels(self) -> impl Iterator<Item = (u16, f32)> {
        let (first, start) = self.from;
        let (last, end) = self.to;
        let span = i32::from(last) - i32::from(first);
        // Zero columns crossed is the press, or a pointer still in the column
        // it was in: one pair, the column itself.
        let crossed = span.unsigned_abs().max(1);
        let direction = span.signum();

        (1..=crossed).map(move |n| {
            let step = i32::from(first) + direction * n as i32;
            let along = n as f32 / crossed as f32;

            (step.clamp(0, i32::from(u16::MAX)) as u16, quantise(start + (end - start) * along))
        })
    }
}

/// Paints `stroke` into a copy of a pattern's Volume row, returning whether
/// it changed anything.
///
/// `steps` is the row's length: a column past it is dropped rather than
/// written, which keeps the levels of a pattern that was shortened and
/// lengthened again (`Pattern::set_length` keeps them too). The `false` is
/// what stops a frame of a drag that reached no new level from publishing a
/// snapshot for nothing — most frames of a slow drag are that frame.
pub fn paint_into(volumes: &mut [f32; MAX_STEPS], steps: u16, stroke: Stroke) -> bool {
    let mut changed = false;

    for (step, level) in stroke.levels() {
        if step >= steps {
            continue;
        }
        if let Some(slot) = volumes.get_mut(usize::from(step)) {
            let level = clamp_volume(level);
            if *slot != level {
                *slot = level;
                changed = true;
            }
        }
    }

    changed
}

/// What a double-click puts a bar on `lane` back to: full volume, the
/// cutoff where the pattern's filter is open ([`Filter::open_cutoff`]), or
/// no resonance.
pub fn reset_level(lane: Lane, pattern: &Pattern) -> f32 {
    match lane {
        Lane::Volume => Pattern::DEFAULT_VOLUME,
        Lane::Cutoff => pattern.filter().open_cutoff(),
        Lane::Resonance => Lane::Resonance.default_level(),
    }
}

/// Puts one bar back to its default ([`reset_level`]), which is what a
/// double-click on it does. `false`, and nothing written, when it is there
/// already — a double-click on an untouched bar should not publish a snapshot
/// to change nothing.
///
/// Does not commit: the gesture it belongs to does
/// ([`VolumeDraw::settle`]), so the presses that led up to it and this are
/// one undo step.
pub fn reset(model: &StutterModel, index: usize, pattern: &Pattern, lane: Lane, step: u16) -> bool {
    let level = reset_level(lane, pattern);
    if pattern.level(lane, step) == level {
        return false;
    }

    model.set_level(index, lane, step, level)
}

/// The Volume row's half-made gesture, as the window holds it between frames.
///
/// Per opened window, like the Time row's ([`super::grid::TimeRowHands`]) and
/// for the same reason: it is a hand over this row rather than anything the
/// instance or the project holds, so it is built afresh in
/// `NiceEguiApp::build` — nice-plug-egui 0.6 never calls `editor_closed`, and
/// a window that reopened still drawing would ramp from a column the hand let
/// go of long ago.
///
/// Two states and not three: every point of this row is a column
/// ([`StepGrid::column_rect`] tiles it), so a press here always has
/// something to paint and there is no "pressed nothing" to tell from "no
/// press" — which is the one thing the Time row needs a third state for.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum VolumeDraw {
    /// No press on the row.
    #[default]
    Idle,
    /// A press is down and drawing: the column it last wrote, and the level
    /// it gave it — where the next frame's ramp starts from.
    Drawing {
        /// The column last written.
        at: u16,
        /// The level it was written at.
        level: f32,
    },
}

impl VolumeDraw {
    /// Whether a press is open on the row.
    ///
    /// What holds the window on the pattern the gesture started on
    /// ([`super::top_bar::edited_this_frame`]): with follow on, a host that
    /// changed pattern between two frames would otherwise move the write out
    /// from under the pointer, and the drag would finish by drawing levels
    /// into a pattern the user never touched.
    pub fn open(&self) -> bool {
        !matches!(self, VolumeDraw::Idle)
    }

    /// The column this gesture last wrote and the level it gave it, if it has
    /// written one.
    pub fn at(&self) -> Option<(u16, f32)> {
        match *self {
            VolumeDraw::Drawing { at, level } => Some((at, level)),
            VolumeDraw::Idle => None,
        }
    }

    /// The pointer is at `pos` with the button down: the stroke that paints,
    /// and the gesture moved on to it.
    ///
    /// The pure half of a frame of the gesture — the write is
    /// [`VolumeDraw::draw_to`] — and the whole of the pointer → (step, level)
    /// mapping this module is about: the column from
    /// [`StepGrid::step_at_x`] and the level from [`level_at`], both clamped
    /// onto the row, so a pointer dragged off any edge of the band keeps
    /// drawing instead of stopping dead.
    pub fn stroke(&mut self, grid: &StepGrid, pos: Pos2) -> Stroke {
        let to = (grid.step_at_x(pos.x), level_at(grid, pos.y));
        let from = self.at().unwrap_or(to);

        *self = VolumeDraw::Drawing { at: to.0, level: to.1 };

        Stroke::new(from, to)
    }

    /// Writes this frame's stroke through the writer API, returning whether
    /// it wrote. Deliberately does **not** commit: a drag is one undo step,
    /// taken when the hand lets go ([`VolumeDraw::settle`]).
    ///
    /// One write for the whole stroke, through
    /// [`StutterModel::set_lane`]: the row is taken out of this frame's
    /// snapshot, painted into on the stack ([`paint_into`]) and put back, so
    /// a sweep across the row publishes one snapshot rather than one per
    /// column. `pattern` is the one `index` names, read from the same
    /// snapshot the grid was built from.
    ///
    /// Drawing on the Cutoff or Res row of a pattern whose filter is off
    /// turns on a low-pass first, in the same gesture — drawing a cutoff is
    /// asking for a filter, and a row that changed nothing audible would
    /// read as broken.
    pub fn draw_to(
        &mut self,
        model: &StutterModel,
        grid: &StepGrid,
        index: usize,
        pattern: &Pattern,
        lane: Lane,
        pos: Pos2,
    ) -> bool {
        let stroke = self.stroke(grid, pos);
        let mut levels = *pattern.lane(lane);
        if !paint_into(&mut levels, grid.steps(), stroke) {
            return false;
        }
        if lane != Lane::Volume && pattern.filter() == Filter::Off {
            model.choose_filter(index, Filter::LowPass);
        }

        model.set_lane(index, lane, levels)
    }

    /// The hand let go: whatever the gesture wrote becomes one undo step, and
    /// the row is idle again. `true` if a press had been open.
    ///
    /// Cheap to call on every frame with no press on the row, which is how
    /// the drawing calls it: an idle row is a `matches!` and nothing else,
    /// and a gesture that ended on the levels it started with still records
    /// no step ([`StutterModel::commit_edits`] compares the patterns).
    pub fn settle(&mut self, model: &StutterModel) -> bool {
        if std::mem::replace(self, VolumeDraw::Idle) == VolumeDraw::Idle {
            return false;
        }

        model.commit_edits();
        true
    }
}

/// `level` as the whole percent the window says it is — the units Bitwig
/// shows a mix in, and the lattice a drag writes on ([`LEVEL_STEPS`]).
pub fn percent(level: f32) -> u32 {
    (clamp_volume(level) * LEVEL_STEPS).round() as u32
}

/// A cutoff as the window says it: "250 Hz", "1.2 kHz".
pub fn hz_label(hz: f32) -> String {
    match hz < 1_000.0 {
        true => format!("{} Hz", hz.round()),
        false => format!("{:.1} kHz", hz / 1_000.0),
    }
}

/// What the row says when the pointer rests on it: the step, the level it is
/// at, and the two gestures over the row — for whichever row it is showing,
/// and with a word on a filter that is off.
pub fn hover_text(lane: Lane, filter: Filter, step: u16, level: f32) -> String {
    let step = step + 1;
    let mut text = match lane {
        Lane::Volume => format!("Step {step}: {} %.\n{DRAW_HINT}", percent(level)),
        Lane::Cutoff => {
            format!("Step {step}: cutoff {}.\n{CUTOFF_HINT}", hz_label(cutoff_hz(level)))
        }
        Lane::Resonance => format!("Step {step}: resonance {} %.\n{RES_HINT}", percent(level)),
    };
    if lane != Lane::Volume && filter == Filter::Off {
        text.push_str(FILTER_OFF_HINT);
    }

    text
}

/// The gestures over the row, as the hover text puts them.
pub const DRAW_HINT: &str = "Drag across the row to draw levels — up for louder, down for \
    quieter.\n\
    Double-click a bar to put it back to 100 %.";

/// The gestures over the Cutoff row.
pub const CUTOFF_HINT: &str = "Drag across the row to draw the filter's cutoff — up for higher, \
    down for lower.\n\
    Double-click a bar to put it back where the filter is open.";

/// The gestures over the Res row.
pub const RES_HINT: &str = "Drag across the row to draw the filter's resonance — up for a \
    stronger peak at the cutoff.\n\
    Double-click a bar to put it back to none.";

/// Added to the Cutoff and Res rows' hover text when the pattern's filter is
/// off.
pub const FILTER_OFF_HINT: &str =
    "\nThe filter is off for this pattern: drawing here turns on Low-pass.";

/// The colour a bar on `lane` is filled with: dimmed on the Cutoff and Res
/// rows of a pattern whose filter is off, where the bars do nothing yet.
pub fn fill_colour(lane: Lane, filter: Filter) -> egui::Color32 {
    match (lane, filter) {
        (Lane::Volume, _) => theme::VOLUME_FILL,
        (Lane::Cutoff, Filter::Off) => theme::CUTOFF_FILL.gamma_multiply(0.35),
        (Lane::Cutoff, _) => theme::CUTOFF_FILL,
        (Lane::Resonance, Filter::Off) => theme::RESONANCE_FILL.gamma_multiply(0.35),
        (Lane::Resonance, _) => theme::RESONANCE_FILL,
    }
}

/// The egui id the row's own interaction is held against: one of this row per
/// window, so the id is a constant rather than anything the frame builds
/// (`super::grid::ui_time_row` has the argument).
fn volume_row_id() -> egui::Id {
    egui::Id::unique("stuttter-volume-row")
}

/// The Volume row, drawn: a bar per step of the pattern being edited, the
/// gesture that draws them, and the hover text that says what a press would
/// do.
///
/// The one impure thing in this module, and deliberately thin — every figure,
/// every rectangle, every string and every decision in it comes from above.
///
/// `pattern` is the one `index` names, read from this frame's snapshot; the
/// write goes through `model` and is therefore visible on the *next* frame,
/// as everywhere else in this window. `draw` is what the window holds over
/// this row between frames ([`VolumeDraw`]) — the only thing here that
/// outlives the frame.
pub(super) fn ui_volume_row(
    ui: &mut egui::Ui,
    row: Rect,
    model: &StutterModel,
    index: usize,
    pattern: &Pattern,
    lane: Lane,
    draw: &mut VolumeDraw,
) {
    let grid = StepGrid::for_pattern(row, pattern);
    let painter = ui.painter();
    let fill_with = fill_colour(lane, pattern.filter());

    // The track first and the fill over it, per column: a bar is the cell the
    // Time row would have drawn there, in the Volume row's two colours, so
    // the two rows read as one grid (`theme::VOLUME_TRACK`).
    for step in 0..grid.steps() {
        painter.rect_filled(grid.cell_rect(step), theme::CELL_CORNER, theme::VOLUME_TRACK);

        let fill = fill_rect(&grid, step, pattern.level(lane, step));
        if fill.is_positive() {
            painter.rect_filled(fill, theme::CELL_CORNER, fill_with);
        }
    }

    // A band the host squeezed to nothing is drawn and not interacted with:
    // an empty rect contains no point, so a press could only land on it by
    // egui's own rounding, and a row with no height has no level in it
    // ([`level_at`]).
    if !grid.row().is_positive() {
        return;
    }

    let response = ui.interact(grid.row(), volume_row_id(), egui::Sense::click_and_drag());

    // The primary button only, from the moment it goes down, for the reason
    // the Time row reads it the same way: a widget that senses clicks as well
    // makes egui hold `dragged` back until a click has been ruled out, and a
    // bar should follow the pointer from the first frame of the press.
    let primary = ui.ctx().input(|input| input.pointer.button_down(egui::PointerButton::Primary));
    let holding = response.is_pointer_button_down_on() && primary;

    // The double-click, before the release below commits: the press that
    // began it has already drawn the bar at the pointer's height, and both
    // writes belong to the one gesture that is about to become an undo step
    // (the module's §One undo step per gesture).
    if response.double_clicked()
        && let Some(pos) = response.interact_pointer_pos()
    {
        reset(model, index, pattern, lane, grid.step_at_x(pos.x));
    }

    // `interact_pointer_pos` is where the pointer is *now* while a button is
    // down on the row, and where the hand let go on the frame of a release
    // (egui 0.37 `Context::interact`).
    match response.interact_pointer_pos().filter(|_| holding) {
        // The first of these frames is the press and the rest are the drag;
        // the gesture tells them apart itself, by whether it has written a
        // column yet ([`VolumeDraw::stroke`]).
        Some(pos) => {
            draw.draw_to(model, &grid, index, pattern, lane, pos);
        }
        // Every way a press can end leaves the button no longer down on the
        // row, so this is also where a finished gesture becomes its undo
        // step.
        None => {
            draw.settle(model);
        }
    }

    // Up and down is what this row's pointer does, which is what the shape
    // says — over the row between gestures, and for as long as one is open
    // even if the pointer has left the band.
    if response.hovered() || draw.open() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
    }

    // And the text, between gestures only: a hand that is drawing already
    // knows what it is drawing, and the level under the pointer is a frame
    // out of date by the time a tooltip could say it.
    if let Some(pos) = response.hover_pos().filter(|_| !draw.open()) {
        let step = grid.step_at_x(pos.x);
        let level = pattern.level(lane, step);
        response.on_hover_text(hover_text(lane, pattern.filter(), step, level));
    }
}

/// A tab's label in the strip over the row.
pub fn lane_label(lane: Lane) -> &'static str {
    match lane {
        Lane::Volume => "Volume",
        Lane::Cutoff => "Cutoff",
        Lane::Resonance => "Res",
    }
}

/// What a tab says when the pointer rests on it.
pub fn lane_hover(lane: Lane) -> &'static str {
    match lane {
        Lane::Volume => "Show the Volume row: the level of each step.",
        Lane::Cutoff => "Show the Cutoff row: the filter's cutoff on each step.",
        Lane::Resonance => "Show the Res row: the filter's resonance on each step.",
    }
}

/// A filter shape as the dropdown lists it.
pub fn filter_label(filter: Filter) -> &'static str {
    match filter {
        Filter::Off => "Off",
        Filter::LowPass => "Low-pass",
        Filter::HighPass => "High-pass",
        Filter::BandPass => "Band-pass",
    }
}

/// What the Filter dropdown says, closed.
pub fn filter_text(filter: Filter) -> String {
    format!("Filter: {}", filter_label(filter))
}

/// What the Filter dropdown says when the pointer rests on it.
pub const FILTER_HOVER: &str = "This pattern's filter. It works like the Volume row: only while \
    the pattern plays, with the cutoff and resonance of each step drawn on the Cutoff and Res \
    rows.\n\
    Low-pass cuts above the cutoff, High-pass below it, Band-pass keeps a band around it.";

/// A filter picked in the dropdown, written to the pattern being edited as
/// one undo step ([`StutterModel::choose_filter`]). `false`, and nothing
/// written, for a pick of the filter it already has.
pub fn choose_filter(model: &StutterModel, index: usize, showing: Filter, chosen: Filter) -> bool {
    if chosen == showing || !model.choose_filter(index, chosen) {
        return false;
    }

    model.commit_edits();
    true
}

/// The strip cut into its slots: the three tabs from the left, the Filter
/// dropdown against the right edge, each emptied rather than overlapping
/// when the strip is too narrow.
pub fn lane_bar_slots(bar: Rect) -> ([Rect; 3], Rect) {
    let left = sane(bar.min.x, f32::MIN, f32::MAX);
    let right = left + sane(bar.width(), 0.0, f32::MAX);
    let (top, bottom) = (bar.min.y, bar.min.y + sane(bar.height(), 0.0, f32::MAX));
    let span = |from: f32, to: f32| Rect::from_min_max(pos2(from, top), pos2(to, bottom));

    let mut tabs = [Rect::NOTHING; 3];
    let mut x = left;
    for tab in &mut tabs {
        let end = sane(x + theme::LANE_TAB_WIDTH, x, right);
        *tab = span(x, end);
        x = sane(end + theme::ITEM_SPACING, end, right);
    }
    let filter_left = sane(right - theme::FILTER_WIDTH, x, right);

    (tabs, span(filter_left, right))
}

/// The strip over the row: three tabs that pick which of the pattern's rows
/// of levels the row shows, and the pattern's Filter dropdown.
///
/// `lane` is the window's own choice and outlives the frame; the filter is
/// the pattern's and is written through the model.
pub(super) fn ui_lane_bar(
    ui: &mut egui::Ui,
    bar: Rect,
    model: &StutterModel,
    index: usize,
    pattern: &Pattern,
    lane: &mut Lane,
) {
    if !bar.is_positive() {
        return;
    }
    let (tabs, filter_slot) = lane_bar_slots(bar);

    ui.scope_builder(egui::UiBuilder::new().max_rect(bar), |ui| {
        ui.spacing_mut().interact_size.y = theme::LANE_BAR_HEIGHT;

        for (rect, tab) in tabs.into_iter().zip(Lane::ALL) {
            if !rect.is_positive() {
                continue;
            }
            let button = egui::Button::new(lane_label(tab)).selected(*lane == tab).truncate();
            if ui.put(rect, button).on_hover_text(lane_hover(tab)).clicked() {
                *lane = tab;
            }
        }

        if !filter_slot.is_positive() {
            return;
        }
        let showing = pattern.filter();
        let mut chosen = showing;
        ui.scope_builder(egui::UiBuilder::new().max_rect(filter_slot), |ui| {
            egui::ComboBox::from_id_salt("stuttter-filter")
                .width(filter_slot.width())
                .selected_text(filter_text(showing))
                .truncate()
                .show_ui(ui, |ui| {
                    for filter in Filter::ALL {
                        ui.selectable_value(&mut chosen, filter, filter_label(filter));
                    }
                })
                .response
                .on_hover_text(FILTER_HOVER);
        });
        choose_filter(model, index, showing, chosen);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::layout::EditorLayout;
    use crate::editor::{DEFAULT_HEIGHT, DEFAULT_WIDTH, GRID_WIDTH};
    use crate::model::StutterData;
    use egui::vec2;

    /// The Volume row of the window at the size it opens at, taken from the
    /// layout rather than written out — the band the bars are really drawn
    /// in.
    fn volume_row() -> Rect {
        let width = crate::editor::STEPS_SHOWN * theme::CELL_PITCH - theme::CELL_GAP;
        Rect::from_min_size(pos2(23.0, 120.0), vec2(width, theme::CELL_HEIGHT))
    }

    /// The row's grid at `steps` columns.
    fn grid(steps: u16) -> StepGrid {
        StepGrid::new(volume_row(), steps)
    }

    /// A model holding one pattern of `steps` steps with a committed
    /// starting point, as the editor leaves it after its first frame, and
    /// the index of that pattern.
    fn model(steps: u16) -> (StutterModel, usize) {
        let model = StutterModel::new(StutterData::new());
        model.set_length(0, steps).expect("the first pattern");
        model.commit_edits();
        assert!(!model.can_undo(), "nothing to step back into yet");

        (model, 0)
    }

    /// The pattern a test writes to, as the frame after a write sees it.
    fn pattern(model: &StutterModel, index: usize) -> Pattern {
        model.data().pattern(index).expect("a pattern").clone()
    }

    /// The row the window draws into is the same columns as the Time row, at
    /// the same width: the two bands are one x-axis (`grid`'s §A long
    /// pattern is scaled).
    #[test]
    fn the_bars_sit_on_the_time_rows_own_columns() {
        let size = vec2(DEFAULT_WIDTH, DEFAULT_HEIGHT);
        let window = EditorLayout::new(Rect::from_min_size(pos2(17.0, 5.0), size));

        for steps in [1_u16, 16, 17, 64] {
            let bars = StepGrid::new(window.volume_row(), steps);
            let cells = StepGrid::new(window.time_row(), steps);

            assert_eq!(bars.row().width(), GRID_WIDTH, "{steps} steps");
            assert_eq!(bars.pitch(), cells.pitch(), "{steps} steps");
            for step in 0..steps {
                assert_eq!(
                    bars.cell_rect(step).x_range(),
                    cells.cell_rect(step).x_range(),
                    "{steps} steps: column {step}"
                );
            }
        }
    }

    /// Pointer height → level: the row's bottom edge is silence, its top edge
    /// is full, and the middle is half.
    #[test]
    fn the_pointers_height_is_the_level() {
        let grid = grid(16);
        let row = grid.row();

        assert_eq!(level_at(&grid, row.max.y), 0.0);
        assert_eq!(level_at(&grid, row.min.y), 1.0);
        assert_eq!(level_at(&grid, row.center().y), 0.5);

        // A quarter of the way up from the bottom, which with a thirty-point
        // row lands on a whole percent of its own accord.
        assert_eq!(level_at(&grid, row.max.y - 0.25 * row.height()), 0.25);

        // Levels rise upwards, which is the direction the fill grows in.
        let mut last = 0.0;
        for step in 0..=30 {
            let y = row.max.y - step as f32;
            let level = level_at(&grid, y);
            assert!(level >= last, "level fell from {last} to {level} at y {y}");
            last = level;
        }
        assert_eq!(last, 1.0);
    }

    /// A pointer off the band's top or bottom keeps drawing at full or at
    /// silence rather than stopping dead, and nonsense reads as full.
    #[test]
    fn a_pointer_off_the_row_clamps_onto_it() {
        let grid = grid(16);
        let row = grid.row();

        assert_eq!(level_at(&grid, row.min.y - 400.0), 1.0);
        assert_eq!(level_at(&grid, row.max.y + 400.0), 0.0);
        assert_eq!(level_at(&grid, f32::NEG_INFINITY), 1.0);
        assert_eq!(level_at(&grid, f32::INFINITY), 0.0);
        assert_eq!(level_at(&grid, f32::NAN), Pattern::DEFAULT_VOLUME);

        // And a band with no height at all, which the window never
        // interacts with ([`ui_volume_row`]).
        let squeezed = StepGrid::new(Rect::from_min_size(row.min, vec2(row.width(), 0.0)), 16);
        assert_eq!(level_at(&squeezed, row.min.y), Pattern::DEFAULT_VOLUME);
    }

    /// Every level a drag can write is on the whole-percent lattice, and
    /// nothing outside `0..=1` survives the mapping (the module's §Levels
    /// land on whole percent).
    #[test]
    fn levels_land_on_whole_percent() {
        assert_eq!(LEVEL_STEPS, 100.0);

        let grid = grid(16);
        let row = grid.row();

        // Every tenth of a point down the row, which is finer than the
        // pointer can be.
        for tick in 0..=(10 * row.height() as i32) {
            let level = level_at(&grid, row.min.y + tick as f32 / 10.0);

            assert!((0.0..=1.0).contains(&level), "{level} at tick {tick}");
            // Quantising is idempotent, which is what "on the lattice"
            // means for a number that is a hundredth: 0.6 is not exactly
            // 60/100 in binary, but it is the nearest f32 to it and rounding
            // it again cannot move it.
            assert_eq!(quantise(level), level, "{level} at tick {tick}");
        }

        for (asked, got) in [(-1.0, 0.0), (0.0, 0.0), (0.004, 0.0), (0.006, 0.01), (1.0, 1.0)] {
            assert_eq!(quantise(asked), got, "{asked}");
        }
        assert_eq!(quantise(f32::NAN), 0.0, "a NaN is clamped, not rounded");
        assert_eq!(quantise(f32::INFINITY), 1.0);
    }

    /// The fill is [`level_at`] read back: the top of the bar is the height
    /// the pointer was at, and the bar is its column's own cell.
    #[test]
    fn the_fill_is_the_pointers_height_read_back() {
        let grid = grid(16);
        let row = grid.row();

        for step in [0_u16, 7, 15] {
            let cell = grid.cell_rect(step);

            // Silence draws nothing; full fills the cell.
            assert!(!fill_rect(&grid, step, 0.0).is_positive(), "step {step}");
            assert_eq!(fill_rect(&grid, step, 1.0), cell, "step {step}");

            for tick in 0..=30 {
                let y = row.max.y - tick as f32;
                let fill = fill_rect(&grid, step, level_at(&grid, y));

                assert_eq!(fill.max.y, cell.max.y, "step {step}: the bar grows from the bottom");
                assert_eq!(fill.x_range(), cell.x_range(), "step {step}: the bar is its cell");
                assert!(cell.contains_rect(fill), "step {step}: the bar is inside its cell");
                assert!((fill.min.y - y).abs() <= 0.5, "step {step}: {} is not {y}", fill.min.y);
            }
        }

        // And nonsense cannot draw outside the cell either.
        let cell = grid.cell_rect(3);
        assert_eq!(fill_rect(&grid, 3, f32::NAN).height(), 0.0);
        assert_eq!(fill_rect(&grid, 3, f32::INFINITY), cell);
    }

    /// A press paints one bar: the column under the pointer, at the level
    /// under the pointer, and nothing else.
    #[test]
    fn a_press_paints_the_bar_under_it() {
        let grid = grid(16);
        let row = grid.row();
        let mut draw = VolumeDraw::default();
        assert!(!draw.open());

        let pos = pos2(grid.cell_rect(5).center().x, row.max.y - 0.5 * row.height());
        let stroke = draw.stroke(&grid, pos);

        assert_eq!(stroke.from, (5, 0.5));
        assert_eq!(stroke.to, (5, 0.5));
        assert_eq!(stroke.levels().collect::<Vec<_>>(), vec![(5, 0.5)]);
        assert_eq!(draw.at(), Some((5, 0.5)));
        assert!(draw.open());
    }

    /// A pointer that stays in one column and moves up and down redraws that
    /// one bar.
    #[test]
    fn a_vertical_move_inside_one_column_redraws_its_bar() {
        let grid = grid(16);
        let row = grid.row();
        let x = grid.cell_rect(9).center().x;
        let mut draw = VolumeDraw::default();

        draw.stroke(&grid, pos2(x, row.max.y));
        assert_eq!(draw.at(), Some((9, 0.0)));

        let stroke = draw.stroke(&grid, pos2(x, row.min.y));
        assert_eq!(stroke.levels().collect::<Vec<_>>(), vec![(9, 1.0)]);
        assert_eq!(draw.at(), Some((9, 1.0)));
    }

    /// A pointer that crossed several columns since the last frame fills in
    /// every one of them, ramped from the level it left to the level it
    /// reached (the module's §A fast drag leaves no gap).
    #[test]
    fn a_fast_drag_ramps_across_the_columns_it_jumped() {
        let grid = grid(16);
        let row = grid.row();
        let mut draw = VolumeDraw::default();

        // Press at step 2, silence.
        draw.stroke(&grid, pos2(grid.cell_rect(2).center().x, row.max.y));
        // One frame later the pointer is at step 6, full.
        let stroke = draw.stroke(&grid, pos2(grid.cell_rect(6).center().x, row.min.y));

        assert_eq!(
            stroke.levels().collect::<Vec<_>>(),
            vec![(3, 0.25), (4, 0.5), (5, 0.75), (6, 1.0)],
            "the column it was on keeps its level; the four it reached ramp"
        );

        // And the same sweep backwards, which is the same ramp the other way.
        let mut back = VolumeDraw::default();
        back.stroke(&grid, pos2(grid.cell_rect(6).center().x, row.min.y));
        let stroke = back.stroke(&grid, pos2(grid.cell_rect(2).center().x, row.max.y));

        assert_eq!(
            stroke.levels().collect::<Vec<_>>(),
            vec![(5, 0.75), (4, 0.5), (3, 0.25), (2, 0.0)]
        );
    }

    /// However far the pointer jumps, the column it is in gets exactly the
    /// level it is at, and every pair is a real column on the whole-percent
    /// lattice.
    #[test]
    fn a_stroke_always_ends_under_the_pointer() {
        for (first, last) in [(0_u16, 63_u16), (63, 0), (7, 8), (8, 7), (31, 31)] {
            for (start, end) in [(0.0, 1.0), (1.0, 0.0), (0.37, 0.37), (0.99, 0.01)] {
                let stroke = Stroke::new((first, start), (last, end));
                let painted = stroke.levels().collect::<Vec<_>>();

                assert_eq!(painted.last().copied(), Some((last, end)), "{first}->{last}");
                assert_eq!(
                    painted.len(),
                    usize::from(first.abs_diff(last).max(1)),
                    "{first}->{last}"
                );
                for &(step, level) in &painted {
                    assert!(step < 64, "{first}->{last}: step {step}");
                    assert_eq!(quantise(level), level, "{first}->{last}: level {level}");
                    assert!(
                        level >= start.min(end) - 0.01 && level <= start.max(end) + 0.01,
                        "{first}->{last}: {level} is outside the ramp"
                    );
                }
                // No column is painted twice, and they are walked in order.
                let mut steps = painted.iter().map(|&(step, _)| step).collect::<Vec<_>>();
                let count = steps.len();
                steps.sort_unstable();
                steps.dedup();
                assert_eq!(steps.len(), count, "{first}->{last}: a column twice");
            }
        }
    }

    /// Painting writes only the columns of the stroke, only inside the
    /// pattern's length, and says whether anything changed.
    #[test]
    fn painting_writes_the_strokes_columns_and_nothing_else() {
        let mut volumes = [Pattern::DEFAULT_VOLUME; MAX_STEPS];

        assert!(paint_into(&mut volumes, 16, Stroke::new((2, 0.5), (4, 0.1))));
        assert_eq!(volumes[2], 1.0, "the column it started on is not repainted");
        assert_eq!(volumes[3], 0.3);
        assert_eq!(volumes[4], 0.1);
        assert!(volumes[5..].iter().all(|&level| level == 1.0));

        // The same stroke again changes nothing, so nothing is published.
        assert!(!paint_into(&mut volumes, 16, Stroke::new((2, 0.5), (4, 0.1))));

        // A column past the row's end is dropped rather than written: the
        // levels of a pattern that was shortened are kept.
        volumes[20] = 0.75;
        assert!(!paint_into(&mut volumes, 16, Stroke::new((18, 0.0), (20, 0.0))));
        assert_eq!(volumes[20], 0.75);

        // And nonsense is clamped on the way in, never stored.
        assert!(paint_into(&mut volumes, 16, Stroke::new((0, f32::NAN), (0, f32::NAN))));
        assert_eq!(volumes[0], 0.0);
    }

    /// A drag across the row writes through the model as it goes and is one
    /// undo step when the hand lets go.
    #[test]
    fn a_drag_across_the_row_is_one_undo_step() {
        let (model, index) = model(16);
        let grid = grid(16);
        let row = grid.row();
        let mut draw = VolumeDraw::default();

        // Press on step 0 at 90 %, then sweep to step 15 at silence, a
        // column at a time as a slow drag would. Below full all the way, so
        // every frame of it really has something to write (a bar already at
        // the level the pointer asks for is not written again).
        let y_at = |level: f32| row.max.y - level * row.height();
        for step in 0..16_u16 {
            let level = 0.9 * (1.0 - f32::from(step) / 15.0);
            let pos = pos2(grid.cell_rect(step).center().x, y_at(level));

            let showing = pattern(&model, index);
            assert!(draw.draw_to(&model, &grid, index, &showing, Lane::Volume, pos), "step {step}");
        }

        let drawn = pattern(&model, index);
        assert_eq!(drawn.volume(0), 0.9);
        assert_eq!(drawn.volume(15), 0.0);
        for step in 0..16_u16 {
            let level = drawn.volume(step);
            assert_eq!(quantise(level), level, "step {step}");
            assert!(level <= drawn.volume(step.saturating_sub(1)), "step {step} rose");
        }

        // Nothing is an undo step until the hand lets go, and then it is one.
        assert!(!model.can_undo(), "a drag commits nothing while it is open");
        assert!(draw.settle(&model));
        assert!(!draw.open());
        assert!(model.can_undo());

        assert!(model.undo());
        let back = pattern(&model, index);
        assert!((0..16).all(|step| back.volume(step) == Pattern::DEFAULT_VOLUME), "one step back");
        assert!(!model.can_undo(), "the whole drag was that one step");
    }

    /// A frame of a drag that reached no new level writes nothing, and a
    /// gesture that ended where it started is no undo step at all.
    #[test]
    fn a_gesture_that_changed_nothing_leaves_no_step() {
        let (model, index) = model(16);
        let grid = grid(16);
        let pos = pos2(grid.cell_rect(4).center().x, grid.row().min.y);
        let mut draw = VolumeDraw::default();

        // Full is where every bar already is, so the press writes nothing.
        assert!(!draw.draw_to(&model, &grid, index, &pattern(&model, index), Lane::Volume, pos));
        assert!(draw.open(), "the press is still open");
        assert!(draw.settle(&model));
        assert!(!model.can_undo());

        // A second frame on the same column at the same level is just as
        // quiet.
        let mut draw = VolumeDraw::default();
        let lower = pos2(pos.x, grid.row().center().y);
        assert!(draw.draw_to(&model, &grid, index, &pattern(&model, index), Lane::Volume, lower));
        assert!(!draw.draw_to(&model, &grid, index, &pattern(&model, index), Lane::Volume, lower));
        draw.settle(&model);
        assert_eq!(pattern(&model, index).volume(4), 0.5);
    }

    /// A drag only ever touches the pattern it is drawn on, and only its own
    /// steps: the other fifteen patterns, and the blocks of this one, are
    /// left alone.
    #[test]
    fn a_drag_leaves_everything_else_alone() {
        let (model, index) = model(8);
        let grid = grid(8);
        let before = model.data();
        let mut draw = VolumeDraw::default();

        let pos = pos2(grid.cell_rect(7).center().x, grid.row().max.y);
        draw.draw_to(&model, &grid, index, &pattern(&model, index), Lane::Volume, pos);
        draw.settle(&model);

        let after = model.data();
        for other in 1..crate::pattern::PATTERN_COUNT {
            assert_eq!(
                after.pattern(other),
                before.pattern(other),
                "pattern {other} was touched"
            );
        }

        let drawn = after.pattern(index).expect("a pattern");
        assert_eq!(drawn.volume(7), 0.0);
        assert_eq!(drawn.length(), 8);
        assert_eq!(drawn.rate(), before.pattern(index).expect("a pattern").rate());
        assert_eq!(drawn.blocks(), before.pattern(index).expect("a pattern").blocks());
    }

    /// A double-click puts one bar back to full, and only that bar.
    #[test]
    fn a_double_click_resets_one_bar_to_full() {
        let (model, index) = model(16);
        let grid = grid(16);
        let mut draw = VolumeDraw::default();

        // Draw two bars down first.
        for step in [3_u16, 4] {
            let pos = pos2(grid.cell_rect(step).center().x, grid.row().max.y);
            draw.draw_to(&model, &grid, index, &pattern(&model, index), Lane::Volume, pos);
        }
        draw.settle(&model);
        assert_eq!(pattern(&model, index).volume(3), 0.0);

        // The double-click's own press draws step 3 at the pointer's height,
        // and the reset over it is what the bar keeps.
        let pos = pos2(grid.cell_rect(3).center().x, grid.row().center().y);
        draw.draw_to(&model, &grid, index, &pattern(&model, index), Lane::Volume, pos);
        assert_eq!(pattern(&model, index).volume(3), 0.5);

        assert!(reset(&model, index, &pattern(&model, index), Lane::Volume, 3));
        draw.settle(&model);

        let after = pattern(&model, index);
        assert_eq!(after.volume(3), Pattern::DEFAULT_VOLUME);
        assert_eq!(after.volume(4), 0.0, "its neighbour is left alone");

        // One undo step for the press and the reset together, and it goes
        // back to the level the bar had before the double-click.
        assert!(model.undo());
        assert_eq!(pattern(&model, index).volume(3), 0.0);
    }

    /// A bar that is full already is not written again — a double-click on an
    /// untouched bar publishes nothing and is no undo step.
    #[test]
    fn resetting_a_full_bar_writes_nothing() {
        let (model, index) = model(16);

        assert!(!reset(&model, index, &pattern(&model, index), Lane::Volume, 0));
        model.commit_edits();
        assert!(!model.can_undo());

        // A step outside the row reads as full ([`Pattern::volume`]), so a
        // reset aimed past the end of the row writes nothing either — and
        // `Pattern::set_volume` would drop it in any case.
        let past = MAX_STEPS as u16 + 10;
        assert!(!reset(&model, index, &pattern(&model, index), Lane::Volume, past));
        model.commit_edits();
        assert!(!model.can_undo());
    }

    /// The hover text names the step as the window numbers them (from one)
    /// and the level as the percentage it is.
    #[test]
    fn the_hover_text_says_the_step_and_its_level() {
        assert_eq!(percent(0.0), 0);
        assert_eq!(percent(0.5), 50);
        assert_eq!(percent(1.0), 100);
        assert_eq!(percent(f32::NAN), 0);

        let text = hover_text(Lane::Volume, Filter::Off, 0, 1.0);
        assert!(text.starts_with("Step 1: 100 %."), "{text}");
        assert!(text.contains("Double-click"), "{text}");
        assert!(hover_text(Lane::Volume, Filter::Off, 15, 0.25).starts_with("Step 16: 25 %."));
    }

    /// A window that reopened starts with no half-made gesture, which is what
    /// `NiceEguiApp::build` resets ([`VolumeDraw`]).
    #[test]
    fn a_fresh_row_holds_no_gesture() {
        let draw = VolumeDraw::default();

        assert_eq!(draw, VolumeDraw::Idle);
        assert!(!draw.open());
        assert_eq!(draw.at(), None);

        // And settling one that never pressed is nothing at all.
        let (model, _) = model(16);
        let mut idle = VolumeDraw::default();
        assert!(!idle.settle(&model));
        assert!(!model.can_undo());
    }

    /// Drawing on the Cutoff row writes that row and not the Volume row, and
    /// on a pattern whose filter is off it turns a low-pass on — all in the
    /// one gesture, so one Undo takes the lot back.
    #[test]
    fn drawing_a_cutoff_turns_the_filter_on_in_the_same_gesture() {
        let (model, index) = model(16);
        let grid = grid(16);
        let mut draw = VolumeDraw::default();
        let low = pos2(grid.cell_rect(2).center().x, grid.row().max.y);

        assert!(draw.draw_to(&model, &grid, index, &pattern(&model, index), Lane::Cutoff, low));
        assert!(draw.settle(&model));

        let drawn = pattern(&model, index);
        assert_eq!(drawn.filter(), Filter::LowPass);
        assert_eq!(drawn.level(Lane::Cutoff, 2), 0.0);
        assert!(drawn.volumes().iter().all(|&level| level == 1.0), "the Volume row moved");

        assert!(model.undo());
        let undone = pattern(&model, index);
        assert_eq!(undone.filter(), Filter::Off, "one undo step for the gesture");
        assert_eq!(undone.level(Lane::Cutoff, 2), 1.0);
    }

    /// A double-click puts a bar back where its row does least: full volume,
    /// the cutoff where the filter is open, no resonance.
    #[test]
    fn a_double_click_puts_each_row_back_to_its_own_default() {
        let (model, index) = model(16);
        model.choose_filter(index, Filter::HighPass);
        model.set_level(index, Lane::Cutoff, 4, 0.7);
        model.set_level(index, Lane::Resonance, 4, 0.7);
        let showing = pattern(&model, index);

        assert_eq!(reset_level(Lane::Volume, &showing), 1.0);
        assert_eq!(reset_level(Lane::Cutoff, &showing), 0.0, "a high-pass is open at the bottom");
        assert_eq!(reset_level(Lane::Resonance, &showing), 0.0);
        assert!(reset(&model, index, &showing, Lane::Cutoff, 4));
        assert!(reset(&model, index, &showing, Lane::Resonance, 4));
        let reset_to = pattern(&model, index);
        assert_eq!(reset_to.level(Lane::Cutoff, 4), 0.0);
        assert_eq!(reset_to.level(Lane::Resonance, 4), 0.0);
        assert!(!reset(&model, index, &reset_to, Lane::Resonance, 4), "already there");
    }

    /// A pick in the Filter dropdown is one undo step, moves an untouched
    /// Cutoff row to where the new filter is open, and a pick of the filter
    /// already there writes nothing.
    #[test]
    fn picking_a_filter_is_one_undo_step() {
        let (model, index) = model(16);
        assert!(!choose_filter(&model, index, Filter::Off, Filter::Off));
        assert!(!model.can_undo());

        assert!(choose_filter(&model, index, Filter::Off, Filter::HighPass));
        let picked = pattern(&model, index);
        assert_eq!(picked.filter(), Filter::HighPass);
        assert!(picked.lane(Lane::Cutoff).iter().all(|&level| level == 0.0));

        assert!(model.undo());
        assert_eq!(pattern(&model, index).filter(), Filter::Off);
    }

    /// The hover text says the row's own units, and on a filter row of a
    /// pattern with the filter off, that drawing turns it on.
    #[test]
    fn each_row_says_its_own_units() {
        assert_eq!(hz_label(250.4), "250 Hz");
        assert_eq!(hz_label(1_234.0), "1.2 kHz");
        assert_eq!(hz_label(20_000.0), "20.0 kHz");

        let cutoff = hover_text(Lane::Cutoff, Filter::LowPass, 0, 1.0);
        assert!(cutoff.starts_with("Step 1: cutoff 20.0 kHz."), "{cutoff}");
        assert!(!cutoff.contains(FILTER_OFF_HINT));
        let off = hover_text(Lane::Resonance, Filter::Off, 3, 0.5);
        assert!(off.starts_with("Step 4: resonance 50 %."), "{off}");
        assert!(off.ends_with(FILTER_OFF_HINT));
        assert!(!hover_text(Lane::Volume, Filter::Off, 0, 1.0).contains(FILTER_OFF_HINT));
        assert_eq!(fill_colour(Lane::Volume, Filter::Off), theme::VOLUME_FILL);
        assert_ne!(fill_colour(Lane::Cutoff, Filter::Off), theme::CUTOFF_FILL, "dimmed");
        assert_eq!(fill_colour(Lane::Cutoff, Filter::BandPass), theme::CUTOFF_FILL);
    }

    /// The strip: three tabs from the left, the dropdown against the right
    /// edge, nothing overlapping, and a strip too narrow empties from the
    /// right rather than spilling.
    #[test]
    fn the_strip_holds_three_tabs_and_the_dropdown() {
        let window = EditorLayout::new(Rect::from_min_size(
            pos2(0.0, 0.0),
            vec2(crate::editor::MIN_WIDTH, crate::editor::MIN_HEIGHT),
        ));
        let bar = window.lane_bar();
        let (tabs, filter) = lane_bar_slots(bar);

        assert_eq!(tabs[0].min.x, bar.min.x);
        for tab in tabs {
            assert_eq!(tab.width(), theme::LANE_TAB_WIDTH);
            assert!(bar.contains_rect(tab));
        }
        assert_eq!(filter.width(), theme::FILTER_WIDTH, "room for all of it at the minimum");
        assert_eq!(filter.max.x, bar.max.x);
        assert!(filter.min.x > tabs[2].max.x);

        for width in [200.0, 100.0, 0.0] {
            let narrow = Rect::from_min_size(bar.min, vec2(width, bar.height()));
            let (tabs, filter) = lane_bar_slots(narrow);
            let all = [tabs[0], tabs[1], tabs[2], filter];
            for (index, rect) in all.iter().enumerate() {
                assert!(narrow.contains_rect(*rect), "{index} at {width}");
                for other in &all[index + 1..] {
                    assert!(!rect.intersect(*other).is_positive(), "{index} at {width}");
                }
            }
        }
    }
}
