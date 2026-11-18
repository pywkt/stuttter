//! The block panel: the selected block's settings, on the right of the
//! window, all on screen at once.
//!
//! Every control is a plain button, radio or drag box — no popup menus, so
//! nothing can be cut off at the window's edge. What each one does is a
//! handler in [`super::block`], unit-tested there; this module is the layout
//! of those controls plus two pure readings of a block that are tested here:
//! [`describe`], the sentence saying what the block does, and
//! [`source_span`], the stretch of song it reads from, which the waveform
//! view washes over ([`super::wave`]).

use egui::Rect;

use super::block::{
    self, BlockEditor, KIND_HOVERS, KIND_LABELS, LOOP_HOVER, LOOP_LABEL, LOOP_OFF_LABEL,
    PRESET_KINDS, RAMP_HOVER, SOURCE_CONTINUE_HOVER, SOURCE_CONTINUE_LABEL, SOURCE_HOVER,
    SOURCE_LABEL, SOURCE_SPOT_HOVER, SOURCE_SPOT_LABEL, SOURCE_START_HOVER, SOURCE_START_LABEL,
    SPEED_DECIMALS, SPEED_DRAG_SPEED, SPOT_HOVER, SPOT_PASSES_LABEL, SPOT_STEP_LABEL,
};
use super::grid::{BlockKind, block_label};
use super::theme;
use crate::actions::MAX_SPEED;
use crate::model::StutterModel;
use crate::pattern::{Action, MAX_SOURCE_PASSES, NoteValue, Pattern, Source};
use crate::state::PrefsCell;

/// The straight note values offered as buttons, longest first; the `T`
/// toggle beside them picks each one's triplet.
pub const BASE_LENGTHS: [NoteValue; 7] = [
    NoteValue::Whole,
    NoteValue::Half,
    NoteValue::Quarter,
    NoteValue::Eighth,
    NoteValue::Sixteenth,
    NoteValue::ThirtySecond,
    NoteValue::SixtyFourth,
];

/// The buttons' labels, in [`BASE_LENGTHS`]' order.
pub const BASE_LABELS: [&str; BASE_LENGTHS.len()] =
    ["1/1", "1/2", "1/4", "1/8", "1/16", "1/32", "1/64"];

/// The triplet toggle's label.
pub const TRIPLET_LABEL: &str = "T";

/// The triplet toggle's hover text.
pub const TRIPLET_HOVER: &str = "Triplet: two thirds of the length, so three fit where two did.";

/// The panel's heading with no block selected.
pub const NOTHING_SELECTED: &str = "No block selected";

/// What the panel says with no block selected: every gesture on the Time
/// row, since there is no hover text there.
pub const EMPTY_HINT: &str = "Click an empty step to add a block. Press and drag across \
    empty steps to make it longer.\n\n\
    Click a block to edit it here. Drag a block to move it, or drag one of its ends to \
    stretch it.\n\n\
    Right-click a block, or press Delete, to remove it. Escape deselects.";

/// The remove button's label.
pub const REMOVE_LABEL: &str = "Remove";

/// The remove button's hover text.
pub const REMOVE_HOVER: &str = "Take this block off the row; its steps play live again. \
    Right-clicking a block does the same.";

/// `length` with its triplet flag set to `triplet`.
pub fn with_triplet(length: NoteValue, triplet: bool) -> NoteValue {
    NoteValue::from_u8((length.to_u8() & !1) | u8::from(triplet))
}

/// The straight note `length` is, or is the triplet of.
pub fn base_of(length: NoteValue) -> NoteValue {
    with_triplet(length, false)
}

/// A speed or a number of beats as short as it can be written: `1`, `0.5`,
/// `-1.25`.
fn number(value: f64) -> String {
    let value = if value.is_finite() { value } else { 0.0 };
    format!("{}", (value * 100.0).round() / 100.0)
}

/// `n` repeats in words: `once`, `twice`, `3 times`, `about 2.5 times`.
fn times(count: f64) -> String {
    let whole = count.round();
    if (count - whole).abs() > 1e-6 {
        return format!("about {} times", number(count));
    }
    match whole as u64 {
        0 | 1 => "once".to_string(),
        2 => "twice".to_string(),
        n => format!("{n} times"),
    }
}

/// How a speed ramp sounds, in words.
fn speed_phrase(from: f64, to: f64) -> String {
    if from == to {
        return match from {
            1.0 => "at normal speed".to_string(),
            0.0 => "frozen".to_string(),
            speed if speed < 0.0 => format!("backwards at {}× speed", number(-speed)),
            speed => format!("at {}× speed", number(speed)),
        };
    }
    format!("with the speed going from {} to {}", number(from), number(to))
}

/// Where a block reads from, in words.
fn source_phrase(pattern: &Pattern, index: usize, action: Action) -> String {
    let block_start = pattern.blocks().get(index).map_or(0, |block| block.start());
    match action.source {
        Source::BlockStart => "from the block's own start".to_string(),
        Source::Spot { step, .. } => {
            // Passes back from the one the block is in: the spot sits
            // `back` steps before the block, which starts `start` steps in.
            let back = i64::from(pattern.spot_steps_back(index).unwrap_or(0));
            let (start, length) = (i64::from(block_start), i64::from(pattern.length()));
            let where_ = match ((back - start).max(0) + length - 1) / length {
                0 => "of this pass".to_string(),
                1 => "of the pass before".to_string(),
                n => format!("{n} passes back"),
            };
            format!("from step {} {where_}", step + 1)
        }
        Source::Continue => match pattern.continues_from(index) {
            Some(_) => "from where the block before it left off".to_string(),
            None => "from its own start (no block ends right before it to continue)".to_string(),
        },
    }
}

/// The furthest block `index` reads behind the song, in beats.
fn reach(pattern: &Pattern, index: usize) -> f64 {
    let (Some(block), Some(path)) = (pattern.blocks().get(index), pattern.block_path(index))
    else {
        return 0.0;
    };
    let beats = block.beats(pattern.step_beats());

    // The block's very end is not played (the next step starts there).
    (0..SAMPLES).map(|k| path.offset(beats * k as f64 / SAMPLES as f64)).fold(0.0, f64::max)
}

/// How many points along a block [`reach`] and [`source_span`] look at.
const SAMPLES: usize = 256;

/// What block `index` of `pattern` does, as a sentence or two: the loop and
/// how many times it repeats, where it reads from, its speed, and how far
/// back it reaches.
pub fn describe(pattern: &Pattern, index: usize) -> String {
    let Some(block) = pattern.blocks().get(index) else {
        return String::new();
    };
    let action = block.action().sanitised();
    let (from, to) = action.speeds();
    let source = source_phrase(pattern, index, action);
    let speed = speed_phrase(from, to);
    let beats = block.beats(pattern.step_beats());

    let what = match action.looping {
        Some(length) => {
            let count = times(beats / length.beats());
            format!("Repeats {length} of audio {count}, taken {source}, {speed}.")
        }
        None => format!("Plays {source}, {speed}."),
    };

    format!("{what}\nReaches back up to {} beats.", number(reach(pattern, index)))
}

/// The stretch of song block `index` reads from, as beats from the start of
/// the pattern's cycle — negative where it reaches into an earlier pass.
/// `None` for a block the row does not have.
pub fn source_span(pattern: &Pattern, index: usize) -> Option<(f64, f64)> {
    let block = pattern.blocks().get(index)?;
    let path = pattern.block_path(index)?;
    let step = pattern.step_beats();
    let (start, beats) = (block.start_beats(step), block.beats(step));

    let reads = (0..SAMPLES).map(|k| {
        let phase = beats * k as f64 / SAMPLES as f64;
        start + phase - path.offset(phase)
    });
    let (low, high) =
        reads.fold((f64::INFINITY, f64::NEG_INFINITY), |(low, high), r| (low.min(r), high.max(r)));
    // At least a hair wide, so a frozen read still shows where it is.
    let high = high.max(low + step * 0.05);

    (low.is_finite() && high.is_finite()).then_some((low, high))
}

/// One row of note-length buttons and the triplet toggle. Returns the length
/// picked, if one was.
fn ui_lengths(ui: &mut egui::Ui, current: Option<NoteValue>, id: &str) -> Option<NoteValue> {
    let triplet = current.is_some_and(NoteValue::is_triplet);
    let base = current.map(base_of);
    let mut picked = None;

    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = theme::CELL_GAP;
        for (length, label) in BASE_LENGTHS.into_iter().zip(BASE_LABELS) {
            if ui.selectable_label(base == Some(length), label).clicked() {
                picked = Some(with_triplet(length, triplet));
            }
        }
        let toggle = ui
            .push_id(id, |ui| ui.selectable_label(triplet, TRIPLET_LABEL))
            .inner
            .on_hover_text(TRIPLET_HOVER);
        if toggle.clicked() {
            let length = base.unwrap_or(NoteValue::Sixteenth);
            picked = Some(with_triplet(length, !triplet));
        }
    });

    picked
}

/// A small dim caption over a group of controls.
fn caption(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add_space(theme::ITEM_SPACING);
    ui.label(egui::RichText::new(text).color(theme::TEXT_DIM).size(theme::HINT_SIZE))
}

/// The panel's tighter spacing: rows of buttons a little shorter than the
/// bar's, so every setting fits the window without scrolling.
fn compact(ui: &mut egui::Ui) {
    let spacing = ui.spacing_mut();
    spacing.interact_size.y = theme::PANEL_CONTROL_HEIGHT;
    spacing.item_spacing.y = theme::CELL_GAP;
    spacing.button_padding.y = 2.0;
}

/// How far one point of drag moves the step and passes boxes.
const SPOT_DRAG_SPEED: f64 = 0.1;

/// One of the two speed boxes.
fn speed_box(speed: &mut f32) -> egui::DragValue<'_> {
    let limit = MAX_SPEED as f32;

    egui::DragValue::new(speed)
        .speed(SPEED_DRAG_SPEED)
        .range(-limit..=limit)
        .max_decimals(SPEED_DECIMALS)
        .update_while_editing(false)
}

/// Draws the panel into `rect`: the selected block of pattern `index`
/// (`showing`), or what to do when none is selected.
pub(super) fn ui_block_panel(
    ui: &mut egui::Ui,
    rect: Rect,
    model: &StutterModel,
    prefs: &PrefsCell,
    index: usize,
    showing: &Pattern,
    editor: &mut BlockEditor,
) {
    ui.painter().rect_filled(rect, theme::CELL_CORNER, theme::WINDOW);
    let inner = rect.shrink(theme::PANEL_PADDING);
    if !inner.is_positive() {
        return;
    }
    let mut panel = ui.new_child(egui::UiBuilder::new().max_rect(inner));
    panel.shrink_clip_rect(rect);

    egui::ScrollArea::vertical().id_salt("stuttter-block-panel").show(&mut panel, |ui| {
        ui.set_width(inner.width() - theme::SCROLL_BAR_STRIP);
        compact(ui);
        match editor.selected(index, showing) {
            Some(target) => ui_block(ui, model, prefs, index, showing, target, editor),
            None => {
                editor.settle(model);
                ui.label(egui::RichText::new(NOTHING_SELECTED).color(theme::TEXT));
                ui.add_space(theme::ITEM_SPACING);
                ui.label(egui::RichText::new(EMPTY_HINT).color(theme::TEXT_DIM));
            }
        }
    });
}

/// The panel's controls for block `target`.
fn ui_block(
    ui: &mut egui::Ui,
    model: &StutterModel,
    prefs: &PrefsCell,
    index: usize,
    showing: &Pattern,
    target: usize,
    editor: &mut BlockEditor,
) {
    let block = showing.blocks()[target];
    let action = block.action().sanitised();
    let kind = BlockKind::of(action);
    let rate = showing.rate();

    // Which block: its label in its colour, and its steps.
    let (first, last) = (block.start() + 1, block.end());
    let steps = match first == last {
        true => format!("step {first}"),
        false => format!("steps {first} to {last}"),
    };
    ui.horizontal(|ui| {
        let label = block_label(action).to_string();
        ui.label(egui::RichText::new(label).color(kind.colour()).strong());
        ui.label(egui::RichText::new(steps).color(theme::TEXT_DIM));
    });

    caption(ui, "Preset");
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = theme::CELL_GAP;
        for ((item, label), hover) in PRESET_KINDS.into_iter().zip(KIND_LABELS).zip(KIND_HOVERS) {
            let text = egui::RichText::new(label).color(item.colour());
            if ui.selectable_label(item == kind, text).on_hover_text(hover).clicked() {
                block::choose_kind(model, prefs, index, target, action, rate, item);
            }
        }
    });

    caption(ui, LOOP_LABEL).on_hover_text(LOOP_HOVER);
    ui.horizontal(|ui| {
        if ui.selectable_label(action.looping.is_none(), LOOP_OFF_LABEL).clicked() {
            block::choose_loop(model, prefs, index, target, action, None);
        }
        if let Some(length) = ui_lengths(ui, action.looping, "loop-triplet") {
            block::choose_loop(model, prefs, index, target, action, Some(length));
        }
    });

    caption(ui, SOURCE_LABEL).on_hover_text(SOURCE_HOVER);
    let spot = match action.source {
        Source::Spot { step, passes } => Some((step, passes)),
        Source::BlockStart | Source::Continue => None,
    };
    ui.horizontal(|ui| {
        let at_start = action.source == Source::BlockStart;
        let start = ui.selectable_label(at_start, SOURCE_START_LABEL);
        if start.on_hover_text(SOURCE_START_HOVER).clicked() {
            block::choose_source(model, prefs, index, target, action, Source::BlockStart);
        }
        let fixed = ui.selectable_label(spot.is_some(), SOURCE_SPOT_LABEL);
        if fixed.on_hover_text(SOURCE_SPOT_HOVER).clicked()
            && spot.is_none()
            && let Some(here) = block::spot_of(showing, target)
        {
            block::choose_source(model, prefs, index, target, action, here);
        }
        let continuing = action.source == Source::Continue;
        let carry = ui.selectable_label(continuing, SOURCE_CONTINUE_LABEL);
        if carry.on_hover_text(SOURCE_CONTINUE_HOVER).clicked() {
            block::choose_source(model, prefs, index, target, action, Source::Continue);
        }
    });
    if let Some((step, passes)) = spot {
        // Counted from 1 on screen, as the panel's heading counts steps.
        let (mut shown, mut back) = (step + 1, passes);
        let idle = ui
            .horizontal(|ui| {
                ui.label(egui::RichText::new(SPOT_STEP_LABEL).color(theme::TEXT_DIM));
                let step_box = egui::DragValue::new(&mut shown)
                    .range(1..=showing.length())
                    .speed(SPOT_DRAG_SPEED)
                    .update_while_editing(false);
                let step_box = ui.add(step_box).on_hover_text(SPOT_HOVER);
                let passes_box = egui::DragValue::new(&mut back)
                    .range(0..=MAX_SOURCE_PASSES)
                    .speed(SPOT_DRAG_SPEED)
                    .update_while_editing(false);
                let passes_box = ui.add(passes_box).on_hover_text(SPOT_HOVER);
                ui.label(egui::RichText::new(SPOT_PASSES_LABEL).color(theme::TEXT_DIM));
                [step_box, passes_box].iter().all(|box_| !box_.dragged() && !box_.has_focus())
            })
            .inner;
        if (shown - 1, back) != (step, passes) {
            let chosen = action.with_source(Source::Spot { step: shown - 1, passes: back });
            editor.drag_source(model, prefs, index, target, action, chosen);
        }
        // One undo step per gesture: committed once the boxes are let go,
        // and never while the pointer is down — that may be the waveform
        // box being dragged, which commits its own gesture when it drops.
        if idle && !ui.input(|input| input.pointer.any_down()) {
            editor.drop_source(model);
        }
    }

    caption(ui, "Speed").on_hover_text(RAMP_HOVER);
    editor.show_speeds(action);
    let (changed, idle) = ui
        .horizontal(|ui| {
            let from = ui.add(speed_box(editor.from_mut())).on_hover_text(RAMP_HOVER);
            ui.label(block::RAMP_TO_LABEL);
            let to = ui.add(speed_box(editor.to_mut())).on_hover_text(RAMP_HOVER);
            let mut changed = false;
            let mut idle = true;
            for response in [from, to] {
                changed |= response.changed();
                idle &= !response.dragged() && !response.has_focus();
            }
            (changed, idle)
        })
        .inner;
    if changed {
        editor.changed(model, prefs, index, target, action);
    }
    if idle {
        editor.settle(model);
    }

    ui.add_space(theme::ITEM_SPACING);
    ui.label(egui::RichText::new(describe(showing, target)).color(theme::TEXT_DIM));

    ui.add_space(theme::ITEM_SPACING);
    ui.horizontal(|ui| {
        if ui.button(block::DUPLICATE_LABEL).on_hover_text(block::DUPLICATE_HOVER).clicked()
            && let Some(copy) = block::duplicate(model, index, target)
        {
            editor.select(model, index, Some(copy));
        }
        let remove = egui::RichText::new(REMOVE_LABEL).color(theme::WARN);
        if ui.button(remove).on_hover_text(REMOVE_HOVER).clicked() {
            block::remove(model, editor, index, target);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::SpeedPreset;
    use crate::pattern::Block;

    /// A sixteen-step 1/16 pattern with these blocks.
    fn row(blocks: &[(u16, u16, Action)]) -> Pattern {
        let mut pattern = Pattern::new();
        let blocks = blocks.iter().map(|&(start, len, action)| Block::new(start, len, action));
        pattern.set_blocks(blocks);
        pattern
    }

    #[test]
    fn the_length_buttons_and_the_triplet_toggle_cover_every_note_value() {
        let mut seen = Vec::new();
        for base in BASE_LENGTHS {
            assert!(!base.is_triplet());
            for triplet in [false, true] {
                let length = with_triplet(base, triplet);
                assert_eq!(base_of(length), base);
                assert_eq!(length.is_triplet(), triplet);
                seen.push(length);
            }
        }
        seen.sort();
        assert_eq!(seen, NoteValue::ALL, "every note value, once");
        for (base, label) in BASE_LENGTHS.into_iter().zip(BASE_LABELS) {
            assert_eq!(base.to_string(), label);
        }
    }

    #[test]
    fn a_repeat_says_what_it_loops_how_often_and_from_where() {
        let pattern = row(&[(0, 12, Action::repeat(NoteValue::Quarter))]);
        let text = describe(&pattern, 0);
        let expected =
            "Repeats 1/4 of audio 3 times, taken from the block's own start, at normal speed.";
        assert!(text.starts_with(expected), "{text}");
        assert!(text.contains("Reaches back up to 2 beats."), "{text}");

        let rising = Action { from: 1.0, to: 2.0, ..Action::repeat(NoteValue::Sixteenth) };
        let spot = rising.with_source(Source::Spot { step: 0, passes: 0 });
        let text = describe(&row(&[(4, 8, spot)]), 0);
        assert!(text.contains("8 times"), "{text}");
        assert!(text.contains("from step 1 of this pass"), "{text}");

        let behind = rising.with_source(Source::Spot { step: 10, passes: 0 });
        let text = describe(&row(&[(4, 8, behind)]), 0);
        assert!(text.contains("from step 11 of the pass before"), "{text}");
        let far = rising.with_source(Source::Spot { step: 0, passes: 2 });
        let text = describe(&row(&[(4, 8, far)]), 0);
        assert!(text.contains("from step 1 2 passes back"), "{text}");
        assert!(text.contains("speed going from 1 to 2"), "{text}");
    }

    #[test]
    fn a_continue_says_whether_it_has_anything_to_continue() {
        let stop = Action::from_preset(SpeedPreset::TapeStop).with_source(Source::Continue);
        let chained = row(&[(0, 12, Action::repeat(NoteValue::Quarter)), (12, 4, stop)]);
        let text = describe(&chained, 1);
        assert!(text.starts_with("Plays from where the block before it left off"), "{text}");
        assert!(text.contains("speed going from 1 to 0"), "{text}");
        assert!(text.contains("3.5 beats"), "{text}");

        let alone = row(&[(4, 4, stop)]);
        assert!(describe(&alone, 0).contains("no block ends right before it"));
        assert_eq!(describe(&alone, 3), "", "a block the row does not have");
    }

    #[test]
    fn the_speed_and_repeat_counts_read_naturally() {
        assert_eq!(times(1.0), "once");
        assert_eq!(times(2.0), "twice");
        assert_eq!(times(4.0), "4 times");
        assert_eq!(times(2.5), "about 2.5 times");
        assert_eq!(speed_phrase(1.0, 1.0), "at normal speed");
        assert_eq!(speed_phrase(-1.0, -1.0), "backwards at 1× speed");
        assert_eq!(speed_phrase(0.5, 0.5), "at 0.5× speed");
        assert_eq!(speed_phrase(0.0, 0.0), "frozen");
    }

    #[test]
    fn the_source_span_is_where_the_block_reads() {
        // A repeat of one beat from the start of a block on beat 2 reads
        // beat 2 to 3, however many times it loops.
        let pattern = row(&[(8, 8, Action::repeat(NoteValue::Quarter))]);
        let (low, high) = source_span(&pattern, 0).unwrap();
        assert_eq!(low, 2.0);
        assert!((high - 3.0).abs() < 0.01, "{high}");

        // A reverse on the pattern's first beat reads back into the pass
        // before: negative beats.
        let reverse = row(&[(0, 4, Action::from_preset(SpeedPreset::Reverse))]);
        let (low, high) = source_span(&reverse, 0).unwrap();
        assert!(low < -0.9 && high <= 0.0 + 1e-9, "{low} {high}");

        assert_eq!(source_span(&reverse, 1), None);
    }
}
