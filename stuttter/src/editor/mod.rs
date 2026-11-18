//! The egui editor: the window the host opens, and the look inside it.
//!
//! [`theme`] is the look — chopdrop's `editor/theme.rs` ported whole with
//! bitstep's step cells back in it, which is why the three plugins read as a
//! family when they sit side by side in one project. [`keys`] is the window
//! *around* the editor: the keys this window has no use for are passed on to
//! Bitwig's, because baseview's X11 backend would otherwise drop them and
//! Space would stop starting the transport. The rest of this file is the
//! window itself: the [`NiceEguiApp`] nice-plug-egui calls back into, the
//! fixed size it opens at, and [`create`], which the plugin's `editor()`
//! hands to the host.
//!
//! # The window
//!
//! A resizable window ([`create`]), cut into its bands by [`layout`]: the
//! two-row top bar ([`top_bar`]: the pattern buttons and Follow, then the
//! edited pattern's rate and length, Mix and Smoothing, Undo/Redo
//! ([`history`]) and the keep-on-top switch), and under it a left column
//! with the Time row ([`grid`]), the Volume row ([`volume`]) and the
//! waveform view ([`wave`]), and the block panel on the right ([`panel`],
//! over the handlers in [`block`]).
//!
//! The Time row is where blocks are made, selected, moved, stretched and
//! removed; the panel is where the selected block's preset, loop, source and
//! speed are set, with a sentence saying what it does, and the waveform
//! washes over the stretch of song the selected block reads from.
//!
//! Every edit goes through the model's writer API, which clamps and records
//! the undo step; the window holds only what a half-made gesture needs. The
//! three host parameters are set through the host (begin/set/end), so they
//! automate. Keys this window has no use for are passed on to Bitwig's
//! ([`keys`]).

pub mod block;
pub mod grid;
pub mod history;
pub mod keys;
pub mod layout;
pub mod panel;
pub mod theme;
pub mod top_bar;
pub mod volume;
pub mod wave;
#[cfg(test)]
mod preview;

use std::sync::Arc;

use nice_plug::context::gui::GuiContext;
use nice_plug::editor::ResizeHint;
use nice_plug::editor::dpi::LogicalSize;
use nice_plug_egui::baseview::HandlerError;
use nice_plug_egui::{
    EguiEditor, EguiEditorState, EguiNiceSettings, Frame, NiceEguiApp, RepaintNotifier,
    create_egui_editor,
};

use crate::StuttterParams;
use crate::editor::layout::EditorLayout;
use crate::pattern::{Lane, Pattern};
use crate::scope::Scope;

/// What the window manager calls the window, when it is floated off
/// Bitwig's own frame.
pub const WINDOW_TITLE: &str = "stuttter";

/// The steps of a default pattern — one 4/4 bar at 1/16. The grid scales
/// whatever the pattern's length is into the width it has, so this is only
/// what the default window is laid out around.
pub const STEPS_SHOWN: f32 = 16.0;

/// The width the window opens at.
pub const DEFAULT_WIDTH: f32 = 900.0;

/// The height the window opens at.
pub const DEFAULT_HEIGHT: f32 = 540.0;

/// The narrowest the window may be dragged: room for the top bar's controls,
/// sixteen usable cells and the block panel.
pub const MIN_WIDTH: f32 = 720.0;

/// The shortest the window may be dragged: room for the bar, both rows, a
/// waveform and most of the block panel (which scrolls past that).
pub const MIN_HEIGHT: f32 = 460.0;

/// The width the grid column has in a window of the default size: the window
/// less its padding, the block panel and the gap before it.
pub const GRID_WIDTH: f32 =
    DEFAULT_WIDTH - 2.0 * theme::PANEL_PADDING - theme::BLOCK_PANEL_WIDTH - theme::COLUMN_GAP;

/// The top bar and the hairline under it.
pub const BAR_HEIGHT: f32 = theme::TOP_BAR_HEIGHT + theme::HAIRLINE;

/// How often an open window redraws with nothing else asking it to: often
/// enough that a pattern change, an automation move or a loaded project shows
/// while the transport is stopped, without redrawing flat out.
pub const IDLE_REPAINT: std::time::Duration = std::time::Duration::from_millis(100);

/// The editor window's size and open flag, as a new instance has them.
///
/// Built by the plugin struct rather than by [`create`] on purpose: the host
/// may close and reopen the window many times over one instance, and all of
/// the window state nice-plug-egui keeps — the open flag the audio thread's
/// repaint calls are wasted without — should survive that.
pub fn default_state() -> Arc<EguiEditorState> {
    // Logical points, so the window is the same size in the same place on a
    // HiDPI screen as on this one; `1.0` is the user zoom on top of whatever
    // the host reports, and the host's own scaling is applied over it.
    EguiEditorState::from_size(LogicalSize::new(DEFAULT_WIDTH, DEFAULT_HEIGHT), 1.0)
}

/// The editor the host is handed, over the window state the plugin instance
/// holds.
///
/// Assembling it opens no window — the host does that later through
/// `Editor::spawn()` — so this is safe to call with no display, which is how
/// it is tested here.
///
/// `params` is the instance's own: the pattern buttons light from `pattern`
/// and the top bar's two rails drive `mix` and `smooth` through the host. The
/// sixteen patterns themselves come through the same struct, in the
/// persistent field beside the three parameters (`crate::StateField`), which
/// is what the grid reads.
///
/// `repaint_notifier` is how the audio thread asks for a frame: the playhead
/// and the waveform move with the song and not with the mouse, and a
/// transport that advanced is not input as far as egui is concerned, so
/// `process()` rings it.
///
/// `scope` is the instance's own waveform summaries, which the engine writes
/// and the bottom band draws ([`wave`]). The instance's and not the engine's,
/// because an instance may be activated again at another sample rate — and so
/// given another engine — while this window is open, and the window should
/// keep drawing (`crate::Stuttter`).
pub fn create(
    state: Arc<EguiEditorState>,
    repaint_notifier: RepaintNotifier,
    params: Arc<StuttterParams>,
    scope: Arc<Scope>,
) -> Option<EguiEditor<StuttterEditor>> {
    let settings = EguiNiceSettings::new()
        // `with_tile`, not `with_title`: nice-plug-egui 0.6.0's own spelling.
        .with_tile(WINDOW_TITLE)
        // Resizable like chopdrop's: the host's own window edges resize it,
        // down to a size the controls still fit in.
        .with_resize_hint(
            ResizeHint::RESIZABLE.with_min_logical_size(LogicalSize::new(MIN_WIDTH, MIN_HEIGHT)),
        );

    create_egui_editor(state, repaint_notifier, settings, StuttterEditor::new(params, scope))
}

/// The editor's per-window state.
///
/// The instance's own parameters, and beside them what belongs to one opened
/// window: the host context a parameter is written through, and Bitwig's
/// window the unused keys are sent to. Each is taken afresh in
/// [`NiceEguiApp::build`], as everything that arrives later will be (the
/// half-made drag over the grid, the pattern being edited), because
/// nice-plug-egui 0.6 never calls [`NiceEguiApp::editor_closed`].
pub struct StuttterEditor {
    /// The instance's parameters: Pattern, which the bar lights the playing
    /// button from, and Mix and Smoothing, which the bar's two rails drive
    /// through the host. The instance's own struct and never a copy: a window
    /// handed a copy would light from a parameter the host is not automating,
    /// and write to one it is not recording.
    params: Arc<StuttterParams>,

    /// Which pattern this window edits, and whether it follows the host's
    /// ([`top_bar::Selection`]). Per opened window, like everything else
    /// here: a hand over one window rather than anything the instance or the
    /// project holds, so it is built afresh in [`NiceEguiApp::build`].
    patterns: top_bar::Selection,

    /// The top bar's length box, and the drag or typed number that has it
    /// ([`top_bar::LengthBox`]). The length itself belongs to the pattern
    /// and not to this window — this is only the half-made gesture over it,
    /// which is why it is per window and reset in [`NiceEguiApp::build`].
    length: top_bar::LengthBox,

    /// The Time row's half-made gestures ([`grid::TimeRowHands`]): the block
    /// a press has hold of and what the drag is doing to it, and the block a
    /// right-press opened the menu on. The blocks themselves belong to the
    /// pattern and are written through the model like everything else — this
    /// is the hand over them, so it is per window and built afresh in
    /// [`NiceEguiApp::build`] too, and it is what keeps the window on the
    /// pattern a gesture started on.
    row: grid::TimeRowHands,

    /// The Volume row's half-made gesture ([`volume::VolumeDraw`]): the
    /// column a drag across the bars last wrote and the level it gave it.
    /// The levels themselves belong to the pattern and are written through
    /// the model — this is the hand over them, so it is per window and built
    /// afresh in [`NiceEguiApp::build`] like the Time row's, and it holds the
    /// window on the pattern the gesture started on too.
    volume: volume::VolumeDraw,

    /// Which of the pattern's rows of levels the Volume row is showing —
    /// Volume, Cutoff or Res ([`volume::ui_lane_bar`]). Per window, and back
    /// to Volume in a new one.
    lane: Lane,

    /// The waveform box being dragged, as it was picked up
    /// ([`block::SourceHold`]), or `None`. Per window like the rows' hands.
    source_hold: Option<block::SourceHold>,

    /// The Mix rail's half-made gesture ([`top_bar::Rail`]): the
    /// `end_set_parameter` it owes the host, and the value being typed into
    /// it. The *value* is the host's and is never here, which is the whole
    /// difference between these two controls and everything else in the
    /// window. Per opened window, like the rest of this struct.
    mix: top_bar::Rail,

    /// The Smoothing rail's, which is the Mix rail's story exactly — two
    /// rails, two hands, two gestures that have to be able to be open at
    /// once.
    smooth: top_bar::Rail,

    /// The window's host context, the only way a parameter may be written
    /// (the host owns the value, the GUI asks).
    /// Taken afresh in [`NiceEguiApp::build`] for each opened window, which
    /// is the only place nice-plug-egui offers it, and `None` until then.
    gui_ctx: Option<GuiContext>,

    /// Bitwig's window around this one: where the keys the editor does not
    /// use are sent, and the pin that is taken off it ([`keys`]). Per opened
    /// window too — it holds that window's X11 id — and `None` when there is
    /// no X11 display to reach, which is how the tests here see it.
    host_window: Option<keys::HostWindow>,

    /// The waveform summaries the bottom band draws ([`wave`]): the
    /// instance's own, shared with whichever engine is writing it.
    ///
    /// Per instance and not per window, like `params` and unlike everything
    /// else that arrives in [`NiceEguiApp::build`] — the audio thread keeps
    /// writing it whether or not a window is open, and a window that reopened
    /// should find the last cycle still in it rather than a view that has to
    /// fill up again. Read-only here: the engine is the only writer
    /// (`crate::scope` §The one-writer contract).
    scope: Arc<Scope>,
}

impl StuttterEditor {
    /// An editor over an instance's parameters, with no window around it yet
    /// — [`NiceEguiApp::build`] finds the host context for each window the
    /// host opens.
    fn new(params: Arc<StuttterParams>, scope: Arc<Scope>) -> StuttterEditor {
        StuttterEditor {
            params,
            patterns: top_bar::Selection::new(),
            length: top_bar::LengthBox::new(),
            row: grid::TimeRowHands::idle(),
            volume: volume::VolumeDraw::default(),
            lane: Lane::Volume,
            source_hold: None,
            mix: top_bar::Rail::default(),
            smooth: top_bar::Rail::default(),
            gui_ctx: None,
            host_window: None,
            scope,
        }
    }

    /// Hands the window around the editor everything the editor itself has
    /// no use for, once a frame.
    ///
    /// [`keys::HostWindow::update`] runs whether or not there is a key to
    /// pass on: it is what finds the host's window in the first place, and
    /// what takes the window's pin off again each time Bitwig puts it back.
    /// Key repeats are passed on along with the rest, so holding a key down
    /// reaches the host as the stream of presses it really is.
    ///
    /// Both of those jobs need a next frame to be done on, and this window
    /// is only repainted per frame while the transport plays — which is no
    /// help on a window that has just opened over a stopped transport. So
    /// while the windows are still moving, this asks
    /// ([`keys::HostWindow::is_settling`]); once they have settled the
    /// editor goes back to being reactive and costs nothing idle.
    fn pass_unused_keys_to_the_host(&mut self, ctx: &egui::Context, typing: bool) {
        let Some(host) = self.host_window.as_mut() else {
            return;
        };
        host.update();
        if host.is_settling() {
            ctx.request_repaint_after(keys::SETTLING_POLL);
        }
        if !host.has_host() {
            return;
        }

        ctx.input(|input| {
            for event in &input.events {
                if let egui::Event::Key { key, pressed, modifiers, .. } = event
                    && let keys::Route::Host(keysym) = keys::route(*key, *modifiers, typing)
                {
                    host.send(keysym, *pressed, *modifiers);
                }
            }
        });
    }

    /// Hands the window around the editor the keep-on-top switch as the
    /// instance's prefs have it, once a frame.
    ///
    /// Read from the pref and not from the click on the toggle, which is what
    /// makes the one path do both jobs: the user flipping the switch
    /// ([`top_bar::toggle_keep_on_top`]) and the host loading a project whose
    /// switch is the other way about arrive here as the same thing — a pref
    /// that no longer matches what the window is doing. Every other frame
    /// hands over a value that has not changed and costs a comparison
    /// ([`keys::pin_change`]).
    fn keep_the_window_on_top(&mut self) {
        let keep_on_top = self.params.state.prefs().get().keep_on_top;
        if let Some(host) = self.host_window.as_mut() {
            host.set_keep_on_top(keep_on_top);
        }
    }

    /// Whether the keyboard belongs to a box in the window this frame, in
    /// which case neither Bitwig nor the history gets a key of it
    /// ([`keys::route`]).
    ///
    /// A number typed into the top bar or into a block's menu is typed into
    /// this window, and with Bitwig listening every digit of it would also
    /// be one of its shortcuts; Ctrl+Z over it undoes the typing in the box,
    /// not the gesture before it.
    ///
    /// egui's own reading of that is the whole of it bar one frame: a rail
    /// that has just been right-clicked into a text box has asked for the
    /// focus and not been given it yet ([`top_bar::Rail`]), and a key in that
    /// frame is meant for the box the user has just opened.
    fn typing(&self, ctx: &egui::Context) -> bool {
        ctx.text_edit_focused() || self.mix.typing() || self.smooth.typing()
    }
}

impl NiceEguiApp for StuttterEditor {
    /// Called once per opened window, before the first frame.
    ///
    /// The `GuiContext` is kept, because it is the only way the top bar's Mix
    /// and Smoothing may be written: a host parameter belongs to the host,
    /// and the GUI asks it to set one. Everything else in the window writes
    /// through the model instead.
    fn build(
        &mut self,
        egui_ctx: egui::Context,
        nice_gui_ctx: GuiContext,
        frame: &mut Frame,
    ) -> Result<(), HandlerError> {
        theme::apply(&egui_ctx);

        // What the renderer clears to before egui paints: the darkest grey of
        // the chrome rather than baseview's black, so the first frame of a
        // window that has just opened is the window's own background.
        frame.set_clear_color(theme::WINDOW.into());

        // Per opened window, and before the first frame: the host has not
        // put the editor window into its own yet, which is why
        // `pass_unused_keys_to_the_host` keeps asking (`editor/keys.rs`).
        // The keep-on-top switch goes in with it, as the project left it: a
        // window that opens with it on must never take Bitwig's mark off,
        // not even for the frame it would take to read the pref
        // (`keys::HostWindow::connect`).
        let keep_on_top = self.params.state.prefs().get().keep_on_top;
        self.host_window = keys::HostWindow::connect(frame, keep_on_top);

        // And everything the last window left behind, since nice-plug-egui
        // 0.6 never calls `editor_closed` to do it on the way out — the host
        // context included, which is per window and not per instance, and
        // the pattern this window edits, which a reopened window starts over
        // with (`top_bar::Selection`).
        self.gui_ctx = Some(nice_gui_ctx);
        self.patterns = top_bar::Selection::new();
        self.length = top_bar::LengthBox::new();
        // The Time row's drag and the menu that may have been open over a
        // block with them: the row they had hold of is still there, but the
        // hand is two windows ago, and a gesture that thought it was open
        // would hold the next window on whatever pattern this one was
        // editing ([`grid::TimeRowHands`]).
        self.row = grid::TimeRowHands::idle();
        // And the hand on the Volume row, for the same reason: a gesture
        // that thought it was open would ramp this window's first frame from
        // a column the last window's hand let go of
        // ([`volume::VolumeDraw`]).
        self.volume = volume::VolumeDraw::default();
        self.lane = Lane::Volume;
        self.source_hold = None;
        // Both rails too, and the `end_set_parameter` either of them might
        // have owed the host: the window it was owed through is gone, and a
        // rail that still thought it was held would swallow the first press
        // of the new one (`top_bar::Rail`).
        self.mix = top_bar::Rail::default();
        self.smooth = top_bar::Rail::default();

        Ok(())
    }

    /// One frame: the window around the editor caught up with first, then
    /// the window's chrome, then each of the four bands over it.
    ///
    /// Everything is placed from [`EditorLayout`] and nothing from egui's own
    /// layout cursor, which is what a fixed-size window buys: the bands are
    /// an arithmetic over the window's rect, so a band that draws more than
    /// it was given cannot push the band under it down the window. Each gets
    /// a child `Ui` of its own rect, or is painted straight into it, which is
    /// the seam each band's own module fills.
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut Frame) {
        self.draw(ui);
    }
}

impl StuttterEditor {
    /// One frame of the window, drawn into `ui` — the whole of
    /// [`NiceEguiApp::ui`], which needs nothing of the host's window frame.
    fn draw(&mut self, ui: &mut egui::Ui) {
        // Before anything is drawn, and whether or not anything is: the keys
        // this window has no use for belong to Bitwig, and the window itself
        // has to be found and unpinned on some frame.
        //
        // The switch first, so a frame that turns it off unpins in that same
        // frame's `update` rather than waiting for the next one — and so a
        // project the host loaded with it the other way about moves the
        // window as soon as the editor is drawn again
        // (`keys::HostWindow::set_keep_on_top`).
        let typing = self.typing(ui.ctx());
        ui.ctx().request_repaint_after(IDLE_REPAINT);
        self.keep_the_window_on_top();
        self.pass_unused_keys_to_the_host(ui.ctx(), typing);

        // And the keys it does keep back: the three shortcuts that step this
        // window's own history ([`history::apply`]). Taken here, before
        // anything is drawn from the model, so the bar and the rows of this
        // very frame show the state stepped to rather than the one left
        // behind — an undo shows at the moment the key goes down, as it does
        // in a host.
        //
        // Not while a hand is still holding something over the grid, which
        // is the one thing a key can arrive in the middle of and a click on
        // the buttons cannot (`history` §The keys). The same reading of
        // "a gesture is half-made" holds the edited pattern still a few
        // lines below, so it is taken once.
        let holding = self.row.busy() || self.volume.open();
        let model = self.params.state.model();
        ui.ctx()
            .input(|input| history::apply(&input.events, typing, holding, model, &mut self.length));

        // The root `Ui`'s whole rect, which for this window is the window:
        // nice-plug-egui draws the editor through egui's root pass, with no
        // panel of its own in the way (`nice-plug-egui` 0.6.0 →
        // `egui::Context::run_ui`).
        let layout = EditorLayout::new(ui.max_rect());
        paint_chrome(ui.painter(), &layout);

        // Which pattern is playing — a held note's, else the host's
        // parameter's — and, with follow on, the one this window therefore
        // edits. Read once a frame, before anything is drawn from it, so the
        // bar, the grid and the rows of a frame all show the same pattern.
        let playing = top_bar::playing_index(self.params.playing());
        // Held back while a gesture is half-made, so a drag finishes on the
        // pattern it started on ([`top_bar::edited_this_frame`]).
        let busy = self.length.editing() || holding;
        let edited = top_bar::edited_this_frame(&mut self.patterns, playing, busy);

        // And the patterns themselves, once a frame: one `SharedCell::get`,
        // as the audio thread takes one at the top of its block, so
        // everything drawn from the model this frame is drawn from one
        // snapshot rather than from two halves of an edit. Taken after the
        // shortcuts above have stepped, which is what puts the state they
        // stepped to in this frame.
        let data = model.shared();

        // The bar's own rows, inside the bar's padding: a child `Ui` because
        // what goes in it is egui widgets, and bounded to the bar so that a
        // control too wide for it is cut off rather than reaching into the
        // Time row.
        let mut bar = ui.new_child(egui::UiBuilder::new().max_rect(layout.bar_controls()));
        // `max_rect` alone only tells egui where to *lay out*: a child `Ui`
        // inherits its parent's clip rect, so without this a control wider
        // than the bar would still be painted, over the Time row under it.
        bar.shrink_clip_rect(layout.top_bar());

        // The first row: the pattern buttons and the follow toggle, each in a
        // slot of its own ([`top_bar::BarSlots`]). The model goes with them
        // for the right-click menu's sake — a copy of a whole pattern over
        // another, or clearing one, are writes like any other in this window
        // — while the left-click picks the pattern this window edits, which
        // is the one thing on the bar that changes nothing in the model.
        let slots = top_bar::BarSlots::new(layout.bar_row(0));
        let setter = self.gui_ctx.as_ref().map(GuiContext::param_setter);
        top_bar::ui_pattern_bar(
            &mut bar,
            &slots,
            model,
            &mut self.patterns,
            playing,
            &self.params,
            setter.as_ref(),
        );

        // The second: the edited pattern's rate and length, written straight
        // through the model's writer API.
        let controls = top_bar::ControlSlots::new(layout.bar_row(1));
        // The pattern this window is showing, taken once: the bar's rate and
        // length and the Time row's grid under them are two views of the same
        // one, and a frame that read it twice could draw them from two halves
        // of an edit.
        let showing = data.pattern(edited);
        if let Some(showing) = showing {
            top_bar::ui_pattern_controls(
                &mut bar,
                &controls,
                model,
                edited,
                showing,
                &mut self.length,
            );
        }

        // Then the two host parameters, which are the one thing in this
        // window that is not written through the model: the setter is this
        // frame's, taken from the window's context, and it is `None` only
        // before `build` has run (`top_bar::ui_param_rails`).
        top_bar::ui_param_rails(
            &mut bar,
            &controls,
            &self.params,
            setter.as_ref(),
            &mut self.mix,
            &mut self.smooth,
        );

        // And the window's own Undo and Redo, which step the model's history
        // and leave those two parameters to Bitwig's ([`history`]).
        history::ui_history_buttons(&mut bar, &controls, model, &mut self.length);

        // Last on the row, the keep-on-top switch: the instance's own pref,
        // flipped here and acted on by the window around the editor at the
        // top of the next frame (`keep_the_window_on_top` above). Not in the
        // model and not in the history — it changes what the window does and
        // never what a sample is (`crate::state::PrefsCell`).
        top_bar::ui_keep_on_top(&mut bar, &controls, self.params.state.prefs());
        // And after it the Notes setting, the other pref on the row: whether
        // notes play the patterns, and from which key.
        top_bar::ui_notes(&mut bar, &controls, self.params.state.prefs());
        top_bar::ui_latch(&mut bar, &controls, self.params.state.prefs());

        // Then the Time row: the edited pattern's steps, with its blocks
        // drawn over them in their actions' colours, and a click on an empty
        // step adding one ([`grid`]). The prefs cell goes with it because
        // that is where the action a click adds is remembered
        // (`grid::action_to_add`) — the instance's own cell, so the memory
        // is saved with the project and is the same one a restore replaces.
        //
        // A window showing a pattern index the snapshot has none of (which
        // cannot happen, since both are clamped to the same sixteen) falls
        // back to the placeholder rather than drawing an empty grid for it.
        let hovered = match showing {
            Some(showing) => grid::ui_time_row(
                ui,
                layout.time_row(),
                model,
                self.params.state.prefs(),
                edited,
                showing,
                &mut self.row,
            ),
            None => {
                name_the_band(ui, layout.time_row(), theme::CELL, "Time row");
                None
            }
        };

        // Then the Volume row, on the same columns under it: a bar per step
        // at the level that step carries, and a drag across them drawing
        // those levels ([`volume`]). No prefs cell this time — there is
        // nothing about a level to remember between gestures, only the hand
        // drawing it.
        // Over it, the strip that picks which of the pattern's three rows of
        // levels it shows, with the pattern's filter beside the tabs.
        if let Some(showing) = showing {
            volume::ui_lane_bar(ui, layout.lane_bar(), model, edited, showing, &mut self.lane);
        }
        match showing {
            Some(showing) => volume::ui_volume_row(
                ui,
                layout.volume_row(),
                model,
                edited,
                showing,
                self.lane,
                &mut self.volume,
            ),
            None => name_the_band(ui, layout.volume_row(), theme::VOLUME_TRACK, "Volume row"),
        }
        // The block panel on the right: the selected block's settings, or
        // what to do when none is selected ([`panel`]).
        if let Some(showing) = showing {
            panel::ui_block_panel(
                ui,
                layout.panel(),
                model,
                self.params.state.prefs(),
                edited,
                showing,
                &mut self.row.editor,
            );
        }
        // Where every block reads from, for the waveform to wash over: the
        // selected one in full, the one under the pointer lightly, the rest
        // as stripes along the foot.
        let selected = showing.and_then(|showing| self.row.editor.selected(edited, showing));
        let washes = showing.map_or_else(Vec::new, |showing| washes(showing, selected, hovered));

        // And the last band, which is the only one that does not come from
        // the snapshot above: the audio itself, as the engine summarised it
        // into this instance's scope ([`wave`]). What came in drawn dim, what
        // went out drawn bright over it in its blocks' colours, the span ahead of the playhead
        // dimmer because it is the previous cycle's, and the playhead over
        // the lot. Nothing to write and nothing to clamp against a pattern —
        // the band's rect and the scope are the whole of its input.
        // The two pattern indices go with it: the summaries are of whatever
        // is *playing*, and this window may well be showing another pattern
        // (follow is off by default), so the band dims itself and drops its
        // playhead rather than claiming to be a picture of the grid above it
        // ([`wave::Subject`]). The output is drawn in the colours of the
        // *playing* pattern's blocks, since that is the pattern the scope's
        // columns are of.
        let playing_pattern = playing.and_then(|index| data.pattern(index));
        let band = layout.waveform();
        let input =
            wave::ui_waveform(ui, band, &self.scope, playing, edited, playing_pattern, &washes);
        if let Some(showing) = showing {
            let (hold, editor) = (&mut self.source_hold, &mut self.row.editor);
            let prefs = self.params.state.prefs();
            apply_wash(hold, editor, model, prefs, input, showing, edited, &washes, band);
        }
        // Last, so it sits over everything under it.
        resize_grip(ui, layout.full());

        // The snapshots this frame's writes retired, freed now that the
        // frame has stopped reading them: the editor collects once a frame
        // and the audio thread never does. Every write collects too, so an
        // instance with no window open still reclaims — this is what keeps a
        // window that is being dragged through from piling them up between
        // writes.
        drop(data);
        model.collect();
    }
}

/// The window's own surface and the one line on it that belongs to no band:
/// the panel grey every band is drawn on, and the hairline under the top bar
/// ([`layout::EditorLayout::separator`]).
///
/// [`theme::WINDOW`] is left showing nowhere at all — the renderer clears to
/// it ([`NiceEguiApp::build`]) so that the first frame of a window that has
/// just opened is this window's own dark rather than baseview's black, and
/// this covers it on every frame after.
/// The grip in the window's bottom-right corner: dragging it asks the host
/// for a new window size, down to [`MIN_WIDTH`] × [`MIN_HEIGHT`].
///
/// The plugin asking is the dependable way round: a host that resizes its
/// own frame is not guaranteed to resize the window inside it (in Bitwig on
/// Linux that left black space beside an unchanged window), whereas a size
/// the plugin requests is applied to the plugin's window and then reported
/// to the host, which resizes its frame to match.
/// What a gesture on the waveform's washes means for the block: picking
/// the selected box up, dragging it (moving its fixed spot, or with its
/// right edge its loop), dropping it as one undo step, or selecting the
/// block whose stripe was clicked.
#[allow(clippy::too_many_arguments)] // the frame's pieces, borrowed apart
fn apply_wash(
    hold: &mut Option<block::SourceHold>,
    editor: &mut block::BlockEditor,
    model: &crate::model::StutterModel,
    prefs: &crate::state::PrefsCell,
    input: wave::WashInput,
    showing: &Pattern,
    edited: usize,
    washes: &[wave::SourceWash],
    band: egui::Rect,
) {
    match input {
        wave::WashInput::Idle => {}
        wave::WashInput::Picked { edge } => {
            let picked = washes.iter().find(|wash| wash.level == wave::WashLevel::Selected);
            *hold = picked.and_then(|wash| {
                let index = wash.block;
                Some(block::SourceHold {
                    index,
                    origin: showing.blocks().get(index)?.action(),
                    spot: block::spot_of(showing, index)?,
                    edge,
                    width: ((wash.to - wash.from) * f64::from(band.width())) as f32,
                })
            });
        }
        wave::WashInput::Moved { dx } => {
            let Some(held) = *hold else { return };
            let Some(block) = showing.blocks().get(held.index) else { return };
            let step_width = band.width() / f32::from(showing.length());
            let chosen = held.action_at(showing, dx, step_width);
            let current = block.action();
            editor.drag_source(model, prefs, edited, held.index, current, chosen);
        }
        wave::WashInput::Dropped => {
            *hold = None;
            editor.drop_source(model);
        }
        wave::WashInput::Select(index) => editor.select(model, edited, Some(index)),
    }
}

/// Every block's source as a wash over the waveform: `selected` in full,
/// `hovered` lightly, the rest faint.
fn washes(
    showing: &Pattern,
    selected: Option<usize>,
    hovered: Option<usize>,
) -> Vec<wave::SourceWash> {
    let span = showing.span_beats();
    (0..showing.blocks().len())
        .filter_map(|index| {
            let (low, high) = panel::source_span(showing, index)?;
            let action = showing.blocks()[index].action();
            let level = match (selected == Some(index), hovered == Some(index)) {
                (true, _) => wave::WashLevel::Selected,
                (false, true) => wave::WashLevel::Hovered,
                (false, false) => wave::WashLevel::Faint,
            };
            Some(wave::SourceWash {
                block: index,
                from: low / span,
                to: high / span,
                colour: grid::BlockKind::of(action).colour(),
                level,
                stretch: action.looping.is_some(),
            })
        })
        .collect()
}

fn resize_grip(ui: &mut egui::Ui, full: egui::Rect) {
    let size = egui::Vec2::splat(theme::RESIZE_GRIP_SIZE);
    let corner = egui::Rect::from_min_size(full.max - size, size);
    let id = egui::Id::unique("stuttter-resize-grip");
    let response = ui.interact(corner, id, egui::Sense::drag());

    if response.hovered() || response.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeNwSe);
    }
    if response.drag_started() {
        ui.data_mut(|data| data.insert_temp(id, full.size()));
    }
    if response.dragged()
        && let Some(delta) = response.total_drag_delta()
    {
        let start = ui.data(|data| data.get_temp::<egui::Vec2>(id)).unwrap_or(full.size());
        let wanted = (start + delta).max(egui::vec2(MIN_WIDTH, MIN_HEIGHT));
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::InnerSize(wanted));
    }

    nice_plug_egui::resizable_window::paint_resize_corner(ui, &response);
}

fn paint_chrome(painter: &egui::Painter, layout: &EditorLayout) {
    painter.rect_filled(layout.full(), egui::CornerRadius::ZERO, theme::PANEL);
    painter.rect_filled(layout.separator(), egui::CornerRadius::ZERO, theme::GRID_LINE);
}

/// One band's placeholder: its surface in `fill`, with `name` dim in the
/// middle of it.
///
/// Painted rather than laid out, which is how the bands it stands in for are
/// drawn too — there is no egui widget per step cell, per volume bar or per
/// waveform column, so the geometry is arithmetic and the painter is the
/// whole of the drawing.
fn name_the_band(ui: &egui::Ui, rect: egui::Rect, fill: egui::Color32, name: &str) {
    let painter = ui.painter();
    painter.rect_filled(rect, theme::CELL_CORNER, fill);
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        name,
        egui::FontId::proportional(theme::HINT_SIZE),
        theme::TEXT_DIM,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh instance's parameters, as [`crate::Stuttter::default`]
    /// builds them: the editor is assembled over one instance's own.
    fn fresh_params() -> Arc<StuttterParams> {
        Arc::new(StuttterParams::default())
    }

    /// A fresh instance's scope, the same way: nothing written into it yet,
    /// which is what a window opened before the host activated the instance
    /// draws from.
    fn fresh_scope() -> Arc<Scope> {
        Arc::new(Scope::new())
    }

    /// A new instance's window opens at the size this module names, in
    /// logical points, and knows it is not on screen yet.
    #[test]
    fn the_window_opens_at_the_default_size() {
        let state = default_state();

        assert!(!state.is_open(), "no window has been spawned");
        assert_eq!(state.user_scale_factor(), 1.0);

        let size = state.logical_size();
        assert_eq!(size.width, DEFAULT_WIDTH);
        assert_eq!(size.height, DEFAULT_HEIGHT);
    }

    /// The window is the grid wide and its four bands tall, both of them
    /// sums over the theme's own figures — the arithmetic that lets the Time
    /// row hold sixteen whole cells and the two rows sit a whole
    /// [`theme::ROW_GAP`] apart.
    // The constants are the subject: these asserts are here so a change to
    // one of them fails the suite with its numbers, which a `const` block
    // (clippy's suggestion) could neither format nor report per test.
    #[allow(clippy::assertions_on_constants)]
    #[test]
    fn the_default_size_holds_the_bar_the_rows_and_the_panel() {
        assert_eq!(STEPS_SHOWN, 16.0, "a default pattern is sixteen steps");
        assert_eq!((DEFAULT_WIDTH, DEFAULT_HEIGHT), (900.0, 540.0));
        assert!(MIN_WIDTH < DEFAULT_WIDTH && MIN_HEIGHT < DEFAULT_HEIGHT);
        assert_eq!(
            GRID_WIDTH,
            DEFAULT_WIDTH
                - 2.0 * theme::PANEL_PADDING
                - theme::BLOCK_PANEL_WIDTH
                - theme::COLUMN_GAP
        );
        assert!(GRID_WIDTH / STEPS_SHOWN >= theme::CELL_PITCH, "cells at least bitstep's size");
        assert_eq!(BAR_HEIGHT, theme::TOP_BAR_HEIGHT + theme::HAIRLINE);
    }

    /// The host is given a resizable editor over the instance's own window
    /// state, with a minimum size. `create` builds no window, so this runs
    /// with no display.
    #[test]
    fn the_host_gets_a_resizable_editor_over_the_instances_window_state() {
        use nice_plug::editor::{Editor, SizeConstraints};

        let state = default_state();
        let editor =
            create(Arc::clone(&state), RepaintNotifier::new(), fresh_params(), fresh_scope())
                .expect("an editor is always built");

        assert_eq!(Arc::strong_count(&state), 2);

        let hint = editor.resize_hint();
        assert!(hint.can_resize && hint.can_resize_horizontally && hint.can_resize_vertically);
        assert!(!hint.preserve_aspect_ratio);
        match hint.size_constraints {
            SizeConstraints::Logical { min_size, max_size } => {
                assert_eq!(min_size, Some(LogicalSize::new(MIN_WIDTH, MIN_HEIGHT)));
                assert_eq!(max_size, None);
            }
            other => panic!("the constraints are in logical points, not {other:?}"),
        }

        drop(editor);
        assert_eq!(Arc::strong_count(&state), 1);
    }

    /// A new editor has no window around it yet, so no host window to send
    /// keys to: both are found per opened window in
    /// [`NiceEguiApp::build`], and a frame drawn before then passes nothing
    /// on rather than reaching for a connection that is not there.
    #[test]
    fn a_new_editor_has_no_host_window_to_send_keys_to() {
        let editor = StuttterEditor::new(fresh_params(), fresh_scope());

        assert!(editor.host_window.is_none(), "no window has been opened");
        assert!(editor.gui_ctx.is_none(), "and so no host context either");

        // And the window's own state starts where `build` will put it back
        // for every window after this one: editing the first pattern,
        // following nothing (`top_bar::Selection::new`), with no half-made
        // length gesture over the bar (`top_bar::LengthBox`).
        assert_eq!(editor.patterns, top_bar::Selection::new());
        assert_eq!(editor.length, top_bar::LengthBox::new());
        assert!(!editor.length.editing());

        // The two rails with it: no gesture open over either of them, and so
        // no `end_set_parameter` owed to a host this editor has not been
        // given a context for yet ([`top_bar::Rail`]).
        assert_eq!(editor.mix, top_bar::Rail::default());
        assert_eq!(editor.smooth, top_bar::Rail::default());
        assert!(!editor.mix.held() && !editor.smooth.held(), "nothing is owed the host");
        assert!(!editor.mix.typing() && !editor.smooth.typing(), "no value is being typed");
    }

    /// A fresh instance plays pattern 1, so the bar lights its first button
    /// — the one the window is editing too, which is what a window that has
    /// just been dropped on a track shows.
    #[test]
    fn the_bar_lights_the_pattern_the_parameters_name() {
        let editor = StuttterEditor::new(fresh_params(), fresh_scope());
        let playing = top_bar::playing_index(editor.params.pattern());

        assert_eq!(playing, Some(0), "pattern 1 is where the parameter starts");
        let lit = top_bar::Look { edited: true, playing: true };
        assert_eq!(editor.patterns.look(0, playing), lit);
    }

    /// The editor is given the instance's own parameters and holds them for
    /// as long as the host keeps the editor: the Mix it drives is the one the
    /// host automates, not a copy of its own.
    #[test]
    fn the_editor_holds_the_instances_params_and_scope() {
        let params = fresh_params();
        let scope = fresh_scope();
        let editor = create(
            default_state(),
            RepaintNotifier::new(),
            Arc::clone(&params),
            Arc::clone(&scope),
        )
        .expect("an editor is always built");

        assert_eq!(Arc::strong_count(&params), 2, "the editor shares the one params struct");
        // And the one scope: the waveform band draws the summaries the engine
        // is writing, not a copy of them that would stay silent forever
        // ([`wave::ui_waveform`]).
        assert_eq!(Arc::strong_count(&scope), 2);

        drop(editor);
        assert_eq!(Arc::strong_count(&params), 1);
        assert_eq!(Arc::strong_count(&scope), 1);
    }
}
