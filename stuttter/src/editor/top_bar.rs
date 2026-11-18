//! The top bar: its pattern buttons and follow toggle, and the edited
//! pattern's rate and length.
//!
//! Sixteen buttons, one per pattern, and a toggle beside them. A click picks
//! the pattern the window *edits*; the pattern the *host* has reached is
//! marked under it; and while follow is on the two are the same thing — the
//! window shows whatever the `pattern` parameter points at, as bitstep's
//! arranger follows its playhead.
//!
//! # Two states, not one selection
//!
//! The playing pattern is not this window's to choose. It is the `pattern`
//! parameter's value ([`crate::StuttterParams::pattern`]), which belongs
//! to the host: Bitwig automates it, a modulator can drive it, and two
//! windows on one instance could not disagree about it. The *edited*
//! pattern is the opposite — a hand over one window, which is why it lives
//! in [`Selection`] and not in the model or the snapshot, and why a second
//! window may edit pattern 3 while the first edits pattern 1.
//!
//! So a button has two independent looks ([`Look`]): egui's selection fill
//! for the one being edited, and [`theme::PATTERN_MARK`] under the one
//! playing. Both at once is the ordinary case — it is what follow makes
//! true, and what a fresh window over a stopped transport already shows.
//!
//! Follow is **off** to begin with, so nothing moves the pattern under the
//! user's hands until they ask for it: with notes or automation switching
//! patterns while the song plays, a window that followed them could not be
//! edited. A new window still opens on the pattern that is playing, once.
//! A plain click on a button leaves follow as it is; a Shift-click turns it
//! off, because with follow on the next frame would put the selection
//! straight back on the playing pattern and the click would do nothing.
//!
//! # Two rows, because one does not fit
//!
//! The bar is [`theme::BAR_ROWS`] rows of controls, and that constant's doc
//! is the arithmetic that made it two: the sixteen buttons and the toggle
//! take 410 of the row's 510 points, and the six controls that follow them
//! want some three hundred. So the strip and the toggle have the first row
//! to themselves ([`BarSlots`]), and the second holds the edited pattern's
//! rate and length with Mix, Smoothing, undo/redo and keep-on-top after them
//! ([`ControlSlots`]).
//!
//! # Rate and length are the pattern's, not the window's
//!
//! Unlike the pattern being edited, these two *are* model state: they are
//! fields of the [`Pattern`] being edited, saved with the project, and
//! shared by every window on the instance. So the controls hold no value of
//! their own — they are drawn from the frame's snapshot and written straight
//! back through the model's writer API ([`choose_rate`]), which is where the
//! clamping and the undo step live.
//!
//! The one exception is the length box's own half-finished gesture
//! ([`LengthBox`]): a drag has to keep the number it is moving, because the
//! model is always a frame behind it. It commits one undo step when the drag
//! ends, and otherwise writes through on every change so the grid follows the
//! pointer.
//!
//! # A whole pattern is copied and cleared from its own button
//!
//! Right-click a button for **copy to…** (a submenu of the 16) or **clear**.
//! Those two are the only edits in this window that are about a *whole* pattern
//! rather than about something inside one, and they are the button's because
//! the button is where a pattern is named: the menu opens over the number it
//! acts on, and the submenu is the same sixteen numbers again, laid out as a
//! little strip of its own ([`copy_target`], [`COPY_COLUMNS`]) rather than as a
//! column taller than the window.
//!
//! Both go through the writer API like every other gesture in the window
//! ([`copy_to`], [`clear`]), and each is one undo step, committed where it
//! is applied — a pick from a menu is a whole gesture, as the rate
//! dropdown's is. Neither moves the pattern being *edited*: a right-press is
//! not a click ([`Selection::select`] is only reached by the left button), so
//! copying pattern 3 over pattern 9 leaves the window looking at whatever it
//! was looking at, and the copy is found by clicking 9.
//!
//! # What is pure here
//!
//! Everything but [`ui_pattern_bar`], [`ui_pattern_controls`] and the menu
//! those two are drawn with: the selection and follow rules ([`Selection`]),
//! the parameter's value read as a button index ([`playing_index`]), both
//! rows' geometry ([`BarSlots`], [`ControlSlots`]), the rate labels
//! ([`rate_label`]), the menu's own arithmetic and words ([`copy_target`],
//! [`copy_item_hover`], [`pattern_menu_title`]) and the four handlers that
//! write ([`choose_rate`], [`LengthBox`], [`copy_to`], [`clear`]) — which
//! are driven through a real [`StutterModel`] in the tests here, with no
//! egui and no display. That is the split every band in this window keeps:
//! the arithmetic and the rules are tested on a machine with no display, and
//! the thin egui glue over them is `Verify: manual`.
//!
//! The figures are all [`theme`]'s. The strip is laid out from the left of
//! the bar with the toggle directly after it — the toggle is about the
//! strip, so it stays with it — and the second row from its left in its
//! controls' fixed order.

use egui::{Rect, pos2};
use nice_plug::context::gui::ParamSetter;
use nice_plug::params::{FloatParam, Param};

use super::layout::sane;
use super::theme;
use crate::StuttterParams;
use crate::model::StutterModel;
use crate::notes;
use crate::pattern::{PATTERN_COUNT, Pattern, Rate};
use crate::starters::Starter;
use crate::state::PrefsCell;

/// What each pattern button says: its number, as the user guide counts
/// patterns — one-based, so button `0` is "1".
///
/// Written out rather than formatted per frame: the bar draws sixteen of
/// these on every repaint of a playing transport, and a `&'static str` costs
/// nothing to hand egui.
pub const PATTERN_LABELS: [&str; PATTERN_COUNT] = [
    "1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11", "12", "13", "14", "15", "16",
];

/// The follow toggle's label: the shortest word for what it does, because
/// the bar has sixteen buttons to fit beside it.
pub const FOLLOW_LABEL: &str = "Follow";

/// What the follow toggle says when the pointer rests on it.
pub const FOLLOW_HOVER: &str = "Follow the playing pattern: while it is on, the window \
    switches to whichever pattern notes, automation or the Pattern parameter play.\n\
    Off (the default), the window stays on the pattern you picked, so you can edit while the \
    song plays. Shift-clicking a pattern turns it off.";

/// What a pattern button says when the pointer rests on it.
pub const PATTERN_HOVER: &str = "Click to edit and play this pattern (it takes over at the \
    next step). Shift-click to edit it without playing it.\n\
    The lit bar marks the pattern playing. Right-click to copy or clear it.";

/// What a pattern button says with Notes on, before the note that plays it.
pub const PATTERN_HOVER_NOTES: &str = "Click to edit this pattern. With Notes on, only notes play \
    patterns: with no note, nothing plays.\n\
    The lit bar marks the pattern playing. Right-click to copy or clear it.";

/// The whole of a pattern button's hover text for pattern `index` (0-based):
/// [`PATTERN_HOVER`] with notes off; with the Notes setting at `start`,
/// [`PATTERN_HOVER_NOTES`] and the note that plays it — and with `latch` on,
/// the one that stops it ([`crate::notes`]).
pub fn pattern_hover(index: usize, start: Option<u8>, latch: bool) -> String {
    let number = index.min(PATTERN_COUNT - 1) as u8 + 1;
    let Some(start) = start else {
        return String::from(PATTERN_HOVER);
    };
    let Some(key) = notes::key_for_pattern(number, start) else {
        return String::from(PATTERN_HOVER);
    };
    let (key, stop) = (notes::key_name(key), notes::key_name(notes::stop_key(start)));
    let plays = match latch {
        false => format!("A note on {key} plays it for as long as the note is held."),
        true => format!(
            "A note on {key} starts it, and it keeps playing until the next note. {stop} stops \
            it."
        ),
    };

    format!("{PATTERN_HOVER_NOTES}\n{plays}")
}

/// The value of the Pattern parameter that plays pattern `index` (0-based):
/// position 0 is Off, so pattern 1 is 1.
pub fn pattern_value(index: usize) -> i32 {
    (index.min(PATTERN_COUNT - 1) + 1) as i32
}

/// The `pattern` parameter's value as a button index — `0..PATTERN_COUNT` —
/// or `None` when no button is lit.
///
/// `None` covers both ways that happens: the parameter at Off
/// ([`crate::PATTERN_OFF`], which reads back as `None` already), and a
/// number outside the sixteen patterns there are. The second cannot come
/// from the parameter, which clamps — but this takes a plain `Option<u8>`,
/// so it is the boundary and it clamps too rather than lighting a button
/// that is not there.
pub fn playing_index(pattern: Option<u8>) -> Option<usize> {
    let number = usize::from(pattern?);
    let index = number.checked_sub(1)?;

    (index < PATTERN_COUNT).then_some(index)
}

/// How one pattern button is drawn: the two states it can be in, which are
/// independent of each other ([`Selection::look`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Look {
    /// This is the pattern the window is editing — the step grid, the volume
    /// row and the rate and length controls are all showing it. Drawn as
    /// egui's selected control ([`theme::visuals`]).
    pub edited: bool,
    /// This is the pattern the host has reached. Drawn as the lit mark under
    /// the button ([`theme::PATTERN_MARK`]).
    pub playing: bool,
}

/// Which pattern one open window is editing, and whether it follows the
/// host's.
///
/// Per window and not per instance: it is a hand over a window, like a
/// half-made drag, so it is built afresh in `NiceEguiApp::build`
/// (nice-plug-egui 0.6 never calls `editor_closed`) and is in neither the
/// model nor the saved snapshot. A window opens on the playing pattern with
/// follow off, whatever the window was doing when it was closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    edited: usize,
    follow: bool,
    /// A click asked the host to play `edited`; until the Pattern parameter
    /// says so (or this many frames pass), the window stays on it rather than
    /// following the parameter's old value back.
    pending: u8,
    /// The window has just opened: the first [`Selection::track`] moves it
    /// to the playing pattern once, follow or not.
    opening: bool,
}

/// How many frames a click's pattern is held while the host applies it.
const PENDING_FRAMES: u8 = 30;

impl Selection {
    /// A window that has just opened: editing the first pattern until the
    /// first [`Selection::track`] moves it to the one playing, and following
    /// nothing after that.
    ///
    /// Follow is **off** rather than on, which is the one judgement in this
    /// module. On, the window would spend the session moving the grid out
    /// from under whoever was drawing on it, every time a note or an
    /// automation lane changed pattern. Off, the window edits what it was
    /// asked to edit and the toggle is right there for the times that is not
    /// what is wanted. Opening on the playing pattern is the one help follow
    /// gave that costs nothing, so a new window keeps it.
    pub fn new() -> Selection {
        Selection { edited: 0, follow: false, pending: 0, opening: true }
    }

    /// The pattern this window is editing: an index into the sixteen,
    /// always in range.
    pub fn edited(&self) -> usize {
        self.edited
    }

    /// Whether this window is following the host's pattern.
    pub fn follow(&self) -> bool {
        self.follow
    }

    /// Turns follow on or off. Turning it on moves nothing by itself — the
    /// next [`Selection::track`], which is the next frame, is what catches
    /// the window up with the host.
    pub fn set_follow(&mut self, on: bool) {
        self.follow = on;
    }

    /// A click on a pattern button: that pattern is the one being edited
    /// from now on, and follow is off.
    ///
    /// Follow goes off even when the click landed on the pattern that is
    /// already playing, so that the gesture means one thing rather than two:
    /// a click says "show me this one and leave it there". An index past the
    /// sixteen patterns is folded onto the last of them rather than stored —
    /// no caller here can produce one, and [`Selection::edited`] promises it
    /// cannot be out of range.
    pub fn select(&mut self, index: usize) {
        self.edited = index.min(PATTERN_COUNT - 1);
        self.follow = false;
        self.pending = 0;
        self.opening = false;
    }

    /// A plain click on pattern `index` with Notes on: edit it, and nothing
    /// else — the Pattern parameter is not heard then, so the click does not
    /// set it ([`ui_pattern_bar`]). Follow is left as it is.
    pub fn edit(&mut self, index: usize) {
        self.edited = index.min(PATTERN_COUNT - 1);
        self.pending = 0;
        self.opening = false;
    }

    /// A plain click on pattern `index`: edit it — the click is also setting
    /// the Pattern parameter to it ([`ui_pattern_bar`]). Follow is left as it
    /// is: off, the window stays on `index` whatever plays next; on, it goes
    /// on following, and `index` is what plays once the host applies it.
    pub fn play(&mut self, index: usize) {
        self.edited = index.min(PATTERN_COUNT - 1);
        self.pending = PENDING_FRAMES;
        self.opening = false;
    }

    /// Catches the window up with the host, once a frame before the bar is
    /// drawn: with follow on, the pattern being edited is the one playing.
    ///
    /// Does nothing with follow off, and nothing when there is nothing to
    /// follow — the parameter at Off, or past the sixteen patterns
    /// ([`playing_index`]) — in which case the window keeps the pattern it
    /// was on rather than falling back to the first. Returns the pattern
    /// being edited, which is what the caller wanted it for.
    ///
    /// The grid's gestures must not call this while one is half-made: a drag
    /// over the step grid belongs to the pattern it started on, and bitstep's
    /// arranger holds its own follow back for the same reason
    /// (`editor/arrange.rs`: `self.follow && gesture.is_none()`).
    pub fn track(&mut self, playing: Option<usize>) -> usize {
        if self.pending > 0 {
            match playing == Some(self.edited) {
                true => self.pending = 0,
                false => {
                    self.pending -= 1;
                    return self.edited;
                }
            }
        }
        let opening = std::mem::take(&mut self.opening);
        if (self.follow || opening)
            && let Some(index) = playing
            && index < PATTERN_COUNT
        {
            self.edited = index;
        }

        self.edited
    }

    /// How the button for `index` is drawn, given the pattern the host is
    /// playing ([`playing_index`]).
    pub fn look(&self, index: usize, playing: Option<usize>) -> Look {
        Look { edited: index == self.edited, playing: playing == Some(index) }
    }
}

impl Default for Selection {
    /// [`Selection::new`] — a window that has just opened.
    fn default() -> Selection {
        Selection::new()
    }
}

/// The top bar's row, cut into slots: the sixteen pattern buttons, the
/// follow toggle, and what is left over.
///
/// Built per frame from [`super::layout::EditorLayout::bar_controls`], the
/// row inside the bar's padding, and cheap enough to build twice in one —
/// it is a handful of sums over one rectangle, like the band split it sits
/// inside.
///
/// Left to right, and clamped at every step: the strip of buttons, a
/// [`theme::ITEM_SPACING`] gap, the toggle, another gap, and [`Self::rest`].
/// A bar too narrow for all of that shrinks the buttons to an even share of
/// what the toggle leaves them (bitstep's rule — the buttons all stay
/// reachable in a window the host squeezed, which beats scrolling a strip of
/// sixteen) and then empties the slots from the right rather than reaching
/// past the bar's edge.
#[derive(Clone, Copy, Debug)]
pub struct BarSlots {
    bar: Rect,
    /// How wide one pattern button is drawn: [`theme::PATTERN_WIDTH`], or
    /// the even share a narrower bar leaves it.
    button: f32,
    // The slots themselves. Only `strip` and `button` are read by the
    // drawing; `bar` and `rest` are the arithmetic's two ends, which is why
    // they are reached through a method of their own below.
    /// The whole strip of pattern buttons, gaps and all — empty when the bar
    /// has no room for even a point of button.
    strip: Rect,
    follow: Rect,
    rest: Rect,
}

impl BarSlots {
    /// Cuts `bar` — the row inside the top bar's padding — into its slots.
    ///
    /// A reversed, infinite or NaN rectangle is squared up first, as the
    /// band split does it ([`super::layout::EditorLayout::new`]): every slot
    /// comes out finite, inside the bar, and never negative.
    pub fn new(bar: Rect) -> BarSlots {
        let left = sane(bar.min.x, f32::MIN, f32::MAX);
        let top = sane(bar.min.y, f32::MIN, f32::MAX);
        let right = left + sane(bar.width(), 0.0, f32::MAX);
        let bottom = top + sane(bar.height(), 0.0, f32::MAX);
        let width = right - left;

        // The toggle's width is the first thing taken off, because the strip
        // is the part that can be squeezed: sixteen narrow digits still read
        // as sixteen patterns, where a toggle without its label reads as
        // nothing at all.
        let toggle = sane(theme::FOLLOW_WIDTH, 0.0, width);
        let room = sane(width - toggle - theme::ITEM_SPACING, 0.0, width);

        // One button: its own width, or an even share of the room when there
        // is less of it than that. The gaps are the strip's own, so the
        // sixteen read as one strip; zero when even a point each does not
        // fit, which empties the strip.
        let gaps = theme::CELL_GAP * (PATTERN_COUNT - 1) as f32;
        let button = sane((room - gaps) / PATTERN_COUNT as f32, 0.0, theme::PATTERN_WIDTH);
        let strip_width = if button > 0.0 { PATTERN_COUNT as f32 * button + gaps } else { 0.0 };

        let strip = Rect::from_min_max(pos2(left, top), pos2(left + strip_width, bottom));
        // The gap before the toggle is only there when there is a strip to
        // be clear of.
        let gap = if strip_width > 0.0 { theme::ITEM_SPACING } else { 0.0 };
        let follow_left = sane(strip.max.x + gap, left, right);
        let follow_right = sane(follow_left + toggle, follow_left, right);
        let rest_left = sane(follow_right + theme::ITEM_SPACING, left, right);

        BarSlots {
            bar: Rect::from_min_max(pos2(left, top), pos2(right, bottom)),
            button,
            strip,
            follow: Rect::from_min_max(pos2(follow_left, top), pos2(follow_right, bottom)),
            rest: Rect::from_min_max(pos2(rest_left, top), pos2(right, bottom)),
        }
    }

    /// The bar's row, with any negative or NaN size squared up.
    ///
    /// Nothing is drawn into it — every slot is cut from the same four edges
    /// — so this is the squared-up frame the slots are read against, and the
    /// only place a reversed, infinite or NaN bar can be seen to have been
    /// dealt with ([`BarSlots::new`]).
    pub fn bar(&self) -> Rect {
        self.bar
    }

    /// Pattern `index`'s button, or `None` when there is none to click —
    /// past the sixteen patterns, or in a bar with no room for them.
    pub fn pattern(&self, index: usize) -> Option<Rect> {
        if index >= PATTERN_COUNT {
            return None;
        }

        let left = self.strip.min.x + index as f32 * (self.button + theme::CELL_GAP);
        let rect = Rect::from_min_max(
            pos2(left, self.strip.min.y),
            pos2(left + self.button, self.strip.max.y),
        );

        rect.is_positive().then_some(rect)
    }

    /// The lit mark under pattern `index`'s button: a bar across the bottom
    /// of it, held off its rounded ends and its bottom edge by
    /// [`theme::PATTERN_MARK_INSET`].
    ///
    /// `None` when the button it would mark is not drawn, and when the
    /// button is too small to hold a mark that still reads as one.
    pub fn playing_mark(&self, index: usize) -> Option<Rect> {
        let button = self.pattern(index)?;
        let inset = sane(theme::PATTERN_MARK_INSET, 0.0, button.width() * 0.5);
        let height = sane(theme::PATTERN_MARK_HEIGHT, 0.0, button.height() - inset);
        let bottom = sane(button.max.y - inset, button.min.y, button.max.y);
        let mark = Rect::from_min_max(
            pos2(button.min.x + inset, sane(bottom - height, button.min.y, bottom)),
            pos2(button.max.x - inset, bottom),
        );

        mark.is_positive().then_some(mark)
    }

    /// The follow toggle's slot, directly after the strip.
    pub fn follow(&self) -> Rect {
        self.follow
    }

    /// What is left of the bar's first row once the strip and the toggle
    /// have their slots: a hundred points at the window's own size, and
    /// **nothing is drawn in it**.
    ///
    /// It was meant to hold the rate, the length, Mix, Smoothing, undo/redo
    /// and keep-on-top, and it cannot: those six want three times as much,
    /// which is why the bar has a second row for them
    /// ([`theme::BAR_ROWS`], [`ControlSlots`]). What is left is slack — room
    /// for the strip to grow into, and the reason a window whose host
    /// squeezed it drops this before it shrinks a button. So no drawing
    /// asks for it, and what reads it is the arithmetic's own test: a bar
    /// whose slots add up is a bar with exactly this much over.
    ///
    /// Empty rather than negative in a bar the strip and the toggle have
    /// already filled.
    pub fn rest(&self) -> Rect {
        self.rest
    }
}

// ---- a pattern button's right-click menu ----

/// The copy submenu's own label, with an ellipsis because the item opens a
/// submenu rather than doing something.
pub const COPY_LABEL: &str = "Copy to…";

/// What the copy item says when the pointer rests on it.
pub const COPY_HOVER: &str = "Put a copy of this whole pattern — its rate, its length, its \
    blocks and its levels — over another one.\n\
    Pick the pattern to replace; one undo step puts it back.";

/// The starter submenu's label: like the copy item, it opens a submenu.
pub const FILL_LABEL: &str = "Fill with…";

/// What the fill item says when the pointer rests on it.
pub const FILL_HOVER: &str = "Replace what is drawn on this pattern with a ready-made one to \
    start from, laid out on its own rate and length.\n\
    Pick one to hear it; one undo step puts back what was here.";

/// The Clear item's face.
pub const CLEAR_LABEL: &str = "Clear";

/// What Clear says when the pointer rests on it.
pub const CLEAR_HOVER: &str = "Empty this pattern: every block off the Time row and every \
    level back to 100 %.\n\
    The rate and the length are left as they are, and it is one undo step.";

/// What a copy target says when the pointer rests on the pattern the menu
/// itself opened over — the one target that is not a copy, and so the one
/// button in the submenu that is greyed out.
pub const COPY_SELF_HOVER: &str = "This is the pattern you are copying.";

/// How many target buttons the copy submenu puts in a row.
///
/// Four, so the sixteen are a four-by-four block some 130 points square
/// rather than a column of sixteen items. The column is the shape a menu
/// would ordinarily take, and it does not fit: sixteen items are taller than
/// this window is ([`super::DEFAULT_HEIGHT`]), and a submenu egui had to
/// scroll would hide half the patterns behind a wheel. Four rows of four also
/// read as the strip the menu was opened from, which is the point of
/// numbering them the same way ([`PATTERN_LABELS`]).
pub const COPY_COLUMNS: usize = 4;

/// How many rows of targets that makes — four, and a row over if
/// [`PATTERN_COUNT`] ever stops dividing by [`COPY_COLUMNS`].
pub const COPY_ROWS: usize = PATTERN_COUNT.div_ceil(COPY_COLUMNS);

/// How big one target button is drawn: a step cell square
/// ([`theme::CELL_WIDTH`]), which is the smallest thing this window asks a
/// pointer to hit anywhere else.
///
/// Wider than the strip's own buttons ([`theme::PATTERN_WIDTH`]) on purpose:
/// the strip is squeezed to fit sixteen buttons and a toggle into one bar
/// row, and a popup has no such neighbour to make room for.
pub const COPY_BUTTON_SIZE: f32 = theme::CELL_WIDTH;

/// Which pattern the copy submenu's button at `row`, `column` copies to —
/// `None` where there is no button, which is a column past
/// [`COPY_COLUMNS`] and a row past the sixteen patterns.
///
/// Reading order, so the block of them is numbered as it is read: 1 to 4
/// across the first row, 5 to 8 across the second. The arithmetic is here
/// rather than in the drawing for the reason the rest of this module's is —
/// it is testable without a display, and a submenu that skipped a pattern or
/// offered one twice would be a copy sent to the wrong place.
pub fn copy_target(row: usize, column: usize) -> Option<usize> {
    if column >= COPY_COLUMNS {
        return None;
    }
    let index = row.checked_mul(COPY_COLUMNS)?.checked_add(column)?;

    (index < PATTERN_COUNT).then_some(index)
}

/// What the menu's first line says: which pattern it belongs to, in the
/// strip's own numbering.
///
/// The Time row's menu names the block it opened over for the same reason
/// ([`super::grid::block_span_text`]) — a menu floating over a window is
/// otherwise read as belonging to whatever it happens to be covering, and
/// here that would be the sixteen buttons it came out of.
///
/// A number past the sixteen patterns cannot reach this from the strip, and
/// it is written out plainly rather than clamped: this is a label, and a
/// label that lied about which pattern was about to be cleared would be
/// worse than one that says 17.
pub fn pattern_menu_title(index: usize) -> String {
    format!("Pattern {}", index + 1)
}

/// What one copy target says when the pointer rests on it: both patterns
/// named, in the order the copy goes.
///
/// Formatted rather than a constant, because it names two of sixteen patterns —
/// and only while the submenu is open, which is the one moment this window is
/// not drawing sixty frames a second of a playing transport.
pub fn copy_item_hover(from: usize, to: usize) -> String {
    format!("Replace pattern {} with a copy of pattern {}.", to + 1, from + 1)
}

/// The copy submenu's pick: pattern `from` copied over pattern `to`, whole —
/// rate, length, blocks and levels ([`StutterModel::copy_pattern`]).
///
/// Committed on the spot, so one pick is one undo step: a pick from a menu
/// is a whole gesture, as a rate picked in the dropdown beside it is
/// ([`choose_rate`]).
///
/// `false` when nothing was published — the two indices are the same
/// pattern, which is the one target the submenu greys out, or either is not
/// a pattern at all — and then there is nothing to undo either.
pub fn copy_to(model: &StutterModel, from: usize, to: usize) -> bool {
    if !model.copy_pattern(from, to) {
        return false;
    }

    model.commit_edits();
    true
}

/// A starter picked from the fill submenu: what is drawn on the pattern is
/// replaced by `starter`, on the pattern's own rate and length
/// ([`StutterModel::fill_pattern`]).
///
/// One undo step, committed here for [`copy_to`]'s reason. `false` only for
/// an index that is not a pattern, which nothing in the strip can produce.
pub fn fill(model: &StutterModel, pattern: usize, starter: Starter) -> bool {
    if !model.fill_pattern(pattern, starter) {
        return false;
    }

    model.commit_edits();
    true
}

/// The Clear item: the pattern is emptied — no blocks, every level back to
/// unity — and its rate and length are left alone
/// ([`StutterModel::clear_pattern`]).
///
/// One undo step, committed here for [`copy_to`]'s reason. `false` only for
/// an index that is not a pattern, which nothing in the strip can produce.
///
/// A pattern that was already empty is cleared again and records **no** step,
/// which needs no guard here: the write publishes an identical replacement,
/// and the history compares the patterns rather than counting the writes
/// (`crate::undo::EditHistory::commit`).
pub fn clear(model: &StutterModel, pattern: usize) -> bool {
    if !model.clear_pattern(pattern) {
        return false;
    }

    model.commit_edits();
    true
}

/// The pattern buttons and the follow toggle, drawn into their slots and
/// read back into `selection`.
///
/// `playing` is the host's pattern as a button index ([`playing_index`]);
/// the caller has already called [`Selection::track`] with it, so what is
/// drawn as edited is what the step grid under the bar will show this frame.
///
/// `model` is here for the right-click menu alone ([`ui_pattern_menu`]) —
/// the left-click picks the pattern this *window* edits, which is no edit to
/// the patterns at all and is why [`Selection`] and not the model holds it.
///
/// Placed rect by rect ([`BarSlots`]) rather than laid out by egui, which is
/// how the whole window is placed: a button that came out a point wider than
/// its slot would push the follow toggle along the bar and, in the worst
/// case, off the end of it.
pub(super) fn ui_pattern_bar(
    ui: &mut egui::Ui,
    slots: &BarSlots,
    model: &StutterModel,
    selection: &mut Selection,
    playing: Option<usize>,
    params: &StuttterParams,
    setter: Option<&ParamSetter>,
) {
    for (index, digits) in PATTERN_LABELS.iter().enumerate() {
        let Some(rect) = slots.pattern(index) else {
            // No room for this button, and so none for the ones after it.
            break;
        };
        let look = selection.look(index, playing);

        let label = egui::RichText::new(*digits).size(theme::PATTERN_LABEL_SIZE);
        let response = ui
            .put(rect, egui::Button::new(label).selected(look.edited).truncate())
            // Built only while the pointer is on the button.
            .on_hover_ui(|ui| {
                let prefs = params.state.prefs();
                ui.label(pattern_hover(index, prefs.notes(), prefs.latch()));
            });

        // A click edits and plays the pattern: the Pattern parameter is set
        // through the host, so it switches at the next step and records as
        // automation like any move of the parameter. Shift-click only edits,
        // and so does any click with Notes on, where the notes alone decide
        // what plays.
        if response.clicked() {
            let notes_on = params.state.prefs().notes().is_some();
            match (ui.input(|input| input.modifiers.shift), notes_on) {
                (true, _) => selection.select(index),
                (false, true) => selection.edit(index),
                (false, false) => {
                    selection.play(index);
                    if let Some(setter) = setter {
                        setter.begin_set_parameter(&params.pattern);
                        setter.set_parameter(&params.pattern, pattern_value(index));
                        setter.end_set_parameter(&params.pattern);
                    }
                }
            }
        }
        ui_pattern_menu(&response, model, index);

        // After the button, so the mark is on top of its frame rather than
        // under it.
        if look.playing
            && let Some(mark) = slots.playing_mark(index)
        {
            ui.painter().rect_filled(mark, egui::CornerRadius::ZERO, theme::PATTERN_MARK);
        }
    }

    let follow = slots.follow();
    if follow.is_positive() {
        let on = selection.follow();
        let response = ui
            .put(follow, egui::Button::new(FOLLOW_LABEL).selected(on).truncate())
            .on_hover_text(FOLLOW_HOVER);

        if response.clicked() {
            selection.set_follow(!on);
        }
    }
}

/// One pattern button's right-click menu: copy this pattern over another,
/// fill it with a starter pattern, or clear it.
///
/// `response` is the button's own ([`ui_pattern_bar`]), which is both what a
/// right-press arrives on and what the popup is anchored to, and `index` is
/// the pattern that button is — so there is nothing for this window to
/// remember between frames: this strip is sixteen widgets, and egui already
/// knows which of them was pressed.
///
/// Opening and closing are egui's too ([`egui::Response::context_menu`]
/// keeps the open state in its own memory): the menu closes on Escape, on a
/// click outside it, and on any click inside it, which is right here because
/// every item is a choice — there is no half-made gesture in this menu for a
/// click to belong to.
///
/// Nothing is decided here: the words, the arithmetic and the writes are all
/// above, so the egui glue stays thin.
fn ui_pattern_menu(response: &egui::Response, model: &StutterModel, index: usize) {
    response.context_menu(|ui| {
        // Which button this menu came out of. The strip is sixteen of them
        // in a row and the popup covers several, so without this the menu
        // could be read as belonging to whichever one it is sitting over
        // ([`pattern_menu_title`]).
        ui.label(egui::RichText::new(pattern_menu_title(index)).color(theme::TEXT_DIM));
        ui.separator();

        ui.menu_button(COPY_LABEL, |ui| {
            // The sixteen targets as four rows of four ([`copy_target`]),
            // numbered as the strip is, with the pattern being copied greyed
            // out: it is the one target that is not a copy, and the model
            // refuses it anyway ([`copy_to`]).
            for row in 0..COPY_ROWS {
                ui.horizontal(|ui| {
                    for column in 0..COPY_COLUMNS {
                        let Some(target) = copy_target(row, column) else {
                            break;
                        };

                        let label = egui::RichText::new(PATTERN_LABELS[target])
                            .size(theme::PATTERN_LABEL_SIZE);
                        let button = egui::Button::new(label)
                            .min_size(egui::vec2(COPY_BUTTON_SIZE, COPY_BUTTON_SIZE));
                        let picked = ui
                            .add_enabled(target != index, button)
                            .on_hover_text(copy_item_hover(index, target))
                            .on_disabled_hover_text(COPY_SELF_HOVER);

                        if picked.clicked() {
                            copy_to(model, index, target);
                            ui.close();
                        }
                    }
                });
            }
        })
        .response
        .on_hover_text(COPY_HOVER);

        // The starter patterns, one item each, their descriptions on hover.
        ui.menu_button(FILL_LABEL, |ui| {
            for starter in Starter::ALL {
                if ui.button(starter.label()).on_hover_text(starter.description()).clicked() {
                    fill(model, index, starter);
                    ui.close();
                }
            }
        })
        .response
        .on_hover_text(FILL_HOVER);

        // And the item that empties the pattern, in the theme's warning
        // colour — as every destructive item in this family of plugins is
        // drawn (`super::panel::REMOVE_LABEL`).
        ui.separator();
        let clear_item = egui::RichText::new(CLEAR_LABEL).color(theme::WARN);
        if ui.button(clear_item).on_hover_text(CLEAR_HOVER).clicked() {
            clear(model, index);
            ui.close();
        }
    });
}

/// What the rate dropdown says for each of the five rates, in
/// [`Rate::ALL`]'s order — which is also the order the dropdown lists them.
///
/// [`Rate`]'s own `Display` spelling, written out rather than formatted per
/// frame for the reason [`PATTERN_LABELS`] is: the bar is redrawn on every
/// repaint of a playing transport, and a `&'static str` costs nothing to
/// hand egui. A test holds these against `Display`, so the two cannot drift.
pub const RATE_LABELS: [&str; 5] = ["1/8", "1/8T", "1/16", "1/16T", "1/32"];

/// What the rate dropdown says when the pointer rests on it.
pub const RATE_HOVER: &str = "How long one step of the edited pattern lasts.\n\
    Sixteen steps of 1/16 are one bar; T is a triplet.";

/// What the length box says when the pointer rests on it.
pub const LENGTH_HOVER: &str = "How many steps the edited pattern has, 1 to 64.\n\
    Drag to change it, or click to type a number.\n\
    Shortening a pattern drops the blocks that fall outside it.";

/// How fast the length box answers a drag: a quarter of a step per point, so
/// the whole 1-to-64 range is about 250 points of travel.
///
/// A sweep that fits the window ([`super::DEFAULT_WIDTH`] is 522) and still
/// lands on a named step without holding Shift — egui's own slow modifier,
/// which divides this by ten.
pub const LENGTH_DRAG_SPEED: f64 = 0.25;

/// What the rate dropdown shows for `rate`.
pub fn rate_label(rate: Rate) -> &'static str {
    // `Rate::ALL[d] == d` for all five discriminants and there are five
    // labels, so the discriminant *is* the label's index — held down by
    // `the_rate_labels_are_the_rates_own_names`, which is what makes this
    // index unable to be out of range.
    RATE_LABELS[usize::from(rate.to_u8())]
}

/// The bar's second row, cut into slots: the edited pattern's rate and
/// length, and what is left over.
///
/// The row the pattern strip does not fit on ([`theme::BAR_ROWS`] says why
/// there are two), laid out from its left with a [`theme::ITEM_SPACING`] gap
/// between each pair: the rate at [`theme::RATE_WIDTH`], the length at
/// [`theme::LENGTH_WIDTH`], Mix and Smoothing at [`theme::PARAM_RAIL_WIDTH`]
/// each, Undo and Redo at [`theme::HISTORY_WIDTH`] each, keep-on-top at
/// [`theme::KEEP_WIDTH`] ([`Self::keep`]), and [`Self::rest`] — the slack
/// the row has over after all seven of them.
///
/// Clamped at every step, like [`BarSlots`]: a row too narrow for all of it
/// empties the slots from the right rather than reaching past the row's
/// edge, and a reversed, infinite or NaN row is squared up first.
#[derive(Clone, Copy, Debug)]
pub struct ControlSlots {
    row: Rect,
    rate: Rect,
    length: Rect,
    mix: Rect,
    smooth: Rect,
    undo: Rect,
    redo: Rect,
    keep: Rect,
    notes: Rect,
    latch: Rect,
    rest: Rect,
}

impl ControlSlots {
    /// Cuts `row` — [`super::layout::EditorLayout::bar_row`] of the bar's
    /// second row — into its slots.
    pub fn new(row: Rect) -> ControlSlots {
        let left = sane(row.min.x, f32::MIN, f32::MAX);
        let top = sane(row.min.y, f32::MIN, f32::MAX);
        let right = left + sane(row.width(), 0.0, f32::MAX);
        let bottom = top + sane(row.height(), 0.0, f32::MAX);

        // One slot and where the next one starts, clamped to the row at both
        // ends: a slot that would reach past the row's right edge is cut off
        // there and every slot after it comes out empty, which is the rule
        // `a_narrow_second_row_empties_its_slots_from_the_right` holds.
        let slot = |from: f32, width: f32| {
            let start = sane(from, left, right);
            let end = sane(start + width, start, right);

            (Rect::from_min_max(pos2(start, top), pos2(end, bottom)), end + theme::ITEM_SPACING)
        };

        let (rate, next) = slot(left, theme::RATE_WIDTH);
        let (length, next) = slot(next, theme::LENGTH_WIDTH);
        let (mix, next) = slot(next, theme::PARAM_RAIL_WIDTH);
        let (smooth, next) = slot(next, theme::PARAM_RAIL_WIDTH);
        let (undo, next) = slot(next, theme::HISTORY_WIDTH);
        let (redo, next) = slot(next, theme::HISTORY_WIDTH);
        let (keep, next) = slot(next, theme::KEEP_WIDTH);
        let (notes, next) = slot(next, theme::NOTES_WIDTH);
        let (latch, next) = slot(next, theme::LATCH_WIDTH);
        let rest_left = sane(next, left, right);

        ControlSlots {
            row: Rect::from_min_max(pos2(left, top), pos2(right, bottom)),
            rate,
            length,
            mix,
            smooth,
            undo,
            redo,
            keep,
            notes,
            latch,
            rest: Rect::from_min_max(pos2(rest_left, top), pos2(right, bottom)),
        }
    }

    /// The row, with any negative or NaN size squared up.
    ///
    /// Nothing is drawn into it, as nothing is drawn into [`BarSlots::bar`]:
    /// it is the squared-up frame the slots are read against.
    pub fn row(&self) -> Rect {
        self.row
    }

    /// The rate dropdown's slot, at the row's left end.
    pub fn rate(&self) -> Rect {
        self.rate
    }

    /// The length box's slot, directly after the rate.
    pub fn length(&self) -> Rect {
        self.length
    }

    /// The Mix rail's slot, after the length ([`Rail`]).
    pub fn mix(&self) -> Rect {
        self.mix
    }

    /// The Smoothing rail's slot, beside Mix and the same width.
    pub fn smooth(&self) -> Rect {
        self.smooth
    }

    /// The Undo button's slot (`super::history`).
    pub fn undo(&self) -> Rect {
        self.undo
    }

    /// The Redo button's slot, directly after Undo.
    pub fn redo(&self) -> Rect {
        self.redo
    }

    /// The keep-on-top toggle's slot, directly after Redo
    /// ([`ui_keep_on_top`]).
    pub fn keep(&self) -> Rect {
        self.keep
    }

    /// The Notes dropdown's slot, after keep-on-top ([`ui_notes`]).
    pub fn notes(&self) -> Rect {
        self.notes
    }

    /// The Latch toggle's slot, the last control on the row ([`ui_latch`]).
    pub fn latch(&self) -> Rect {
        self.latch
    }

    /// What is left of the row after all nine controls: slack, and nothing
    /// is drawn in it. Here so that the arithmetic can be read off the slots
    /// — a row that has room over is the row behaving as
    /// [`theme::BAR_ROWS`] says it does, which is what its test asserts and
    /// the only thing that asks for this.
    pub fn rest(&self) -> Rect {
        self.rest
    }
}

/// The pattern this frame draws and writes to: the window's own, caught up
/// with the host's ([`Selection::track`]) unless a gesture is half-made.
///
/// `gesture` is [`LengthBox::editing`] or an open drag over the Time row
/// ([`super::grid::TimeRowDrag::open`]) — every half-made gesture
/// in the window, taken together. With follow on and a drag
/// in progress, a host that changed pattern between two frames would
/// otherwise move the write out from under the pointer, and the drag would
/// finish by setting the length of a pattern the user never touched. So a
/// gesture belongs to the pattern it started on, which is what
/// [`Selection::track`]'s own doc asks of its callers and what bitstep's
/// arranger does for the same reason (`editor/arrange.rs`:
/// `self.follow && gesture.is_none()`).
///
/// The window catches up on the first frame after the gesture ends, so
/// follow is late rather than ignored.
pub fn edited_this_frame(
    selection: &mut Selection,
    playing: Option<usize>,
    gesture: bool,
) -> usize {
    if gesture {
        return selection.edited();
    }

    selection.track(playing)
}

/// A rate picked in the bar's dropdown, written through to the pattern being
/// edited. `true` when the pattern changed.
///
/// `showing` is the rate the dropdown was *drawn* with — the pattern's own,
/// as this frame's snapshot had it — and a frame that picked nothing writes
/// nothing. That check is the whole point of the function: a dropdown reports
/// its value on every frame whether or not it was touched, and writing it
/// back each time would publish a new snapshot sixty times a second and fill
/// the undo stack with steps that changed nothing.
///
/// Committed on the spot, because a pick is a whole gesture — there is no
/// half-chosen rate the way there is a half-finished length drag
/// ([`LengthBox`]) — so one pick is one undo step.
///
/// The rate is *not* clamped here and does not need to be: [`Rate`] has five
/// values and all five are rates. The length beside it is the opposite case.
pub fn choose_rate(model: &StutterModel, pattern: usize, showing: Rate, chosen: Rate) -> bool {
    if chosen == showing || !model.set_rate(pattern, chosen) {
        return false;
    }

    model.commit_edits();
    true
}

/// The length box's own state: the step count it is showing, and whether a
/// gesture owns it.
///
/// egui's `DragValue` needs a `&mut u16` to move, and that number cannot
/// simply be the pattern's — it belongs to a gesture while one is in
/// progress. Hence the two fields and the two rules they encode:
///
/// - **While a gesture is open the box keeps its own value**
///   ([`LengthBox::show`] does nothing), because a drag that was pulled back
///   to the model's value under the pointer could not move at all: the model
///   is a step behind, and the next frame would undo every point of travel.
/// - **Every change is written through at once**
///   ([`LengthBox::changed`]) but **nothing is committed until the gesture
///   ends** ([`LengthBox::settle`]), so the grid under the bar follows the
///   drag live and the whole drag is still one undo step, committed on
///   release and not one per pixel.
///
/// Per opened window, like [`Selection`]: it is a hand over one box, so it
/// is built afresh in `NiceEguiApp::build`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LengthBox {
    /// The step count the box is showing. Handed to the drawing by
    /// [`LengthBox::show`] rather than read from here.
    length: u16,
    gesture: bool,
}

impl LengthBox {
    /// A box nobody has touched, showing a new pattern's length.
    ///
    /// The figure is replaced by the edited pattern's on the first
    /// [`LengthBox::show`], which is before the box is ever drawn — so this
    /// value is only ever seen by a caller that asks before the first frame.
    pub fn new() -> LengthBox {
        LengthBox { length: Pattern::DEFAULT_LENGTH, gesture: false }
    }

    /// Whether a gesture — a drag of the box, or a number being typed into
    /// it — is open. What holds the undo step and the box's value.
    pub fn editing(&self) -> bool {
        self.gesture
    }

    /// Puts the edited pattern's own length in the box, once a frame before
    /// it is drawn, and hands back what the box will show.
    ///
    /// Does nothing while a gesture is open (see [`LengthBox`]). With none
    /// open this is what catches the box up with everything that changes the
    /// length behind its back: a click on another pattern button, an undo, a
    /// project the host loaded.
    pub fn show(&mut self, length: u16) -> u16 {
        if !self.gesture {
            self.length = length;
        }

        self.length
    }

    /// What egui's `DragValue` moves. The gesture is only open once
    /// [`LengthBox::changed`] says the box came up with something.
    pub fn value_mut(&mut self) -> &mut u16 {
        &mut self.length
    }

    /// The box came up with a new length: written straight through to the
    /// pattern, and deliberately *not* committed.
    ///
    /// `showing` is the length the box was drawn with, so a change that
    /// landed back on the pattern's own value writes nothing — the same
    /// guard, and for the same reason, as [`choose_rate`]'s.
    ///
    /// Returns the length the pattern actually got, which the box is set to
    /// as well: the model clamps to `1..=`[`Pattern::MAX_LENGTH`], so a
    /// typed 999 reads back as 64 and the box says 64 rather than arguing
    /// with it. `None` only for an index that is not a pattern, which leaves
    /// the box alone.
    pub fn changed(&mut self, model: &StutterModel, pattern: usize, showing: u16) -> Option<u16> {
        self.gesture = true;

        if self.length == showing {
            return Some(showing);
        }

        let got = model.set_length(pattern, self.length)?;
        self.length = got;

        Some(got)
    }

    /// The gesture is over: everything it wrote becomes one undo step.
    /// `true` if there was a gesture to close.
    ///
    /// Cheap to call on every frame of an idle box, which is how the drawing
    /// calls it — a model with nothing to commit only loads one atomic
    /// ([`StutterModel::commit_edits`]).
    pub fn settle(&mut self, model: &StutterModel) -> bool {
        if !std::mem::take(&mut self.gesture) {
            return false;
        }

        model.commit_edits();
        true
    }
}

impl Default for LengthBox {
    /// [`LengthBox::new`] — a box nobody has touched.
    fn default() -> LengthBox {
        LengthBox::new()
    }
}

/// The edited pattern's rate and length, drawn into their slots on the bar's
/// second row and written straight through the model's writer API.
///
/// `showing` is the pattern being edited as this frame's snapshot has it —
/// read once at the top of the frame, so what the dropdown says and what the
/// grid under the bar draws are the same pattern — and `pattern` is its
/// index, which is what the writes go to.
///
/// Placed rect by rect ([`ControlSlots`]), as the pattern bar is: the rate
/// dropdown is laid out inside a child `Ui` of its own slot rather than put
/// into it, because a `ComboBox` is a container and not a `Widget`, and its
/// width is forced to the slot's so that the length box beside it cannot be
/// pushed along the row.
pub(super) fn ui_pattern_controls(
    ui: &mut egui::Ui,
    slots: &ControlSlots,
    model: &StutterModel,
    pattern: usize,
    showing: &Pattern,
    length: &mut LengthBox,
) {
    let rate_slot = slots.rate();
    if rate_slot.is_positive() {
        let was = showing.rate();
        // What the dropdown picked, which with nothing picked is what it was
        // drawn with — so `choose_rate` writes only on the frame of a pick.
        let mut chosen = was;

        ui.scope_builder(egui::UiBuilder::new().max_rect(rate_slot), |ui| {
            egui::ComboBox::from_id_salt("stuttter-rate")
                .width(rate_slot.width())
                .selected_text(rate_label(was))
                // The five labels fit `RATE_WIDTH`, but a window the host
                // squeezed hands this a narrower slot, and a truncated
                // "1/16…" beats a dropdown that reaches into the row.
                .truncate()
                .show_ui(ui, |ui| {
                    for (&rate, label) in Rate::ALL.iter().zip(RATE_LABELS) {
                        ui.selectable_value(&mut chosen, rate, label);
                    }
                })
                .response
                .on_hover_text(RATE_HOVER);
        });

        choose_rate(model, pattern, was, chosen);
    }

    let length_slot = slots.length();
    if length_slot.is_positive() {
        let was = showing.length();
        length.show(was);

        let response = ui
            .put(
                length_slot,
                egui::DragValue::new(length.value_mut())
                    .speed(LENGTH_DRAG_SPEED)
                    .range(1..=Pattern::MAX_LENGTH)
                    // Typed figures are taken on Enter, not digit by digit:
                    // typing "32" over "16" would otherwise pass through a
                    // length of 3 and cut every block past step 3.
                    .update_while_editing(false)
                    // egui's own floor is `interact_size` (40 points wide,
                    // `theme::apply`), which is wider than this slot: left
                    // to it the box would overhang the row.
                    .min_size(length_slot.size()),
            )
            .on_hover_text(LENGTH_HOVER);

        if response.changed() {
            length.changed(model, pattern, was);
        }

        // The gesture is over as soon as the box is neither being dragged nor
        // typed into. `drag_stopped` and `lost_focus` are the two signals
        // that say so, and the state is asked as well: a gesture left open
        // would pin the box to its own figure and stop `show` ever reading
        // the pattern's back again.
        let idle = !response.dragged() && !response.has_focus();
        if response.drag_stopped() || response.lost_focus() || idle {
            length.settle(model);
        }
    }
}

/// What the Mix rail says it is, on the rail itself.
///
/// The host parameter's own name ([`StuttterParams::mix`]): a control bound
/// to a parameter would be two names for one thing if it called itself
/// anything else, and Bitwig's automation lane for it is right there in the
/// same project saying "Mix".
pub const MIX_LABEL: &str = "Mix";

/// What Mix says when the pointer rests on it.
pub const MIX_HOVER: &str = "How much of the chopped signal you hear against the untouched \
    input.\n\
    Drag or click the rail to set it; right-click to type a value.\n\
    This is a plugin parameter: Bitwig records the move as automation, and \
    Bitwig's undo — not this window's — covers it.";

/// What the Smoothing rail says it is.
///
/// Shortened from the parameter's own "Smoothing", which does not fit beside
/// a value in [`theme::PARAM_RAIL_WIDTH`]. The whole word is the first thing
/// on its hover text, so the rail still names the parameter it drives.
pub const SMOOTH_LABEL: &str = "Smooth";

/// What Smoothing says when the pointer rests on it.
pub const SMOOTH_HOVER: &str = "Smoothing: the length of the short crossfade used wherever the \
    effect would otherwise jump and click.\n\
    Drag or click the rail to set it; right-click to type a value.\n\
    A plugin parameter, like Mix: Bitwig records the move as automation.";

/// Where along `rail` a pointer at `x` is, as a 0-to-1 position along it.
///
/// **Absolute**, like chopdrop's master rail: the value is *where* the pointer
/// is, so a click lands on the value under it and a drag follows the hand. A
/// hand that overshot an end arrives at that end, which is what lets a rail
/// [`theme::PARAM_RAIL_WIDTH`] wide be dragged to exactly 0 % or exactly
/// 100 % — egui goes on reporting the widget the button went down on, so a
/// drag that has left the rail is still answered here.
///
/// A rail with no width, and a pointer that arrived as a NaN, both come out
/// at the rail's left end rather than propagating (`max` then `min`).
pub fn rail_position(rail: Rect, x: f32) -> f32 {
    let width = rail.width();
    if !width.is_finite() || width <= 0.0 {
        return 0.0;
    }

    sane((x - rail.min.x) / width, 0.0, 1.0)
}

/// What one frame of a gesture over a rail asks of its host parameter: the
/// three calls a host wants, in the order it wants them.
///
/// A value and the brackets round it are three separate things to tell a
/// host, and all three can fall in one frame — a bare click is a whole
/// gesture — which is why this is a record of what to send and not an enum
/// of one thing.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RailGesture {
    /// Tell the host a move has begun
    /// ([`ParamSetter::begin_set_parameter`]).
    pub begin: bool,
    /// The value the move has reached, as a 0-to-1 position along the rail;
    /// `None` in a frame that moved nothing.
    pub set: Option<f32>,
    /// Tell the host the move is over ([`ParamSetter::end_set_parameter`]).
    pub end: bool,
}

/// A Mix or Smoothing rail's own state: the automation gesture it has open,
/// and the value being typed into it.
///
/// Per opened window, like [`Selection`] and [`LengthBox`] — a hand over a
/// rail belongs to the window it is over, so both rails are built afresh in
/// `NiceEguiApp::build`. The *value* is never here: it belongs to the host,
/// which is the whole point of binding these two controls to parameters
/// rather than to the model.
///
/// The gesture is tracked here rather than read off egui's drag state for
/// chopdrop's reason (`editor/top_bar.rs`, §The rail is chopdrop's own): egui
/// calls a press that never moved a *click* and not a drag, so a widget that
/// ended its gesture on `drag_stopped` would leave the host recording a
/// gesture nobody is making.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Rail {
    /// Whether the host has been told a gesture is under way and not yet
    /// that it is over: `begin_set_parameter` sent, `end_set_parameter`
    /// still owed.
    held: bool,

    /// What has been typed into the rail, while the rail is a text box;
    /// `None` whenever it is a rail. Empty when the box opens — the figure
    /// the box replaced is its hint, so the first thing typed is the whole
    /// of the new value and nothing has to be selected away first.
    typed: Option<String>,

    /// Whether that box still has to ask for the keyboard focus: set when it
    /// opens and spent on the first frame it is drawn. A box that asked every
    /// frame could never be left — it would take the focus back in the same
    /// frame Escape or a click elsewhere gave it up.
    focus: bool,
}

impl Rail {
    /// Whether the rail is a text box this frame rather than a rail.
    pub fn typing(&self) -> bool {
        self.typed.is_some()
    }

    /// Whether a gesture is open — the `end_set_parameter` this rail still
    /// owes the host.
    pub fn held(&self) -> bool {
        self.held
    }

    /// Reads one frame of the pointer over the rail and says what to tell
    /// the host.
    ///
    /// `on_rail` is whether the primary button went down on *this* rail and
    /// has not come up yet (egui's `is_pointer_button_down_on`);
    /// `primary_down` whether that button is down anywhere at all, which is
    /// what closes the gesture — the release this ends on may well arrive
    /// with the pointer somewhere else entirely, and a bare click's release
    /// arrives in the frame it was pressed in. `at` is where along the rail
    /// the gesture is ([`rail_position`]), or `None` in a frame with no
    /// pointer over it.
    ///
    /// Pure, so the whole of the gesture is tested without a window: what is
    /// left over in the drawing is reading these three off egui's response
    /// and handing the result to [`apply_rail`].
    pub fn frame(&mut self, on_rail: bool, primary_down: bool, at: Option<f32>) -> RailGesture {
        // The primary button alone: the other one opens the box below, and a
        // press of it over the rail must not move the value on its way
        // there.
        let pressed = on_rail && primary_down;
        let mut gesture = RailGesture::default();

        if pressed && !self.held {
            gesture.begin = true;
            self.held = true;
        }

        if pressed {
            gesture.set = at.map(|at| sane(at, 0.0, 1.0));
        }

        // Whether or not the pointer ended up over the rail, and whether or
        // not the frame it came up in was the frame it went down in: the
        // gesture is this rail's to close.
        if self.held && !primary_down {
            gesture.end = true;
            self.held = false;
        }

        gesture
    }

    /// Turns the rail into an empty box asking for a value.
    fn open(&mut self) {
        self.typed = Some(String::new());
        self.focus = true;
    }

    /// Puts the rail back, keeping nothing of what was in the box.
    fn close(&mut self) {
        self.typed = None;
        self.focus = false;
    }
}

/// Sends `gesture` to the host as the three calls it is, and reports whether
/// a value was set.
///
/// Bracketed around the value and never the other way about: a host told a
/// value before it is told a gesture began records it as an edit of the
/// automation lane rather than as a move of the knob (chopdrop's
/// `editor/top_bar.rs`, where that order is documented).
///
/// The value is asked for as the **plain** one, so what the host ends up with
/// is a value the parameter could have reached on its own
/// ([`ParamSetter::set_parameter`] normalizes it back), and only when it
/// differs from the value the parameter already has: a host records what it
/// is told, and a frame in which the hand did not move the value has nothing
/// to say. The brackets are still sent either way — a press that set nothing
/// is still a gesture the host was told about and has to be told the end of.
pub fn apply_rail(param: &FloatParam, setter: &ParamSetter, gesture: RailGesture) -> bool {
    if gesture.begin {
        setter.begin_set_parameter(param);
    }

    let moved = gesture.set.is_some_and(|at| {
        let value = param.preview_plain(sane(at, 0.0, 1.0));
        let moved = value != param.modulated_plain_value();
        if moved {
            setter.set_parameter(param, value);
        }

        moved
    });

    if gesture.end {
        setter.end_set_parameter(param);
    }

    moved
}

/// The two rails on the bar's second row, Mix and Smoothing, each drawn into
/// its slot and each driving its host parameter.
///
/// `setter` is the window's host context ([`StuttterEditor`]'s `gui_ctx`,
/// turned into a setter for the frame): the only way a parameter may be
/// written, because the host owns the value and the GUI asks for it. Without
/// one the rails are drawn and read their parameters but answer nothing,
/// which is only reachable before `NiceEguiApp::build` has run — and
/// nice-plug-egui never draws a frame without it.
///
/// [`StuttterEditor`]: super::StuttterEditor
pub(super) fn ui_param_rails(
    ui: &mut egui::Ui,
    slots: &ControlSlots,
    params: &StuttterParams,
    setter: Option<&ParamSetter>,
    mix: &mut Rail,
    smooth: &mut Rail,
) {
    ui_param_rail(ui, slots.mix(), &params.mix, MIX_LABEL, MIX_HOVER, setter, mix);
    ui_param_rail(ui, slots.smooth(), &params.smooth, SMOOTH_LABEL, SMOOTH_HOVER, setter, smooth);
}

/// One rail: the gesture over it, and the drawing of it.
///
/// Placed into its rect like everything else in this bar, so the rails cannot
/// push the buttons beside them along the row. A slot with no width — a
/// window the host squeezed — draws nothing at all rather than a sliver that
/// a click would still move the parameter with.
fn ui_param_rail(
    ui: &mut egui::Ui,
    rect: Rect,
    param: &FloatParam,
    label: &str,
    hover: &str,
    setter: Option<&ParamSetter>,
    rail: &mut Rail,
) {
    if !rect.is_positive() {
        return;
    }

    if rail.typing() {
        ui_rail_entry(ui, rect, param, setter, rail);
        return;
    }

    // The rail's own id, hashed from its label rather than taken from the
    // `Ui`'s auto counter: the bar's controls are placed rect by rect, so two
    // rails of the same size would otherwise be told apart only by the order
    // they happened to be drawn in. Fixed, so the half-made gesture egui
    // keeps against this id survives the next frame — `Id::unique` is a hash
    // of its argument and nothing else, for all that the name suggests
    // otherwise (egui 0.37's own doc: the globally unique *root* id).
    let id = egui::Id::unique(("stuttter-rail", label));
    let response = ui.interact(rect, id, egui::Sense::click_and_drag());

    let primary_down = ui.input(|input| input.pointer.primary_down());
    let at = response.interact_pointer_pos().map(|pointer| rail_position(rect, pointer.x));
    let gesture = rail.frame(response.is_pointer_button_down_on(), primary_down, at);

    if let Some(setter) = setter {
        apply_rail(param, setter, gesture);
    }

    // After the gesture above, so that the one hand that could ask for both
    // at once — the other button clicked without letting the first one go —
    // does not leave the box open over a gesture nothing can close.
    if response.secondary_clicked() && !rail.held() {
        rail.open();
    }

    paint_rail(ui, rect, param, label, &response);
    response.on_hover_text(hover);
}

/// Draws a rail: the control's box, the fill the value has reached inside it,
/// and the parameter's name and value over both.
///
/// Every colour is the theme's own ([`theme::visuals`]) rather than this
/// module's, which is chopdrop's rail exactly: the box is a control's grey and
/// lightens under the pointer and darkens under a press as the buttons beside
/// it do, the hairline round it is [`theme::GRID_LINE`], and the fill is the
/// colour a lit control takes.
///
/// The fill is a whole rail's worth of rounded rectangle under a clip the
/// value's width, and not a rectangle that width: a fill of its own would
/// round its right-hand edge in the middle of the rail and read as a pill
/// sliding along it rather than as a level filling it.
///
/// The name goes inside at the left in the dimmer of the theme's two text
/// tones and the value at the right in the brighter one, because the bar has
/// no room for a label beside the rail (`theme::PARAM_RAIL_WIDTH`) and
/// because that is the hierarchy the rest of the window reads at: what a
/// control is, dim; what it says, bright.
fn paint_rail(
    ui: &egui::Ui,
    rect: Rect,
    param: &FloatParam,
    label: &str,
    response: &egui::Response,
) {
    let visuals = *ui.style().interact(response);
    let edge = visuals.bg_stroke;
    let painter = ui.painter();

    painter.rect(rect, theme::CONTROL_CORNER, visuals.bg_fill, edge, egui::StrokeKind::Inside);

    let filled = sane(param.modulated_normalized_value(), 0.0, 1.0) * rect.width();
    if filled > 0.0 {
        let clip = Rect::from_min_size(rect.min, egui::vec2(filled, rect.height()));
        painter.with_clip_rect(clip).rect_filled(
            rect,
            theme::CONTROL_CORNER,
            ui.visuals().selection.bg_fill,
        );
        // The hairline again, because the fill was painted over the half of
        // it that is inside the box.
        painter.rect_stroke(rect, theme::CONTROL_CORNER, edge, egui::StrokeKind::Inside);
    }

    let inside = rect.shrink2(theme::CONTROL_PADDING);
    let font = egui::TextStyle::Body.resolve(ui.style());
    painter.text(
        inside.left_center(),
        egui::Align2::LEFT_CENTER,
        label,
        font.clone(),
        theme::TEXT_DIM,
    );
    painter.text(
        inside.right_center(),
        egui::Align2::RIGHT_CENTER,
        param.to_string(),
        font,
        theme::TEXT,
    );
}

/// The rail as a box to type a value into, which a right-click opened.
///
/// A right-click and not a click or a double click, because every press of
/// the primary button over a rail *is* a value: the box has to be reached by
/// a gesture that does not move the parameter on the way in, or Escape could
/// not put the value back where it was.
///
/// Enter or a click elsewhere takes what was typed, Escape abandons it, and a
/// figure the parameter cannot read is simply not taken — the parameter's own
/// parser is the host's, so "50", "50 %" and "50%" all land on Mix, and "4"
/// and "4 ms" both land on Smoothing.
///
/// What it takes is bracketed by the same begin and end a drag sends, because
/// a typed value is an automation gesture too: a host told of a value outside
/// one would record it as an edit of the automation lane rather than as a
/// move of the knob.
fn ui_rail_entry(
    ui: &mut egui::Ui,
    rect: Rect,
    param: &FloatParam,
    setter: Option<&ParamSetter>,
    rail: &mut Rail,
) {
    let typed = rail.typed.as_mut().expect("the box is open");
    let response = ui.put(
        rect,
        egui::TextEdit::singleline(typed)
            .min_size(rect.size())
            .desired_width(rect.width())
            .margin(theme::CONTROL_PADDING)
            .align(egui::Align2::CENTER_CENTER)
            .clip_text(true)
            // The figure the box replaced, as its hint: the box opens empty,
            // so the first thing typed is the whole of the new value, and the
            // value is still readable while it is being typed over.
            .hint_text(param.to_string()),
    );

    if std::mem::take(&mut rail.focus) {
        response.request_focus();
    }

    if response.lost_focus() {
        // Escape leaves a text box in egui by clearing the focus, so it
        // arrives as the same frame a taken box does and is told apart by the
        // key itself — egui's own `DragValue` reads its own the same way, and
        // so does the length box above.
        let escaped = ui.input(|input| input.key_pressed(egui::Key::Escape));
        let typed = rail.typed.take().unwrap_or_default();
        rail.close();

        if let (false, Some(setter), Some(normalized)) =
            (escaped, setter, param.string_to_normalized_value(typed.trim()))
        {
            let whole = RailGesture { begin: true, set: Some(normalized), end: true };
            apply_rail(param, setter, whole);
        }
    }
}

/// The keep-on-top toggle's label: what the switch does, in the two words
/// that fit [`theme::KEEP_WIDTH`].
pub const KEEP_LABEL: &str = "On top";

/// What the keep-on-top toggle says when the pointer rests on it — including a
/// caveat, since whether a window manager acts on a `WM_TRANSIENT_FOR` that
/// arrives after the window was mapped is its own business and not something
/// this plugin can promise (`super::keys` §Keep on top).
pub const KEEP_HOVER: &str = "Keep this window above Bitwig's.\n\
    On, the desktop holds it in front, as Bitwig itself asks for; off, the two \
    windows stack like any other pair and Alt+Tab moves between them.\n\
    Saved with the project, and on for a new device.\n\
    If turning it back on does not put the window in front again, close the \
    window and open it once more — that is the window manager's call.";

/// Flips the keep-on-top pref and hands back what it now says.
///
/// The only place the switch is written, as [`super::grid::remember_action`]
/// is for the pref beside it: a click on the toggle, read as "whatever it was
/// not". Nothing is clamped and nothing can be — a bool is already one of
/// the two things it may be — and nothing is committed either, because this
/// is not a pattern: the window's prefs are saved with the project but are
/// not in the edit history, so Undo does not move the switch (the history
/// covers the patterns alone).
///
/// What the switch *does* is the window's, a frame later and once per change
/// ([`super::keys::HostWindow::set_keep_on_top`]): the editor hands the pref
/// over on every frame, so a project load moves the window exactly as a
/// click on this toggle does.
pub fn toggle_keep_on_top(prefs: &PrefsCell) -> bool {
    let on = !prefs.get().keep_on_top;
    prefs.set_keep_on_top(on);

    on
}

/// The keep-on-top toggle, drawn into the last slot on the bar's second row.
///
/// Lit while the switch is on and drawn exactly as the follow toggle above it
/// is (`egui::Button::selected`), because the two are the same kind of thing:
/// a switch with two states, one of them lit. A slot with no width — a window
/// the host squeezed — draws nothing rather than a sliver a click could still
/// flip the switch with.
pub(super) fn ui_keep_on_top(ui: &mut egui::Ui, slots: &ControlSlots, prefs: &PrefsCell) {
    let rect = slots.keep();
    if !rect.is_positive() {
        return;
    }

    let on = prefs.get().keep_on_top;
    let response = ui
        .put(rect, egui::Button::new(KEEP_LABEL).selected(on).truncate())
        .on_hover_text(KEEP_HOVER);

    if response.clicked() {
        toggle_keep_on_top(prefs);
    }
}

/// What the Notes dropdown says, closed: "Notes off", or "Notes C6" with the
/// C its keys start from.
pub fn notes_label(start: Option<u8>) -> String {
    match start {
        None => String::from("Notes off"),
        Some(start) => format!("Notes {}", notes::key_name(start)),
    }
}

/// One choice in the open Notes dropdown: "Off", or the sixteen keys it
/// covers — "C6–D#7".
pub fn notes_choice(start: Option<u8>) -> String {
    match start {
        None => String::from("Off"),
        Some(start) => notes::range_name(start),
    }
}

/// What the Notes dropdown says when the pointer rests on it.
pub const NOTES_HOVER: &str = "Play the patterns from notes. Pick where the sixteen keys start: \
    the first plays pattern 1, the next pattern 2, and so on. A pattern plays for as long as \
    its note is held, or with Latch on until the next note. With no note, nothing plays: the \
    Pattern parameter is not used while Notes is on.\n\
    Choose a range the instrument before this device does not use, or its notes will switch \
    patterns: after a drum machine, C6 is above every pad, so stutter notes can go in the \
    same clip and make no sound.\n\
    Saved with the project, and off for a new device.";

/// The Notes dropdown, drawn into the last slot on the bar's second row: off,
/// or the C the pattern keys start from ([`crate::notes`]).
///
/// A pref like keep-on-top beside it — saved with the project, not in the
/// edit history, written straight into the cell `process()` reads it from
/// ([`PrefsCell::set_notes`]). A slot with no width draws nothing.
pub(super) fn ui_notes(ui: &mut egui::Ui, slots: &ControlSlots, prefs: &PrefsCell) {
    let rect = slots.notes();
    if !rect.is_positive() {
        return;
    }

    let current = prefs.notes();
    let mut chosen = current;
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
        egui::ComboBox::from_id_salt("stuttter-notes")
            .width(rect.width())
            .selected_text(notes_label(current))
            .truncate()
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut chosen, None, notes_choice(None));
                for start in notes::starts() {
                    ui.selectable_value(&mut chosen, Some(start), notes_choice(Some(start)));
                }
            })
            .response
            .on_hover_text(NOTES_HOVER);
    });

    if chosen != current {
        prefs.set_notes(chosen);
    }
}

/// The Latch toggle's label.
pub const LATCH_LABEL: &str = "Latch";

/// What the Latch toggle says when the pointer rests on it.
pub const LATCH_HOVER: &str = "Off (the default): a pattern plays for as long as its note is \
    held.\n\
    On: a note starts its pattern and it keeps playing after the note ends, until the next \
    pattern note — so a short drum hit is enough. The key after the sixteen (E7 from C6) \
    stops it, and so does stopping the transport.\n\
    Saved with the project.";

/// The Latch toggle, drawn into the last slot on the bar's second row:
/// whether notes play their pattern while held or latch it
/// ([`crate::notes`]). Greyed out with Notes off, where it means nothing.
/// A pref like Notes beside it, written straight into the cell `process()`
/// reads it from ([`PrefsCell::set_latch`]).
pub(super) fn ui_latch(ui: &mut egui::Ui, slots: &ControlSlots, prefs: &PrefsCell) {
    let rect = slots.latch();
    if !rect.is_positive() {
        return;
    }

    let on = prefs.latch();
    let button = egui::Button::new(LATCH_LABEL).selected(on).truncate();
    let response = ui
        .add_enabled_ui(prefs.notes().is_some(), |ui| ui.put(rect, button))
        .inner
        .on_hover_text(LATCH_HOVER)
        .on_disabled_hover_text(LATCH_HOVER);

    if response.clicked() {
        prefs.set_latch(!on);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::SpeedPreset;
    use crate::editor::layout::EditorLayout;
    use crate::editor::{DEFAULT_HEIGHT, DEFAULT_WIDTH};
    use crate::pattern::{Action, MAX_STEPS};
    use crate::state::Prefs;
    use crate::{SMOOTH_MAX_MS, SMOOTH_MIN_MS};
    use egui::vec2;
    use nice_plug::context::gui::{GuiContext, GuiContextInner};
    use nice_plug::params::internals::ParamPtr;
    use nice_plug::plugin::PluginState;

    /// The window at the size it opens at, wherever the host put it.
    fn window() -> EditorLayout {
        EditorLayout::new(Rect::from_min_size(pos2(17.0, 5.0), vec2(DEFAULT_WIDTH, DEFAULT_HEIGHT)))
    }

    /// The bar's second row at the window's own size: the row the rate and
    /// the length are laid into.
    fn control_row() -> Rect {
        window().bar_row(1)
    }

    /// A model with a committed starting point, as the editor leaves it
    /// after its first frame: sixteen default patterns and an empty history.
    fn committed_model() -> StutterModel {
        let model = StutterModel::default();
        model.commit_edits();
        assert!(!model.can_undo(), "nothing to step back into yet");

        model
    }

    /// The pattern the audio thread would read for `index` — what a write
    /// through the writer API actually published.
    fn published(model: &StutterModel, index: usize) -> Pattern {
        model.shared().pattern(index).expect("a pattern of the sixteen").clone()
    }

    /// The bar's first row of a window at the size it opens at.
    fn bar() -> Rect {
        window().bar_row(0)
    }

    /// A window that has just opened goes to the playing pattern once, and
    /// does not follow it after that.
    #[test]
    fn a_new_window_opens_on_the_playing_pattern_and_then_stays_put() {
        let mut selection = Selection::new();

        assert_eq!(selection, Selection::default());
        assert_eq!(selection.edited(), 0);
        assert!(!selection.follow(), "follow is off by default");

        // The host is on pattern 4: the window opens there, and stays there
        // when a note or automation moves on.
        assert_eq!(selection.track(Some(3)), 3);
        assert_eq!(selection.track(Some(8)), 3);

        // Opened with nothing playing, it stays on pattern 1 when something
        // starts.
        let mut quiet = Selection::new();
        assert_eq!(quiet.track(None), 0);
        assert_eq!(quiet.track(Some(5)), 0);
    }

    /// A plain click edits the pattern and asks the host to play it, and
    /// leaves follow as it was. With follow off the window stays on it
    /// whatever plays; with it on, the window holds it while the parameter
    /// catches up instead of snapping back to the old value, then follows.
    #[test]
    fn a_click_plays_the_pattern_and_waits_for_the_host() {
        let mut selection = Selection::new();
        assert_eq!(selection.track(Some(0)), 0);

        selection.play(5);
        assert!(!selection.follow(), "a click does not turn follow on");
        for playing in [Some(0), Some(5), Some(9), None] {
            assert_eq!(selection.track(playing), 5);
        }

        selection.set_follow(true);
        selection.play(5);
        assert_eq!(selection.edited(), 5);
        assert!(selection.follow());
        // The host has not applied it yet: the window stays on 5.
        assert_eq!(selection.track(Some(0)), 5);
        assert_eq!(selection.track(Some(0)), 5);
        // Now it has, and from here on the window follows the parameter.
        assert_eq!(selection.track(Some(5)), 5);
        assert_eq!(selection.track(Some(9)), 9);

        // A host that never applies it does not hold the window for ever.
        selection.play(2);
        for _ in 0..PENDING_FRAMES {
            assert_eq!(selection.track(Some(9)), 2);
        }
        assert_eq!(selection.track(Some(9)), 9);
        assert_eq!(pattern_value(0), 1, "position 0 of the parameter is Off");
        assert_eq!(pattern_value(15), 16);
    }

    /// With Notes on a plain click only edits: no wait for the host, and
    /// follow as it was.
    #[test]
    fn a_click_with_notes_on_only_edits() {
        let mut selection = Selection::new();
        selection.edit(6);
        assert_eq!(selection.edited(), 6);
        assert!(!selection.follow());
        assert_eq!(selection.track(Some(2)), 6, "nothing is pending, nothing follows");

        selection.set_follow(true);
        selection.edit(9);
        assert!(selection.follow(), "follow is left alone");
        assert_eq!(selection.track(Some(2)), 2, "and goes on following");
    }

    /// A shift-click picks the pattern being edited without playing it — and
    /// takes the window off follow, so that it sticks instead of being undone
    /// by the next frame.
    #[test]
    fn a_shift_click_selects_for_edit_and_turns_follow_off() {
        let mut selection = Selection::new();

        selection.select(7);
        assert_eq!(selection.edited(), 7);
        assert!(!selection.follow());

        selection.set_follow(true);
        selection.select(2);
        assert_eq!(selection.edited(), 2);
        assert!(!selection.follow(), "a click says 'show me this one and leave it there'");

        // Including a click on the pattern that is already playing: the
        // gesture means one thing, not two.
        selection.set_follow(true);
        assert_eq!(selection.track(Some(5)), 5);
        selection.select(5);
        assert_eq!(selection.edited(), 5);
        assert!(!selection.follow());
    }

    /// With follow on, the edited pattern is the host's; with it off, the
    /// window is left alone. Nothing to follow — Off, or a pattern that does
    /// not exist — leaves the window on the pattern it was on.
    #[test]
    fn follow_tracks_the_host_and_nothing_else_moves_the_selection() {
        let mut selection = Selection::new();
        selection.select(9);

        // Off: the host moves, the window does not.
        for playing in [Some(0), Some(15), None] {
            assert_eq!(selection.track(playing), 9);
        }

        // On: the window catches up on the next frame, not on the toggle.
        selection.set_follow(true);
        assert_eq!(selection.edited(), 9, "turning follow on moves nothing by itself");
        assert_eq!(selection.track(Some(0)), 0);
        assert_eq!(selection.track(Some(15)), 15);

        // And nothing to follow leaves the last pattern shown, rather than
        // falling back to the first.
        assert_eq!(selection.track(None), 15);
        assert_eq!(selection.track(Some(PATTERN_COUNT)), 15);
        assert!(selection.follow(), "following nothing is still following");
    }

    /// The two looks are independent: the pattern being edited is filled,
    /// the one playing is marked, and the ordinary case is one button that
    /// is both.
    #[test]
    fn the_look_separates_the_edited_button_from_the_lit_one() {
        let mut selection = Selection::new();
        selection.select(3);

        assert_eq!(selection.look(3, Some(3)), Look { edited: true, playing: true });
        assert_eq!(selection.look(3, Some(8)), Look { edited: true, playing: false });
        assert_eq!(selection.look(8, Some(8)), Look { edited: false, playing: true });
        assert_eq!(selection.look(8, None), Look { edited: false, playing: false });

        // Exactly one button is edited and at most one is lit, which is what
        // the bar draws from.
        let edited = (0..PATTERN_COUNT).filter(|&i| selection.look(i, Some(3)).edited).count();
        let lit = (0..PATTERN_COUNT).filter(|&i| selection.look(i, Some(3)).playing).count();
        assert_eq!((edited, lit), (1, 1));
        assert_eq!((0..PATTERN_COUNT).filter(|&i| selection.look(i, None).playing).count(), 0);
    }

    /// The `pattern` parameter read as a button: one-based at the parameter,
    /// zero-based at the strip, and nothing lit for Off or for a number that
    /// is not a pattern.
    #[test]
    fn the_parameter_lights_the_button_it_names() {
        assert_eq!(playing_index(Some(1)), Some(0));
        assert_eq!(playing_index(Some(16)), Some(PATTERN_COUNT - 1));

        assert_eq!(playing_index(None), None, "Off lights nothing");
        assert_eq!(playing_index(Some(0)), None, "Off again, spelled as a number");
        assert_eq!(playing_index(Some(17)), None, "there is no seventeenth pattern");
        assert_eq!(playing_index(Some(u8::MAX)), None);

        // Every pattern the parameter can name has a button, and no two name
        // the same one.
        let buttons: Vec<_> = (1..=PATTERN_COUNT as u8).map(|n| playing_index(Some(n))).collect();
        assert_eq!(buttons, (0..PATTERN_COUNT).map(Some).collect::<Vec<_>>());
    }

    /// A selection can never leave the sixteen patterns, whatever it is
    /// handed.
    #[test]
    fn a_nonsense_index_cannot_leave_the_sixteen() {
        let mut selection = Selection::new();

        for index in [PATTERN_COUNT, PATTERN_COUNT + 1, usize::MAX] {
            selection.select(index);
            assert_eq!(selection.edited(), PATTERN_COUNT - 1);
        }

        selection.set_follow(true);
        for index in [PATTERN_COUNT, usize::MAX] {
            assert_eq!(selection.track(Some(index)), PATTERN_COUNT - 1, "{index}");
        }
    }

    /// The bar of a window at its own size: sixteen whole buttons, a
    /// [`theme::CELL_GAP`] apart, the follow toggle after them at its own
    /// width, and room left over for the second row's controls.
    #[test]
    fn the_default_bar_holds_sixteen_buttons_the_toggle_and_room_to_spare() {
        let bar = bar();
        let slots = BarSlots::new(bar);

        assert_eq!(slots.button, theme::PATTERN_WIDTH);
        assert_eq!(slots.bar(), bar);

        let mut last: Option<Rect> = None;
        for index in 0..PATTERN_COUNT {
            let button = slots.pattern(index).unwrap_or_else(|| panic!("button {index}"));

            assert_eq!(button.width(), theme::PATTERN_WIDTH, "button {index}");
            assert_eq!(button.height(), theme::CONTROL_HEIGHT, "button {index}");
            assert!(bar.contains_rect(button), "button {index} is outside the bar");
            assert!(slots.strip.contains_rect(button), "button {index} is outside the strip");

            if let Some(previous) = last {
                assert_eq!(button.min.x - previous.max.x, theme::CELL_GAP, "gap before {index}");
            }
            last = Some(button);
        }
        assert_eq!(slots.pattern(PATTERN_COUNT), None, "there is no seventeenth button");

        // The strip ends where its last button does, and the toggle follows
        // it by one item's spacing.
        assert_eq!(slots.strip.max.x, last.expect("sixteen buttons").max.x);
        assert_eq!(slots.follow().min.x - slots.strip.max.x, theme::ITEM_SPACING);
        assert_eq!(slots.follow().width(), theme::FOLLOW_WIDTH);
        assert_eq!(slots.follow().height(), theme::CONTROL_HEIGHT);

        // And what is left is the next tasks' room.
        assert!(slots.rest().is_positive(), "the rate and Mix have to go somewhere");
        assert_eq!(slots.rest().min.x - slots.follow().max.x, theme::ITEM_SPACING);
        assert_eq!(slots.rest().max.x, bar.max.x);
        assert!(bar.contains_rect(slots.rest()));
    }

    /// Every slot is inside the bar and no two of them share a point, so a
    /// click lands on one control or on none.
    #[test]
    fn the_slots_never_overlap() {
        for width in [DEFAULT_WIDTH, 460.0, 300.0, 120.0, 60.0, 10.0] {
            let bar = Rect::from_min_size(pos2(4.0, 9.0), vec2(width, theme::CONTROL_HEIGHT));
            let slots = BarSlots::new(bar);

            let mut rects: Vec<Rect> =
                (0..PATTERN_COUNT).filter_map(|i| slots.pattern(i)).collect();
            rects.push(slots.follow());
            rects.push(slots.rest());

            for (index, &rect) in rects.iter().enumerate() {
                assert!(bar.contains_rect(rect), "slot {index} at {width}");
                for &other in rects.iter().skip(index + 1) {
                    assert!(!rect.intersect(other).is_positive(), "slot {index} at {width}");
                }
            }
        }
    }

    /// A bar narrower than the strip wants shrinks the buttons to an even
    /// share rather than pushing the toggle off the end: all sixteen stay
    /// reachable in a window the host squeezed.
    #[test]
    fn a_narrow_bar_shrinks_the_buttons_to_an_even_share() {
        let narrow = Rect::from_min_size(pos2(0.0, 0.0), vec2(260.0, theme::CONTROL_HEIGHT));
        let slots = BarSlots::new(narrow);

        assert!(slots.button > 0.0);
        assert!(slots.button < theme::PATTERN_WIDTH, "the buttons gave way");
        assert_eq!(slots.pattern(PATTERN_COUNT - 1).map(|r| r.max.x), Some(slots.strip.max.x));
        assert_eq!(slots.follow().width(), theme::FOLLOW_WIDTH, "the toggle keeps its label");
        assert!(narrow.contains_rect(slots.follow()));
        assert!(!slots.rest().is_positive(), "there is nothing left over");

        // The buttons are all the same width, which is what keeps the strip
        // reading as a strip.
        let widths: Vec<f32> = (0..PATTERN_COUNT)
            .map(|i| slots.pattern(i).expect("sixteen buttons").width())
            .collect();
        assert!(widths.iter().all(|&w| w == slots.button), "{widths:?}");
    }

    /// And a bar with no room at all: no buttons, no toggle, no panic — the
    /// drawing simply has nothing to place.
    #[test]
    fn a_bar_squeezed_to_nothing_has_no_slots() {
        for size in [vec2(0.0, theme::CONTROL_HEIGHT), vec2(DEFAULT_WIDTH, 0.0), vec2(0.0, 0.0)] {
            let slots = BarSlots::new(Rect::from_min_size(pos2(2.0, 3.0), size));

            for index in 0..PATTERN_COUNT {
                assert_eq!(slots.pattern(index), None, "button {index} at {size:?}");
                assert_eq!(slots.playing_mark(index), None, "mark {index} at {size:?}");
            }
            assert!(!slots.follow().is_positive(), "at {size:?}");
            assert!(!slots.rest().is_positive(), "at {size:?}");
        }
    }

    /// The lit mark sits inside the button it marks, across the bottom of
    /// it, clear of its rounded corners.
    #[test]
    fn the_lit_mark_sits_inside_the_button_it_marks() {
        let slots = BarSlots::new(bar());

        for index in 0..PATTERN_COUNT {
            let button = slots.pattern(index).expect("sixteen buttons");
            let mark = slots.playing_mark(index).expect("a mark under each");

            assert!(button.contains_rect(mark), "mark {index}");
            assert_eq!(mark.height(), theme::PATTERN_MARK_HEIGHT, "mark {index}");
            assert_eq!(mark.width(), button.width() - 2.0 * theme::PATTERN_MARK_INSET);
            assert_eq!(button.max.y - mark.max.y, theme::PATTERN_MARK_INSET, "mark {index}");
            assert!(mark.min.y > button.center().y, "the mark is under the digits");
        }

        assert_eq!(slots.playing_mark(PATTERN_COUNT), None);
    }

    /// Host data is host data even when it arrives as a rectangle: a
    /// reversed, infinite or NaN bar is squared up rather than carried into
    /// every slot.
    #[test]
    fn a_nonsense_bar_is_squared_up() {
        let reversed = Rect::from_min_max(pos2(500.0, 400.0), pos2(100.0, 50.0));
        let nan = Rect::from_min_size(pos2(f32::NAN, f32::NAN), vec2(f32::NAN, f32::NAN));

        for input in [reversed, Rect::EVERYTHING, Rect::NOTHING, nan] {
            let slots = BarSlots::new(input);
            let bar = slots.bar();

            assert!(bar.width() >= 0.0 && bar.height() >= 0.0, "{input:?}");
            assert!(bar.min.x.is_finite() && bar.min.y.is_finite(), "{input:?}");
            assert!(bar.max.x.is_finite() && bar.max.y.is_finite(), "{input:?}");

            let mut rects = vec![slots.strip, slots.follow(), slots.rest()];
            rects.extend((0..PATTERN_COUNT).filter_map(|i| slots.pattern(i)));
            rects.extend((0..PATTERN_COUNT).filter_map(|i| slots.playing_mark(i)));
            for rect in rects {
                assert!(rect.width() >= 0.0 && rect.height() >= 0.0, "{input:?}");
                assert!(bar.contains_rect(rect), "{input:?}");
            }
        }
    }

    // ---- the second row: the edited pattern's rate and length ----

    /// The dropdown's five labels are [`Rate`]'s own names, in the order
    /// [`Rate::ALL`] lists them — so [`rate_label`] can index by the
    /// discriminant and the two spellings cannot drift apart.
    #[test]
    fn the_rate_labels_are_the_rates_own_names() {
        assert_eq!(RATE_LABELS.len(), Rate::ALL.len());

        for (index, rate) in Rate::ALL.iter().enumerate() {
            assert_eq!(usize::from(rate.to_u8()), index, "{rate} is not at its discriminant");
            assert_eq!(RATE_LABELS[index], rate.to_string(), "label {index}");
            assert_eq!(rate_label(*rate), rate.to_string(), "{rate}");
        }

        // The intended spelling, written out so a change to `Display` fails
        // here with the name it moved.
        assert_eq!(RATE_LABELS, ["1/8", "1/8T", "1/16", "1/16T", "1/32"]);
        assert_eq!(rate_label(Rate::DEFAULT), "1/16");
    }

    /// Every slot of the second row, in the order they are laid out: the rate
    /// and the length, Mix and Smoothing, Undo and Redo and keep-on-top, each
    /// at its full width and a gap after the one before it — with room still
    /// left over at the end, which is the whole reason the bar grew a second
    /// row ([`theme::BAR_ROWS`]).
    #[test]
    fn the_second_row_holds_its_nine_controls_and_room_to_spare() {
        let row = control_row();
        let slots = ControlSlots::new(row);

        assert_eq!(slots.row(), row);
        assert_eq!(row.height(), theme::CONTROL_HEIGHT, "the row is one control tall");
        assert_eq!(slots.rate().min.x, row.min.x, "the rate opens the row");

        let laid_out = [
            ("rate", slots.rate(), theme::RATE_WIDTH),
            ("length", slots.length(), theme::LENGTH_WIDTH),
            ("mix", slots.mix(), theme::PARAM_RAIL_WIDTH),
            ("smooth", slots.smooth(), theme::PARAM_RAIL_WIDTH),
            ("undo", slots.undo(), theme::HISTORY_WIDTH),
            ("redo", slots.redo(), theme::HISTORY_WIDTH),
            ("keep", slots.keep(), theme::KEEP_WIDTH),
            ("notes", slots.notes(), theme::NOTES_WIDTH),
            ("latch", slots.latch(), theme::LATCH_WIDTH),
        ];

        for (name, rect, width) in laid_out {
            assert_eq!(rect.width(), width, "the {name} slot's width");
            assert_eq!(rect.height(), theme::CONTROL_HEIGHT, "the {name} slot's height");
            assert!(row.contains_rect(rect), "the {name} is outside the row");
        }

        // Each one exactly a gap after the one before it, and the leftover
        // after the last of them.
        let mut in_order: Vec<Rect> = laid_out.iter().map(|(_, rect, _)| *rect).collect();
        in_order.push(slots.rest());
        for pair in in_order.windows(2) {
            assert_eq!(pair[1].min.x - pair[0].max.x, theme::ITEM_SPACING, "{pair:?}");
            assert!(!pair[0].intersect(pair[1]).is_positive(), "the slots touch, never overlap");
        }

        assert_eq!(slots.rest().max.x, row.max.x);
        assert!(row.contains_rect(slots.rest()));
        assert!(slots.rest().width() > 0.0, "the row is drawn with slack over");
    }

    /// A row the host squeezed empties its slots from the right rather than
    /// reaching past its edge, and one squeezed to nothing has no slots at
    /// all — the rule [`BarSlots`] follows, so a window the host made narrow
    /// draws fewer controls and never a control over the Time row.
    #[test]
    fn a_narrow_second_row_empties_its_slots_from_the_right() {
        let full = theme::RATE_WIDTH + theme::ITEM_SPACING + theme::LENGTH_WIDTH;
        let squeezed =
            [control_row().width(), 450.0, 300.0, full, 70.0, theme::RATE_WIDTH, 20.0, 0.0];

        for width in squeezed {
            let row = Rect::from_min_size(pos2(8.0, 3.0), vec2(width, theme::CONTROL_HEIGHT));
            let slots = ControlSlots::new(row);

            let rects = [
                slots.rate(),
                slots.length(),
                slots.mix(),
                slots.smooth(),
                slots.undo(),
                slots.redo(),
                slots.keep(),
                slots.notes(),
                slots.latch(),
                slots.rest(),
            ];
            for (index, &rect) in rects.iter().enumerate() {
                assert!(rect.width() >= 0.0, "slot {index} at {width}");
                assert!(row.contains_rect(rect), "slot {index} at {width}");
                for &other in rects.iter().skip(index + 1) {
                    assert!(!rect.intersect(other).is_positive(), "slot {index} at {width}");
                }
            }

            // From the right and not in patches: a slot the row could not fit
            // is the last one with anything in it.
            if let Some(first) = rects.iter().position(|rect| !rect.is_positive()) {
                assert!(
                    rects[first..].iter().all(|rect| !rect.is_positive()),
                    "a slot came back after an empty one at {width}"
                );
            }

            // The rate is the last to go, and nothing is left over before the
            // rate and the length both have their full width.
            if width < full {
                assert!(!slots.mix().is_positive(), "no room for a rail at {width}");
                assert!(!slots.rest().is_positive(), "nothing to spare at {width}");
            }
            if width <= theme::RATE_WIDTH {
                assert!(!slots.length().is_positive(), "no length box at {width}");
            }
        }

        let nothing = ControlSlots::new(Rect::from_min_size(pos2(4.0, 4.0), vec2(0.0, 0.0)));
        for rect in [
            nothing.rate(),
            nothing.length(),
            nothing.mix(),
            nothing.smooth(),
            nothing.undo(),
            nothing.redo(),
            nothing.keep(),
            nothing.notes(),
            nothing.latch(),
            nothing.rest(),
        ] {
            assert!(!rect.is_positive());
        }
    }

    /// Host data is host data even when it arrives as a rectangle: a
    /// reversed, infinite or NaN row is squared up rather than carried into
    /// every slot.
    #[test]
    fn a_nonsense_second_row_is_squared_up() {
        let reversed = Rect::from_min_max(pos2(500.0, 400.0), pos2(100.0, 50.0));
        let nan = Rect::from_min_size(pos2(f32::NAN, f32::NAN), vec2(f32::NAN, f32::NAN));

        for input in [reversed, Rect::EVERYTHING, Rect::NOTHING, nan] {
            let slots = ControlSlots::new(input);
            let row = slots.row();

            assert!(row.width() >= 0.0 && row.height() >= 0.0, "{input:?}");
            assert!(row.min.x.is_finite() && row.min.y.is_finite(), "{input:?}");
            assert!(row.max.x.is_finite() && row.max.y.is_finite(), "{input:?}");

            for rect in [
                slots.rate(),
                slots.length(),
                slots.mix(),
                slots.smooth(),
                slots.undo(),
                slots.redo(),
                slots.keep(),
                slots.rest(),
            ] {
                assert!(rect.width() >= 0.0 && rect.height() >= 0.0, "{input:?}");
                assert!(row.contains_rect(rect), "{input:?}");
            }
        }
    }

    /// A rate picked in the dropdown reaches the pattern being edited, and
    /// only that one: the write goes through the model's writer API, so what
    /// the audio thread reads next block is the new rate.
    #[test]
    fn choosing_a_rate_writes_it_through_to_the_edited_pattern() {
        let model = committed_model();
        let was = published(&model, 5).rate();
        assert_eq!(was, Rate::DEFAULT, "a new pattern is 1/16");

        assert!(choose_rate(&model, 5, was, Rate::ThirtySecond));

        assert_eq!(published(&model, 5).rate(), Rate::ThirtySecond);
        assert_eq!(published(&model, 5).step_beats(), 0.125, "and the step with it");
        for other in [0, 4, 6, PATTERN_COUNT - 1] {
            assert_eq!(published(&model, other).rate(), Rate::DEFAULT, "pattern {other} moved");
        }

        // One pick is one undo step, committed on the spot: there is no
        // half-chosen rate to wait for.
        assert!(model.can_undo(), "the pick is on the stack already");
        assert!(model.undo());
        assert_eq!(published(&model, 5).rate(), Rate::DEFAULT);

        // And every one of the five lands.
        for rate in Rate::ALL {
            let showing = published(&model, 2).rate();
            choose_rate(&model, 2, showing, rate);
            assert_eq!(published(&model, 2).rate(), rate, "{rate}");
        }
    }

    /// The frame that picked nothing writes nothing. A dropdown reports its
    /// value on every frame whether or not it was touched, so without this
    /// guard an open window would publish a snapshot per frame and stack up
    /// undo steps that changed nothing.
    #[test]
    fn a_rate_that_was_not_picked_writes_nothing() {
        let model = committed_model();
        let edits = model.edit_count();

        for _ in 0..60 {
            let showing = published(&model, 0).rate();
            assert!(!choose_rate(&model, 0, showing, showing), "nothing was picked");
        }

        assert_eq!(model.edit_count(), edits, "sixty idle frames wrote nothing");
        assert!(!model.can_undo(), "and left nothing on the undo stack");

        // An index that is not a pattern publishes nothing either, and does
        // not panic: nothing in the editor can produce one, and this is the
        // boundary, so it clamps.
        //
        // Measured against what was *published* rather than against
        // `edit_count`, which counts writes attempted and is bumped before
        // the index is even looked at — the undo history compares the data,
        // so an attempt that published nothing is not a step to step back
        // into either (`crate::undo::EditHistory::commit`).
        assert!(!choose_rate(&model, PATTERN_COUNT, Rate::DEFAULT, Rate::Eighth));
        assert!(!choose_rate(&model, usize::MAX, Rate::DEFAULT, Rate::Eighth));

        model.commit_edits();
        assert!(!model.can_undo(), "an index that is not a pattern left no step behind");
        for index in 0..PATTERN_COUNT {
            assert_eq!(published(&model, index).rate(), Rate::DEFAULT, "pattern {index}");
        }
    }

    /// A length drag: every step of it is written through so the grid under
    /// the bar follows the pointer, and the whole of it is **one** undo step,
    /// committed when the gesture ends.
    #[test]
    fn a_length_drag_writes_through_and_commits_one_undo_step() {
        let model = committed_model();
        let mut box_ = LengthBox::new();

        // The frame before the drag: the box shows the pattern's own length.
        assert_eq!(box_.show(published(&model, 0).length()), Pattern::DEFAULT_LENGTH);
        assert!(!box_.editing());

        // And then the drag, one frame per step egui came up with.
        for wanted in [15, 14, 12, 9, 7, 8] {
            let showing = published(&model, 0).length();
            *box_.value_mut() = wanted;

            assert_eq!(box_.changed(&model, 0, showing), Some(wanted));
            assert!(box_.editing(), "the gesture is open at {wanted}");
            assert_eq!(published(&model, 0).length(), wanted, "the grid follows the drag");
            assert_eq!(box_.length, wanted);
            assert!(!model.can_undo(), "nothing is committed mid-drag");
        }

        // Released.
        assert!(box_.settle(&model));
        assert!(!box_.editing());
        assert!(model.can_undo(), "the gesture is one step now");

        // One step, not six: stepping back lands on the length before the
        // drag started and not on one of the figures it passed through.
        assert!(model.undo());
        assert_eq!(published(&model, 0).length(), Pattern::DEFAULT_LENGTH);
        assert!(!model.undo(), "there was nothing before it");

        // Settling twice is settling once — the drawing calls it on every
        // idle frame.
        assert!(!box_.settle(&model));
        assert!(!box_.settle(&model));
    }

    /// What the box can come up with is clamped by the model, and the box
    /// takes the clamped figure rather than arguing with it: a typed 999
    /// reads back as 64 and the box says 64.
    #[test]
    fn the_length_box_shows_what_the_pattern_actually_got() {
        let model = committed_model();
        let mut box_ = LengthBox::new();

        for (typed, got) in [(999, MAX_STEPS as u16), (0, 1), (64, 64), (1, 1), (33, 33)] {
            let showing = published(&model, 3).length();
            *box_.value_mut() = typed;

            assert_eq!(box_.changed(&model, 3, showing), Some(got), "typed {typed}");
            assert_eq!(box_.length, got, "the box argued with the model about {typed}");
            assert_eq!(published(&model, 3).length(), got, "typed {typed}");
        }

        // An index that is not a pattern leaves the box and the patterns
        // alone rather than panicking or publishing.
        *box_.value_mut() = 12;
        assert_eq!(box_.changed(&model, PATTERN_COUNT, 33), None);
        assert_eq!(box_.length, 12, "nothing came back to clamp it with");
        assert_eq!(published(&model, 3).length(), 33, "pattern 3 kept what it had");
    }

    /// A gesture owns the box's figure: the pattern's own length is not read
    /// back into it mid-drag, because the model is a frame behind the pointer
    /// and the next frame would undo every point of travel.
    #[test]
    fn a_half_made_drag_keeps_the_box_from_being_read_back() {
        let model = committed_model();
        let mut box_ = LengthBox::new();

        *box_.value_mut() = 20;
        assert_eq!(box_.changed(&model, 0, Pattern::DEFAULT_LENGTH), Some(20));

        // Whatever the model says — another pattern's length, an undo behind
        // the box's back — the open gesture keeps its own.
        for length in [1, 16, 64] {
            assert_eq!(box_.show(length), 20, "the drag was pulled back to {length}");
        }

        // Once it is over, the box follows the model again: a click on
        // another pattern's button, an undo, a project the host loaded.
        box_.settle(&model);
        assert_eq!(box_.show(7), 7);
        assert_eq!(box_.show(64), 64);
        assert_eq!(box_.length, 64);
    }

    /// Shortening a pattern drops the blocks that fall outside it — the
    /// model's rule, not this module's — and the gesture's one undo step
    /// puts them back together with the length.
    #[test]
    fn shortening_a_pattern_drops_its_blocks_and_one_undo_puts_them_back() {
        let model = committed_model();
        assert_eq!(model.add_block(1, 2, 2, Action::default()), Some(0));
        assert_eq!(model.add_block(1, 12, 4, Action::default()), Some(1));
        model.commit_edits();
        assert_eq!(published(&model, 1).blocks().len(), 2);

        let mut box_ = LengthBox::new();
        box_.show(published(&model, 1).length());
        *box_.value_mut() = 6;
        assert_eq!(box_.changed(&model, 1, Pattern::DEFAULT_LENGTH), Some(6));
        box_.settle(&model);

        let shortened = published(&model, 1);
        assert_eq!(shortened.length(), 6);
        assert_eq!(shortened.blocks().len(), 1, "the block at step 12 is outside now");
        assert_eq!(shortened.blocks()[0].start(), 2);

        assert!(model.undo());
        let restored = published(&model, 1);
        assert_eq!(restored.length(), Pattern::DEFAULT_LENGTH);
        assert_eq!(restored.blocks().len(), 2, "one step back brings both of them");
    }

    /// Both handlers write to the pattern they are given and to no other, so
    /// a window editing pattern 9 while the host plays pattern 1 changes 9.
    #[test]
    fn the_handlers_write_to_the_pattern_being_edited_and_no_other() {
        let model = committed_model();
        let mut selection = Selection::new();
        selection.select(9);
        let edited = selection.track(playing_index(Some(1)));
        assert_eq!(edited, 9, "follow is off, so the host's pattern is not the edited one");

        let showing = published(&model, edited);
        assert!(choose_rate(&model, edited, showing.rate(), Rate::EighthTriplet));

        let mut box_ = LengthBox::new();
        box_.show(published(&model, edited).length());
        *box_.value_mut() = 48;
        assert_eq!(box_.changed(&model, edited, showing.length()), Some(48));
        box_.settle(&model);

        let written = published(&model, edited);
        assert_eq!(written.rate(), Rate::EighthTriplet);
        assert_eq!(written.length(), 48);

        let untouched = Pattern::new();
        for other in (0..PATTERN_COUNT).filter(|&i| i != edited) {
            let pattern = published(&model, other);
            assert_eq!(pattern.rate(), untouched.rate(), "pattern {other}");
            assert_eq!(pattern.length(), untouched.length(), "pattern {other}");
        }
    }

    /// Follow is held back while the length box has a gesture in it: a drag
    /// belongs to the pattern it started on, so a host that changes pattern
    /// mid-drag does not move the write out from under the pointer.
    #[test]
    fn a_half_made_gesture_holds_follow_back() {
        let model = committed_model();
        let mut selection = Selection::new();
        selection.set_follow(true);
        let mut box_ = LengthBox::new();

        // Following, with nothing half-made: the window goes where the host
        // is, and the box reads that pattern's length.
        assert_eq!(edited_this_frame(&mut selection, Some(4), box_.editing()), 4);
        assert_eq!(box_.show(published(&model, 4).length()), Pattern::DEFAULT_LENGTH);

        // A drag starts on pattern 4, and the host moves on under it.
        *box_.value_mut() = 24;
        assert_eq!(box_.changed(&model, 4, Pattern::DEFAULT_LENGTH), Some(24));
        assert!(box_.editing());

        for moved in [Some(7), Some(0), None, Some(15)] {
            assert_eq!(
                edited_this_frame(&mut selection, moved, box_.editing()),
                4,
                "the drag followed the host to {moved:?}"
            );
        }
        assert!(selection.follow(), "follow is held back, not turned off");

        // So the whole drag landed on pattern 4 and on nothing else.
        *box_.value_mut() = 30;
        assert_eq!(box_.changed(&model, 4, 24), Some(30));
        box_.settle(&model);
        assert_eq!(published(&model, 4).length(), 30);
        for other in (0..PATTERN_COUNT).filter(|&i| i != 4) {
            assert_eq!(published(&model, other).length(), Pattern::DEFAULT_LENGTH, "{other}");
        }

        // And the window catches up on the first frame after the gesture:
        // late rather than ignored.
        assert_eq!(edited_this_frame(&mut selection, Some(15), box_.editing()), 15);
        assert_eq!(box_.show(published(&model, 15).length()), Pattern::DEFAULT_LENGTH);
    }

    /// A host that records what the GUI asked it to do to a parameter, in the
    /// order it asked.
    ///
    /// The one thing the rails cannot be tested without: Mix and Smoothing
    /// are written by the *host* and not by the plugin, so a rail with
    /// nothing behind it would prove only that an arithmetic was done.
    /// chopdrop's own harness (`editor/top_bar.rs`), which is where this shape
    /// comes from.
    #[derive(Default)]
    struct RecordingHost {
        /// `begin` / `set <normalized>` / `end`, which is the shape of one
        /// automation gesture.
        calls: std::sync::Mutex<Vec<String>>,
    }

    impl RecordingHost {
        fn push(&self, call: String) {
            self.calls.lock().expect("the recording host is never poisoned").push(call);
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().expect("the recording host is never poisoned").clone()
        }
    }

    impl nice_plug::context::gui::GuiContextInner for RecordingHost {
        fn plugin_api(&self) -> nice_plug::context::PluginApi {
            nice_plug::context::PluginApi::Clap
        }

        unsafe fn raw_begin_set_parameter(&self, _param: ParamPtr) {
            self.push("begin".to_string());
        }

        unsafe fn raw_set_parameter_normalized(&self, _param: ParamPtr, normalized: f32) {
            self.push(format!("set {normalized}"));
        }

        unsafe fn raw_end_set_parameter(&self, _param: ParamPtr) {
            self.push("end".to_string());
        }

        fn get_state(&self) -> PluginState {
            PluginState {
                version: String::new(),
                params: std::collections::BTreeMap::new(),
                fields: std::collections::BTreeMap::new(),
            }
        }

        fn set_state(&self, _state: PluginState) {}

        fn request_restart(&self) {}
    }

    /// An instance's parameters with a host listening for what the rails ask
    /// of them: the `GuiContext` has to outlive the setter taken from it, so
    /// both are held here as the editor holds them.
    struct Bound {
        params: StuttterParams,
        host: std::sync::Arc<RecordingHost>,
        gui_ctx: GuiContext,
    }

    impl Bound {
        fn new() -> Bound {
            let host = std::sync::Arc::new(RecordingHost::default());
            let inner = std::sync::Arc::clone(&host) as std::sync::Arc<dyn GuiContextInner>;

            Bound { params: StuttterParams::default(), host, gui_ctx: GuiContext::new(inner) }
        }

        fn setter(&self) -> ParamSetter<'_> {
            self.gui_ctx.param_setter()
        }

        fn calls(&self) -> Vec<String> {
            self.host.calls()
        }
    }

    /// A rail's slot at the window's own size — what [`rail_position`] maps a
    /// pointer along.
    fn rail() -> Rect {
        ControlSlots::new(control_row()).mix()
    }

    /// The two rails name the parameters they drive and say who owns them,
    /// which is the one thing about these two controls that is not like
    /// everything else in the window (the host's parameters are not in this
    /// plugin's undo history).
    #[test]
    fn each_rail_names_its_parameter_and_says_what_owns_it() {
        let params = StuttterParams::default();

        assert_eq!(MIX_LABEL, params.mix.name(), "the rail calls Mix what the host calls it");
        assert_eq!(params.smooth.name(), "Smoothing");
        assert!(
            params.smooth.name().starts_with(SMOOTH_LABEL),
            "'{SMOOTH_LABEL}' has to read as the parameter's own name shortened"
        );
        assert!(SMOOTH_LABEL.len() < params.smooth.name().len(), "it is the shortening");

        for hover in [MIX_HOVER, SMOOTH_HOVER] {
            assert!(hover.contains("right-click"), "the typed value is not offered: {hover}");
            assert!(hover.contains("Bitwig"), "who owns the value is not said: {hover}");
        }
        assert!(MIX_HOVER.contains("automation") && SMOOTH_HOVER.contains("automation"));
        assert!(SMOOTH_HOVER.starts_with("Smoothing"), "the whole word is on the hover");
    }

    /// The rail is absolute: the value is where along it the pointer is, so a
    /// click lands on the value under the pointer and a drag follows the hand.
    #[test]
    fn a_rails_position_is_where_along_it_the_pointer_is() {
        let rail = rail();

        assert_eq!(rail_position(rail, rail.min.x), 0.0, "the left end is 0");
        assert_eq!(rail_position(rail, rail.max.x), 1.0, "the right end is 1");
        assert_eq!(rail_position(rail, rail.center().x), 0.5, "the middle is half");
        assert_eq!(rail_position(rail, rail.min.x + rail.width() * 0.25), 0.25);

        // A hand that overshot either end arrives at that end, which is what
        // lets a rail a hundred points wide be dragged to exactly 0 % and
        // exactly 100 %.
        assert_eq!(rail_position(rail, rail.min.x - 500.0), 0.0);
        assert_eq!(rail_position(rail, rail.max.x + 500.0), 1.0);
        assert_eq!(rail_position(rail, f32::NEG_INFINITY), 0.0);
        assert_eq!(rail_position(rail, f32::INFINITY), 1.0);
        assert_eq!(rail_position(rail, f32::NAN), 0.0, "a NaN pointer is not a NaN value");
    }

    /// A slot the host squeezed to nothing — or a nonsense one — answers 0
    /// rather than an infinity or a NaN: the clamp is at the boundary, as
    /// everywhere else in this window.
    #[test]
    fn a_rail_with_no_width_has_no_positions_along_it() {
        let empty = Rect::from_min_size(pos2(30.0, 7.0), vec2(0.0, theme::CONTROL_HEIGHT));

        for x in [-10.0, 0.0, 30.0, 90.0, f32::NAN] {
            assert_eq!(rail_position(empty, x), 0.0, "at {x}");
            assert_eq!(rail_position(Rect::NOTHING, x), 0.0, "at {x}");
            assert_eq!(rail_position(Rect::EVERYTHING, x), 0.0, "at {x}");
        }
    }

    /// One press, one gesture: the press opens it, every frame of it that
    /// moves asks for a value, and the button coming back up closes it —
    /// which is the whole of what a host needs to record a move as automation
    /// rather than as an edit.
    #[test]
    fn a_press_opens_one_gesture_and_the_release_closes_it() {
        let mut rail = Rail::default();
        assert!(!rail.held() && !rail.typing(), "a new rail owes the host nothing");

        // The frame the button went down in.
        let down = rail.frame(true, true, Some(0.25));
        assert_eq!(down, RailGesture { begin: true, set: Some(0.25), end: false });
        assert!(rail.held(), "the end is owed");

        // A frame of the drag: the value moves, the gesture does not reopen.
        let dragging = rail.frame(true, true, Some(0.75));
        assert_eq!(dragging, RailGesture { begin: false, set: Some(0.75), end: false });

        // And the release, with the pointer gone from the rail: egui reports
        // the widget the button went down on, so this is still the rail's.
        let up = rail.frame(true, false, Some(0.75));
        assert_eq!(up, RailGesture { begin: false, set: None, end: true });
        assert!(!rail.held(), "nothing is owed any more");

        // An idle frame after it asks for nothing at all.
        assert_eq!(rail.frame(false, false, None), RailGesture::default());
    }

    /// The release is answered wherever the pointer ended up — the hand may
    /// have left the window — and a frame with no pointer position still
    /// closes the gesture rather than leaving the host recording one nobody
    /// is making.
    #[test]
    fn a_gesture_closes_even_when_the_pointer_has_gone() {
        let mut rail = Rail::default();

        assert!(rail.frame(true, true, Some(0.5)).begin);
        let gone = rail.frame(false, false, None);

        assert_eq!(gone, RailGesture { begin: false, set: None, end: true });
        assert!(!rail.held());
    }

    /// A press whose frame has no pointer position asks for no value and
    /// still opens the gesture: the brackets are about the hand, the value is
    /// about where it is.
    #[test]
    fn a_press_with_nowhere_to_set_is_still_a_gesture() {
        let mut rail = Rail::default();

        let opened = RailGesture { begin: true, set: None, end: false };
        assert_eq!(rail.frame(true, true, None), opened);
        assert!(rail.frame(true, false, None).end, "and the release still closes it");
    }

    /// The other mouse button does not move the value: a press of it over the
    /// rail is how the box to type a value into is opened, and the rail must
    /// not slide under the pointer on the way there.
    #[test]
    fn a_press_that_is_not_the_primary_button_moves_nothing() {
        let mut rail = Rail::default();

        // `on_rail` without `primary_down`: the pointer is over the rail with
        // the other button down.
        assert_eq!(rail.frame(true, false, Some(0.9)), RailGesture::default());
        assert!(!rail.held(), "nothing was opened, so nothing is owed");
    }

    /// A second press is a second gesture, with its own brackets: a host told
    /// of two moves records two.
    #[test]
    fn a_second_press_is_a_second_gesture() {
        let mut rail = Rail::default();

        for position in [0.1_f32, 0.9] {
            assert!(rail.frame(true, true, Some(position)).begin, "press at {position}");
            assert!(rail.frame(true, false, Some(position)).end, "release at {position}");
        }
    }

    /// A position outside the rail cannot reach the host even if a caller
    /// hands one in: the gesture clamps what it reports, as the mapping
    /// clamps what it reads.
    #[test]
    fn a_gesture_clamps_the_position_it_reports() {
        let mut rail = Rail::default();

        assert_eq!(rail.frame(true, true, Some(5.0)).set, Some(1.0));
        assert_eq!(rail.frame(true, true, Some(-5.0)).set, Some(0.0));
        assert_eq!(rail.frame(true, true, Some(f32::NAN)).set, Some(0.0), "a NaN is not a value");
    }

    /// What reaches the host, in the order it reaches it: the gesture's begin,
    /// the value as a plain one the parameter could have reached on its own,
    /// and the gesture's end.
    #[test]
    fn a_gesture_reaches_the_host_as_three_calls_in_order() {
        let bound = Bound::new();
        let setter = bound.setter();
        let whole = RailGesture { begin: true, set: Some(0.5), end: true };

        assert!(apply_rail(&bound.params.mix, &setter, whole), "the value moved");
        assert_eq!(bound.calls(), vec!["begin", "set 0.5", "end"], "Mix at half");

        // Smoothing is the same rail over a range that does not start at
        // zero: half way along it is the middle of 0.5 ms to 20 ms, and what
        // the host is told is the normalized position of that plain value.
        let bound = Bound::new();
        let setter = bound.setter();
        assert!(apply_rail(&bound.params.smooth, &setter, whole));
        assert_eq!(bound.calls(), vec!["begin", "set 0.5", "end"]);
        assert_eq!(bound.params.smooth.preview_plain(0.5), (SMOOTH_MIN_MS + SMOOTH_MAX_MS) * 0.5);
    }

    /// A frame that moved nothing says nothing: the value is only sent when
    /// it differs from the one the parameter already has, because a host
    /// records what it is told and a held rail that has not moved has nothing
    /// to say.
    #[test]
    fn a_value_the_parameter_already_has_is_not_sent() {
        let bound = Bound::new();
        let setter = bound.setter();

        // Mix starts fully wet, which is the rail's right-hand end.
        assert_eq!(bound.params.mix.modulated_normalized_value(), 1.0);
        let at_the_end = RailGesture { begin: true, set: Some(1.0), end: true };

        assert!(!apply_rail(&bound.params.mix, &setter, at_the_end), "nothing moved");
        assert_eq!(bound.calls(), vec!["begin", "end"], "the brackets are still owed");
    }

    /// The brackets are sent on their own frames too, which is what a drag
    /// looks like from the host's side: one begin, a value per frame that
    /// moved, one end.
    #[test]
    fn a_drag_is_one_pair_of_brackets_round_the_values_it_set() {
        let bound = Bound::new();
        let setter = bound.setter();
        let slot = rail();
        let mut rail = Rail::default();

        // Pressed a quarter along, dragged to three quarters, let go.
        for (on_rail, down, x) in [
            (true, true, Some(slot.min.x + slot.width() * 0.25)),
            (true, true, Some(slot.min.x + slot.width() * 0.5)),
            (true, true, Some(slot.min.x + slot.width() * 0.75)),
            (true, false, Some(slot.max.x)),
        ] {
            let at = x.map(|x| rail_position(slot, x));
            apply_rail(&bound.params.mix, &setter, rail.frame(on_rail, down, at));
        }

        assert_eq!(
            bound.calls(),
            vec!["begin", "set 0.25", "set 0.5", "set 0.75", "end"],
            "one gesture, three values, and the release that closed it"
        );
        assert!(!rail.held());
    }

    /// A value typed into the box the right-click opens is a whole gesture of
    /// its own, parsed by the parameter's own parser — the host's — so what
    /// the host is told is a value it could have sent itself.
    #[test]
    fn a_typed_value_is_one_whole_gesture() {
        let bound = Bound::new();
        let setter = bound.setter();

        let normalized = bound
            .params
            .mix
            .string_to_normalized_value("50 %")
            .expect("the host's own parser reads a percentage");
        let typed = RailGesture { begin: true, set: Some(normalized), end: true };

        assert!(apply_rail(&bound.params.mix, &setter, typed));
        assert_eq!(bound.calls(), vec!["begin", "set 0.5", "end"]);

        // And a figure outside the range is pulled to the nearest end rather
        // than refused, which is what the parameter does with it.
        let bound = Bound::new();
        let setter = bound.setter();
        let over = bound.params.smooth.string_to_normalized_value("500").expect("a number");
        let typed = RailGesture { begin: true, set: Some(over), end: true };

        assert!(apply_rail(&bound.params.smooth, &setter, typed));
        assert_eq!(bound.calls(), vec!["begin", "set 1", "end"], "clamped to 20 ms");
    }

    /// A gesture with nothing in it reaches the host as nothing: the rails are
    /// drawn on every frame of a playing transport, and a window nobody is
    /// touching must not fill a host's automation lane.
    #[test]
    fn an_untouched_rail_tells_the_host_nothing() {
        let bound = Bound::new();
        let setter = bound.setter();
        let mut rail = Rail::default();

        for _ in 0..60 {
            let gesture = rail.frame(false, false, None);
            assert_eq!(gesture, RailGesture::default());
            assert!(!apply_rail(&bound.params.mix, &setter, gesture));
        }

        assert!(bound.calls().is_empty(), "sixty idle frames said nothing");
    }

    /// A fresh box is a fresh window's box, which is what
    /// `NiceEguiApp::build` puts back for every window the host opens.
    #[test]
    fn a_new_length_box_has_no_gesture_in_it() {
        let box_ = LengthBox::new();

        assert_eq!(box_, LengthBox::default());
        assert_eq!(box_.length, Pattern::DEFAULT_LENGTH);
        assert!(!box_.editing());
    }

    // ---- the pattern button's right-click menu ----

    /// A pattern worth copying: all four of the things a pattern holds set
    /// to something no other pattern has, so a copy that dropped any one of
    /// them shows.
    fn drawn_on(model: &StutterModel, pattern: usize) {
        assert!(model.set_rate(pattern, Rate::ThirtySecond));
        assert_eq!(model.set_length(pattern, 24), Some(24));
        assert!(model.set_volume(pattern, 2, 0.25));
        assert_eq!(model.add_block(pattern, 0, 3, Action::default()), Some(0));
        assert_eq!(
            model.add_block(pattern, 8, 4, Action::from_preset(SpeedPreset::TapeStop)),
            Some(1)
        );
    }

    /// The copy submenu's pick takes the whole of one pattern over another —
    /// rate, length, blocks and levels — and leaves every other pattern
    /// alone. One undo step puts the pattern it replaced back.
    #[test]
    fn copying_a_pattern_takes_the_whole_of_it_and_one_step_puts_it_back() {
        let model = committed_model();
        drawn_on(&model, 2);
        // The target is not a fresh pattern either, so a copy that published
        // nothing could not pass by accident.
        assert_eq!(model.set_length(9, 4), Some(4));
        assert!(model.set_volume(9, 0, 0.5));
        model.commit_edits();
        let replaced = published(&model, 9);

        assert!(copy_to(&model, 2, 9));

        let copy = published(&model, 9);
        assert_eq!(copy, published(&model, 2), "the whole pattern came over");
        assert_eq!(copy.rate(), Rate::ThirtySecond);
        assert_eq!(copy.length(), 24);
        assert_eq!(copy.blocks().len(), 2);
        assert_eq!(copy.volumes()[2], 0.25);
        for other in [0, 1, 3, 8, 10, PATTERN_COUNT - 1] {
            assert_eq!(published(&model, other), Pattern::new(), "pattern {other} moved");
        }

        // One pick is one undo step, committed on the spot, as a rate picked
        // in the dropdown beside it is ([`choose_rate`]).
        assert!(model.can_undo(), "the copy is on the stack already");
        assert!(model.undo());
        assert_eq!(published(&model, 9), replaced, "the pattern it replaced is back");
        assert_eq!(published(&model, 2).blocks().len(), 2, "and the source never moved");

        assert!(model.redo());
        assert_eq!(published(&model, 9), copy);
    }

    /// The one target the submenu greys out writes nothing, and neither does
    /// an index that is not a pattern at all.
    #[test]
    fn copying_onto_the_button_the_menu_opened_over_does_nothing() {
        let model = committed_model();
        drawn_on(&model, 5);
        model.commit_edits();
        let before = model.data();

        assert!(!copy_to(&model, 5, 5), "a pattern cannot be copied over itself");
        for (from, to) in [(5, PATTERN_COUNT), (PATTERN_COUNT, 5), (usize::MAX, 0)] {
            assert!(!copy_to(&model, from, to), "{from} → {to}");
        }

        assert_eq!(model.data(), before, "nothing was published");

        // And nothing reached the history either: one step back is the state
        // before the pattern was drawn, not before a refused copy.
        assert!(model.undo());
        assert_eq!(published(&model, 5), Pattern::new());
        assert!(!model.can_undo(), "the refused copies left no step behind");
    }

    /// Clear empties the pattern it opened over — the blocks off the row and
    /// every level back to unity — and leaves its rate and its length where
    /// they were. One undo step puts what was drawn back.
    #[test]
    fn clearing_a_pattern_empties_it_and_one_step_puts_it_back() {
        let model = committed_model();
        drawn_on(&model, 3);
        model.commit_edits();
        let drawn = published(&model, 3);

        assert!(clear(&model, 3));

        let cleared = published(&model, 3);
        assert!(cleared.blocks().is_empty(), "the row is empty");
        assert!(
            cleared.volumes().iter().all(|&level| level == Pattern::DEFAULT_VOLUME),
            "every level is back to unity"
        );
        assert_eq!(cleared.rate(), Rate::ThirtySecond, "the rate is the grid, not the drawing");
        assert_eq!(cleared.length(), 24, "and so is the length");

        assert!(model.can_undo(), "the clear is on the stack already");
        assert!(model.undo());
        assert_eq!(published(&model, 3), drawn);

        assert!(model.redo());
        assert_eq!(published(&model, 3), cleared);
    }

    /// Clearing a pattern that is already empty is no step at all, so an
    /// Undo after it steps back past it rather than appearing to do nothing.
    /// No guard in the handler does that — the history compares the patterns
    /// (`crate::undo::EditHistory::commit`).
    ///
    /// An index that is not a pattern publishes nothing and does not panic,
    /// like every other boundary in this module.
    #[test]
    fn clearing_a_pattern_that_is_already_empty_is_no_step_at_all() {
        let model = committed_model();

        for _ in 0..3 {
            assert!(clear(&model, 7), "an empty pattern is still a pattern");
        }
        assert!(!model.can_undo(), "clearing nothing is not an edit");

        assert!(!clear(&model, PATTERN_COUNT));
        assert!(!clear(&model, usize::MAX));
        model.commit_edits();
        assert!(!model.can_undo(), "an index that is not a pattern left no step behind");
        for index in 0..PATTERN_COUNT {
            assert_eq!(published(&model, index), Pattern::new(), "pattern {index} moved");
        }

        // And one real clear after all of that is still exactly one step.
        drawn_on(&model, 1);
        model.commit_edits();
        assert!(clear(&model, 1));

        assert!(model.undo());
        assert_eq!(published(&model, 1).blocks().len(), 2, "one step back brings both blocks");
    }

    /// A starter replaces what is drawn and keeps the rate and the length,
    /// leaves the other fifteen alone, and is one undo step.
    #[test]
    fn filling_a_pattern_replaces_it_and_one_step_puts_it_back() {
        let model = committed_model();
        drawn_on(&model, 3);
        model.commit_edits();
        let drawn = published(&model, 3);

        for starter in Starter::ALL {
            assert!(fill(&model, 3, starter), "{starter:?}");
            let filled = published(&model, 3);
            assert_eq!(filled, starter.fill(&drawn), "{starter:?}");
            assert_eq!((filled.rate(), filled.length()), (drawn.rate(), drawn.length()));
        }
        for index in (0..PATTERN_COUNT).filter(|&index| index != 3) {
            assert_eq!(published(&model, index), Pattern::new(), "pattern {index} moved");
        }

        for _ in Starter::ALL {
            assert!(model.undo());
        }
        assert_eq!(published(&model, 3), drawn, "one step back per fill");

        assert!(!fill(&model, PATTERN_COUNT, Starter::Gate));
        assert!(!fill(&model, usize::MAX, Starter::Gate));
    }

    /// The copy submenu offers each of the sixteen patterns exactly once, in
    /// the strip's own reading order, and has no button anywhere else.
    #[test]
    fn the_copy_submenu_offers_each_of_the_sixteen_exactly_once() {
        let offered: Vec<_> = (0..COPY_ROWS)
            .flat_map(|row| (0..COPY_COLUMNS).filter_map(move |column| copy_target(row, column)))
            .collect();

        assert_eq!(offered, (0..PATTERN_COUNT).collect::<Vec<_>>());
        assert_eq!(COPY_ROWS * COPY_COLUMNS, PATTERN_COUNT, "four rows of four, with none over");

        // Off the block of them in either direction, and nowhere near an
        // overflow: the arithmetic is a multiply and an add.
        assert_eq!(copy_target(0, COPY_COLUMNS), None);
        assert_eq!(copy_target(COPY_ROWS, 0), None);
        assert_eq!(copy_target(usize::MAX, 0), None);
        assert_eq!(copy_target(usize::MAX, COPY_COLUMNS - 1), None);

        // And the whole block fits a window this size, which is why it is a
        // block and not a column of sixteen items.
        let tall = COPY_ROWS as f32 * (COPY_BUTTON_SIZE + theme::ITEM_SPACING);
        assert!(tall < DEFAULT_HEIGHT, "{tall} points of submenu in {DEFAULT_HEIGHT}");
        assert!(
            PATTERN_COUNT as f32 * (COPY_BUTTON_SIZE + theme::ITEM_SPACING) > DEFAULT_HEIGHT,
            "a column of sixteen would not have fitted, which is the point"
        );
    }

    /// Every word in the menu names what it acts on: the title names the
    /// pattern the menu opened over, and a target's hover text names both
    /// patterns in the order the copy goes.
    #[test]
    fn the_menu_says_which_patterns_it_is_about() {
        for (index, digits) in PATTERN_LABELS.iter().enumerate() {
            let title = pattern_menu_title(index);
            assert_eq!(title, format!("Pattern {digits}"));
            assert!(title.ends_with(digits), "{title}");
        }

        let hover = copy_item_hover(2, 9);
        assert!(hover.contains("pattern 10"), "{hover}");
        assert!(hover.contains("pattern 3"), "{hover}");
        assert!(
            hover.find("pattern 10") < hover.find("pattern 3"),
            "the target is named first — it is the one being replaced: {hover}"
        );

        // The two items say what they do to a whole pattern, and Clear says
        // what it leaves behind, which is the one thing a user could be
        // surprised by.
        assert!(COPY_HOVER.contains("undo") && CLEAR_HOVER.contains("undo"));
        assert!(CLEAR_HOVER.contains("rate") && CLEAR_HOVER.contains("length"));
        assert!(PATTERN_HOVER.contains("Right-click"), "the button says the menu is there");
        assert_eq!(pattern_hover(0, None, true), PATTERN_HOVER, "notes off names no key");
        let c1 = Some(36);
        assert!(pattern_hover(0, c1, false).starts_with(PATTERN_HOVER_NOTES));
        assert!(pattern_hover(0, c1, false).contains("note on C1"));
        assert!(pattern_hover(15, c1, false).contains("note on D#2"));
        assert!(pattern_hover(0, Some(96), false).contains("as long as the note is held"));
        assert!(!pattern_hover(0, Some(96), false).contains("E7"), "held has no stop key");
        assert!(pattern_hover(0, Some(96), true).contains("note on C6 starts it"));
        assert!(pattern_hover(0, Some(96), true).contains("E7 stops it"), "latched names stop");
    }

    /// The keep-on-top toggle: a click is the switch the other way about, it
    /// starts **on** for a new instance, and it leaves the pref beside it
    /// alone — which is what [`PrefsCell::set_keep_on_top`] is for.
    #[test]
    fn the_keep_on_top_toggle_flips_the_switch_and_nothing_else() {
        let prefs = PrefsCell::default();
        assert!(prefs.get().keep_on_top, "a new instance opens pinned");

        assert!(!toggle_keep_on_top(&prefs), "a click turns it off");
        assert!(!prefs.get().keep_on_top);
        assert!(toggle_keep_on_top(&prefs), "and the next click turns it back on");
        assert!(prefs.get().keep_on_top);

        // The remembered action is the switch's neighbour and not its
        // passenger: flipping one leaves the other exactly as the project
        // left it (`editor/grid.rs`, the same rule the other way round).
        let remembered = Action::from_preset(SpeedPreset::Reverse);
        let both = PrefsCell::new(Prefs { last_action: remembered, ..Prefs::default() });
        assert!(!toggle_keep_on_top(&both));
        let expected = Prefs { keep_on_top: false, last_action: remembered, ..Prefs::default() };
        assert_eq!(both.get(), expected);
    }

    /// The hover text carries the caveat: what the switch does, that it is
    /// saved with the project, and what to do if the window manager does not
    /// put the window back in front.
    #[test]
    fn the_keep_on_top_toggle_says_what_it_does_and_what_it_cannot_promise() {
        assert!(KEEP_HOVER.contains("above Bitwig"), "{KEEP_HOVER}");
        assert!(KEEP_HOVER.contains("Alt+Tab"), "off is a window like any other");
        assert!(KEEP_HOVER.contains("Saved with the project"), "{KEEP_HOVER}");
        assert!(
            KEEP_HOVER.contains("close the window and open it once more"),
            "the reopen caveat: {KEEP_HOVER}"
        );
        assert!(!KEEP_LABEL.is_empty() && KEEP_LABEL.len() < 10);
    }
}
