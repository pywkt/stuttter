//! Editing one block: the handlers behind the block panel
//! ([`super::panel`]), the selection, and removing a block.
//!
//! A block is made and shaped on the Time row ([`super::grid`]); everything
//! else about it is here — its preset, loop, source and speeds, a copy of it,
//! and taking it off the row. Every handler takes the model and nothing of
//! egui's, so each is unit-tested with no window, and each is **one undo
//! step**, committed where it is applied. The two speed boxes are the
//! exception: they are dragged, so they write through on every frame and
//! commit when the hand lets go ([`BlockEditor::settle`]).
//!
//! # Choosing is what writes the memory
//!
//! [`super::grid::action_to_add`] adds "the last-used block", and the panel
//! is the only thing that *chooses* one: every handler that sets an action
//! calls [`super::grid::remember_action`] with it, so the next click on an
//! empty step adds the block last dialled in here, on every pattern, and the
//! project remembers it. A pick that changes nothing still chooses.

use super::grid::{BlockKind, GridHit, remember_action};
use crate::actions::SpeedPreset;
use crate::model::StutterModel;
use crate::pattern::{Action, MAX_SOURCE_PASSES, NoteValue, Pattern, Rate, Source};
use crate::state::PrefsCell;

// ---- what the menu says ----

/// The six one-click presets at the top of the menu, in the order they are
/// listed. Each fills in the block's loop and speeds; the source is kept.
pub const PRESET_KINDS: [BlockKind; 6] = [
    BlockKind::Repeat,
    BlockKind::Reverse,
    BlockKind::HalfSpeed,
    BlockKind::DoubleSpeed,
    BlockKind::TapeStop,
    BlockKind::TapeStart,
];

/// The presets' item labels, in [`PRESET_KINDS`]' order.
pub const KIND_LABELS: [&str; PRESET_KINDS.len()] =
    ["Repeat", "Reverse", "Half speed", "Double speed", "Tape stop", "Tape start"];

/// The presets' hover texts, in [`PRESET_KINDS`]' order.
pub const KIND_HOVERS: [&str; PRESET_KINDS.len()] = [
    "Loop one step of audio over and over at live speed — the classic stutter. \
     Set the loop length, where it reads from and its speed below.",
    "Run backwards at pitch through the audio before the block's source.",
    "Play on from the source at half speed, an octave down.",
    "Play at double speed an octave up; from the block's start it arrives back \
     exactly on the live input as the block ends.",
    "Start at normal speed and slow smoothly to a standstill, pitch falling \
     with it.",
    "Start from a standstill and speed smoothly up to normal, pitch rising \
     with it.",
];

/// The loop setting's name in the menu.
pub const LOOP_LABEL: &str = "Loop";

/// The loop setting's hover text.
pub const LOOP_HOVER: &str = "How long one repeat is: every loop length the read head goes \
    back to where it started. 1/1 is a whole bar, down to 1/64, plus triplets. \
    Off lets the head run on through the block.";

/// What the loop menu calls "no loop".
pub const LOOP_OFF_LABEL: &str = "Off";

/// The note values' labels, one per [`NoteValue`] in [`NoteValue::ALL`]'s
/// order. A test holds these against [`NoteValue`]'s own `Display`.
pub const NOTE_LABELS: [&str; NoteValue::ALL.len()] = [
    "1/1", "1/1T", "1/2", "1/2T", "1/4", "1/4T", "1/8", "1/8T", "1/16", "1/16T", "1/32", "1/32T",
    "1/64", "1/64T",
];

/// The source setting's name in the menu.
pub const SOURCE_LABEL: &str = "Source";

/// The source setting's hover text.
pub const SOURCE_HOVER: &str = "Where the block starts reading: at its own start (the first \
    loop plays what would have played live), a fixed spot earlier in the song (drag the box \
    on the waveform to move it), or wherever the block just before it left off — so a tape \
    stop can slow down the beat a repeat was looping.";

/// The three source buttons.
pub const SOURCE_START_LABEL: &str = "Block start";
/// See [`SOURCE_START_LABEL`].
pub const SOURCE_SPOT_LABEL: &str = "Fixed spot";
/// See [`SOURCE_START_LABEL`].
pub const SOURCE_CONTINUE_LABEL: &str = "Continue";

/// The three source buttons' hover texts.
pub const SOURCE_START_HOVER: &str =
    "Read from the block's own start: the first time round plays what would have played live.";
/// See [`SOURCE_START_HOVER`].
pub const SOURCE_SPOT_HOVER: &str = "Read from a fixed step of the pattern, so the repeat is \
    heard at once and stays the same wherever the block goes. Drag the box on the waveform to \
    move it, or set the step and the passes back below.";

/// The step box's label, under a fixed spot.
pub const SPOT_STEP_LABEL: &str = "Step";

/// The passes box's label, under a fixed spot.
pub const SPOT_PASSES_LABEL: &str = "passes back";

/// The hover text on the step and passes boxes.
pub const SPOT_HOVER: &str = "The step the block reads from, and how many extra passes of the \
    pattern further back. A step at or after the block's own start is in the pass before, \
    since the future has not been recorded yet.\n\
    Drag the box on the waveform to move it; drag its right edge to change the loop length.";
/// See [`SOURCE_START_HOVER`].
pub const SOURCE_CONTINUE_HOVER: &str = "Carry on from where the block just before this one left \
    off — so a tape stop can slow down the beat a repeat was looping.";

/// The From box's label.
pub const RAMP_FROM_LABEL: &str = "Speed";

/// The arrow between the From and To boxes.
pub const RAMP_TO_LABEL: &str = "→";

/// The hover text on the two speed boxes.
pub const RAMP_HOVER: &str = "The read speed at the block's start and end, from -2 to +2, \
    ramped between them: 1 is live speed, 2 an octave up, 0.5 an octave down, \
    0 frozen, -1 backwards at pitch. On a loop every repeat plays at the speed \
    of its moment in the block, so 1 → 2 is a rising stutter.\n\
    Drag either box, or click it and type a number.";

/// How far one point of drag moves a speed box.
pub const SPEED_DRAG_SPEED: f64 = 0.01;

/// How many decimal places a speed box shows.
pub const SPEED_DECIMALS: usize = 2;

/// The duplicate item's label.
pub const DUPLICATE_LABEL: &str = "Duplicate";

/// The duplicate item's hover text.
pub const DUPLICATE_HOVER: &str = "Put a copy of this block in the space right after it — same \
    length, same settings, cut short if the gap is smaller.\n\
    Nothing happens where there is no room for a copy at all.";

// ---- what an item chooses ----

/// The speed preset a preset item names, or `None` for Repeat.
pub fn kind_preset(kind: BlockKind) -> Option<SpeedPreset> {
    match kind {
        BlockKind::Repeat | BlockKind::Ramp => None,
        BlockKind::Reverse => Some(SpeedPreset::Reverse),
        BlockKind::HalfSpeed => Some(SpeedPreset::HalfSpeed),
        BlockKind::DoubleSpeed => Some(SpeedPreset::DoubleSpeed),
        BlockKind::TapeStop => Some(SpeedPreset::TapeStop),
        BlockKind::TapeStart => Some(SpeedPreset::TapeStart),
    }
}

/// The action picking preset `kind` makes of a block showing `showing` on a
/// pattern at `rate`. The source is always kept.
///
/// A speed preset turns the loop off and sets its two speeds. Repeat on a
/// block that already loops changes nothing; on one that does not, it loops
/// one step of the pattern's rate at live speed.
pub fn kind_action(kind: BlockKind, showing: Action, rate: Rate) -> Action {
    let showing = showing.sanitised();
    if let Some(preset) = kind_preset(kind) {
        return Action { source: showing.source, ..Action::from_preset(preset) };
    }
    if showing.is_repeat() {
        return showing;
    }

    Action { source: showing.source, ..Action::repeat_at(rate) }
}

/// `showing` with its loop set to `looping`.
pub fn with_loop(showing: Action, looping: Option<NoteValue>) -> Action {
    Action { looping, ..showing.sanitised() }
}

/// `showing` with its source set to `source`.
pub fn with_source(showing: Action, source: Source) -> Action {
    Action { source, ..showing.sanitised() }
}

/// The block under the pointer, if the pointer is over one.
pub fn hovered_block(hit: Option<GridHit>) -> Option<usize> {
    match hit {
        Some(GridHit::Body { index, .. } | GridHit::Edge { index, .. }) => Some(index),
        Some(GridHit::Step { .. }) | None => None,
    }
}

// ---- the handlers (every one a write through the writer API) ----

/// Gives the block at `index` of pattern `pattern` the action `chosen`, and
/// remembers it as the one the next click adds — **without** committing.
///
/// `showing` is the action the block had when the menu was drawn, from this
/// frame's snapshot, and it is what makes a pick that changes nothing write
/// nothing: the same guard, for the same reason, as
/// [`super::top_bar::choose_rate`]'s. The memory is set either way, because
/// picking an item is a choice whether or not the block needed changing (the
/// module header's §Choosing is what writes the memory).
///
/// `false` when the row is unchanged — the block already held `chosen`, or
/// there was nowhere to write at all (`index` is not a block of it, `pattern`
/// not one of the sixteen), in which case nothing is published and the memory
/// is left alone too.
fn write_action(
    model: &StutterModel,
    prefs: &PrefsCell,
    pattern: usize,
    index: usize,
    showing: Action,
    chosen: Action,
) -> bool {
    let chosen = chosen.sanitised();

    if chosen == showing.sanitised() {
        remember_action(prefs, chosen);
        return false;
    }
    if !model.set_block_action(pattern, index, chosen) {
        return false;
    }

    remember_action(prefs, chosen);
    true
}

/// One pick from the menu: [`write_action`] committed, because a pick is a
/// whole gesture and therefore one undo step.
///
/// `true` when the pattern changed, which is what the undo step is of.
pub fn choose_action(
    model: &StutterModel,
    prefs: &PrefsCell,
    pattern: usize,
    index: usize,
    showing: Action,
    chosen: Action,
) -> bool {
    if !write_action(model, prefs, pattern, index, showing, chosen) {
        return false;
    }

    model.commit_edits();
    true
}

/// The action items: the block becomes [`kind_action`]'s answer for `kind`.
pub fn choose_kind(
    model: &StutterModel,
    prefs: &PrefsCell,
    pattern: usize,
    index: usize,
    showing: Action,
    rate: Rate,
    kind: BlockKind,
) -> bool {
    choose_action(model, prefs, pattern, index, showing, kind_action(kind, showing, rate))
}

/// Sets the block's loop: one undo step.
pub fn choose_loop(
    model: &StutterModel,
    prefs: &PrefsCell,
    pattern: usize,
    index: usize,
    showing: Action,
    looping: Option<NoteValue>,
) -> bool {
    choose_action(model, prefs, pattern, index, showing, with_loop(showing, looping))
}

/// Sets the block's source: one undo step.
pub fn choose_source(
    model: &StutterModel,
    prefs: &PrefsCell,
    pattern: usize,
    index: usize,
    showing: Action,
    source: Source,
) -> bool {
    choose_action(model, prefs, pattern, index, showing, with_source(showing, source))
}

/// The Duplicate item: a copy of the block in the space right after it,
/// returning the copy's index on the row
/// ([`StutterModel::duplicate_block`] does the fitting).
///
/// Committed, like every other item, so the copy is one undo step. `None`
/// where there was no room for one — the block ends at the end of the
/// pattern, its neighbour is adjacent, the row is full — in which case
/// nothing is published and there is nothing to undo.
///
/// The memory is deliberately *not* written: duplicating does not choose an
/// action, it copies one, and [`super::grid::add_block_at`] does not write
/// the memory for the same reason.
pub fn duplicate(model: &StutterModel, pattern: usize, index: usize) -> Option<usize> {
    let copy = model.duplicate_block(pattern, index)?;
    model.commit_edits();

    Some(copy)
}

/// The Delete item: the block comes off the row, leaving its steps playing
/// live.
///
/// Committed, so it is one undo step. `false` where there was nothing to
/// delete — `index` is not a block of `pattern`, or `pattern` is not one of
/// the sixteen — and then nothing is published either.
pub fn delete(model: &StutterModel, pattern: usize, index: usize) -> bool {
    if !model.delete_block(pattern, index) {
        return false;
    }

    model.commit_edits();
    true
}

/// The Delete key: [`delete`] on whatever block the pointer is over.
///
/// `hit` is the row's own hover lookup ([`super::grid::StepGrid::hit`]) — the
/// same one the hover text is written from — so the key takes away the block
/// that text names. `false` over an empty step and off the row, where there
/// is no block to delete and the key does nothing at all.
pub fn delete_hovered(model: &StutterModel, pattern: usize, hit: Option<GridHit>) -> bool {
    match hovered_block(hit) {
        Some(index) => delete(model, pattern, index),
        None => false,
    }
}

// ---- the selected block and its speed gesture ----

/// Which block the panel is editing, and the half-made gesture over its two
/// speed boxes, as one window holds them between frames.
///
/// Per opened window, and built afresh in `NiceEguiApp::build` —
/// nice-plug-egui 0.6 never calls `editor_closed`.
///
/// The selection is an index into the Time row of one pattern. The row can
/// change underneath it — an undo, another window, a project the host
/// loaded — so it is looked up in each frame's snapshot every time it is
/// used ([`BlockEditor::selected`]), and a selection of another pattern, or
/// past the end of the row, reads as nothing selected.
///
/// # The speed boxes
///
/// egui's `DragValue` needs a `&mut f32` to move, which cannot be the
/// block's own: the model is a frame behind, and a drag pulled back to it
/// could not move at all. So while a gesture is open the boxes keep their
/// own figures ([`BlockEditor::show_speeds`] does nothing), and every change
/// is written through at once ([`BlockEditor::changed`]) but committed only
/// when the hand lets go ([`BlockEditor::settle`]): the block can be heard
/// while it is dialled in and the whole drag is still one undo step.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BlockEditor {
    /// The pattern the selection belongs to.
    pattern: usize,
    /// The selected block's index on that pattern's row.
    selected: Option<usize>,
    speeds: (f32, f32),
    /// Whether one of the speed boxes has a gesture open — a drag, or a
    /// number half typed into it.
    gesture: bool,
    /// Whether the waveform box is being dragged: a gesture of its own, so
    /// the speed boxes settling every idle frame does not commit it.
    dragging: bool,
}

impl BlockEditor {
    /// Nothing selected.
    pub fn new() -> BlockEditor {
        BlockEditor {
            pattern: 0,
            selected: None,
            speeds: (1.0, 1.0),
            gesture: false,
            dragging: false,
        }
    }

    /// Selects block `index` of `pattern` (or nothing), settling any speed
    /// gesture left open on the block selected before.
    pub fn select(&mut self, model: &StutterModel, pattern: usize, index: Option<usize>) {
        if (self.pattern, self.selected) != (pattern, index) {
            self.settle(model);
            self.drop_source(model);
        }
        self.pattern = pattern;
        self.selected = index;
    }

    /// Selects the block under `hit`, or nothing over an empty step.
    pub fn select_hit(&mut self, model: &StutterModel, pattern: usize, hit: Option<GridHit>) {
        self.select(model, pattern, hovered_block(hit));
    }

    /// The selected block on `pattern`'s row as `showing` has it, or `None`
    /// when nothing is selected there or the row no longer has that block.
    pub fn selected(&self, pattern: usize, showing: &Pattern) -> Option<usize> {
        self.selected.filter(|&index| self.pattern == pattern && index < showing.blocks().len())
    }

    /// Block `deleted` of `pattern` came off the row: a selection of it goes,
    /// and one after it moves down with the row.
    pub fn deleted(&mut self, pattern: usize, deleted: usize) {
        if self.pattern != pattern {
            return;
        }
        self.selected = match self.selected {
            Some(index) if index == deleted => None,
            Some(index) if index > deleted => Some(index - 1),
            other => other,
        };
    }

    /// Whether a speed box has a gesture open — the one thing in the panel
    /// that holds the window's edited pattern still and keeps the undo keys
    /// from firing mid-gesture.
    pub fn gesture_open(&self) -> bool {
        self.gesture || self.dragging
    }

    /// A frame of the waveform drag: block `index` now reads `chosen`,
    /// written through at once so it can be heard, not committed until
    /// [`BlockEditor::drop_source`].
    pub fn drag_source(
        &mut self,
        model: &StutterModel,
        prefs: &PrefsCell,
        pattern: usize,
        index: usize,
        showing: Action,
        chosen: Action,
    ) -> bool {
        self.dragging = true;
        write_action(model, prefs, pattern, index, showing, chosen)
    }

    /// The waveform drag let go: commit it, if one was open, as one undo
    /// step.
    pub fn drop_source(&mut self, model: &StutterModel) -> bool {
        if !std::mem::take(&mut self.dragging) {
            return false;
        }

        model.commit_edits();
        true
    }

    /// Puts the block's own speeds in the two boxes, once a frame before they
    /// are drawn, and hands back what they will show. Does nothing while a
    /// gesture is open.
    pub fn show_speeds(&mut self, action: Action) -> (f32, f32) {
        if !self.gesture {
            let action = action.sanitised();
            self.speeds = (action.from, action.to);
        }

        self.speeds
    }

    /// The start speed's box, for the `DragValue`.
    pub fn from_mut(&mut self) -> &mut f32 {
        &mut self.speeds.0
    }

    /// The end speed's box.
    pub fn to_mut(&mut self) -> &mut f32 {
        &mut self.speeds.1
    }

    /// A box came up with a new figure: open the gesture, write the block
    /// through at once with the boxes' speeds (clamped), and remember it as
    /// the block a click adds. Not committed — [`BlockEditor::settle`] does
    /// that when the hand lets go.
    pub fn changed(
        &mut self,
        model: &StutterModel,
        prefs: &PrefsCell,
        pattern: usize,
        index: usize,
        showing: Action,
    ) -> bool {
        self.gesture = true;

        let chosen =
            Action { from: self.speeds.0, to: self.speeds.1, ..showing.sanitised() }.sanitised();
        self.speeds = (chosen.from, chosen.to);

        write_action(model, prefs, pattern, index, showing, chosen)
    }

    /// Both boxes are idle again: commit the gesture, if one was open, as one
    /// undo step.
    pub fn settle(&mut self, model: &StutterModel) -> bool {
        if !std::mem::take(&mut self.gesture) {
            return false;
        }

        model.commit_edits();
        true
    }

    /// Settles any gesture and selects nothing.
    pub fn clear(&mut self, model: &StutterModel) {
        self.settle(model);
        self.drop_source(model);
        self.selected = None;
    }
}

impl Default for BlockEditor {
    /// Nothing selected.
    fn default() -> BlockEditor {
        BlockEditor::new()
    }
}

/// Removes block `index` of `pattern` — the right-click and the Delete key —
/// as one undo step, keeping the selection on the block it was on.
pub fn remove(
    model: &StutterModel,
    editor: &mut BlockEditor,
    pattern: usize,
    index: usize,
) -> bool {
    editor.settle(model);
    if !delete(model, pattern, index) {
        return false;
    }
    editor.deleted(pattern, index);
    true
}

/// What the panel calls a loop setting: `1/16`, or `Off`.
pub fn loop_label(looping: Option<NoteValue>) -> &'static str {
    match looping {
        Some(length) => NOTE_LABELS[length.to_u8() as usize],
        None => LOOP_OFF_LABEL,
    }
}

/// What the panel calls a source: `Block start`, `Step 3`, `Step 3, 2 passes
/// back`, `Continue`. Steps are counted from 1, as the panel shows them.
pub fn source_label(source: Source) -> String {
    match source {
        Source::BlockStart => SOURCE_START_LABEL.to_string(),
        Source::Spot { step, passes: 0 } => format!("Step {}", step + 1),
        Source::Spot { step, passes: 1 } => format!("Step {}, 1 pass back", step + 1),
        Source::Spot { step, passes } => format!("Step {}, {passes} passes back", step + 1),
        Source::Continue => SOURCE_CONTINUE_LABEL.to_string(),
    }
}

/// The fixed spot that plays what block `index` of `pattern` plays now: its
/// own spot if it has one, otherwise the step its read head starts on,
/// rounded to the nearest step. What the Fixed spot button and the first
/// drag of the waveform box turn a block into, so neither changes the sound
/// by itself.
pub fn spot_of(pattern: &Pattern, index: usize) -> Option<Source> {
    let block = pattern.blocks().get(index)?;
    if let source @ Source::Spot { .. } = block.action().sanitised().source {
        return Some(source);
    }
    let path = pattern.block_path(index)?;
    let back = (path.offset(0.0) / pattern.step_beats()).round().max(0.0) as u32;

    Some(spot_back(pattern, block.start(), back))
}

/// The spot `back` steps before step `start` of `pattern`, as a step and
/// extra passes ([`Pattern::spot_steps_back`] the other way round), with the
/// passes held to [`MAX_SOURCE_PASSES`].
pub fn spot_back(pattern: &Pattern, start: u16, back: u32) -> Source {
    let length = i64::from(pattern.length());
    let step = (i64::from(start) - i64::from(back)).rem_euclid(length);
    let nearest = (i64::from(start) - step).rem_euclid(length);
    let passes = (i64::from(back) - nearest) / length;

    Source::Spot { step: step as u16, passes: passes.clamp(0, MAX_SOURCE_PASSES.into()) as u8 }
}

/// `spot` moved `steps` along the pattern, staying inside it; the passes
/// stay as they are. This is the waveform drag: the box follows the pointer
/// and never wraps round the band's edge.
pub fn moved_spot(pattern: &Pattern, spot: Source, steps: i32) -> Source {
    let Source::Spot { step, passes } = spot else {
        return spot;
    };
    let last = i64::from(pattern.length()) - 1;
    let step = (i64::from(step) + i64::from(steps)).clamp(0, last);

    Source::Spot { step: step as u16, passes }
}

/// A drag of the waveform box, as it was when picked up: which block, what
/// it did then, and whether the hand took the box or its right edge.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SourceHold {
    /// The block's index on the Time row.
    pub index: usize,
    /// Its action when the drag began.
    pub origin: Action,
    /// Its source then as a fixed spot ([`spot_of`]).
    pub spot: Source,
    /// Whether the right edge was taken (the loop length) rather than the
    /// box (the spot).
    pub edge: bool,
    /// How wide the box was then, in points.
    pub width: f32,
}

impl SourceHold {
    /// The action the drag asks for with the pointer `dx` points right of
    /// where it began, over a band `step_width` points to a step: the box
    /// moved by whole steps, or the loop scaled with the box and snapped to
    /// the nearest note value.
    pub fn action_at(&self, pattern: &Pattern, dx: f32, step_width: f32) -> Action {
        if self.edge {
            let Some(length) = self.origin.looping else {
                return self.origin;
            };
            let width = self.width.max(1.0);
            let ratio = f64::from((width + dx).max(1.0) / width);
            return with_loop(self.origin, Some(nearest_note(length.beats() * ratio)));
        }
        let steps = match step_width > 0.0 && dx.is_finite() {
            true => (dx / step_width).round() as i32,
            false => 0,
        };
        with_source(self.origin, moved_spot(pattern, self.spot, steps))
    }
}

/// The note value nearest `beats`, measured as a ratio so a 1/64 is as easy
/// to land on as a whole note: what the waveform box's right edge snaps a
/// loop to.
pub fn nearest_note(beats: f64) -> NoteValue {
    let beats = if beats.is_finite() { beats.max(1e-6) } else { NoteValue::DEFAULT.beats() };
    let distance = |note: &NoteValue| (note.beats() / beats).ln().abs();

    NoteValue::ALL.into_iter().min_by(|a, b| distance(a).total_cmp(&distance(b))).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::grid::{BlockEdge, REPEAT_LABELS, REPEAT_PREFIX, block_label};
    use crate::model::StutterData;
    use crate::pattern::{Block, PATTERN_COUNT};
    use crate::state::Prefs;

    /// The row every test below opens a menu over: a two-step Repeat of the
    /// default stutter on steps 3 and 4, and a four-step tape stop on steps 9
    /// to 12 of a sixteen-step pattern — one block of each kind the menu's
    /// two settings sections are drawn for.
    fn menu_pattern() -> Pattern {
        let mut pattern = Pattern::new();
        pattern.set_blocks([
            Block::new(2, 2, Action::default()),
            Block::new(8, 4, Action::from_preset(SpeedPreset::TapeStop)),
        ]);

        assert_eq!(pattern.blocks().len(), 2, "the fixture's blocks both fit");
        pattern
    }

    /// A committed model holding that row as pattern 0, as the editor leaves
    /// it after a frame: nothing to undo yet.
    fn menu_model() -> StutterModel {
        let model = StutterModel::new(StutterData::from_patterns([menu_pattern()]));
        model.commit_edits();
        assert!(!model.can_undo() && !model.can_redo(), "nothing to step into yet");

        model
    }

    /// The pattern the audio thread would read for `index` — what a handler
    /// actually published, not what it was handed.
    fn published(model: &StutterModel, index: usize) -> Pattern {
        model.shared().pattern(index).expect("a pattern of the sixteen").clone()
    }

    /// The action of block `block` of published pattern 0.
    fn action_of(model: &StutterModel, block: usize) -> Action {
        published(model, 0).blocks()[block].action()
    }

    /// The row's blocks as `(start, len)` pairs.
    fn spans(pattern: &Pattern) -> Vec<(u16, u16)> {
        pattern.blocks().iter().map(|block| (block.start(), block.len())).collect()
    }

    /// How many undo steps the model has, stepping back over all of them.
    fn undo_steps(model: &StutterModel) -> usize {
        let mut steps = 0;
        while model.undo() {
            steps += 1;
        }

        steps
    }

    /// Selecting a block by where the press landed: either part of a block
    /// selects it, an empty step selects nothing, and none of it is an edit.
    #[test]
    fn a_press_selects_the_block_it_landed_on() {
        let model = menu_model();
        let pattern = published(&model, 0);
        let mut editor = BlockEditor::new();

        assert_eq!(editor.selected(0, &pattern), None, "a new window selects nothing");

        editor.select_hit(&model, 0, Some(GridHit::Body { index: 1, step: 9 }));
        assert_eq!(editor.selected(0, &pattern), Some(1));
        assert_eq!(editor.selected(3, &pattern), None, "a selection belongs to its pattern");

        let edge = GridHit::Edge { index: 0, edge: BlockEdge::Start };
        editor.select_hit(&model, 0, Some(edge));
        assert_eq!(editor.selected(0, &pattern), Some(0), "an end is the same block");

        editor.select_hit(&model, 0, Some(GridHit::Step { step: 5 }));
        assert_eq!(editor.selected(0, &pattern), None);

        assert_eq!(hovered_block(Some(GridHit::Body { index: 3, step: 0 })), Some(3));
        assert_eq!(hovered_block(Some(GridHit::Edge { index: 4, edge: BlockEdge::End })), Some(4));
        assert_eq!(hovered_block(Some(GridHit::Step { step: 0 })), None);
        assert_eq!(hovered_block(None), None);

        // A selection past the end of the row reads as nothing.
        editor.select(&model, 0, Some(7));
        assert_eq!(editor.selected(0, &pattern), None);

        assert_eq!(spans(&published(&model, 0)), [(2, 2), (8, 4)]);
        assert!(!model.can_undo());
    }

    /// Removing a block — the right-click and the Delete key — is one undo
    /// step, and the selection follows the row: the removed block's goes, a
    /// later one's moves down.
    #[test]
    fn removing_a_block_keeps_the_selection_on_the_right_block() {
        let model = menu_model();
        let mut editor = BlockEditor::new();

        editor.select(&model, 0, Some(1));
        assert!(remove(&model, &mut editor, 0, 0));
        assert_eq!(spans(&published(&model, 0)), [(8, 4)]);
        assert_eq!(editor.selected(0, &published(&model, 0)), Some(0), "moved down with the row");

        assert!(remove(&model, &mut editor, 0, 0));
        assert_eq!(editor.selected(0, &published(&model, 0)), None, "the selected block went");
        assert!(!remove(&model, &mut editor, 0, 0), "nothing left to remove");

        assert_eq!(undo_steps(&model), 2);
        assert_eq!(spans(&published(&model, 0)), [(2, 2), (8, 4)]);
    }

    /// Each of the six preset items: the block becomes that preset, the
    /// memory is set to it, and the whole pick is one undo step.
    #[test]
    fn each_preset_item_writes_its_action_and_remembers_it() {
        for kind in PRESET_KINDS {
            let model = menu_model();
            let prefs = PrefsCell::default();
            let rate = published(&model, 0).rate();
            let showing = action_of(&model, 0);
            let wanted = kind_action(kind, showing, rate);

            let changed = choose_kind(&model, &prefs, 0, 0, showing, rate, kind);

            assert_eq!(changed, wanted != showing, "{kind:?}");
            assert_eq!(action_of(&model, 0), wanted, "{kind:?}");
            assert_eq!(BlockKind::of(wanted), kind, "{kind:?} is drawn as what was picked");
            assert_eq!(prefs.get().last_action, wanted, "{kind:?} is what the next click adds");
            assert_eq!(
                action_of(&model, 1),
                Action::from_preset(SpeedPreset::TapeStop),
                "{kind:?} left the other block alone"
            );
            // The span is never touched by the menu: an action is all it sets.
            assert_eq!(spans(&published(&model, 0)), [(2, 2), (8, 4)], "{kind:?}");

            assert_eq!(undo_steps(&model), usize::from(changed), "{kind:?} is one step");
            assert_eq!(action_of(&model, 0), showing, "{kind:?} came back undone");
        }
    }

    /// Picking the item that is already ticked changes nothing and records
    /// nothing — but it still *chooses*, so the memory follows it (the
    /// module header's §Choosing is what writes the memory).
    #[test]
    fn picking_the_action_a_block_already_has_still_chooses_it() {
        let model = menu_model();
        let prefs = PrefsCell::new(Prefs {
            keep_on_top: true,
            last_action: Action::from_preset(SpeedPreset::Reverse),
            notes: None,
            latch: false,
        });
        let rate = published(&model, 0).rate();
        let stop = action_of(&model, 1);

        assert!(!choose_kind(&model, &prefs, 0, 1, stop, rate, BlockKind::TapeStop));

        assert_eq!(action_of(&model, 1), stop, "the block is as it was");
        assert!(!model.can_undo(), "and nothing was committed");
        assert_eq!(prefs.get().last_action, stop, "but the pick is remembered");
        assert!(prefs.get().keep_on_top, "and nothing else in the prefs moved");
    }

    /// The loop and the source, each keeping everything else, both through
    /// the writer API and both one undo step.
    #[test]
    fn the_settings_keep_what_they_do_not_change() {
        let model = menu_model();
        let prefs = PrefsCell::default();

        let showing = action_of(&model, 0);
        assert!(choose_loop(&model, &prefs, 0, 0, showing, Some(NoteValue::Quarter)));
        let quarter = Action::repeat(NoteValue::Quarter);
        assert_eq!(action_of(&model, 0), quarter, "the loop changed, nothing else did");

        let showing = action_of(&model, 0);
        let before = Source::Spot { step: 1, passes: 0 };
        assert!(choose_source(&model, &prefs, 0, 0, showing, before));
        let expected = quarter.with_source(before);
        assert_eq!(action_of(&model, 0), expected, "the source changed, the loop did not");
        assert_eq!(prefs.get().last_action, quarter, "and the pick, less its spot, is the memory");

        assert_eq!(undo_steps(&model), 2);
        assert_eq!(action_of(&model, 0), Action::default());

        // Picking what a block already has writes nothing.
        let model = menu_model();
        let showing = action_of(&model, 0);
        assert!(!choose_loop(&model, &prefs, 0, 0, showing, Some(NoteValue::Sixteenth)));
        assert!(!choose_source(&model, &prefs, 0, 0, showing, Source::BlockStart));
        assert!(!model.can_undo());
    }

    /// Every block has all three settings: a tape stop can loop, and can
    /// continue the block before it.
    #[test]
    fn a_speed_block_takes_a_loop_and_a_source_too() {
        let model = menu_model();
        let prefs = PrefsCell::default();
        let stop = action_of(&model, 1);

        let looped = with_loop(stop, Some(NoteValue::Eighth));
        assert_eq!((looped.looping, looped.speeds()), (Some(NoteValue::Eighth), (1.0, 0.0)));
        assert!(choose_source(&model, &prefs, 0, 1, stop, Source::Continue));
        let continuing = action_of(&model, 1);
        assert_eq!(continuing, stop.with_source(Source::Continue));
        assert_eq!(BlockKind::of(continuing), BlockKind::TapeStop, "still a tape stop");
        assert!(!choose_loop(&model, &prefs, 0, 1, continuing, None), "it has no loop already");
    }

    /// The presets fill in the loop and speeds and keep the source; Repeat on
    /// a block that already loops keeps it as it is.
    #[test]
    fn a_preset_keeps_the_source_and_repeat_keeps_a_loop() {
        let rate = Rate::ThirtySecond;
        let before = Source::Spot { step: 2, passes: 0 };
        let repeat = Action::repeat(NoteValue::Quarter).with_source(before);

        assert_eq!(kind_action(BlockKind::Repeat, repeat, rate), repeat, "the loop is kept");
        let reverse = Action::from_preset(SpeedPreset::Reverse).with_source(before);
        assert_eq!(
            kind_action(BlockKind::Repeat, reverse, rate),
            Action::repeat_at(rate).with_source(before),
            "a block with no loop gets one step of this pattern's rate"
        );
        assert_eq!(
            kind_action(BlockKind::TapeStop, repeat, rate),
            Action::from_preset(SpeedPreset::TapeStop).with_source(before),
            "a speed preset turns the loop off and keeps the source"
        );

        for preset in SpeedPreset::ALL {
            let kind = BlockKind::of(Action::from_preset(preset));
            assert_eq!(kind_preset(kind), Some(preset), "{preset:?} round-trips");
        }
        assert_eq!(kind_preset(BlockKind::Repeat), None);
    }

    /// The two speed boxes: written through on every change so the ramp can
    /// be heard being dialled in, clamped to ±2 where a typed figure is not,
    /// the preset's name dropped, and the whole gesture one undo step.
    #[test]
    fn a_speed_box_drag_is_one_undo_step() {
        let model = menu_model();
        let prefs = PrefsCell::default();
        let mut menu = BlockEditor::new();

        menu.select(&model, 0, Some(1));
        let showing = action_of(&model, 1);
        assert_eq!(menu.show_speeds(showing), (1.0, 0.0), "the boxes open on the block's own");
        assert!(!menu.gesture, "showing a value is not a gesture");

        // One frame of the drag: the end speed pulled up off zero.
        *menu.to_mut() = 0.25;
        assert!(menu.changed(&model, &prefs, 0, 1, showing));
        assert!(menu.gesture, "the gesture is open");
        assert_eq!(action_of(&model, 1), Action::ramp(1.0, 0.25), "published at once");
        assert_eq!(BlockKind::of(action_of(&model, 1)), BlockKind::Ramp, "no longer a tape stop");
        assert!(!model.can_undo(), "and not committed yet");

        // The next frame of the same drag, with a figure past the limit —
        // typed, since no drag reaches it — which comes back clamped.
        let showing = action_of(&model, 1);
        *menu.from_mut() = 9.0;
        assert!(menu.changed(&model, &prefs, 0, 1, showing));
        assert_eq!(action_of(&model, 1), Action::ramp(2.0, 0.25));
        assert_eq!(menu.show_speeds(action_of(&model, 1)), (2.0, 0.25), "the box says what it got");

        // A frame that asks for what the block already has writes nothing.
        let showing = action_of(&model, 1);
        assert!(!menu.changed(&model, &prefs, 0, 1, showing));

        // The hand lets go: one step for the whole drag.
        assert!(menu.settle(&model));
        assert!(!menu.gesture);
        assert!(!menu.settle(&model), "a settled gesture settles once");
        assert_eq!(prefs.get().last_action, Action::ramp(2.0, 0.25), "the ramp is the memory");

        assert_eq!(undo_steps(&model), 1);
        assert_eq!(action_of(&model, 1), Action::from_preset(SpeedPreset::TapeStop));
        assert_eq!(spans(&published(&model, 0)), [(2, 2), (8, 4)], "a ramp moves nothing");
    }

    /// While a gesture is open the boxes keep their own figures; with none
    /// open they follow the block, wherever it changed — an undo, another
    /// window, a project the host loaded.
    #[test]
    fn the_boxes_hold_their_figure_only_while_they_are_being_moved() {
        let model = menu_model();
        let prefs = PrefsCell::default();
        let mut menu = BlockEditor::new();
        let stop = action_of(&model, 1);

        assert_eq!(menu.show_speeds(stop), (1.0, 0.0));
        *menu.to_mut() = -1.5;
        assert!(menu.changed(&model, &prefs, 0, 1, stop));

        // Mid-gesture the block disagrees with the boxes — the model is a
        // frame behind the pointer — and the boxes win.
        assert_eq!(menu.show_speeds(Action::from_preset(SpeedPreset::DoubleSpeed)), (1.0, -1.5));

        assert!(menu.settle(&model));
        assert_eq!(
            menu.show_speeds(Action::from_preset(SpeedPreset::DoubleSpeed)),
            (2.0, 2.0),
            "with no gesture open the boxes follow the block"
        );
        // A repeat shows its own speeds, which are live speed.
        assert_eq!(menu.show_speeds(Action::default()), (1.0, 1.0));
    }

    /// Duplicate: a copy in the space right after the block, one undo step,
    /// and nothing at all where there is no room for one.
    #[test]
    fn duplicate_puts_a_copy_in_the_space_after_the_block() {
        let model = menu_model();

        assert_eq!(duplicate(&model, 0, 0), Some(1), "the copy's own index on the row");
        assert_eq!(spans(&published(&model, 0)), [(2, 2), (4, 2), (8, 4)]);
        assert_eq!(action_of(&model, 1), Action::default(), "same action");

        // The gap the tape stop has left is one step shorter than it is, so
        // its copy is cut to the gap.
        assert_eq!(duplicate(&model, 0, 2), Some(3));
        assert_eq!(spans(&published(&model, 0)), [(2, 2), (4, 2), (8, 4), (12, 4)]);
        assert_eq!(action_of(&model, 3), Action::from_preset(SpeedPreset::TapeStop));

        assert_eq!(undo_steps(&model), 2, "one step per copy");
        assert_eq!(spans(&published(&model, 0)), [(2, 2), (8, 4)]);

        // No room: a block that ends at the end of the pattern, and one
        // whose neighbour is already against it.
        let model = StutterModel::new(StutterData::from_patterns([Pattern::new()]));
        model.commit_edits();
        assert_eq!(model.add_block(0, 15, 1, Action::default()), Some(0));
        assert_eq!(model.add_block(0, 0, 1, Action::default()), Some(0));
        assert_eq!(model.add_block(0, 1, 1, Action::default()), Some(1));
        model.commit_edits();

        assert_eq!(duplicate(&model, 0, 2), None, "it ends where the pattern does");
        assert_eq!(duplicate(&model, 0, 0), None, "its neighbour is adjacent");
        assert_eq!(spans(&published(&model, 0)), [(0, 1), (1, 1), (15, 1)], "nothing published");
        assert_eq!(undo_steps(&model), 1, "only the blocks the fixture added");
    }

    /// Delete, from the menu and from the key: the block comes off the row,
    /// as one undo step, and the key acts on the block under the pointer and
    /// on nothing else.
    #[test]
    fn delete_takes_the_block_off_the_row() {
        let model = menu_model();

        assert!(delete(&model, 0, 0));
        assert_eq!(spans(&published(&model, 0)), [(8, 4)]);
        assert_eq!(undo_steps(&model), 1);
        assert_eq!(spans(&published(&model, 0)), [(2, 2), (8, 4)], "and back again");

        // The key, over each part of a block and over a step with none.
        let model = menu_model();
        assert!(delete_hovered(&model, 0, Some(GridHit::Body { index: 1, step: 10 })));
        assert_eq!(spans(&published(&model, 0)), [(2, 2)]);

        let edge = GridHit::Edge { index: 0, edge: BlockEdge::End };
        assert!(delete_hovered(&model, 0, Some(edge)), "an end deletes the block it belongs to");
        assert_eq!(spans(&published(&model, 0)), []);

        assert!(!delete_hovered(&model, 0, Some(GridHit::Step { step: 3 })), "nothing is there");
        assert!(!delete_hovered(&model, 0, None), "nor off the row");
        assert_eq!(undo_steps(&model), 2, "one step per block deleted, and none for the misses");
        assert_eq!(spans(&published(&model, 0)), [(2, 2), (8, 4)]);
    }

    /// Selecting another block, or deselecting, is what commits a speed drag
    /// that was still open.
    #[test]
    fn moving_the_selection_settles_a_half_made_speed_gesture() {
        let model = menu_model();
        let prefs = PrefsCell::default();
        let mut editor = BlockEditor::new();

        editor.select(&model, 0, Some(1));
        let showing = action_of(&model, 1);
        editor.show_speeds(showing);
        *editor.from_mut() = -1.0;
        assert!(editor.changed(&model, &prefs, 0, 1, showing));
        assert!(editor.gesture_open());
        assert!(!model.can_undo(), "not committed while the hand is on the box");

        editor.select(&model, 0, Some(0));
        assert!(!editor.gesture_open());
        assert_eq!(undo_steps(&model), 1, "the drag became its one step on the way out");

        // Clearing settles too, and selecting what is already selected does
        // not end a gesture.
        let model = menu_model();
        let mut editor = BlockEditor::new();
        editor.select(&model, 0, Some(1));
        let showing = action_of(&model, 1);
        editor.show_speeds(showing);
        *editor.to_mut() = 0.5;
        assert!(editor.changed(&model, &prefs, 0, 1, showing));
        editor.select(&model, 0, Some(1));
        assert!(editor.gesture_open(), "the same block, the same gesture");
        editor.clear(&model);
        assert!(!editor.gesture_open());
        assert_eq!(editor.selected(0, &published(&model, 0)), None);
        assert_eq!(BlockEditor::default(), BlockEditor::new(), "the window opens with none");
    }

    /// Every handler with nowhere to write leaves everything exactly as it
    /// was: the patterns, the history and the memory.
    #[test]
    fn a_handler_with_nowhere_to_write_changes_nothing() {
        let model = menu_model();
        let prefs = PrefsCell::default();
        let rate = published(&model, 0).rate();
        let showing = action_of(&model, 0);
        let gone = published(&model, 0).blocks().len();

        for (pattern, index) in [(PATTERN_COUNT, 0), (0, gone), (99, 99)] {
            let where_ = format!("pattern {pattern}, block {index}");

            assert!(
                !choose_kind(&model, &prefs, pattern, index, showing, rate, BlockKind::Reverse),
                "{where_}"
            );
            let quarter = Some(NoteValue::Quarter);
            assert!(!choose_loop(&model, &prefs, pattern, index, showing, quarter), "{where_}");
            assert!(
                !choose_source(&model, &prefs, pattern, index, showing, Source::Continue),
                "{where_}"
            );
            assert_eq!(duplicate(&model, pattern, index), None, "{where_}");
            assert!(!delete(&model, pattern, index), "{where_}");

            let hit = GridHit::Body { index, step: 0 };
            assert!(!delete_hovered(&model, pattern, Some(hit)), "{where_}");

            let mut menu = BlockEditor::new();
            menu.show_speeds(showing);
            *menu.from_mut() = 0.5;
            assert!(!menu.changed(&model, &prefs, pattern, index, showing), "{where_}");
        }

        assert_eq!(spans(&published(&model, 0)), [(2, 2), (8, 4)], "the row is as it was");
        assert_eq!(action_of(&model, 0), showing);
        assert!(!model.can_undo(), "nothing was committed");
        assert!(!model.can_redo(), "and no future was thrown away");
        assert_eq!(prefs.get(), Prefs::default(), "a refused item chooses nothing");
    }

    /// Pinning a block's source to a fixed spot leaves it reading where it
    /// read: from its own start, from a spot, and continuing a repeat.
    #[test]
    fn pinning_a_source_to_a_spot_does_not_change_the_sound() {
        let stop = Action::from_preset(SpeedPreset::TapeStop).with_source(Source::Continue);
        let mut chain = Pattern::new();
        chain.set_blocks([
            Block::new(0, 12, Action::repeat(NoteValue::Quarter)),
            Block::new(12, 4, stop),
        ]);
        for pattern in [menu_pattern(), chain] {
            for index in 0..pattern.blocks().len() {
                let spot = spot_of(&pattern, index).expect("a block of the row");
                assert!(matches!(spot, Source::Spot { .. }), "{spot:?}");
                let mut pinned = pattern.clone();
                let action = pattern.blocks()[index].action().with_source(spot);
                assert!(pinned.set_block_action(index, action));

                let before = pattern.block_path(index).unwrap().offset(0.0);
                let after = pinned.block_path(index).unwrap().offset(0.0);
                assert_eq!(before, after, "block {index} reads where it did");
            }
        }
        assert_eq!(spot_of(&menu_pattern(), 0), Some(Source::Spot { step: 2, passes: 0 }));
        assert_eq!(spot_of(&menu_pattern(), 5), None, "a block the row does not have");
    }

    /// [`spot_back`] is [`Pattern::spot_steps_back`] the other way round,
    /// for every start and every distance up to the passes a spot can hold.
    #[test]
    fn a_spot_back_counts_back_the_steps_it_was_given() {
        let mut pattern = Pattern::new();
        let length = u32::from(pattern.length());
        for start in 0..pattern.length() {
            for back in 0..length * (u32::from(MAX_SOURCE_PASSES) + 1) {
                let spot = spot_back(&pattern, start, back);
                pattern.set_blocks([Block::new(start, 1, Action::default().with_source(spot))]);
                assert_eq!(pattern.spot_steps_back(0), Some(back), "{start} back {back}");
            }
        }
        // Further back than a spot can hold stops at the most passes.
        let far = spot_back(&pattern, 0, length * 40);
        assert_eq!(far, Source::Spot { step: 0, passes: MAX_SOURCE_PASSES });
    }

    /// The waveform drag moves a spot by whole steps, stopping at the ends
    /// of the pattern rather than wrapping, and keeps its passes.
    #[test]
    fn a_dragged_spot_stays_inside_the_pattern() {
        let pattern = Pattern::new();
        let spot = Source::Spot { step: 3, passes: 1 };
        assert_eq!(moved_spot(&pattern, spot, 5), Source::Spot { step: 8, passes: 1 });
        assert_eq!(moved_spot(&pattern, spot, -10), Source::Spot { step: 0, passes: 1 });
        assert_eq!(moved_spot(&pattern, spot, 100), Source::Spot { step: 15, passes: 1 });
        assert_eq!(moved_spot(&pattern, Source::Continue, 3), Source::Continue);
    }

    /// The edge drag snaps to the nearest note value by ratio.
    #[test]
    fn a_loop_snaps_to_the_nearest_note_value() {
        assert_eq!(nearest_note(1.0), NoteValue::Quarter);
        assert_eq!(nearest_note(0.27), NoteValue::Sixteenth);
        assert_eq!(nearest_note(0.7), NoteValue::QuarterTriplet);
        assert_eq!(nearest_note(4.0), NoteValue::Whole);
        assert_eq!(nearest_note(400.0), NoteValue::Whole);
        assert_eq!(nearest_note(0.0), NoteValue::SixtyFourthTriplet);
        assert_eq!(nearest_note(f64::NAN), NoteValue::DEFAULT);
        for note in NoteValue::ALL {
            assert_eq!(nearest_note(note.beats()), note, "{note} is its own nearest");
        }
    }

    /// A hold turns the pointer's travel into an action: the box by whole
    /// steps into a fixed spot, the edge into a loop scaled with the box.
    #[test]
    fn a_held_box_moves_the_spot_and_its_edge_scales_the_loop() {
        let pattern = menu_pattern();
        let origin = pattern.blocks()[0].action();
        let spot = spot_of(&pattern, 0).unwrap();
        let hold = SourceHold { index: 0, origin, spot, edge: false, width: 10.0 };
        let step = 20.0;

        assert_eq!(hold.action_at(&pattern, 0.0, step), origin.with_source(spot));
        let moved = hold.action_at(&pattern, 3.4 * step, step);
        assert_eq!(moved.source, Source::Spot { step: 5, passes: 0 });
        let left = hold.action_at(&pattern, -50.0 * step, step);
        assert_eq!(left.source, Source::Spot { step: 0, passes: 0 });
        assert_eq!(hold.action_at(&pattern, f32::NAN, step).source, spot);

        let edge = SourceHold { edge: true, ..hold };
        let wider = edge.action_at(&pattern, 10.0, step);
        assert_eq!(wider.looping, Some(NoteValue::Eighth), "twice as wide, twice as long");
        assert_eq!(wider.source, origin.source, "the edge leaves the source alone");
        let squeezed = edge.action_at(&pattern, -100.0, step);
        assert_eq!(squeezed.looping, Some(NoteValue::SixtyFourthTriplet));

        let stop = pattern.blocks()[1].action();
        let unlooped = SourceHold { index: 1, origin: stop, ..edge };
        assert_eq!(unlooped.action_at(&pattern, 40.0, step), stop, "no loop to stretch");
    }

    /// The waveform drag is heard as it moves, is one undo step when it
    /// drops, and is not committed by the speed boxes settling meanwhile.
    #[test]
    fn a_waveform_drag_is_one_undo_step() {
        let model = menu_model();
        let prefs = PrefsCell::default();
        let mut menu = BlockEditor::new();
        menu.select(&model, 0, Some(0));

        let showing = action_of(&model, 0);
        for step in [1, 0, 1] {
            let chosen = showing.with_source(Source::Spot { step, passes: 0 });
            menu.drag_source(&model, &prefs, 0, 0, action_of(&model, 0), chosen);
            assert!(menu.gesture_open());
            assert!(!menu.settle(&model), "the speed boxes have nothing to settle");
            assert_eq!(action_of(&model, 0), chosen, "published at once");
        }
        assert!(!model.can_undo(), "not committed mid-drag");
        assert!(menu.drop_source(&model));
        assert!(!menu.gesture_open());
        assert!(!menu.drop_source(&model), "only once");
        assert_eq!(undo_steps(&model), 1);
        assert_eq!(action_of(&model, 0), showing);

        // Selecting another block mid-drag commits it too.
        let model = menu_model();
        let chosen = showing.with_source(Source::Spot { step: 0, passes: 0 });
        menu.drag_source(&model, &prefs, 0, 0, showing, chosen);
        menu.select(&model, 0, Some(1));
        assert_eq!(undo_steps(&model), 1);
        assert_eq!(prefs.get().last_action, Action::default(), "a spot is not remembered");
    }

    /// The menu's words: one label and one hover text per preset, the note
    /// labels the same note values the blocks wear.
    #[test]
    fn the_menu_labels_match_the_types_they_name() {
        assert_eq!(KIND_HOVERS.len(), KIND_LABELS.len());
        for (label, hover) in KIND_LABELS.into_iter().zip(KIND_HOVERS) {
            assert!(!label.is_empty(), "every item has a face");
            assert!(!hover.is_empty(), "{label} says what it does");
        }

        // The note labels are the note values themselves, and the Repeat
        // block labels are these with the row's own prefix in front.
        for (note, label) in NoteValue::ALL.into_iter().zip(NOTE_LABELS) {
            assert_eq!(label, note.to_string(), "{note}");
            assert_eq!(loop_label(Some(note)), label, "{note}");
            assert_eq!(
                REPEAT_LABELS[note.to_u8() as usize],
                format!("{REPEAT_PREFIX}{label}"),
                "{note}"
            );
        }
        assert_eq!(loop_label(None), LOOP_OFF_LABEL);

        assert_eq!(source_label(Source::BlockStart), SOURCE_START_LABEL);
        assert_eq!(source_label(Source::Spot { step: 2, passes: 0 }), "Step 3");
        assert_eq!(source_label(Source::Spot { step: 0, passes: 1 }), "Step 1, 1 pass back");
        assert_eq!(source_label(Source::Spot { step: 15, passes: 3 }), "Step 16, 3 passes back");
        assert_eq!(source_label(Source::Continue), "Continue");

        for text in [
            LOOP_LABEL,
            LOOP_HOVER,
            SOURCE_LABEL,
            SOURCE_HOVER,
            SOURCE_START_LABEL,
            SOURCE_SPOT_LABEL,
            SOURCE_SPOT_HOVER,
            SPOT_STEP_LABEL,
            SPOT_PASSES_LABEL,
            SPOT_HOVER,
            SOURCE_CONTINUE_LABEL,
            RAMP_FROM_LABEL,
            RAMP_TO_LABEL,
            RAMP_HOVER,
            DUPLICATE_LABEL,
            DUPLICATE_HOVER,
        ] {
            assert!(!text.is_empty());
            // The window's text is egui's bundled Ubuntu-Light, which has no
            // glyph for the minus sign this plugin's prose uses: a speed
            // range written with one would come out blank.
            assert!(!text.contains('\u{2212}'), "{text}");
        }

        assert_eq!(block_label(Action::ramp(1.0, 0.5)).to_string(), "1→0.5");
    }

    /// A menu edit publishes and reclaims, like every other write through
    /// the writer API: a user clicking through the menu does not pile up
    /// retired snapshots.
    #[test]
    fn a_menu_edit_publishes_and_reclaims() {
        let model = menu_model();
        let prefs = PrefsCell::default();
        let rate = published(&model, 0).rate();

        for kind in PRESET_KINDS {
            let showing = action_of(&model, 0);
            choose_kind(&model, &prefs, 0, 0, showing, rate, kind);
        }
        assert_eq!(duplicate(&model, 0, 0), Some(1));
        assert!(delete(&model, 0, 1));
        assert!(model.undo());
        model.collect();

        assert_eq!(model.live_allocs(), 1, "only the published snapshot is left alive");
    }
}
