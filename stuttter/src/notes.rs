//! Patterns played from notes: which key does what, and which pattern the
//! notes so far ask for.
//!
//! Off unless the window's **Notes** setting says which C to start from
//! ([`crate::state::Prefs::notes`]). From there, sixteen keys in a row play
//! the sixteen patterns: set to C6, C6 plays pattern 1, C#6 pattern 2, up to
//! D#7 for pattern 16 (Bitwig's names, where 60 is C3). Off by default
//! because a device after an instrument is handed that instrument's notes —
//! a drum machine's start at C1 — and they would switch patterns on every
//! hit. A high octave the instrument does not use keeps them apart.
//!
//! Two ways to play, the window's **Latch** switch
//! ([`crate::state::Prefs::latch`]):
//!
//! - **Held** (the default): a pattern plays for as long as its note is
//!   held. When several are held the newest wins, and letting it go hands
//!   back to the one held before it.
//! - **Latched**: a note starts its pattern and it keeps playing after the
//!   note ends, until the next pattern note, or the key right after the
//!   sixteen — Stop, E7 from C6.
//!
//! Either way, with no note playing a pattern nothing plays: while Notes is
//! on the Pattern parameter is not heard at all.
//!
//! Pure, like the other core modules: the shell turns nice-plug's note events
//! into [`trigger_for_key`] and [`HeldNotes`] calls, and nothing here
//! allocates on that path.

use crate::pattern::PATTERN_COUNT;

/// The lowest key the Notes setting can start from: C-2, MIDI key 0.
pub const LOWEST_START: u8 = 0;
/// The highest: C7, MIDI key 108, whose sixteen keys end at D#8 (123) and
/// whose Stop is E8 (124) — the last C whose seventeen still fit under
/// MIDI's 127.
pub const HIGHEST_START: u8 = 108;
/// Where the setting goes when it is first switched on: C6, an octave a drum
/// machine leaves empty, so stutter notes can share the drums' clip.
pub const SUGGESTED_START: u8 = 96;

/// Every key the setting can start from, lowest first: the C of each octave
/// from C-2 to C7.
pub fn starts() -> impl Iterator<Item = u8> {
    (LOWEST_START..=HIGHEST_START).step_by(12)
}

/// A stored start key made legal: the C at or below it, no higher than
/// [`HIGHEST_START`]. For hostile project files.
pub fn clamp_start(key: u8) -> u8 {
    let key = key.min(HIGHEST_START);
    key - key % 12
}

/// The pattern (`1..=16`) that `key` plays with the setting at `start`, or
/// `None` for a key outside the sixteen.
pub fn pattern_for_key(key: u8, start: u8) -> Option<u8> {
    let offset = key.checked_sub(start)?;

    (usize::from(offset) < PATTERN_COUNT).then_some(offset + 1)
}

/// What a note on a key does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    /// Start pattern `1..=16`; it plays until the next trigger.
    Play(u8),
    /// Stop the latched pattern: nothing plays until the next note.
    Stop,
}

/// What a note on `key` does with the setting at `start`: one of the sixteen
/// patterns, Stop on the key after them (latched only), or `None` for any
/// other key.
pub fn trigger_for_key(key: u8, start: u8) -> Option<Trigger> {
    match pattern_for_key(key, start) {
        Some(number) => Some(Trigger::Play(number)),
        None => (key.checked_sub(start) == Some(PATTERN_COUNT as u8)).then_some(Trigger::Stop),
    }
}

/// The key that stops the triggered pattern with the setting at `start`: the
/// one right after pattern 16's.
pub fn stop_key(start: u8) -> u8 {
    start.saturating_add(PATTERN_COUNT as u8)
}

/// The key that plays pattern `number` (`1..=16`) with the setting at
/// `start`, or `None` outside them.
pub fn key_for_pattern(number: u8, start: u8) -> Option<u8> {
    (1..=PATTERN_COUNT as u8).contains(&number).then(|| start.saturating_add(number - 1))
}

/// The sixteen pattern keys a setting at `start` covers, by name: "C6–D#7".
pub fn range_name(start: u8) -> String {
    let last = start.saturating_add(PATTERN_COUNT as u8 - 1);
    format!("{}–{}", key_name(start), key_name(last))
}

/// The note name of `key` in Bitwig's naming, where 60 is C3: "C1", "D#2".
pub fn key_name(key: u8) -> String {
    const NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
    let octave = i32::from(key / 12) - 2;

    format!("{}{octave}", NAMES[usize::from(key % 12)])
}

/// Whether `number` is one of the sixteen patterns.
fn is_pattern(number: u8) -> bool {
    (1..=PATTERN_COUNT as u8).contains(&number)
}

/// The patterns whose notes are down, oldest first: what plays is the top.
///
/// Held, each note pushes its pattern and its release takes it out again.
/// Latched, a note replaces the lot ([`HeldNotes::latch`]) and releases are
/// not passed in at all, so the top is simply the last note's pattern.
///
/// Each pattern appears at most once: pressing a key that is already held
/// moves it to the top rather than counting it twice, so one release always
/// lets it go. That is the right answer for the case it covers — the same key
/// held on two channels — because the pattern is the same either way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeldNotes {
    /// Pattern numbers (`1..=16`), oldest first; only `..len` is used.
    order: [u8; PATTERN_COUNT],
    /// How many of `order` are held.
    len: usize,
}

impl HeldNotes {
    /// No notes held.
    pub const fn new() -> HeldNotes {
        HeldNotes { order: [0; PATTERN_COUNT], len: 0 }
    }

    /// The pattern the notes ask for: the newest, or `None` with nothing
    /// held.
    pub fn top(&self) -> Option<u8> {
        self.len.checked_sub(1).map(|last| self.order[last])
    }

    /// Pattern `number`'s note goes down, which makes it the newest held.
    /// A number outside `1..=16` is ignored.
    pub fn press(&mut self, number: u8) {
        if !is_pattern(number) {
            return;
        }
        self.release(number);
        // Room is guaranteed: each of the sixteen patterns is in here at most
        // once, and `number` was just taken out.
        self.order[self.len] = number;
        self.len += 1;
    }

    /// Pattern `number`'s note comes up. A pattern that is not held is
    /// ignored.
    pub fn release(&mut self, number: u8) {
        let Some(at) = self.order[..self.len].iter().position(|&held| held == number) else {
            return;
        };
        self.order.copy_within(at + 1..self.len, at);
        self.len -= 1;
    }

    /// A latched note: pattern `number` and nothing else, until the next.
    /// A number outside `1..=16` is ignored.
    pub fn latch(&mut self, number: u8) {
        if is_pattern(number) {
            self.clear();
            self.press(number);
        }
    }

    /// Every note up at once: Stop, a reset, or the transport stopping.
    pub fn clear(&mut self) {
        self.len = 0;
    }
}

impl Default for HeldNotes {
    fn default() -> HeldNotes {
        HeldNotes::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// From the start key, sixteen keys are the sixteen patterns in order,
    /// and nothing either side plays one.
    #[test]
    fn sixteen_keys_from_the_start_play_the_sixteen_patterns() {
        let start = SUGGESTED_START;
        assert_eq!(pattern_for_key(95, start), None);
        assert_eq!(pattern_for_key(96, start), Some(1));
        assert_eq!(pattern_for_key(97, start), Some(2));
        assert_eq!(pattern_for_key(111, start), Some(16));
        assert_eq!(pattern_for_key(112, start), None);
        assert_eq!(pattern_for_key(36, start), None, "a drum machine's kick plays nothing");

        for start in starts() {
            for number in 1..=16 {
                let key = key_for_pattern(number, start).unwrap();
                assert!(key <= 127);
                assert_eq!(pattern_for_key(key, start), Some(number));
            }
        }
        assert_eq!(key_for_pattern(0, start), None);
        assert_eq!(key_for_pattern(17, start), None);
    }

    /// The choices are the ten Cs from C-2 to C7, and a stored start is
    /// squared to one of them.
    #[test]
    fn the_setting_starts_on_a_c_that_leaves_room_for_sixteen_keys() {
        let all: Vec<u8> = starts().collect();
        assert_eq!(all, vec![0, 12, 24, 36, 48, 60, 72, 84, 96, 108]);
        assert!(all.contains(&SUGGESTED_START));
        assert_eq!(clamp_start(97), 96);
        assert_eq!(clamp_start(255), HIGHEST_START);
        assert_eq!(range_name(96), "C6–D#7");
        assert_eq!(range_name(36), "C1–D#2");
    }

    /// The names are Bitwig's, where middle C (60) is C3.
    #[test]
    fn keys_are_named_the_way_bitwig_names_them() {
        assert_eq!(key_name(60), "C3");
        assert_eq!(key_name(36), "C1");
        assert_eq!(key_name(51), "D#2");
        assert_eq!(key_name(0), "C-2");
    }

    /// The key after pattern 16's is Stop, and the keys either side of the
    /// seventeen do nothing.
    #[test]
    fn the_key_after_the_sixteen_patterns_is_stop() {
        let start = SUGGESTED_START;
        assert_eq!(trigger_for_key(96, start), Some(Trigger::Play(1)));
        assert_eq!(trigger_for_key(111, start), Some(Trigger::Play(16)));
        assert_eq!(trigger_for_key(112, start), Some(Trigger::Stop));
        assert_eq!(stop_key(start), 112);
        assert_eq!(key_name(stop_key(start)), "E7");
        assert_eq!(trigger_for_key(95, start), None);
        assert_eq!(trigger_for_key(113, start), None);
        assert_eq!(trigger_for_key(36, start), None, "a drum machine's kick does nothing");

        for start in starts() {
            assert!(stop_key(start) <= 127);
            assert_eq!(trigger_for_key(stop_key(start), start), Some(Trigger::Stop));
        }
    }

    /// The newest held note wins, and letting it go hands back to the one
    /// before it.
    #[test]
    fn the_newest_held_note_plays() {
        let mut held = HeldNotes::new();
        assert_eq!(held.top(), None);

        held.press(3);
        held.press(7);
        assert_eq!(held.top(), Some(7));

        held.release(7);
        assert_eq!(held.top(), Some(3));
        held.release(3);
        assert_eq!(held.top(), None);
    }

    /// Letting go of an older note leaves the newest playing, and a key
    /// pressed twice is held once.
    #[test]
    fn releasing_an_older_note_changes_nothing_audible() {
        let mut held = HeldNotes::new();
        held.press(1);
        held.press(2);
        held.press(3);
        held.press(1);

        held.release(2);
        assert_eq!(held.top(), Some(1));
        held.release(1);
        assert_eq!(held.top(), Some(3));
        held.release(3);
        assert_eq!(held.top(), None);
    }

    /// A latched note replaces whatever was there; numbers outside the
    /// sixteen are ignored either way.
    #[test]
    fn a_latched_note_replaces_the_lot() {
        let mut held = HeldNotes::new();
        held.press(4);
        held.press(9);
        held.latch(2);
        assert_eq!(held.top(), Some(2));
        held.release(2);
        assert_eq!(held.top(), None, "nothing else was left under it");

        held.latch(0);
        held.latch(17);
        held.press(17);
        assert_eq!(held.top(), None);
        for number in 1..=16 {
            held.press(number);
        }
        assert_eq!(held.top(), Some(16));
        held.clear();
        assert_eq!(held.top(), None);
    }
}
