//! The only place the look of the editor lives.
//!
//! Every colour and every size the window is drawn with is named here, so a
//! later restyling is an edit to this file rather than a hunt through the
//! drawing code. Nothing here reads or writes the model: the helpers take the
//! value they colour (a palette entry's `0xRRGGBB`, an amount to brighten by)
//! and hand back an `egui` colour or stroke.
//!
//! This is chopdrop's `editor/theme.rs` — itself bitstep's, trimmed — ported
//! whole, keeping every name and value the three plugins share so that they
//! read as a family when they sit side by side in one project. What a lane list
//! alone needed is gone (the row grips, the field widths, the drop highlight,
//! the activity light); what a step grid needs is back from bitstep at its own
//! values — [`CELL_WIDTH`] / [`CELL_HEIGHT`], [`CELL_GAP`] and [`CELL_PITCH`] —
//! so that a sixteen-step pattern here is exactly as wide as a sixteen-step
//! bitstep lane. The four things stuttter draws that neither sibling does are
//! in the same style, next to the constants they are built from: a block's
//! colour per action, the waveform view's two signals, the Volume row's bars
//! and the playhead.
//!
//! The palette is [`crate::state::LANE_PALETTE`] converted to [`Color32`],
//! not a second list of its own: a block's colour is read off the action a
//! *saved* pattern stores, so a palette that drifted from the one in
//! `state.rs` would repaint every reloaded project.

use egui::{Color32, CornerRadius, Shadow, Stroke, Vec2, Visuals};

use crate::state::LANE_PALETTE;

// ---- greys (Bitwig-like dark greys) ----

/// Behind everything; the darkest tone, so panels read as raised.
pub const WINDOW: Color32 = Color32::from_gray(24);

/// Top bar, Time row, Volume row — the chrome the grid is drawn on.
pub const PANEL: Color32 = Color32::from_gray(33);

/// Controls sitting on [`PANEL`] (buttons, drag values, dropdowns).
pub const CONTROL: Color32 = Color32::from_gray(45);

/// A control under the pointer.
pub const CONTROL_HOVER: Color32 = Color32::from_gray(56);

/// A control being clicked or held.
pub const CONTROL_ACTIVE: Color32 = Color32::from_gray(66);

/// Separators and outlines: the top bar's lower edge, the lines between the
/// window's bands, and the line egui draws around every widget.
pub const GRID_LINE: Color32 = Color32::from_gray(54);

/// An empty step's cell, on an unshaded column.
pub const CELL: Color32 = Color32::from_gray(38);

/// An empty step's cell on a shaded column — which columns those are is the
/// grid's business, not the theme's.
pub const CELL_SHADED: Color32 = Color32::from_gray(47);

/// Normal label text.
pub const TEXT: Color32 = Color32::from_gray(205);

/// Secondary text: step numbers, units, inactive pattern buttons.
pub const TEXT_DIM: Color32 = Color32::from_gray(138);

/// The one non-grey of the chrome: the active pattern button, the follow and
/// keep-on-top toggles, a focused control, the playhead's line. A block's
/// colour is the palette's job, not this one's.
pub const ACCENT: Color32 = Color32::from_rgb(0x55, 0x9D, 0xD6);

/// Text and items that have to say something is wrong or about to be — a
/// `clear` or a `delete` waiting for its second click.
///
/// A red of the same family as the palette's, so it sits in the window
/// without matching any one action: the point is that the item has changed
/// meaning, not that it belongs to anything.
pub const WARN: Color32 = Color32::from_rgb(0xE8, 0x6A, 0x6A);

/// What is thrown under a menu or a dropdown.
///
/// Tighter and more squarely under its surface than egui's stock shadow,
/// which is offset ten points to the right and twenty down: a block's menu
/// opens over a grid whose columns are [`CELL_PITCH`] apart, and a shadow
/// cast that far sideways darkens the very cells the pointer is aiming at
/// next. Straight down and a little stronger instead, so the surface still
/// lifts off the grid.
pub const SHADOW: Shadow =
    Shadow { offset: [0, 4], blur: 12, spread: 0, color: Color32::from_black_alpha(140) };

// ---- the playhead ----

/// The playhead's line through the waveform view, where there are no cells
/// to wash: the chrome's one non-grey, because what it marks is "now" rather
/// than anything the pattern holds.
pub const PLAYHEAD_LINE: Color32 = ACCENT;

/// How wide that line is drawn — bitstep's song-grid playhead, at its value.
/// Twice a [`HAIRLINE`], because it moves every frame and has to be followed
/// across the view rather than merely found in it.
pub const PLAYHEAD_LINE_WIDTH: f32 = 2.0;

// ---- the waveform view ----

/// Behind the waveform: the window's own darkest grey, so the view reads as
/// sunken into the panel the rows are drawn on rather than as another row.
pub const WAVE_BACKGROUND: Color32 = WINDOW;

/// The input signal — what the plugin heard, drawn dim, because it is there
/// to be compared against the output rather than read on its own.
pub const WAVE_INPUT: Color32 = Color32::from_gray(84);

/// The output signal where no block is playing — what the plugin passed
/// through, drawn bright: the one thing in the view the eye should land on
/// first.
///
/// Where a block is playing the output is drawn in that block's colour
/// instead (`crate::editor::wave::output_colour`), so what the effect did
/// never reads as the same grey as the input behind it.
pub const WAVE_OUTPUT: Color32 = Color32::from_gray(190);

/// How much of its colour a waveform column drawn before the view last
/// wrapped keeps ([`dim`]) — the scope's generation-based dimming, so the
/// span ahead of the playhead reads plainly as the last time round.
///
/// bitstep's `MUTED_FACTOR`, at its value: the same amount of "still there,
/// not now" in both windows.
pub const WAVE_STALE_FACTOR: f32 = 0.45;

/// The least one waveform column is drawn: a [`HAIRLINE`], so a column of
/// silence is a line on the centre of the view rather than nothing at all.
///
/// What makes an idle view read as a waveform display with no signal in it
/// instead of as an empty surface — and what keeps a column holding one
/// frame, which has no height of its own, visible while the frames after it
/// widen it (`crate::editor::wave::WaveView::trace_rect`).
pub const WAVE_TRACE_MIN: f32 = HAIRLINE;

// ---- the Volume row ----

/// A Volume-row bar's filled part.
///
/// The chrome's accent and not a block's colour: a step's volume belongs to
/// the pattern rather than to whichever block happens to sit over it, and a
/// bar in the action's colour would say otherwise.
pub const VOLUME_FILL: Color32 = ACCENT;

/// The rest of the bar — the same cell grey the Time row's empty steps are,
/// so the two rows read as one grid with the bars drawn into it.
pub const VOLUME_TRACK: Color32 = CELL;

/// A Cutoff-row bar's filled part: its own colour, so the row says which of
/// the three it is at a glance.
pub const CUTOFF_FILL: Color32 = LANE_SWATCHES[1];

/// A Res-row bar's filled part.
pub const RESONANCE_FILL: Color32 = LANE_SWATCHES[3];

// ---- block colours ----

/// [`LANE_PALETTE`] as `egui` colours, in the same order.
///
/// The conversion and not a second list, which is the coupling to keep: the
/// block colours below are entries of *this*, so a palette edited in
/// `state.rs` repaints the window and nothing else has to be kept in step by
/// hand.
pub const LANE_SWATCHES: [Color32; LANE_PALETTE.len()] = {
    let mut swatches = [Color32::BLACK; LANE_PALETTE.len()];
    let mut index = 0;
    while index < LANE_PALETTE.len() {
        swatches[index] = rgb(LANE_PALETTE[index]);
        index += 1;
    }
    swatches
};

/// Repeat — the classic stutter and the action a new block is given, so it
/// wears the palette's blue: the hue nearest the chrome's own accent, and
/// therefore the one that reads as "nothing unusual here".
pub const BLOCK_REPEAT: Color32 = LANE_SWATCHES[6];

/// Reverse — violet, a step round the wheel from Repeat's blue, because it is
/// the other action that plays the recording at its own speed.
pub const BLOCK_REVERSE: Color32 = LANE_SWATCHES[8];

/// Half speed — cyan: the two fixed speeds are neighbours in the palette, so
/// a pattern alternating them reads as one pair of opposites.
pub const BLOCK_HALF_SPEED: Color32 = LANE_SWATCHES[5];

/// Double speed — green, Half speed's neighbour.
pub const BLOCK_DOUBLE_SPEED: Color32 = LANE_SWATCHES[4];

/// Tape stop — red, the palette's warmest: the gesture that takes the music
/// away, and the one most often placed on the last block of a pattern.
pub const BLOCK_TAPE_STOP: Color32 = LANE_SWATCHES[0];

/// Tape start — yellow, warm like its opposite and plainly not it.
pub const BLOCK_TAPE_START: Color32 = LANE_SWATCHES[2];

/// A hand-edited speed ramp, the one block whose label is a pair of numbers
/// ("0.5→1.5") rather than a name — pink, which no preset wears, so a ramp
/// is never mistaken for one of them.
pub const BLOCK_SPEED_RAMP: Color32 = LANE_SWATCHES[9];

/// Every block colour, one per action in their fixed order: Repeat, Reverse,
/// Half speed, Double speed, Tape stop, Tape start, free Speed ramp.
///
/// The assignment above is chosen once and kept, so this list is what the
/// grid maps an `Action` through and what the tests hold to being seven
/// distinct palette entries.
pub const BLOCK_COLOURS: [Color32; 7] = [
    BLOCK_REPEAT,
    BLOCK_REVERSE,
    BLOCK_HALF_SPEED,
    BLOCK_DOUBLE_SPEED,
    BLOCK_TAPE_STOP,
    BLOCK_TAPE_START,
    BLOCK_SPEED_RAMP,
];

/// How much of its action colour a block's body keeps ([`dim`]).
///
/// A wash and not the full colour, for two reasons. The label on the block is
/// drawn *in* that colour, so what is under it has to be darker than the
/// colour itself; and the Volume row's bars sit directly below the Time
/// row's cells, so a row of solid palette blocks would shout over the levels
/// it is supposed to be read together with.
///
/// bitstep's `PITCH_MODE_ON_FACTOR`, at its value — the same amount of
/// "coloured, but not the thing to read" in both windows.
pub const BLOCK_BODY_FACTOR: f32 = 0.35;

/// The outline a block is drawn with, in its full action colour: what makes
/// two blocks of the same action in neighbouring steps read as two blocks,
/// since their washed bodies are the same tone.
pub const BLOCK_OUTLINE_WIDTH: f32 = HAIRLINE;

// ---- sizes and spacing, in logical points ----

/// One step cell: square, and big enough to hit comfortably with a mouse —
/// a block's short label and a volume bar both fit in it.
///
/// bitstep's, at its value, and the reason the two plugins' grids line up:
/// a sixteen-step pattern is exactly as wide here as a sixteen-step lane
/// there.
pub const CELL_WIDTH: f32 = 30.0;
pub const CELL_HEIGHT: f32 = 30.0;

/// Gap between neighbouring cells in a row.
pub const CELL_GAP: f32 = 2.0;

/// Distance from one step column to the next — what the grid maths steps by.
pub const CELL_PITCH: f32 = CELL_WIDTH + CELL_GAP;

/// Gap between the window's rows, and the row-to-row distance that follows
/// from it.
pub const ROW_GAP: f32 = 4.0;
pub const ROW_PITCH: f32 = CELL_HEIGHT + ROW_GAP;

/// The narrowest a cell may be drawn at (`crate::editor::grid`).
///
/// The Time row fits whatever the pattern's length is into the window's fixed
/// width, so the cell is not [`CELL_WIDTH`] once the pattern is longer than
/// [`crate::editor::STEPS_SHOWN`]: at the longest pattern there is
/// ([`crate::pattern::MAX_STEPS`]) it is a quarter of it. This is the floor
/// that keeps a cell visible when the arithmetic would otherwise take it to
/// nothing — a host that ignored the fixed-size hint and squeezed the window.
pub const MIN_CELL_WIDTH: f32 = 1.0;

/// How wide the grab for a block's edge is, inside the block: the strip at
/// either end of it that resizes it rather than moving it.
///
/// bitstep's `ARRANGE_EDGE_WIDTH`, at its value — the same gesture over the
/// same kind of block, so the two windows feel the same under the hand.
pub const BLOCK_EDGE_GRAB: f32 = 7.0;

/// The most of a block *each* of its two edge handles may take.
///
/// A third, so there is always a middle third left to grab and drag the
/// block by — bitstep's blocks have one handle and may give it half
/// ([`BLOCK_EDGE_GRAB`]), but a block here is grabbed at both ends and the
/// Time row's cells can be six points wide.
pub const BLOCK_EDGE_SHARE: f32 = 1.0 / 3.0;

/// The size a block's short label is drawn at: the pattern buttons' size,
/// which is the smallest text the window uses — a label has to fit inside a
/// block that may be one narrow step wide.
pub const BLOCK_LABEL_SIZE: f32 = PATTERN_LABEL_SIZE;

/// How much clear space a block's label needs either side of it before it is
/// drawn at all: a label touching the block's outline reads as a smudge, so
/// one that does not fit with this to spare is left out and the block's
/// colour speaks for it.
pub const BLOCK_LABEL_PADDING: f32 = 2.0;

/// Every control of the chrome — a button in the top bar, a drag value, an
/// item in a menu — is drawn exactly as tall as a step cell, so the window's
/// bands read as one grid instead of a toolbar stacked on two rows stacked on
/// a waveform.
pub const CONTROL_HEIGHT: f32 = CELL_HEIGHT;

/// Inside a control, between its frame and its text.
///
/// Half of egui's stock horizontal padding, because the top bar spends its
/// width on sixteen pattern buttons and everything else: at 8 points a side
/// the rate dropdown truncated every triplet label.
pub const CONTROL_PADDING: Vec2 = Vec2::new(4.0, 2.0);

/// The narrowest egui may lay a control out at, and the one size here that is
/// egui's own rather than this grid's: only the controls that size themselves
/// see it (a menu item, a dropdown), since everything in the top bar and the
/// two rows is placed into a rect of its own.
pub const CONTROL_MIN_WIDTH: f32 = 40.0;

/// Padding inside the top bar and around the rows.
pub const PANEL_PADDING: f32 = 6.0;

/// How many rows of controls the top bar holds.
///
/// Two, because one does not fit. The bar's row is the grid's width less the
/// panel's padding — 510 points — and the sixteen pattern buttons with the
/// follow toggle take 410 of it
/// ([`PATTERN_WIDTH`] and [`CELL_GAP`] sixteen times over, plus
/// [`FOLLOW_WIDTH`]), which is a hundred for the rate, the length, Mix,
/// Smoothing, undo/redo and keep-on-top together. Those six want something
/// like three hundred at the widths the sibling plugins draw them at, and
/// the strip cannot give any of it back: there are sixteen patterns and
/// there always will be ([`crate::pattern::PATTERN_COUNT`]), and buttons
/// shrunk to nine points would read as nothing.
///
/// So the bar is two rows of [`CONTROL_HEIGHT`]: the patterns and the follow
/// toggle on the first, and everything else on the second, which leaves both
/// of them room to spare. The alternatives were compact icon buttons and
/// folding the rate and length into a popover, and neither buys anything
/// like the 200 points that were missing. The window is one
/// [`ROW_PITCH`] taller for it ([`crate::editor::DEFAULT_HEIGHT`]).
pub const BAR_ROWS: f32 = 2.0;

/// The strip across the top: [`BAR_ROWS`] rows of [`CONTROL_HEIGHT`] a
/// [`ROW_GAP`] apart, with [`PANEL_PADDING`] above and below them — so each
/// row of the bar is as tall as a row of the grid, and the gap between them
/// is the grid's own.
pub const TOP_BAR_HEIGHT: f32 =
    BAR_ROWS * CONTROL_HEIGHT + (BAR_ROWS - 1.0) * ROW_GAP + 2.0 * PANEL_PADDING;

/// Space between controls inside a panel.
pub const ITEM_SPACING: f32 = 4.0;

/// One of the top bar's sixteen pattern buttons.
///
/// Narrow for a control with text in it, because the sixteen of them share
/// one bar with the rate, the length, Mix, Smoothing, undo/redo and
/// keep-on-top: two digits at [`PATTERN_LABEL_SIZE`] inside
/// [`CONTROL_PADDING`], and nothing more. The strip they make is spaced by
/// [`CELL_GAP`] rather than [`ITEM_SPACING`], so it reads as one strip of
/// sixteen the way a row of steps does and not as sixteen separate controls.
pub const PATTERN_WIDTH: f32 = 20.0;

/// The digits on a pattern button: smaller than a control's own text, which
/// is what lets sixteen two-digit buttons fit in [`PATTERN_WIDTH`] each.
pub const PATTERN_LABEL_SIZE: f32 = 11.0;

/// The lit mark across the bottom of the *playing* pattern's button.
///
/// A mark and not a fill, because the two states of a button are different
/// things and both can be true at once: the pattern being *edited* takes
/// egui's selection fill ([`visuals`]) and the one the host has reached is
/// marked under it, so a button can plainly be both.
pub const PATTERN_MARK_HEIGHT: f32 = 3.0;

/// How far the lit mark is held off a button's rounded ends, so it reads as
/// a bar under the digits rather than as a square corner poking out of the
/// [`CONTROL_CORNER`] it sits in.
///
/// That corner's own radius, and not a figure of its own: it is exactly how
/// much of the button's bottom edge is not straight, so a restyling of the
/// controls' corners takes the mark with it.
pub const PATTERN_MARK_INSET: f32 = CONTROL_CORNER.sw as f32;

/// The lit mark's colour: the chrome's one accent, as the playhead's line is
/// — both of them say "the host is here".
pub const PATTERN_MARK: Color32 = ACCENT;

/// The follow toggle at the end of the pattern strip: wide enough for its
/// own label, which is the shortest word that says what it does.
pub const FOLLOW_WIDTH: f32 = 56.0;

/// The rate dropdown for the pattern being edited: 1/8, 1/16, 1/32, 1/8T,
/// 1/16T.
///
/// Wide enough for "1/16T" — the longest of the five — beside the dropdown's
/// own triangle at [`CONTROL_PADDING`], and not a point wider: it opens the
/// bar's second row ([`BAR_ROWS`]), which has Mix, Smoothing, undo/redo and
/// keep-on-top to fit after it.
pub const RATE_WIDTH: f32 = 56.0;

/// The length box beside it: the edited pattern's step count, 1 to
/// [`crate::pattern::MAX_STEPS`].
///
/// Two digits inside [`CONTROL_PADDING`] and nothing else — no unit and no
/// label, because there is no room in the bar for either and the box's hover
/// text says what it is. Narrower than [`CONTROL_MIN_WIDTH`] on purpose: the
/// box is placed into this rect rather than left to size itself, so egui's
/// own floor does not apply to it.
pub const LENGTH_WIDTH: f32 = 36.0;

/// The Mix and Smoothing rails that follow them, each the same width as the
/// other: the two are one pair of controls and a pair drawn at two widths reads
/// as two unrelated things.
///
/// Enough for the control's name and its value side by side inside
/// [`CONTROL_PADDING`] — "Smooth" with "20.0 ms", which is the longest pair
/// either of them can show — and that is the whole of the figure: a rail
/// carries its own label, because the bar has no room for a label beside it
/// (the strip of sixteen took the first row, [`BAR_ROWS`]).
pub const PARAM_RAIL_WIDTH: f32 = 104.0;

/// One of the two history buttons, Undo and Redo, which share a width for
/// the reason the rails do.
///
/// "Undo" inside [`CONTROL_PADDING`] and nothing more. Words rather than the
/// arrows a host draws: the bundled font is Ubuntu-Light and an arrow glyph
/// it does not carry would come out as a blank button.
pub const HISTORY_WIDTH: f32 = 46.0;

/// The keep-on-top toggle at the end of the second row.
///
/// A toggle like the follow one above it and drawn the same way, so the two
/// lit controls in the bar read as one kind of thing — but its own width,
/// because its label is its own and this row's leftover is what it has to
/// fit in ([`BAR_ROWS`] counted it in: the six controls before it take 416
/// of the row's 510).
pub const KEEP_WIDTH: f32 = 64.0;
/// The Notes dropdown after it ([`super::top_bar::ui_notes`]): room for
/// "Notes C-2", the longest thing it shows.
pub const NOTES_WIDTH: f32 = 84.0;
/// The Latch toggle after the Notes dropdown
/// ([`super::top_bar::ui_latch`]), drawn like the other toggles.
pub const LATCH_WIDTH: f32 = 56.0;

/// The Time row's height: taller than a bitstep cell so a block's label,
/// its repeat ticks and its selection outline all read at a glance.
pub const TIME_ROW_HEIGHT: f32 = 56.0;

/// The Volume row's height.
pub const VOLUME_ROW_HEIGHT: f32 = 44.0;

/// The strip over the Volume row: the Volume / Cutoff / Res tabs and the
/// pattern's Filter dropdown ([`super::volume::ui_lane_bar`]).
pub const LANE_BAR_HEIGHT: f32 = PANEL_CONTROL_HEIGHT;

/// One of the strip's three tabs.
pub const LANE_TAB_WIDTH: f32 = 64.0;

/// The strip's Filter dropdown: room for "Filter: Band-pass".
pub const FILTER_WIDTH: f32 = 140.0;

/// The waveform view takes whatever height is left under the two rows; this
/// is the least it is drawn at in a window of the minimum size.
pub const WAVEFORM_MIN_HEIGHT: f32 = 3.0 * CELL_HEIGHT;

/// The width of the block panel on the window's right: the selected block's
/// presets, loop, source and speed.
pub const BLOCK_PANEL_WIDTH: f32 = 290.0;

/// The gap between the grid column and the block panel.
pub const COLUMN_GAP: f32 = 8.0;

/// The outline drawn around the selected block on the Time row.
pub const SELECTED_OUTLINE: Color32 = TEXT;

/// How thick that outline is.
pub const SELECTED_OUTLINE_WIDTH: f32 = 2.0;

/// How much of a block's colour its repeat ticks are drawn at.
pub const REPEAT_TICK_FACTOR: f32 = 0.7;

/// The closest repeat ticks are drawn together, in points; closer than this
/// they are left out rather than smearing into a block of colour.
pub const REPEAT_TICK_MIN_SPACING: f32 = 3.0;

/// How strongly the region the selected block reads from is washed over the
/// waveform, as an alpha.
pub const SOURCE_WASH_ALPHA: f32 = 0.22;

/// How tall another block's source stripe is along the waveform's foot.
pub const SOURCE_STRIPE_HEIGHT: f32 = 4.0;

/// How strongly another block's source stripe is drawn, as an alpha.
pub const SOURCE_STRIPE_ALPHA: f32 = 0.7;

/// How wide the selected source's right-edge grab zone is, in points.
pub const SOURCE_EDGE_GRIP: f32 = 8.0;

/// How wide the bar marking that edge is drawn.
pub const SOURCE_EDGE_BAR: f32 = 3.0;

/// The bundled font the proportional text falls back to for glyphs
/// Ubuntu-Light lacks, such as `→`.
pub const ARROW_FALLBACK_FONT: &str = "Hack";

/// How tall a row of buttons is inside the block panel: shorter than a bar
/// control, so the whole panel fits the window.
pub const PANEL_CONTROL_HEIGHT: f32 = 22.0;

/// The resize grip in the window's bottom-right corner, in points square.
pub const RESIZE_GRIP_SIZE: f32 = 16.0;

/// Seconds the pointer has to rest on a control before its hover text shows.
/// Long enough that moving across the window does not set them off.
pub const TOOLTIP_DELAY: f32 = 1.2;

/// The widest a hover text is laid out, in points.
pub const TOOLTIP_WIDTH: f32 = 280.0;

/// The strip a vertical scroll bar takes out of a scrolling area's width —
/// egui's own [`egui::style::ScrollStyle::thin`] figure, which [`apply`]
/// installs.
///
/// Named here so that a window's arithmetic can allow for it without
/// reaching into a style that is only built when a context exists.
pub const SCROLL_BAR_STRIP: f32 = 6.0;

/// Anything drawn by hand: a step cell, a block, a volume bar, the waveform.
pub const CELL_CORNER: CornerRadius = CornerRadius::same(3);

/// Buttons and other panel controls.
pub const CONTROL_CORNER: CornerRadius = CornerRadius::same(4);

/// Hairlines: outlines, separators.
pub const HAIRLINE: f32 = 1.0;

/// The caret in the fields the editor types into. Two points, as egui's own,
/// because a hairline caret is lost in the text it sits between.
pub const TEXT_CURSOR_WIDTH: f32 = 2.0;

/// The size a hint is drawn at — the one text in the window that is not a
/// widget's. A little above the body text, because it is all there is to
/// read.
pub const HINT_SIZE: f32 = 15.0;

// ---- colour helpers ----

/// A stored `0xRRGGBB` colour as an opaque `egui` colour.
///
/// Only the low 24 bits are read: a snapshot from a future version (or a
/// hostile one) that sets the top byte must not come out as a different
/// colour, let alone a transparent one.
pub const fn rgb(colour: u32) -> Color32 {
    let [_, r, g, b] = colour.to_be_bytes();
    Color32::from_rgb(r, g, b)
}

/// Takes a colour down without tinting it — a waveform column from the span
/// before this one.
pub fn dim(colour: Color32, factor: f32) -> Color32 {
    colour.gamma_multiply(factor.clamp(0.0, 1.0))
}

// ---- egui's own styling ----

/// The [`Visuals`] the editor's widgets inherit: egui's dark theme with this
/// module's greys and accent substituted, so stock widgets (buttons, combo
/// boxes, menus) match the hand-drawn parts without being styled one by one.
pub fn visuals() -> Visuals {
    let mut visuals = Visuals::dark();

    visuals.panel_fill = PANEL;
    visuals.window_fill = PANEL;
    visuals.extreme_bg_color = WINDOW;
    visuals.faint_bg_color = CELL_SHADED;
    visuals.window_stroke = Stroke::new(HAIRLINE, GRID_LINE);
    visuals.window_corner_radius = CONTROL_CORNER;
    visuals.menu_corner_radius = CONTROL_CORNER;
    visuals.window_shadow = SHADOW;
    visuals.popup_shadow = SHADOW;
    visuals.hyperlink_color = ACCENT;
    visuals.warn_fg_color = WARN;
    visuals.text_cursor.stroke = Stroke::new(TEXT_CURSOR_WIDTH, ACCENT);
    visuals.selection.bg_fill = dim(ACCENT, 0.55);
    // A selected control — a lit follow or keep-on-top toggle, the pattern
    // button being edited — keeps its text at full brightness over that fill.
    visuals.selection.stroke = Stroke::new(HAIRLINE, TEXT);

    for (widget, fill) in [
        (&mut visuals.widgets.noninteractive, PANEL),
        (&mut visuals.widgets.inactive, CONTROL),
        (&mut visuals.widgets.hovered, CONTROL_HOVER),
        (&mut visuals.widgets.active, CONTROL_ACTIVE),
        (&mut visuals.widgets.open, CONTROL_HOVER),
    ] {
        widget.bg_fill = fill;
        widget.weak_bg_fill = fill;
        widget.bg_stroke = Stroke::new(HAIRLINE, GRID_LINE);
        widget.corner_radius = CONTROL_CORNER;
        widget.fg_stroke = Stroke::new(HAIRLINE, TEXT);
        // Widgets growing under the pointer would pull the top bar's buttons
        // out of line with the grid's columns.
        widget.expansion = 0.0;
    }
    // Everything above is a control's own text. This is the rest: a label
    // beside a drag value, a heading inside a menu, a step number. egui takes
    // a *non-interactive* widget's foreground as the colour for all of it, so
    // this one line is the editor's whole text hierarchy — and the reason
    // `override_text_color` is left `None`, since a value there would flatten
    // the two back into one tone.
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(HAIRLINE, TEXT_DIM);

    visuals
}

/// Installs [`visuals`] and this module's spacing on a context. Called once
/// per window, from the editor's `build`.
///
/// The plugin is dark whatever the desktop is doing, so the theme is pinned
/// and *both* of egui's styles are written: were the host or the OS to flip
/// the preference mid-session, the light style it switched to would otherwise
/// be egui's stock one.
pub fn apply(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);

    // The proportional font (Ubuntu-Light) has no arrow glyph, so `→` in a
    // ramp's label drew as an empty box: fall back to the bundled monospace
    // font, which has it.
    let mut fonts = egui::FontDefinitions::default();
    if let Some(family) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
        family.push(ARROW_FALLBACK_FONT.to_owned());
    }
    ctx.set_fonts(fonts);
    ctx.all_styles_mut(|style| {
        style.visuals = visuals();

        // The spacing egui lays out *with*, as against the geometry this
        // module hands the grid: the rows and the top bar are placed rect by
        // rect, but a menu and a dropdown lay themselves out, and they have
        // to come out at the same rhythm as the rest of the window rather
        // than at egui's stock one.
        let spacing = &mut style.spacing;
        spacing.item_spacing = egui::vec2(ITEM_SPACING, ITEM_SPACING);
        spacing.window_margin = egui::Margin::same(PANEL_PADDING as i8);
        spacing.menu_margin = egui::Margin::same(PANEL_PADDING as i8);
        spacing.menu_spacing = ITEM_SPACING;
        spacing.button_padding = CONTROL_PADDING;
        spacing.icon_spacing = ITEM_SPACING;
        spacing.interact_size = egui::vec2(CONTROL_MIN_WIDTH, CONTROL_HEIGHT);
        // Thin scroll bars rather than egui's stock floating ones, which
        // hover *over* what they scroll: a bar that overlapped the grid's
        // last column would take the click meant for it. These reserve their
        // own strip instead ([`SCROLL_BAR_STRIP`]).
        spacing.scroll = egui::style::ScrollStyle::thin();
        spacing.tooltip_width = TOOLTIP_WIDTH;

        // Hover text only once the pointer has come to rest on a control,
        // and only after a while: a window you move across should not keep
        // popping text up. No grace period, so each one waits its turn.
        let interaction = &mut style.interaction;
        interaction.show_tooltips_only_when_still = true;
        interaction.tooltip_delay = TOOLTIP_DELAY;
        interaction.tooltip_grace_time = 0.0;
        debug_assert_eq!(
            spacing.scroll.allocated_width(),
            SCROLL_BAR_STRIP,
            "the width the window leaves for the bar is not the width egui takes"
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The palette is `state`'s, converted — not a second list — and every
    /// entry of it reaches the screen opaque.
    #[test]
    fn the_palette_mirrors_state() {
        assert_eq!(LANE_SWATCHES.len(), LANE_PALETTE.len());
        assert_eq!(LANE_SWATCHES.len(), 10);

        for (index, &colour) in LANE_PALETTE.iter().enumerate() {
            let expected =
                Color32::from_rgb((colour >> 16) as u8, (colour >> 8) as u8, colour as u8);
            assert_eq!(LANE_SWATCHES[index], expected, "swatch {index}");
        }

        assert!(LANE_SWATCHES.iter().all(Color32::is_opaque), "palette colours are opaque");
    }

    /// A stored colour reaches the screen as itself, and a stray top byte (a
    /// future version's flag, a corrupt snapshot) changes nothing.
    #[test]
    fn rgb_reads_only_the_low_twenty_four_bits() {
        assert_eq!(rgb(0x12_34_56), Color32::from_rgb(0x12, 0x34, 0x56));
        assert_eq!(rgb(0xFF_12_34_56), rgb(0x12_34_56));
        assert_eq!(rgb(0), Color32::BLACK);
        assert_eq!(rgb(0xFF_FF_FF), Color32::WHITE);
    }

    /// Clamping, so a caller cannot produce a colour outside the range by
    /// over- or under-shooting.
    #[test]
    fn dim_clamps_its_amount() {
        let block = BLOCK_REPEAT;

        assert_eq!(dim(block, 5.0), block);
        assert_eq!(dim(block, -1.0), block.gamma_multiply(0.0));
    }

    /// The seven block kinds wear seven different palette entries, and nothing
    /// else: a block's colour is what says which action it is before its
    /// label is read.
    #[test]
    fn every_action_has_its_own_palette_colour() {
        assert_eq!(BLOCK_COLOURS.len(), 7);

        for (index, &colour) in BLOCK_COLOURS.iter().enumerate() {
            assert!(LANE_SWATCHES.contains(&colour), "block colour {index} is not the palette's");
            assert!(colour.is_opaque(), "block colour {index}");
            for (other_index, &other) in BLOCK_COLOURS.iter().enumerate().skip(index + 1) {
                assert_ne!(colour, other, "actions {index} and {other_index} share a colour");
            }
        }

        // The assignment itself, written down so that a change to it fails
        // here rather than quietly repainting saved projects.
        assert_eq!(BLOCK_REPEAT, rgb(0x6E_96_D6), "Repeat is the palette's blue");
        assert_eq!(BLOCK_REVERSE, rgb(0xC2_7E_D6));
        assert_eq!(BLOCK_HALF_SPEED, rgb(0x6E_C2_C9));
        assert_eq!(BLOCK_DOUBLE_SPEED, rgb(0x6E_C9_8A));
        assert_eq!(BLOCK_TAPE_STOP, rgb(0xE2_6E_6E));
        assert_eq!(BLOCK_TAPE_START, rgb(0xE2_C9_5E));
        assert_eq!(BLOCK_SPEED_RAMP, rgb(0xD6_7E_A8));

        // Every one of them reads over the two cell shades it is drawn on.
        let background = CELL.r().max(CELL_SHADED.r());
        for (index, &colour) in BLOCK_COLOURS.iter().enumerate() {
            let brightest = colour.r().max(colour.g()).max(colour.b());
            assert!(brightest > background + 32, "block colour {index} is lost on a cell");
        }
    }

    /// The step cell is bitstep's, so the two grids line up: sixteen steps
    /// here are sixteen steps there, to the point.
    // The constants are the subject: these asserts are here so a change to
    // one of them fails the suite with its numbers, which a `const` block
    // (clippy's suggestion) could neither format nor report per test.
    #[allow(clippy::assertions_on_constants)]
    #[test]
    fn the_step_cell_is_bitsteps() {
        assert_eq!(CELL_WIDTH, 30.0);
        assert_eq!(CELL_HEIGHT, 30.0);
        assert_eq!(CELL_GAP, 2.0);
        assert_eq!(CELL_PITCH, CELL_WIDTH + CELL_GAP);
        assert_eq!(CELL_CORNER, CornerRadius::same(3));
        assert_eq!(CONTROL_CORNER, CornerRadius::same(4));

        // A sixteen-step row: the cells and the gaps between them, with no
        // gap hanging off the end. The figure a bitstep lane comes out at.
        assert_eq!(16.0 * CELL_PITCH - CELL_GAP, 510.0);

        // Both corners are rounded, and the hand-drawn one less than the
        // controls', so a block inside the grid does not read as a button.
        assert!(CELL_CORNER.nw > 0 && CELL_CORNER.nw < CONTROL_CORNER.nw);
    }

    /// The chrome is built out of one control height: a row of the grid is
    /// one of them, the top bar is one inside its padding, and the waveform
    /// view is three. That is what lines the window's bands up, so they are
    /// sums rather than numbers anyone has to keep in step by hand.
    // The constants are the subject: these asserts are here so a change to
    // one of them fails the suite with its numbers.
    #[allow(clippy::assertions_on_constants)]
    #[test]
    fn the_chrome_is_built_from_the_control_height() {
        assert_eq!(CONTROL_HEIGHT, CELL_HEIGHT);
        assert_eq!(ROW_PITCH, CELL_HEIGHT + ROW_GAP);
        assert_eq!(
            TOP_BAR_HEIGHT,
            BAR_ROWS * CONTROL_HEIGHT + (BAR_ROWS - 1.0) * ROW_GAP + 2.0 * PANEL_PADDING
        );
        assert!(BAR_ROWS >= 1.0, "the bar holds controls");
        assert!(
            TOP_BAR_HEIGHT > BAR_ROWS * CONTROL_HEIGHT,
            "the bar's padding has to fit around its rows"
        );
        assert!(2.0 * CONTROL_PADDING.y < CONTROL_HEIGHT, "a control is all padding");
        assert!(2.0 * CONTROL_PADDING.x < CELL_WIDTH, "a cell's label is all padding");
        assert!(ROW_GAP > 0.0, "two rows would touch");
        assert!(ROW_GAP > CELL_GAP, "the rows read as rows rather than as one block of cells");
        assert!(WAVEFORM_MIN_HEIGHT > ROW_PITCH, "the waveform is more than another row");
        assert!(HINT_SIZE > 0.0 && CONTROL_MIN_WIDTH > 0.0);
    }

    /// The top bar's pattern strip: sixteen buttons and the follow toggle,
    /// inside the width the window actually has.
    ///
    /// The strip is the one part of the bar whose width is fixed by the data
    /// — there are sixteen patterns and there always will be
    /// ([`crate::pattern::PATTERN_COUNT`]) — so it is the part that has to
    /// be checked against the window rather than against the controls beside
    /// it, which get abbreviated instead. It is also the figure that decided
    /// [`BAR_ROWS`]: it leaves one row far too little for the six controls
    /// that follow it, and this is where that arithmetic is checked.
    // The constants are the subject: these asserts are here so a change to
    // one of them fails the suite with its numbers.
    #[allow(clippy::assertions_on_constants)]
    #[test]
    fn the_top_bars_pattern_strip_fits_sixteen_buttons() {
        let count = crate::pattern::PATTERN_COUNT as f32;
        let strip = count * PATTERN_WIDTH + (count - 1.0) * CELL_GAP;
        // The bar's row: the grid's width, less the panel's padding either
        // side of it (`crate::editor::DEFAULT_WIDTH`).
        let row = 16.0 * CELL_PITCH - CELL_GAP;

        assert_eq!(strip, 350.0, "sixteen buttons and fifteen cell gaps");
        assert_eq!(row, 510.0);
        assert!(
            strip + ITEM_SPACING + FOLLOW_WIDTH < row,
            "the strip and the follow toggle have to leave the bar some room"
        );

        // Why there are two rows: what the strip and the toggle leave over
        // is a hundred points, and the rate and the length alone are most of
        // it — so Mix, Smoothing, undo/redo and keep-on-top could not have
        // shared the first row with them (`BAR_ROWS`).
        let spare = row - (strip + ITEM_SPACING + FOLLOW_WIDTH);
        assert_eq!(spare, 100.0, "the first row's room beside the toggle");
        assert!(
            RATE_WIDTH + ITEM_SPACING + LENGTH_WIDTH < spare,
            "the rate and the length would have taken the whole of it"
        );
        assert_eq!(BAR_ROWS, 2.0, "so they go on a second row");

        // And the second row holds them with room left for the four that
        // follow.
        assert!(
            RATE_WIDTH + ITEM_SPACING + LENGTH_WIDTH + ITEM_SPACING < row * 0.5,
            "the rate and the length take less than half the second row"
        );

        // The whole of that row: the rate, the length, the two rails and the
        // two history buttons, a gap between each pair — and what is left
        // over is keep-on-top's.
        let second = RATE_WIDTH
            + LENGTH_WIDTH
            + 2.0 * PARAM_RAIL_WIDTH
            + 2.0 * HISTORY_WIDTH
            + 6.0 * ITEM_SPACING;
        assert_eq!(second, 416.0, "the six controls of the second row and their gaps");
        assert!(
            row - second > CONTROL_MIN_WIDTH,
            "keep-on-top has to fit in what the second row has left"
        );

        // And keep-on-top itself fits that leftover with a gap before it,
        // which is the whole of the row accounted for.
        assert!(
            second + ITEM_SPACING + KEEP_WIDTH < row,
            "the seventh control does not fit the second row"
        );
        assert!(2.0 * CONTROL_PADDING.x < KEEP_WIDTH, "the toggle is all padding");
        assert!(KEEP_WIDTH > HISTORY_WIDTH, "'On top' is wider than 'Undo'");
        assert!(2.0 * CONTROL_PADDING.x < HISTORY_WIDTH, "a history button is all padding");
        assert!(LENGTH_WIDTH < HISTORY_WIDTH, "'Undo' is wider than two digits");
        assert!(HISTORY_WIDTH < PARAM_RAIL_WIDTH, "a rail carries a name and a value");
        assert!(2.0 * CONTROL_PADDING.x < LENGTH_WIDTH, "the length box is all padding");
        assert!(LENGTH_WIDTH < RATE_WIDTH, "two digits need less room than '1/16T'");

        // A button is narrower than a step cell but still a control's
        // height, and its digits fit inside its padding.
        assert!(PATTERN_WIDTH < CELL_WIDTH, "sixteen cells would be the whole window");
        assert!(2.0 * CONTROL_PADDING.x < PATTERN_WIDTH, "a button is all padding");
        assert!(PATTERN_LABEL_SIZE < HINT_SIZE, "two digits have to fit the narrow button");

        // The lit mark sits inside the button it marks, under the digits.
        assert_eq!(PATTERN_MARK, ACCENT);
        assert!(PATTERN_MARK_HEIGHT > 0.0 && PATTERN_MARK_HEIGHT < CONTROL_HEIGHT * 0.5);
        assert!(2.0 * PATTERN_MARK_INSET < PATTERN_WIDTH, "the mark would be inset to nothing");
        assert!(
            PATTERN_MARK_INSET >= f32::from(CONTROL_CORNER.sw),
            "the mark has to clear the button's rounded corners"
        );
    }

    /// The bands of the window add up out of these same figures, in the order
    /// they are drawn: the bar and its hairline, then the two rows and the
    /// waveform inside the panel's padding. [`crate::editor::layout`] cuts a
    /// window back into them and [`crate::editor::DEFAULT_HEIGHT`] sums them
    /// into the size it opens at, so the figures have to leave room for each
    /// other — and the total is written down here, where a change to any one of
    /// them fails with the number it moved.
    // The constants are the subject: these asserts are here so a change to
    // one of them fails the suite with its numbers.
    #[allow(clippy::assertions_on_constants)]
    #[test]
    fn the_bands_of_the_window_are_these_figures_stacked() {
        assert_eq!(TOP_BAR_HEIGHT, 76.0, "two control rows inside the panel's padding");
        assert_eq!(HAIRLINE, 1.0);
        assert_eq!(PANEL_PADDING, 6.0);
        assert_eq!(CELL_HEIGHT, 30.0);
        assert_eq!(ROW_PITCH, 34.0, "a row of cells and the gap under it");
        assert_eq!((TIME_ROW_HEIGHT, VOLUME_ROW_HEIGHT), (56.0, 44.0));

        // The least height the window can be: the bar, both rows, the strip
        // between them and a waveform of its minimum height.
        let bands = TOP_BAR_HEIGHT
            + HAIRLINE
            + TIME_ROW_HEIGHT
            + LANE_BAR_HEIGHT
            + VOLUME_ROW_HEIGHT
            + 3.0 * ROW_GAP
            + WAVEFORM_MIN_HEIGHT;
        assert!(bands + 2.0 * PANEL_PADDING <= crate::editor::MIN_HEIGHT);

        // Every band has room for what goes in it, and the padding never
        // eats a band: the two rows and the waveform are what is left of the
        // window once the bar, its line and the padding are taken off.
        assert!(PANEL_PADDING > 0.0 && PANEL_PADDING < CELL_HEIGHT * 0.5);
        assert!(HAIRLINE < PANEL_PADDING, "the line between two bands is not a band");
        assert!(ROW_PITCH > CELL_HEIGHT, "a row has to clear the row under it");
    }

    /// The playhead: a line through the waveform, which is the one band it
    /// is drawn in (`editor::wave`).
    // The constants are the subject: these asserts are here so a change to
    // one of them fails the suite with its numbers.
    #[allow(clippy::assertions_on_constants)]
    #[test]
    fn the_playhead_draws_a_line_on_the_waveform() {
        assert_eq!(PLAYHEAD_LINE, ACCENT);
        assert!(PLAYHEAD_LINE_WIDTH > HAIRLINE, "the playhead has to out-read a separator");
    }

    /// The waveform view's two signals are one grey twice over — the output
    /// bright, the input dim under it — and a column from the span before
    /// this one is dimmer again without being gone.
    // The constants are the subject: these asserts are here so a change to
    // one of them fails the suite with its numbers.
    #[allow(clippy::assertions_on_constants)]
    #[test]
    fn the_waveform_reads_the_output_over_the_input() {
        assert!(WAVE_OUTPUT.intensity() > WAVE_INPUT.intensity(), "the output is the bright one");
        assert!(
            WAVE_INPUT.intensity() > WAVE_BACKGROUND.intensity(),
            "the dim signal still has to be seen against the view it is drawn on"
        );
        assert!(WAVE_STALE_FACTOR > 0.0 && WAVE_STALE_FACTOR < 1.0);

        let stale = dim(WAVE_OUTPUT, WAVE_STALE_FACTOR);
        assert!(stale.intensity() < WAVE_OUTPUT.intensity(), "a generation old draws dimmer");
        assert!(stale.intensity() > 0.0, "a generation old is still drawn");

        // And silence is a line rather than nothing, on a view three rows
        // tall: the floor has to be a fraction of the height, not a share of
        // it.
        assert_eq!(WAVE_TRACE_MIN, HAIRLINE);
        assert!(WAVE_TRACE_MIN < WAVEFORM_MIN_HEIGHT, "a silent column is a line, not a block");
    }

    /// A Volume-row bar is the chrome's accent over the grid's own cell grey,
    /// so a painted level reads as a level rather than as a block.
    #[test]
    fn a_volume_bars_fill_reads_over_its_track() {
        assert_eq!(VOLUME_FILL, ACCENT);
        assert_eq!(VOLUME_TRACK, CELL);
        assert!(VOLUME_FILL.intensity() > VOLUME_TRACK.intensity());
        assert!(
            !BLOCK_COLOURS.contains(&VOLUME_FILL),
            "a bar in an action's colour would say the level belonged to a block"
        );
    }

    /// The style a window opens with is this module's, not egui's default
    /// dark theme — the one check that needs a context, and it needs no
    /// display to make one.
    #[test]
    fn the_context_takes_this_palette() {
        let ctx = egui::Context::default();
        apply(&ctx);

        let style = ctx.global_style();
        assert!(style.visuals.dark_mode);
        assert_eq!(style.visuals.panel_fill, PANEL);
        assert_eq!(style.visuals.extreme_bg_color, WINDOW);
        assert_eq!(style.visuals.faint_bg_color, CELL_SHADED);
        assert_eq!(style.visuals.widgets.inactive.bg_fill, CONTROL);
        assert_eq!(style.visuals.window_shadow, SHADOW);
        assert_eq!(style.visuals.popup_shadow, SHADOW);

        // The text hierarchy: a control says its own text at full
        // brightness, everything else is secondary, and no override is left
        // in place to collapse the two.
        assert_eq!(style.visuals.override_text_color, None);
        assert_eq!(style.visuals.widgets.inactive.fg_stroke.color, TEXT);
        assert_eq!(style.visuals.widgets.hovered.fg_stroke.color, TEXT);
        assert_eq!(style.visuals.selection.stroke.color, TEXT);
        assert_eq!(style.visuals.widgets.noninteractive.fg_stroke.color, TEXT_DIM);
        assert_eq!(style.visuals.text_color(), TEXT_DIM, "a plain label is secondary text");

        // No widget grows under the pointer: one that did would pull the top
        // bar's buttons out of line with the grid's columns.
        for widget in [
            &style.visuals.widgets.noninteractive,
            &style.visuals.widgets.inactive,
            &style.visuals.widgets.hovered,
            &style.visuals.widgets.active,
            &style.visuals.widgets.open,
        ] {
            assert_eq!(widget.expansion, 0.0);
            assert_eq!(widget.corner_radius, CONTROL_CORNER);
        }

        // The spacing a menu or a dropdown lays itself out with.
        assert_eq!(style.spacing.item_spacing, egui::vec2(ITEM_SPACING, ITEM_SPACING));
        assert_eq!(style.spacing.button_padding, CONTROL_PADDING);
        assert_eq!(style.spacing.interact_size.y, CONTROL_HEIGHT);
        assert_eq!(style.spacing.menu_margin, egui::Margin::same(PANEL_PADDING as i8));
        assert_eq!(style.spacing.scroll.allocated_width(), SCROLL_BAR_STRIP);

        // Light stays dark: a theme flip must not hand the editor egui's
        // stock palette.
        ctx.set_theme(egui::Theme::Light);
        assert_eq!(ctx.global_style().visuals.panel_fill, PANEL);
    }
}
