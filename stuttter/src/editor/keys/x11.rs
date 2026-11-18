//! The X11 side of [`super::HostWindow`]. Uses only `x11rb`, through a
//! connection of its own (baseview keeps its connection to itself).
//!
//! The host's window is not known on the first frame. nice-plug creates the
//! editor window on its own, on the root window, when the host asks for the
//! editor, and puts it into the host's window only when the host passes
//! that window in, which can be after the editor has drawn a frame. So the
//! editor window's `ReparentNotify` events are listened for, and the host's
//! window is taken from them. Checking just once, on the first frame, left
//! the editor with no host window at all: keys were not passed on and the
//! window stayed pinned.
//!
//! Ported from chopdrop (bitstep before it), [`Target::is_settling`]
//! included: bitstep had no need of it because its editor repainted
//! continuously and so always had a next frame to be called on, where this
//! one repaints per frame only while the transport is playing and is
//! reactive the rest of the time — so it has to be told when to ask for one.
//!
//! What is stuttter's own is the keep-on-top switch in front of the
//! unpinning ([`Target::set_keep_on_top`], [`super`] §Keep on top): with it
//! on, nothing here touches `WM_TRANSIENT_FOR` or raises anything, and the
//! three X11 calls the switch costs — read the mark, delete it, put it back —
//! are [`Target::transient_for`], [`Target::unpin`] and [`Target::repin`].

use std::collections::HashMap;
use std::time::{Duration, Instant};

use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ChangeWindowAttributesAux, ClientMessageEvent, ConnectionExt, EventMask,
    KEY_PRESS_EVENT, KEY_RELEASE_EVENT, KeyPressEvent, PropMode, Property,
};
use x11rb::rust_connection::RustConnection;
// `change_property32`, which lives beside the generated protocol rather than
// in it: the one call that writes a window property back.
use x11rb::wrapper::ConnectionExt as _;

use super::{Pin, pin_change};

/// How long after a change to the host window it is checked for unpinning:
/// time for the window manager to finish showing it, raised above the main
/// window as its dialog, before it stops being one.
const SETTLE: Duration = Duration::from_millis(300);

/// How long after connecting frames are asked for just to watch the editor
/// window being put into a host window. The host does it within a frame or
/// two of opening the editor, so past this it is taken to be standing on its
/// own and the editor is let go idle. A later move is still noticed — every
/// frame drawn for any other reason reads the waiting events — it is only
/// the asking that stops.
const ADOPTION: Duration = Duration::from_secs(5);

/// The editor's window, the host's window it sits in, and a connection of
/// our own to reach them with.
pub struct Target {
    conn: RustConnection,
    root: u32,
    /// The editor's own window.
    own: u32,
    /// The window `own` sits in, once that is a host window rather than the
    /// root.
    host: Option<u32>,
    /// The `WM_STATE` and `_NET_ACTIVE_WINDOW` atoms; `NONE` if the server
    /// had none to give.
    wm_state: Atom,
    net_active_window: Atom,
    /// When to check whether the host window can be unpinned, if a check
    /// is due.
    check_at: Option<Instant>,
    /// Whether the host window has just been opened, so the first unpin
    /// should also bring it to the front.
    raise: bool,
    /// The keep-on-top switch as the window last saw it
    /// ([`Self::set_keep_on_top`]): **on** leaves Bitwig's mark alone, which
    /// is what a fresh instance does (`crate::state::Prefs`).
    keep_on_top: bool,
    /// The `WM_TRANSIENT_FOR` value taken off the host window, kept so that
    /// turning the switch back on can put back what was really there rather
    /// than a guess at it. `None` until a mark has been taken off, and again
    /// once one has been put back ([`Self::repin`]).
    pinned: Option<u32>,
    /// When to stop asking for frames on account of there being no host
    /// window yet. See [`ADOPTION`].
    hunt_until: Instant,
    /// Keysym → the keycode that types it, from the server's keymap.
    keycodes: HashMap<u32, u8>,
}

impl Target {
    /// Connects for the editor window `own`, with the keep-on-top switch as
    /// the project left it. `None` only if the server cannot be reached; an
    /// editor window that has no host window yet is fine, and is picked up by
    /// [`Self::update`] once it gets one.
    pub fn connect(own: u32, keep_on_top: bool) -> Option<Target> {
        let (conn, screen) = x11rb::connect(None).ok()?;
        let root = conn.setup().roots.get(screen)?.root;

        // Hear about `own` moving into another window. Selected before the
        // tree is asked for, so a move in between is not missed.
        let events = ChangeWindowAttributesAux::new().event_mask(EventMask::STRUCTURE_NOTIFY);
        conn.change_window_attributes(own, &events).ok()?.check().ok()?;

        let (min, max) = (conn.setup().min_keycode, conn.setup().max_keycode);
        let count = max.checked_sub(min)?.checked_add(1)?;
        let map = conn.get_keyboard_mapping(min, count).ok()?.reply().ok()?;
        let per = usize::from(map.keysyms_per_keycode).max(1);
        let mut keycodes = HashMap::new();
        for (index, keysyms) in map.keysyms.chunks(per).enumerate() {
            let Ok(code) = u8::try_from(usize::from(min) + index) else {
                break;
            };
            // The plain and shifted symbols; the first key that has one
            // wins, which is the main keyboard's rather than the keypad's.
            for &keysym in keysyms.iter().take(2) {
                keycodes.entry(keysym).or_insert(code);
            }
        }

        let atom = |name: &[u8]| {
            conn.intern_atom(false, name)
                .ok()
                .and_then(|cookie| cookie.reply().ok())
                .map_or(x11rb::NONE, |reply| reply.atom)
        };
        let wm_state = atom(b"WM_STATE");
        let net_active_window = atom(b"_NET_ACTIVE_WINDOW");

        let mut target = Target {
            conn,
            root,
            own,
            host: None,
            wm_state,
            net_active_window,
            check_at: None,
            raise: false,
            keep_on_top,
            pinned: None,
            hunt_until: Instant::now() + ADOPTION,
            keycodes,
        };
        if let Some(tree) = target.conn.query_tree(own).ok().and_then(|c| c.reply().ok()) {
            target.adopt(tree.parent);
        }
        Some(target)
    }

    /// Whether there is a host window to send keys to.
    pub fn has_host(&self) -> bool {
        self.host.is_some()
    }

    /// See [`super::HostWindow::is_settling`]. Either the host has not put
    /// the editor window into its own yet, or a pin check is waiting out its
    /// [`SETTLE`] — and neither of those arrives as an egui event, so a
    /// frame has to be asked for or [`Self::update`] is never called again.
    pub fn is_settling(&self) -> bool {
        self.check_at.is_some() || (self.host.is_none() && Instant::now() < self.hunt_until)
    }

    /// See [`super::HostWindow::update`].
    pub fn update(&mut self) {
        let mut parent = None;
        let mut check_pin = false;
        while let Ok(Some(event)) = self.conn.poll_for_event() {
            match event {
                Event::ReparentNotify(event) if event.window == self.own => {
                    parent = Some(event.parent);
                }
                Event::PropertyNotify(event)
                    if Some(event.window) == self.host
                        && event.state == Property::NEW_VALUE
                        && (event.atom == u32::from(AtomEnum::WM_TRANSIENT_FOR)
                            || event.atom == self.wm_state) =>
                {
                    check_pin = true;
                }
                _ => {}
            }
        }
        if let Some(parent) = parent {
            self.adopt(parent);
        } else if check_pin && !self.keep_on_top {
            // With the switch on, Bitwig putting its mark back is Bitwig
            // doing exactly what the switch asks for, so there is nothing to
            // check and no frame to ask for.
            self.check_at = Some(Instant::now() + SETTLE);
        }
        if let Some(at) = self.check_at
            && Instant::now() >= at
        {
            self.check_at = None;
            if let Some(host) = self.host
                && self.unpin(host)
                && std::mem::take(&mut self.raise)
            {
                self.activate(host);
            }
        }
    }

    /// Takes `parent` as the host window, unless it is the root.
    fn adopt(&mut self, parent: u32) {
        if parent == x11rb::NONE || parent == self.root {
            self.host = None;
            self.check_at = None;
            self.raise = false;
            return;
        }
        self.host = Some(parent);
        self.raise = true;
        // Nothing has been taken off *this* window: a value remembered from
        // the window the editor was in before belongs to a window that is
        // gone, and putting it back on this one would mark the editor as
        // another window's dialog.
        self.pinned = None;
        // Hear about the host pinning the window, or the window manager
        // taking it on, later. Selected whichever way the switch is set: it
        // can be turned off at any moment, and the editor would otherwise
        // never hear of the mark it then has to take off.
        let events = ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE);
        if let Ok(cookie) = self.conn.change_window_attributes(parent, &events) {
            cookie.ignore_error();
        }
        // With the switch on there is no check to make — the mark is to stay
        // exactly as Bitwig set it — so the editor is not asked for the
        // frames one would need either ([`Self::is_settling`]).
        self.check_at = (!self.keep_on_top).then(|| Instant::now() + SETTLE);
    }

    /// Catches up with the keep-on-top switch: see [`super`] §Keep on top for
    /// what each of its three transitions means, and [`pin_change`] for the
    /// decision itself, which is pure and tested beside it.
    ///
    /// Called on every frame with the pref as it stands, so the only cost of
    /// a switch nobody touched is the comparison inside [`pin_change`].
    pub fn set_keep_on_top(&mut self, keep_on_top: bool) {
        let Some(change) = pin_change(self.keep_on_top, keep_on_top) else {
            return;
        };
        self.keep_on_top = keep_on_top;

        match change {
            // Due now and not a [`SETTLE`] out: the window the user has just
            // clicked in has plainly been shown for a while, so the wait that
            // exists for a window being mapped buys nothing. A window somehow
            // not shown yet fails [`Self::is_shown`] and waits for the
            // `WM_STATE` that says it is, as it does when it opens.
            Pin::Remove => self.check_at = Some(Instant::now()),
            Pin::Restore => {
                // Nothing left to check: the mark is going back on, and a
                // check that fired after it would take it off again.
                self.check_at = None;
                // Unpinned once more, the window would drop behind Bitwig's
                // again, so the next [`Pin::Remove`] is owed a raise just as
                // the first one was ([`Self::activate`]).
                self.raise = true;
                if let Some(host) = self.host {
                    self.repin(host);
                }
            }
        }
    }

    /// Takes `WM_TRANSIENT_FOR` off `window` once the window manager is
    /// showing it, remembering the value it had. See [`super`] §Keep on top
    /// for why. Whether it was showing, so the mark was taken off.
    ///
    /// Reached only with the switch **off**: every caller is behind
    /// [`Self::check_at`], which is only ever set while it is
    /// ([`Self::set_keep_on_top`], [`Self::adopt`], [`Self::update`]).
    ///
    /// The value is read before it is deleted and kept in [`Self::pinned`],
    /// which is what lets the switch going back on put back what was really
    /// there. A read that comes back with nothing leaves the last remembered
    /// value standing rather than forgetting it: the mark may already be off
    /// — a re-check of a window nothing re-pinned — and the value from when
    /// it was taken off is still the one to restore.
    fn unpin(&mut self, window: u32) -> bool {
        if !self.is_shown(window) {
            return false;
        }
        if let Some(parent) = self.transient_for(window) {
            self.pinned = Some(parent);
        }
        if let Ok(cookie) =
            self.conn.delete_property(window, AtomEnum::WM_TRANSIENT_FOR.into())
        {
            cookie.ignore_error();
        }
        let _ = self.conn.flush();
        true
    }

    /// Puts the remembered `WM_TRANSIENT_FOR` back on `window`, so the
    /// desktop keeps the editor above Bitwig's main window again — the
    /// switch's third transition ([`Pin::Restore`]).
    ///
    /// The remembered value is **spent**: the mark is on the window now, so
    /// the next [`Self::unpin`] reads the live one, which is the value to
    /// restore the time after. With nothing remembered there is nothing to
    /// put back — the switch was never off while this window was open — and
    /// the mark Bitwig set is still there untouched.
    ///
    /// Whether the window manager acts on a mark that arrives after the
    /// window was mapped is its own business; KWin's answer is `Verify:
    /// manual` and the toggle's hover text carries the caveat.
    fn repin(&mut self, window: u32) {
        let Some(parent) = self.pinned.take() else {
            return;
        };
        if let Ok(cookie) = self.conn.change_property32(
            PropMode::REPLACE,
            window,
            AtomEnum::WM_TRANSIENT_FOR,
            AtomEnum::WINDOW,
            &[parent],
        ) {
            cookie.ignore_error();
        }
        let _ = self.conn.flush();
    }

    /// The window `window` is marked as belonging to, if any: its
    /// `WM_TRANSIENT_FOR`, which for the editor's host window is Bitwig's
    /// main window.
    ///
    /// `None` for a window with no mark, a server that would not answer, and
    /// a mark of `None` itself — a window transient for nothing is not a
    /// value worth putting back.
    fn transient_for(&self, window: u32) -> Option<u32> {
        self.conn
            .get_property(false, window, AtomEnum::WM_TRANSIENT_FOR, AtomEnum::WINDOW, 0, 1)
            .ok()?
            .reply()
            .ok()?
            .value32()?
            .next()
            .filter(|&parent| parent != x11rb::NONE)
    }

    /// Asks the window manager to bring `window` to the front and focus it.
    ///
    /// Needed once, right after the window opens. Bitwig maps it with
    /// `_NET_WM_USER_TIME` 0, "do not focus", so the main window keeps the
    /// focus; as its dialog the window was still kept above it, but once
    /// unpinned it is an unfocused window like any other and drops behind
    /// the main window. The request is marked as coming from a pager (source
    /// 2), as a taskbar click would be, because the window manager turns
    /// down an application's own request from a process the user did not
    /// click in.
    fn activate(&self, window: u32) {
        /// EWMH's source indication for pagers and taskbars.
        const SOURCE_PAGER: u32 = 2;
        if self.net_active_window == x11rb::NONE {
            return;
        }
        let event = ClientMessageEvent::new(
            32,
            window,
            self.net_active_window,
            [SOURCE_PAGER, x11rb::CURRENT_TIME, x11rb::NONE, 0, 0],
        );
        let mask = EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY;
        if let Ok(cookie) = self.conn.send_event(false, self.root, mask, event) {
            cookie.ignore_error();
        }
        let _ = self.conn.flush();
    }

    /// Whether the window manager is showing `window` as a desktop window:
    /// it has marked it `WM_STATE` Normal. A panel inside one of the host's
    /// windows never has that mark, so it is never unpinned.
    ///
    /// Waiting for the mark matters: unpinned before then, the window is
    /// mapped as a new window of its own that the user did not click, and
    /// focus-stealing prevention holds it back behind the host's main
    /// window. Unpinned after, it is in front and only needs the focus (see
    /// [`Self::activate`]).
    fn is_shown(&self, window: u32) -> bool {
        /// ICCCM's `NormalState`.
        const NORMAL_STATE: u32 = 1;
        self.wm_state != x11rb::NONE
            && self
                .conn
                .get_property(false, window, self.wm_state, AtomEnum::ANY, 0, 1)
                .ok()
                .and_then(|cookie| cookie.reply().ok())
                .and_then(|reply| reply.value32().and_then(|mut values| values.next()))
                == Some(NORMAL_STATE)
    }

    /// Sends one key event to the host window. Propagated, so it reaches
    /// whichever of the host's windows above ours listens for keys.
    /// Failures are ignored: a key that does not arrive is all it costs.
    pub fn send(&self, keysym: u32, pressed: bool, state: u16) {
        let Some(host) = self.host else {
            return;
        };
        let Some(&detail) = self.keycodes.get(&keysym) else {
            return;
        };
        let (response_type, mask) = match pressed {
            true => (KEY_PRESS_EVENT, EventMask::KEY_PRESS),
            false => (KEY_RELEASE_EVENT, EventMask::KEY_RELEASE),
        };
        let event = KeyPressEvent {
            response_type,
            detail,
            sequence: 0,
            time: x11rb::CURRENT_TIME,
            root: self.root,
            event: host,
            child: x11rb::NONE,
            root_x: 0,
            root_y: 0,
            event_x: 0,
            event_y: 0,
            state: state.into(),
            same_screen: true,
        };
        if let Ok(cookie) = self.conn.send_event(true, host, mask, event) {
            cookie.ignore_error();
        }
        let _ = self.conn.flush();
    }
}
