//! The host's window around the editor: passing it the keys the editor does
//! not use, and keeping it from being pinned above the host's main window.
//!
//! Ported from chopdrop's `editor/keys.rs` (bitstep's before that), since
//! every detail below came from testing in Bitwig on this machine. The keys
//! are chopdrop's unchanged — this plugin has no song mode either, so Tab is
//! egui's to move between fields with — and the unpinning is bitstep's, with
//! stuttter's own switch in front of it ([`Pin`]).
//!
//! On Linux the plugin window takes the keyboard focus when it is clicked
//! (egui-baseview asks for it), and baseview's X11 backend drops every key the
//! editor does not use, where on Windows and macOS it hands them to the host.
//! So once the window had been clicked, Bitwig never saw Space and the
//! transport could not be started from it. This module sends those keys on to
//! the window the editor is embedded in, which is where they would have gone
//! had the editor not been listening.
//!
//! Only transport and shortcut keys are passed on — Space, anything held with
//! Ctrl, and F1–F12 — and never while something in the window is being typed
//! into (the top bar's number boxes). Keys the editor uses itself (Escape,
//! Enter, Tab, the arrows, Delete) stay here, so a key meant for a popover
//! cannot delete something in the host — and Delete is one of this plugin's
//! own, the key that deletes the hovered block. So do undo and redo
//! ([`history_key`]): with the editor focused they step through stuttter's
//! own edits ([`super::history::apply`]), as in any plugin with an undo of
//! its own, and Bitwig never sees them.
//!
//! [`route`] is the whole of that sorting — one place where a press becomes
//! one of the three things it can become, in the order the editor applies
//! the rules. chopdrop's copy of this module had the two rules and no table
//! over them, each asked in turn by whichever caller cared; the table is
//! stuttter's, and chopdrop's `HistoryKey` went into it: the way through the
//! history is [`Step`], the one the bar's two buttons take as well.
//!
//! # Keep on top
//!
//! Bitwig shows a plugin's editor in a desktop window of its own, which it
//! marks as belonging to its main window (`WM_TRANSIENT_FOR`). Window
//! managers keep such a window above its main window, so the editor stays on
//! top of Bitwig and comes back up whenever Bitwig is focused. bitstep and
//! chopdrop simply take that mark off; here it is a switch saved with the
//! project and **on** by default ([`crate::state::Prefs::keep_on_top`]),
//! which is the one thing in this module that is stuttter's own:
//!
//! - **On:** Bitwig's mark is left exactly as it is. Nothing is deleted and
//!   no bring-to-front is sent — the desktop is already keeping the window
//!   in front, which is what the switch asks for.
//! - **Off:** bitstep's behaviour exactly. When the editor's parent is a
//!   desktop window of that kind, the mark is taken off once the window
//!   manager is showing it, and again whenever the host puts it back, so the
//!   two windows stack like any others and Alt+Tab moves between them. The
//!   first time, the window is also brought to the front and focused: Bitwig
//!   opens it without the focus, which only its dialog mark had kept it in
//!   front of the main window.
//! - **Back on while the window is open:** the mark that was deleted is put
//!   back, with the value it had — which is why the value is read before it
//!   is deleted rather than assumed to be Bitwig's main window
//!   ([`x11::Target`]).
//!
//! [`pin_change`] is the whole of that decision, kept pure here so the three
//! transitions are read in one place; what each of them does to the window
//! is the X11 side's ([`x11::Target::set_keep_on_top`]). Whether KWin puts
//! the window back in front on the third transition without the window being
//! reopened is `Verify: manual` — the caveat goes in the toggle's hover
//! text rather than a fight with the window manager, and
//! [`super::top_bar::KEEP_HOVER`] carries it.

use std::time::Duration;

use egui::{Key, Modifiers};

use super::history::Step;

/// How often the editor asks for a frame while [`HostWindow::is_settling`]:
/// several times over the 300 ms a pin check waits, and nothing at all once
/// the windows have settled.
pub const SETTLING_POLL: Duration = Duration::from_millis(100);

/// X11 modifier bits, as a key event's `state` carries them.
const SHIFT_MASK: u16 = 1;
const CONTROL_MASK: u16 = 1 << 2;
const ALT_MASK: u16 = 1 << 3;

/// The X keysym of Space.
const KEYSYM_SPACE: u32 = 0x20;

/// The X keysym of F1; F2..F12 follow it.
const KEYSYM_F1: u32 = 0xFFBE;

/// Where a key press goes: the three things that can become of one, which
/// [`route`] decides between.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    /// This window's own edit history, stepped [`Step`]'s way
    /// ([`super::history::apply`]). The only shortcut the editor claims that
    /// the host has one of its own for.
    History(Step),

    /// On to the host's window, as this X keysym ([`forwarded_keysym`]).
    Host(u32),

    /// Nowhere: the key stays in this window, for egui and the Time row to
    /// use — which is every key there is while a box in the window is being
    /// typed into.
    Editor,
}

/// The undo/redo shortcut `key` held with `modifiers` stands for, if any:
/// Ctrl+Z undoes, Ctrl+Shift+Z and Ctrl+Y redo. Alt+anything is left alone.
pub fn history_key(key: Key, modifiers: Modifiers) -> Option<Step> {
    if !modifiers.command || modifiers.alt {
        return None;
    }
    match (key, modifiers.shift) {
        (Key::Z, false) => Some(Step::Undo),
        (Key::Z, true) | (Key::Y, false) => Some(Step::Redo),
        _ => None,
    }
}

/// The X keysym to pass `key` on to the host as, when it is held with
/// `modifiers` — or `None` when the editor keeps it.
pub fn forwarded_keysym(key: Key, modifiers: Modifiers) -> Option<u32> {
    if key == Key::Space {
        return Some(KEYSYM_SPACE);
    }
    let name = key.name();
    if let Some(number) = name.strip_prefix('F').and_then(|n| n.parse::<u32>().ok())
        && (1..=12).contains(&number)
    {
        return Some(KEYSYM_F1 + number - 1);
    }
    // Letters and digits only with Ctrl: on their own they are typing, and
    // Bitwig's shortcuts for them (save, undo, copy…) all use Ctrl.
    let mut chars = name.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if modifiers.ctrl && c.is_ascii_alphanumeric() => {
            // Lower case: the keysym on the key itself, with Shift in the
            // state the way a real key press carries it.
            Some(u32::from(c.to_ascii_lowercase()))
        }
        _ => None,
    }
}

/// Where the press of `key` with `modifiers` goes in a frame whose keyboard
/// belongs to a box in the window when `typing`.
///
/// The window's key decision table, in the order the rules are asked:
///
/// | the frame | the key | where it goes |
/// |---|---|---|
/// | a box is being typed into | anything at all | the window: it is the box's |
/// | otherwise | Ctrl+Z, Ctrl+Shift+Z, Ctrl+Y | this window's history |
/// | otherwise | Space, F1–F12, Ctrl+letter, Ctrl+digit | the host's window |
/// | otherwise | anything else | the window |
///
/// Both orderings in it are the point. `typing` comes first because a digit
/// typed into the length box is a Bitwig shortcut as well, and the box is
/// what the user is looking at. Then the history, because Ctrl+Z is both an
/// undo shortcut here and a Ctrl+letter the host has a shortcut for, and
/// whichever rule is asked first wins — asking [`history_key`] before
/// [`forwarded_keysym`] is the whole of what keeps undo local.
pub fn route(key: Key, modifiers: Modifiers, typing: bool) -> Route {
    if typing {
        return Route::Editor;
    }
    if let Some(step) = history_key(key, modifiers) {
        return Route::History(step);
    }
    match forwarded_keysym(key, modifiers) {
        Some(keysym) => Route::Host(keysym),
        None => Route::Editor,
    }
}

/// What a change of the keep-on-top switch asks of the host's window — the
/// three transitions of this module's §Keep on top, as the two that do
/// something.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pin {
    /// Take Bitwig's `WM_TRANSIENT_FOR` mark off the window, remembering
    /// the value for a later [`Pin::Restore`], and bring the window to the
    /// front once — it is an ordinary unfocused window from here on, and
    /// would otherwise drop behind Bitwig's at the moment it is unpinned.
    Remove,

    /// Put the remembered mark back, so the desktop keeps the window above
    /// Bitwig's again. Nothing is raised: the window manager is being asked
    /// to hold it in front, which is the whole of what the switch means.
    Restore,
}

/// What turning the switch from `was` to `now` asks of the window, or `None`
/// where it asks for nothing — which is every frame but the one a click or a
/// project load falls in, since the editor hands this module the pref on
/// every frame ([`HostWindow::set_keep_on_top`]).
///
/// Pure, and the one place the three transitions are decided: the X11 side
/// below does what this says and makes no decision of its own, so the rule
/// is read and tested without a display.
pub fn pin_change(was: bool, now: bool) -> Option<Pin> {
    match (was, now) {
        // Off: bitstep's unpinning, which is what was held back while the
        // switch was on.
        (true, false) => Some(Pin::Remove),
        // Back on while the window is open: the deleted mark goes back.
        (false, true) => Some(Pin::Restore),
        // The switch stood still, and a window that is already as the user
        // asked for is not touched — least of all by asking the window
        // manager to raise it on every frame.
        (true, true) | (false, false) => None,
    }
}

/// `modifiers` as the `state` field of an X key event.
pub fn x11_state(modifiers: Modifiers) -> u16 {
    let mut state = 0;
    if modifiers.shift {
        state |= SHIFT_MASK;
    }
    if modifiers.ctrl {
        state |= CONTROL_MASK;
    }
    if modifiers.alt {
        state |= ALT_MASK;
    }
    state
}

/// The window the editor is embedded in: sends it keys and keeps it unpinned.
/// One per open window.
pub struct HostWindow {
    #[cfg(target_os = "linux")]
    target: x11::Target,
}

impl HostWindow {
    /// Connects for the editor window behind `frame`, with the keep-on-top
    /// switch as the project left it (`keep_on_top`). The host's window is
    /// found when the editor window is put into it, which can be after the
    /// first frame (see [`x11::Target`]). `None` when there is no X11 display
    /// — not Linux, say — in which case keys simply stay with the editor.
    ///
    /// The switch is passed in here rather than read on the first frame so
    /// that a window opened with it **on** never unpins at all: the host puts
    /// the editor window into its own before any frame of it is drawn, and a
    /// window that defaulted to off would take Bitwig's mark off and put it
    /// straight back.
    #[cfg(target_os = "linux")]
    pub fn connect(frame: &nice_plug_egui::Frame, keep_on_top: bool) -> Option<HostWindow> {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};

        let handle = frame.baseview_window().window_handle().ok()?;
        let own = match handle.as_raw() {
            RawWindowHandle::Xlib(handle) => u32::try_from(handle.window).ok()?,
            RawWindowHandle::Xcb(handle) => handle.window.get(),
            _ => return None,
        };
        x11::Target::connect(own, keep_on_top).map(|target| HostWindow { target })
    }

    #[cfg(not(target_os = "linux"))]
    pub fn connect(_frame: &nice_plug_egui::Frame, _keep_on_top: bool) -> Option<HostWindow> {
        // Elsewhere baseview already hands unused keys to the host, and the
        // host's own window manager is the one keeping it in front.
        None
    }

    /// Catches the window up with the keep-on-top switch, which the editor
    /// hands over on every frame: a value that has not changed does nothing
    /// at all, and a change does what [`pin_change`] says ([`Pin`]).
    ///
    /// Driven from the pref rather than from the click, so the switch follows
    /// a project the host loaded while the window was open as well as the
    /// toggle on the bar.
    pub fn set_keep_on_top(&mut self, keep_on_top: bool) {
        #[cfg(target_os = "linux")]
        self.target.set_keep_on_top(keep_on_top);
        #[cfg(not(target_os = "linux"))]
        let _ = keep_on_top;
    }

    /// Catches up with what happened to the windows since the last frame:
    /// the editor being put into the host's window, and the host pinning
    /// that window above its main window. Call once a frame. Cheap: it reads
    /// only events already waiting, and asks the server something only when
    /// one of them calls for it.
    pub fn update(&mut self) {
        #[cfg(target_os = "linux")]
        self.target.update();
    }

    /// Whether there is a host window to send keys to yet.
    pub fn has_host(&self) -> bool {
        #[cfg(target_os = "linux")]
        return self.target.has_host();
        #[cfg(not(target_os = "linux"))]
        false
    }

    /// Whether the windows are still moving under the editor, so it should
    /// ask egui for another frame rather than go idle.
    ///
    /// Two of the three things [`Self::update`] waits for happen behind
    /// egui's back: the host putting the editor window into its own, and the
    /// window manager finishing showing that window so its pin can come off.
    /// Neither is an egui event, so neither wakes a reactive editor. bitstep
    /// repainted continuously and never had to ask; here the unpin check
    /// would otherwise be scheduled 300 ms out and never run, and the window
    /// would sit above Bitwig for good.
    pub fn is_settling(&self) -> bool {
        #[cfg(target_os = "linux")]
        return self.target.is_settling();
        #[cfg(not(target_os = "linux"))]
        false
    }

    /// Sends one key press or release.
    pub fn send(&self, keysym: u32, pressed: bool, modifiers: Modifiers) {
        #[cfg(target_os = "linux")]
        self.target.send(keysym, pressed, x11_state(modifiers));
        #[cfg(not(target_os = "linux"))]
        let _ = (keysym, pressed, modifiers);
    }
}

#[cfg(target_os = "linux")]
mod x11;

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Modifiers = Modifiers::NONE;

    #[test]
    fn space_and_function_keys_always_go_to_the_host() {
        assert_eq!(forwarded_keysym(Key::Space, NONE), Some(0x20));
        assert_eq!(forwarded_keysym(Key::Space, Modifiers::SHIFT), Some(0x20));
        assert_eq!(forwarded_keysym(Key::F1, NONE), Some(0xFFBE));
        assert_eq!(forwarded_keysym(Key::F12, NONE), Some(0xFFC9));
    }

    #[test]
    fn letters_and_digits_go_only_with_ctrl() {
        assert_eq!(forwarded_keysym(Key::S, NONE), None);
        assert_eq!(forwarded_keysym(Key::S, Modifiers::CTRL), Some(u32::from(b's')));
        assert_eq!(forwarded_keysym(Key::Num1, Modifiers::CTRL), Some(u32::from(b'1')));
    }

    #[test]
    fn keys_the_editor_uses_stay_with_it() {
        for key in [Key::Escape, Key::Enter, Key::Tab, Key::Delete, Key::Backspace, Key::ArrowLeft]
        {
            assert_eq!(forwarded_keysym(key, NONE), None, "{key:?}");
            assert_eq!(forwarded_keysym(key, Modifiers::CTRL), None, "{key:?} with Ctrl");
        }
        // Not a function key, even though the name starts with F.
        assert_eq!(forwarded_keysym(Key::F13, NONE), None);
        // Nor is the letter F, whose name starts with it too.
        assert_eq!(forwarded_keysym(Key::F, NONE), None);
    }

    /// A held Ctrl, as X11 hands one to egui: both bits, which is what
    /// [`history_key`] reads.
    const CTRL: Modifiers = Modifiers::CTRL.plus(Modifiers::COMMAND);

    #[test]
    fn undo_and_redo_keys_are_recognised() {
        assert_eq!(history_key(Key::Z, CTRL), Some(Step::Undo));
        assert_eq!(history_key(Key::Z, CTRL | Modifiers::SHIFT), Some(Step::Redo));
        assert_eq!(history_key(Key::Y, CTRL), Some(Step::Redo));
        assert_eq!(history_key(Key::Z, NONE), None, "plain Z is typing");
        assert_eq!(history_key(Key::Z, CTRL | Modifiers::ALT), None);
        assert_eq!(history_key(Key::Y, CTRL | Modifiers::SHIFT), None);
        assert_eq!(history_key(Key::S, CTRL), None, "save still goes to the host");
    }

    /// [`route`]'s table, row by row: every kind of key this window can be
    /// handed, in a frame with a box being typed into and in a frame without
    /// one, against where the key is to go.
    ///
    /// The table is the window's key contract and the one place it is
    /// decided, so it is written out here rather than derived from the
    /// two rules underneath it — a row that changes is meant to be read as
    /// a changed row.
    #[test]
    fn the_routing_table_sends_each_key_where_it_belongs() {
        let z = u32::from(b'z');
        let table: &[(Key, Modifiers, Route)] = &[
            // This window's history: the three shortcuts, and nothing else.
            (Key::Z, CTRL, Route::History(Step::Undo)),
            (Key::Z, CTRL | Modifiers::SHIFT, Route::History(Step::Redo)),
            (Key::Y, CTRL, Route::History(Step::Redo)),
            // Held with Alt as well, Ctrl+Z is no longer the shortcut — and
            // it is still a Ctrl+letter, so it goes on to the host.
            (Key::Z, CTRL | Modifiers::ALT, Route::Host(z)),
            // Nor is Ctrl+Shift+Y one, which is Bitwig's to do what it likes
            // with.
            (Key::Y, CTRL | Modifiers::SHIFT, Route::Host(u32::from(b'y'))),
            // The host's: the transport, the function keys, and the
            // shortcuts that are not this window's.
            (Key::Space, NONE, Route::Host(0x20)),
            (Key::Space, Modifiers::SHIFT, Route::Host(0x20)),
            (Key::F1, NONE, Route::Host(0xFFBE)),
            (Key::F12, NONE, Route::Host(0xFFC9)),
            (Key::S, CTRL, Route::Host(u32::from(b's'))),
            (Key::Num1, CTRL, Route::Host(u32::from(b'1'))),
            // The window's own: typing, the keys egui moves and closes
            // things with, and Delete, which takes a block off the row.
            (Key::Z, NONE, Route::Editor),
            (Key::S, NONE, Route::Editor),
            (Key::Delete, NONE, Route::Editor),
            (Key::Escape, NONE, Route::Editor),
            (Key::Enter, NONE, Route::Editor),
            (Key::Tab, NONE, Route::Editor),
            (Key::ArrowLeft, NONE, Route::Editor),
            // Not a function key, though its name starts with F; nor is the
            // letter F.
            (Key::F13, NONE, Route::Editor),
            (Key::F, NONE, Route::Editor),
        ];

        for &(key, modifiers, expected) in table {
            assert_eq!(route(key, modifiers, false), expected, "{key:?} with {modifiers:?}");
            assert_eq!(
                route(key, modifiers, true),
                Route::Editor,
                "{key:?} with {modifiers:?} belongs to the box being typed into"
            );
        }
    }

    /// The editor's own shortcuts are asked about first, so Bitwig never
    /// sees them — the one row of the table the order of the rules decides.
    #[test]
    fn undo_and_redo_are_kept_back_from_the_host() {
        for modifiers in [CTRL, CTRL | Modifiers::SHIFT] {
            assert_eq!(
                forwarded_keysym(Key::Z, modifiers),
                Some(u32::from(b'z')),
                "Ctrl+Z is a Ctrl+letter, so the forwarding rule would take it"
            );
            assert_eq!(
                route(Key::Z, modifiers, false),
                Route::History(history_key(Key::Z, modifiers).expect("a shortcut")),
                "but the history is asked first"
            );
        }
    }

    /// The keep-on-top switch's three transitions (§Keep on top): the two
    /// that ask the window for something, and the frames that ask for
    /// nothing — which is nearly all of them, since the editor hands the
    /// pref over on every frame.
    #[test]
    fn the_keep_on_top_switch_asks_for_one_thing_per_change() {
        assert_eq!(pin_change(true, false), Some(Pin::Remove), "off: bitstep's unpinning");
        assert_eq!(pin_change(false, true), Some(Pin::Restore), "back on: the mark goes back");
        assert_eq!(pin_change(true, true), None, "on and left on: Bitwig's mark is left alone");
        assert_eq!(pin_change(false, false), None, "off and left off: nothing to re-ask");
    }

    #[test]
    fn modifiers_map_to_x11_state_bits() {
        assert_eq!(x11_state(NONE), 0);
        assert_eq!(x11_state(Modifiers::SHIFT), 1);
        assert_eq!(x11_state(Modifiers::CTRL), 4);
        assert_eq!(x11_state(Modifiers::ALT), 8);
        assert_eq!(x11_state(Modifiers::CTRL | Modifiers::SHIFT), 5);
    }
}
