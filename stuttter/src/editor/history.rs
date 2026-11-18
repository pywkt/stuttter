//! Undo and redo in this window: the two buttons at the end of the top bar's
//! second row, and the steps they take through the model's own history.
//!
//! The history itself is [`StutterModel`]'s, and so is every decision about
//! it: what a step contains, how deep the stacks go
//! ([`crate::undo::UNDO_DEPTH`]), that a new edit throws the redone future
//! away, and that a gesture which ended where it started is no step at all
//! ([`StutterModel::commit_edits`]). Each gesture over the window has
//! already committed itself where it was applied — a rate picked in the bar's
//! dropdown, the release of a length drag ([`LengthBox::settle`]), and the
//! release of a drag over the Time row
//! ([`super::grid::TimeRowDrag::settle`]). So nothing here is policy: this
//! module asks the model to step, and says which way.
//!
//! # What the buttons are *not* for
//!
//! The three host parameters. Pattern, Mix and Smoothing belong to the host —
//! Bitwig automates them, records a move of one as automation, and covers it
//! with its own undo — so they are not in this history at all and these
//! buttons never move them. That is the one thing a user could expect of an
//! Undo button in a plugin window and not get, which is why both hover texts
//! say it.
//!
//! # Greyed out means "as of the last commit"
//!
//! [`Step::available`] is [`StutterModel::can_undo`] and its pair, which are
//! answered from the last commit and not from this instant. Between gestures
//! — which is every frame a user can reach these buttons in, since a pointer
//! on a button is not on the grid — that is the whole truth. The one gap is a
//! number still being typed into the length box: the gesture is open, its
//! writes are not committed yet, and the button is greyed although
//! [`take`] would find something to do. It costs a click: clearing the box's
//! focus is what the click does, the gesture settles, and the next frame the
//! button is live.
//!
//! # The keys
//!
//! Ctrl+Z, Ctrl+Shift+Z and Ctrl+Y do the same two things, and reach the same
//! [`take`]: [`apply`] is a frame's key events turned into steps, which is
//! why this module is named for the history rather than for its buttons
//! (chopdrop's `editor/history.rs` is the same module, keys first). Which keys
//! those are, and that Bitwig never sees them, is [`super::keys::route`]'s —
//! nothing about the shortcuts is decided here either.
//!
//! What [`apply`] does decide is **when not to step**. A number is typed into
//! this window — the length box, a speed box in a block's menu, the figure a
//! right-click puts on the Mix or Smoothing rail —
//! and while one of those has the keyboard, Ctrl+Z belongs to it: egui's own
//! text box undoes the typing in it, which is what a user reaching for the
//! shortcut over a half-typed figure means by it. That is the `typing`
//! argument, and it is an argument rather than a read of egui's context so
//! that the whole dispatch is testable without a window.
//!
//! The other time it does not step is while a **hand is still holding
//! something** on the grid: a block being moved or stretched, a level being
//! drawn across the Volume row, or a block's menu left open over it
//! ([`super::grid::TimeRowHands::busy`], [`super::volume::VolumeDraw::open`]).
//! That is the `holding` argument, and it is a decision of this module's.
//! The two buttons need no such guard — a pointer holding a block is not on
//! a button — but a key arrives whatever the hand is doing, and
//! taking the step would strand that hand: the half-made gesture would be
//! committed as a step of its own, stepped back over, and then left writing
//! to a row the undo had replaced, where the block it had hold of may be at
//! another index or gone. Settling those gestures the way [`take`] settles
//! the length box is the other way this could have gone, and it is worse: a
//! drag settled under a pointer that is still down holds a hand over a row
//! it can no longer write to, with nothing on screen to say why. So the key
//! is ignored and the gesture the hand is making stands. Let go and press it
//! again, and the drag that was let go of is the step it takes back.
//!
//! Two smaller details, both deliberate. Key *repeats* count, so holding the
//! shortcut walks back through the history the way it does in a host. And one
//! frame can carry more than one press — egui hands over everything that
//! arrived since the last frame at once — so each press is a step, in the
//! order it was pressed, rather than a frame being worth one step however
//! much was typed into it.

use egui::{Event, Rect};

use super::keys::{self, Route};
use super::top_bar::{ControlSlots, LengthBox};
use crate::model::StutterModel;

/// The Undo button's face.
pub const UNDO_LABEL: &str = "Undo";

/// The Redo button's face.
///
/// Words and not the arrows a host draws them as: the editor's text comes
/// from egui's bundled Ubuntu-Light (its `default_fonts` feature),
/// and an arrow glyph it does not carry would come out as a blank button.
pub const REDO_LABEL: &str = "Redo";

/// What Undo says when the pointer rests on it.
pub const UNDO_HOVER: &str = "Step back through the edits to the patterns — one step per \
    gesture, so a whole drag comes back at once.\n\
    The patterns only: Pattern, Mix and Smoothing belong to Bitwig, and \
    Bitwig's own undo covers those.";

/// What Redo says when the pointer rests on it.
pub const REDO_HOVER: &str = "Step forward again after an undo.\n\
    Editing something after an undo starts a new future: there is nothing \
    left to redo past that point.";

/// Which way through the edit history a step goes.
///
/// Two buttons' worth of behaviour as one value, because the only thing that
/// differs between them is which of the model's two stacks they take from —
/// the label, the hover text and the slot included. It is what the keyboard
/// shortcuts resolve to as well ([`super::keys::history_key`]): one way
/// through the history, whether a click or a key asked for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Back one step: [`StutterModel::undo`].
    Undo,
    /// Forward one step: [`StutterModel::redo`].
    Redo,
}

impl Step {
    /// Both of them, in the order they are drawn — back before forward, as
    /// every host draws them.
    pub const BOTH: [Step; 2] = [Step::Undo, Step::Redo];

    /// The button's face.
    pub fn label(self) -> &'static str {
        match self {
            Step::Undo => UNDO_LABEL,
            Step::Redo => REDO_LABEL,
        }
    }

    /// What it says when the pointer rests on it.
    pub fn hover(self) -> &'static str {
        match self {
            Step::Undo => UNDO_HOVER,
            Step::Redo => REDO_HOVER,
        }
    }

    /// What a greyed-out button says instead: the stack behind it is empty.
    ///
    /// Shown only while the button is disabled, which is the one state of it
    /// a user could otherwise mistake for the window having stopped
    /// answering.
    pub fn empty_hover(self) -> &'static str {
        match self {
            Step::Undo => "nothing to undo",
            Step::Redo => "nothing to redo",
        }
    }

    /// Whether this step has anything to do as of the last commit — what
    /// greys its button out (see the module header on the one gap in that).
    pub fn available(self, model: &StutterModel) -> bool {
        match self {
            Step::Undo => model.can_undo(),
            Step::Redo => model.can_redo(),
        }
    }

    /// Where on the bar's second row the button goes.
    pub fn slot(self, slots: &ControlSlots) -> Rect {
        match self {
            Step::Undo => slots.undo(),
            Step::Redo => slots.redo(),
        }
    }
}

/// Takes one step through the model's history, reporting whether there was
/// one to take.
///
/// `length` is the top bar's half-made length gesture, and it is **ended
/// first**. Two things come of that, both of them wanted:
///
/// - the gesture becomes its own undo step before the step is taken, so an
///   Undo clicked over a half-typed length lands on the state before that
///   number rather than on the gesture before it — which is what
///   [`StutterModel::undo`] would do on its own anyway, since it commits
///   first;
/// - the box stops holding its own figure ([`LengthBox::show`] does nothing
///   while a gesture is open), so the restored length is in it on the very
///   next frame instead of a stale number the user would have to click away.
///
/// A Redo over a half-made gesture therefore finds nothing to do, and that is
/// correct rather than unfortunate: settling the gesture is a new edit, and a
/// new edit clears the redone future in this editor as in every other
/// (`crate::undo::EditHistory::commit`).
pub fn take(step: Step, model: &StutterModel, length: &mut LengthBox) -> bool {
    length.settle(model);

    match step {
        Step::Undo => model.undo(),
        Step::Redo => model.redo(),
    }
}

/// Takes the steps a frame's `events` ask for, in the order they were
/// pressed, and reports how many of them found something to do.
///
/// `typing` is whether the keyboard belongs to a box in the window this
/// frame, in which case the shortcut is that box's and no step is taken at
/// all — [`super::keys::route`] is where that comes first. `holding` is
/// whether a hand is still making a gesture over the grid, in which case the
/// step is ignored and that gesture stands. Both are in the module header;
/// neither is read off egui's context here, so the whole dispatch is
/// testable without a window.
///
/// The count is for the tests and for a caller that wants to know whether
/// the window moved under the user. A step that had nowhere to go is not
/// counted: the model reports that itself, and asking for an undo with an
/// empty stack is simply nothing happening.
pub fn apply(
    events: &[Event],
    typing: bool,
    holding: bool,
    model: &StutterModel,
    length: &mut LengthBox,
) -> usize {
    if holding {
        return 0;
    }

    requested(events, typing).filter(|step| take(*step, model, length)).count()
}

/// The steps `events` ask for, in order: every key press
/// [`super::keys::route`] hands to the history rather than to the host or to
/// egui.
///
/// Presses only. A release carries the same key and modifiers as the press
/// before it, so counting both would double every step.
fn requested(events: &[Event], typing: bool) -> impl Iterator<Item = Step> + '_ {
    events.iter().filter_map(move |event| match event {
        Event::Key { key, pressed: true, modifiers, .. } => {
            match keys::route(*key, *modifiers, typing) {
                Route::History(step) => Some(step),
                Route::Host(_) | Route::Editor => None,
            }
        }
        _ => None,
    })
}

/// The Undo and Redo buttons, drawn into their slots on the bar's second row.
///
/// Placed rect by rect like the controls before them, each greyed out when
/// its stack is empty, and each stepping the model's history on a click. A
/// slot with no width — a window the host squeezed — draws nothing rather
/// than a sliver a click could still land on.
pub(super) fn ui_history_buttons(
    ui: &mut egui::Ui,
    slots: &ControlSlots,
    model: &StutterModel,
    length: &mut LengthBox,
) {
    for step in Step::BOTH {
        let rect = step.slot(slots);
        if !rect.is_positive() {
            continue;
        }

        // `add_enabled_ui` and not `add_enabled`, because the button is
        // *placed* in its slot rather than laid out: a disabled `Ui` is how
        // egui 0.37 greys out a widget that is put somewhere.
        let response = ui
            .add_enabled_ui(step.available(model), |ui| {
                ui.put(rect, egui::Button::new(step.label()).truncate())
            })
            .inner
            .on_hover_text(step.hover())
            .on_disabled_hover_text(step.empty_hover());

        if response.clicked() {
            take(step, model, length);
        }
    }
}

#[cfg(test)]
mod tests {
    use egui::{Key, Modifiers};

    use super::*;
    use crate::editor::theme;
    use crate::pattern::{Pattern, Rate};

    /// A held Ctrl, as X11 hands one to egui: both bits, which is what
    /// [`keys::history_key`] reads (`editor/keys.rs`).
    const CTRL: Modifiers = Modifiers::CTRL.plus(Modifiers::COMMAND);

    /// A key press, as egui-baseview hands one on.
    fn press(key: Key, modifiers: Modifiers) -> Event {
        Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }
    }

    /// Its release.
    fn release(key: Key, modifiers: Modifiers) -> Event {
        Event::Key { key, physical_key: None, pressed: false, repeat: false, modifiers }
    }

    /// A model with a committed starting point, as the editor leaves it after
    /// its first frame: sixteen default patterns and an empty history.
    fn committed_model() -> StutterModel {
        let model = StutterModel::default();
        model.commit_edits();
        assert!(!model.can_undo() && !model.can_redo(), "nothing to step into yet");

        model
    }

    /// The length the audio thread would read for pattern `index` — what a
    /// write through the writer API actually published.
    fn published_length(model: &StutterModel, index: usize) -> u16 {
        model.shared().pattern(index).expect("a pattern of the sixteen").length()
    }

    /// One finished gesture, as the bar's rate dropdown commits one: the
    /// edited pattern's rate written through and committed.
    fn one_committed_gesture(model: &StutterModel) {
        assert!(model.set_rate(0, Rate::ThirtySecond));
        model.commit_edits();
        assert!(model.can_undo(), "a committed gesture is a step");
    }

    /// A length drag that has not been let go of yet: written through, not
    /// committed, and the box still holding it.
    fn half_made_drag(model: &StutterModel, box_: &mut LengthBox, to: u16) {
        let was = published_length(model, 0);
        box_.show(was);
        *box_.value_mut() = to;
        assert_eq!(box_.changed(model, 0, was), Some(to));
        assert!(box_.editing(), "the gesture is open");
    }

    /// The shape of the whole thing: Undo steps back over a finished gesture
    /// and Redo steps forward into it again, both through the writer API so
    /// what the audio thread reads next block is the restored pattern.
    #[test]
    fn undo_steps_back_over_a_gesture_and_redo_forward_again() {
        let model = committed_model();
        let mut length = LengthBox::new();
        one_committed_gesture(&model);

        assert!(take(Step::Undo, &model, &mut length), "there was a step to take");
        assert_eq!(
            model.shared().pattern(0).expect("a pattern").rate(),
            Rate::DEFAULT,
            "back to the rate the pattern was made with"
        );
        assert!(!model.can_undo(), "and nothing before it");
        assert!(model.can_redo());

        assert!(take(Step::Redo, &model, &mut length));
        assert_eq!(
            model.shared().pattern(0).expect("a pattern").rate(),
            Rate::ThirtySecond,
            "forward into the edit again"
        );
        assert!(model.can_undo() && !model.can_redo());
    }

    /// A button with nothing to do says so and steps nowhere: the greyed
    /// state and the handler agree, which is what keeps a click that got
    /// through anyway from doing something a disabled button implied it
    /// could not.
    #[test]
    fn a_step_with_nowhere_to_go_does_nothing() {
        let model = committed_model();
        let mut length = LengthBox::new();

        for step in Step::BOTH {
            assert!(!step.available(&model), "{step:?} is greyed out");
            assert!(!take(step, &model, &mut length), "{step:?} found nothing to do");
        }

        assert_eq!(published_length(&model, 0), Pattern::DEFAULT_LENGTH, "nothing moved");
        assert!(!length.editing(), "and no gesture was invented");
    }

    /// Every write between two commits is one step, however many writes it
    /// made: a drag undoes as a whole, back to where the hand started.
    #[test]
    fn a_whole_drag_comes_back_as_one_step() {
        let model = committed_model();
        let mut length = LengthBox::new();

        // Every frame of a drag from 16 to 20 steps, as the length box writes
        // them: through to the model, nothing committed.
        for to in 17..=20 {
            half_made_drag(&model, &mut length, to);
        }
        assert_eq!(published_length(&model, 0), 20);

        assert!(take(Step::Undo, &model, &mut length));
        assert_eq!(published_length(&model, 0), Pattern::DEFAULT_LENGTH, "the whole drag, at once");
        assert!(!model.can_undo(), "four writes were one step");
    }

    /// A half-made gesture is ended before the step is taken, so the box is
    /// free to read the restored length on the very next frame rather than
    /// holding the figure that was just undone.
    #[test]
    fn a_half_made_gesture_is_settled_before_the_step() {
        let model = committed_model();
        let mut length = LengthBox::new();
        half_made_drag(&model, &mut length, 40);

        assert!(take(Step::Undo, &model, &mut length), "the open gesture is the step undone");
        assert!(!length.editing(), "the box is no longer holding the gesture");

        let restored = published_length(&model, 0);
        assert_eq!(restored, Pattern::DEFAULT_LENGTH);
        assert_eq!(length.show(restored), restored, "and shows what the pattern actually has");
    }

    /// Redo over a half-made gesture finds nothing: settling the gesture is a
    /// new edit, and a new edit clears the redone future.
    #[test]
    fn a_new_gesture_clears_what_there_was_to_redo() {
        let model = committed_model();
        let mut length = LengthBox::new();
        one_committed_gesture(&model);
        assert!(take(Step::Undo, &model, &mut length));
        assert!(model.can_redo(), "the undone gesture is there to redo");

        half_made_drag(&model, &mut length, 24);
        assert!(!take(Step::Redo, &model, &mut length), "the new edit took the future");
        assert_eq!(published_length(&model, 0), 24, "and left the new edit alone");

        // What is left is the new edit, which steps back as its own gesture.
        assert!(take(Step::Undo, &model, &mut length));
        assert_eq!(published_length(&model, 0), Pattern::DEFAULT_LENGTH);
    }

    /// Walking the stacks the whole way: four committed gestures, back to the
    /// start and forward again, with the model answering each time whether
    /// there was anywhere left to go.
    #[test]
    fn the_buttons_walk_the_stacks_both_ways() {
        let model = committed_model();
        let mut length = LengthBox::new();

        for to in [8_u16, 12, 32, 64] {
            assert_eq!(model.set_length(0, to), Some(to));
            model.commit_edits();
        }

        let mut back = 0;
        while take(Step::Undo, &model, &mut length) {
            back += 1;
        }
        assert_eq!(back, 4, "one step per committed gesture");
        assert_eq!(published_length(&model, 0), Pattern::DEFAULT_LENGTH);
        assert!(!Step::Undo.available(&model) && Step::Redo.available(&model));

        let mut forward = 0;
        while take(Step::Redo, &model, &mut length) {
            forward += 1;
        }
        assert_eq!(forward, 4);
        assert_eq!(published_length(&model, 0), 64, "back at the newest state");
        assert!(Step::Undo.available(&model) && !Step::Redo.available(&model));
    }

    /// An undo publishes through the writer API like any other edit, so an
    /// instance whose editor is open does not pile up retired snapshots: the
    /// step is a publish and a collect, not a leak.
    #[test]
    fn a_step_publishes_and_reclaims() {
        let model = committed_model();
        let mut length = LengthBox::new();
        one_committed_gesture(&model);

        assert!(take(Step::Undo, &model, &mut length));
        assert!(take(Step::Redo, &model, &mut length));
        model.collect();

        assert_eq!(model.live_allocs(), 1, "only the published snapshot is left alive");
    }

    /// The shape of the keyboard half: Ctrl+Z steps back over a committed
    /// gesture and Ctrl+Shift+Z steps forward into it again, exactly as the
    /// two buttons do.
    #[test]
    fn ctrl_z_steps_back_and_ctrl_shift_z_forward() {
        let model = committed_model();
        let mut length = LengthBox::new();
        one_committed_gesture(&model);

        assert_eq!(apply(&[press(Key::Z, CTRL)], false, false, &model, &mut length), 1);
        assert_eq!(
            model.shared().pattern(0).expect("a pattern").rate(),
            Rate::DEFAULT,
            "back to the rate the pattern was made with"
        );

        let shift_z = [press(Key::Z, CTRL | Modifiers::SHIFT)];
        assert_eq!(apply(&shift_z, false, false, &model, &mut length), 1);
        assert_eq!(
            model.shared().pattern(0).expect("a pattern").rate(),
            Rate::ThirtySecond,
            "forward into the edit again"
        );
    }

    /// Ctrl+Y redoes as well, as it does in Bitwig itself
    /// ([`keys::history_key`]).
    #[test]
    fn ctrl_y_redoes_too() {
        let model = committed_model();
        let mut length = LengthBox::new();
        one_committed_gesture(&model);

        assert_eq!(apply(&[press(Key::Z, CTRL)], false, false, &model, &mut length), 1);
        assert_eq!(apply(&[press(Key::Y, CTRL)], false, false, &model, &mut length), 1);
        assert_eq!(model.shared().pattern(0).expect("a pattern").rate(), Rate::ThirtySecond);
    }

    /// A frame carries everything that arrived since the last one, so two
    /// presses in it are two steps — and they are taken in the order they
    /// were pressed.
    #[test]
    fn two_presses_in_one_frame_are_two_steps() {
        let model = committed_model();
        let mut length = LengthBox::new();
        for to in [8_u16, 12] {
            assert_eq!(model.set_length(0, to), Some(to));
            model.commit_edits();
        }

        let frame = [press(Key::Z, CTRL), press(Key::Z, CTRL)];
        assert_eq!(apply(&frame, false, false, &model, &mut length), 2);
        assert_eq!(published_length(&model, 0), Pattern::DEFAULT_LENGTH, "both steps back");

        let frame = [press(Key::Z, CTRL | Modifiers::SHIFT), press(Key::Z, CTRL)];
        let both = apply(&frame, false, false, &model, &mut length);
        assert_eq!(both, 2, "forward and straight back again");
        let left = published_length(&model, 0);
        assert_eq!(left, Pattern::DEFAULT_LENGTH, "where the pair of them leaves it");
    }

    /// The release that follows a press is not a second step.
    #[test]
    fn a_release_is_not_a_step() {
        let model = committed_model();
        let mut length = LengthBox::new();
        one_committed_gesture(&model);

        let frame = [press(Key::Z, CTRL), release(Key::Z, CTRL)];
        assert_eq!(apply(&frame, false, false, &model, &mut length), 1, "one step back, not two");
        assert!(!model.can_undo());
        assert!(model.can_redo());
    }

    /// While a figure is being typed into a box the box has the shortcut:
    /// the model is not touched, and neither is its history — nor is the
    /// half-made length gesture settled out from under the box.
    #[test]
    fn a_box_being_typed_into_keeps_the_shortcut() {
        let model = committed_model();
        let mut length = LengthBox::new();
        one_committed_gesture(&model);
        half_made_drag(&model, &mut length, 24);

        assert_eq!(apply(&[press(Key::Z, CTRL)], true, false, &model, &mut length), 0);
        assert_eq!(published_length(&model, 0), 24, "the edit is still there");
        assert!(length.editing(), "and the gesture is still open");
    }

    /// Nothing else a frame can carry is a step — not the keys the editor
    /// passes on to the host, and not the letter on its own.
    #[test]
    fn other_keys_are_no_business_of_the_history() {
        let model = committed_model();
        let mut length = LengthBox::new();
        one_committed_gesture(&model);

        let frame = [
            press(Key::Z, Modifiers::NONE),
            press(Key::Y, CTRL | Modifiers::SHIFT),
            press(Key::S, CTRL),
            press(Key::Space, Modifiers::NONE),
            press(Key::Delete, Modifiers::NONE),
            Event::PointerGone,
        ];
        assert_eq!(apply(&frame, false, false, &model, &mut length), 0);
        assert_eq!(model.shared().pattern(0).expect("a pattern").rate(), Rate::ThirtySecond);
        assert!(model.can_undo() && !model.can_redo(), "the step is still where it was");
    }

    /// A shortcut with nowhere to go does nothing and says so, which is what
    /// a window whose history is empty answers every time.
    #[test]
    fn a_shortcut_with_nowhere_to_go_is_not_counted() {
        let model = committed_model();
        let mut length = LengthBox::new();

        assert_eq!(apply(&[press(Key::Z, CTRL)], false, false, &model, &mut length), 0);
        let shift_z = [press(Key::Z, CTRL | Modifiers::SHIFT)];
        assert_eq!(apply(&shift_z, false, false, &model, &mut length), 0);
        assert_eq!(published_length(&model, 0), Pattern::DEFAULT_LENGTH, "nothing moved");
        assert!(!length.editing(), "and no gesture was invented");
    }

    /// A key pressed while a hand is still holding something over the grid
    /// is ignored, and the gesture that hand is making stands (see the
    /// module header).
    ///
    /// The committed step behind it is left where it was, so letting go and
    /// pressing again takes the drag back the way it always did.
    #[test]
    fn a_key_during_a_gesture_leaves_the_hand_alone() {
        let model = committed_model();
        let mut length = LengthBox::new();
        one_committed_gesture(&model);

        // Mid-drag the row is already written through, uncommitted, the way
        // `grid::TimeRowDrag` leaves it on every frame of a move.
        assert_eq!(model.set_length(0, 24), Some(24));

        assert_eq!(apply(&[press(Key::Z, CTRL)], false, true, &model, &mut length), 0);
        assert_eq!(published_length(&model, 0), 24, "the hand's own write is still there");
        assert_eq!(
            model.shared().pattern(0).expect("a pattern").rate(),
            Rate::ThirtySecond,
            "and the step before it was not taken back"
        );

        // The same press once the hand has let go, which is what the drag
        // itself does: its own write is the step that comes back first.
        model.commit_edits();
        assert_eq!(apply(&[press(Key::Z, CTRL)], false, false, &model, &mut length), 1);
        assert_eq!(published_length(&model, 0), Pattern::DEFAULT_LENGTH);
    }

    /// A key pressed over a half-made length gesture ends that gesture
    /// first, like a click on the button does — so the shortcut lands on
    /// the state before the figure being dragged, and the box is free to
    /// read the restored length on the next frame ([`take`]).
    #[test]
    fn a_shortcut_settles_a_half_made_gesture_first() {
        let model = committed_model();
        let mut length = LengthBox::new();
        half_made_drag(&model, &mut length, 40);

        assert_eq!(apply(&[press(Key::Z, CTRL)], false, false, &model, &mut length), 1);
        assert!(!length.editing(), "the box is no longer holding the gesture");
        assert_eq!(published_length(&model, 0), Pattern::DEFAULT_LENGTH, "where the drag started");
    }

    /// The two buttons are two different buttons: their own label, their own
    /// hover text, their own slot, and each of them says the one thing a user
    /// could otherwise expect of it and not get.
    #[test]
    fn each_button_says_what_it_is_and_what_it_is_not() {
        assert_eq!(Step::BOTH, [Step::Undo, Step::Redo], "back before forward");
        assert_eq!(Step::Undo.label(), "Undo");
        assert_eq!(Step::Redo.label(), "Redo");
        assert_ne!(Step::Undo.hover(), Step::Redo.hover());
        assert_ne!(Step::Undo.empty_hover(), Step::Redo.empty_hover());

        // The host's three parameters are not in this history, which is the
        // one surprise an undo button in a plugin window can hold.
        assert!(UNDO_HOVER.contains("Bitwig's own undo"), "{UNDO_HOVER}");
        for name in ["Pattern", "Mix", "Smoothing"] {
            assert!(UNDO_HOVER.contains(name), "{name} is not named in {UNDO_HOVER}");
        }
        assert!(REDO_HOVER.contains("nothing left to redo"), "{REDO_HOVER}");
    }

    /// The slots are the ones the bar laid out for them, side by side at the
    /// row's right-hand end and never on top of each other.
    #[test]
    fn the_buttons_take_the_slots_the_bar_laid_out() {
        use crate::editor::layout::EditorLayout;
        use crate::editor::{DEFAULT_HEIGHT, DEFAULT_WIDTH};
        use egui::{pos2, vec2};

        let size = vec2(DEFAULT_WIDTH, DEFAULT_HEIGHT);
        let window = EditorLayout::new(Rect::from_min_size(pos2(3.0, 9.0), size));
        let slots = ControlSlots::new(window.bar_row(1));

        let undo = Step::Undo.slot(&slots);
        let redo = Step::Redo.slot(&slots);

        assert_eq!(undo, slots.undo());
        assert_eq!(redo, slots.redo());
        assert_eq!(undo.width(), theme::HISTORY_WIDTH);
        assert_eq!(redo.width(), theme::HISTORY_WIDTH);
        assert_eq!(redo.min.x - undo.max.x, theme::ITEM_SPACING);
        assert!(!undo.intersect(redo).is_positive(), "the two buttons touch, never overlap");
    }
}
