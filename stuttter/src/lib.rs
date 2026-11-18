//! stuttter — a stereo CLAP audio effect that chops, repeats, reverses and
//! tape-stops the incoming audio in time with the song.
//!
//! The pure modules — [`history`], [`actions`], [`pattern`], [`model`],
//! [`state`] and [`undo`] — import no nice-plug types; this file is the
//! nice-plug boundary: the `Plugin` impl, the host parameters, `process()`,
//! the editor the host is handed and the CLAP export.
//!
//! # What is here
//!
//! The declarations: the identity, the audio configuration — **stereo in,
//! stereo out, no note ports**, which is the one structural difference from
//! the chopdrop shell this is adapted from — the `ClapPlugin` constants and
//! the CLAP export. And the three host parameters: [`StuttterParams`], with
//! the clamping accessors [`Plugin::process`] reads them through. And the
//! remote-controls page those three parameters sit on:
//! [`ClapPlugin::remote_controls`].
//!
//! And the window the host opens over it: the themed, fixed-size editor of
//! [`editor`], assembled in [`Plugin::editor`] over the window state and the
//! repaint handle this instance holds, and built over this instance's own
//! parameters.
//!
//! Behind both of them, the audio: [`history`], the stereo ring buffer of
//! the recorded past every read comes out of, together with the two-head
//! crossfade [`history::Reader`] that smooths the jumps reading it makes,
//! and [`actions`], the offset maths that chooses where in it to read.
//!
//! Alongside those: [`pattern`], what a pattern *is* — the rate, the length,
//! the Time row's blocks and the Volume row's levels, with the limits and
//! invariants that make a published pattern safe for the audio thread to read
//! without checking anything, plus the beat maths over it: step and slice
//! lengths, the span a pattern repeats at, and the project beat → step, block
//! and phase the engine drives the offset maths from. And over sixteen of
//! those, [`model`]: the writer mutex every edit goes through and the
//! `SharedCell` it publishes an immutable [`model::StutterData`] to, which is
//! the one way a pattern reaches `process()` — and, over those two doors,
//! [`undo`]: the snapshot stacks the model commits a gesture's worth of edits
//! to, so the editor's Undo and Redo step whole gestures.
//!
//! Those patterns are *saved* from here too: [`StateField`] is
//! the `#[persist = "state"]` field nice-plug hands the host, and it carries
//! this instance's own model and its two GUI prefs — the keep-on-top switch
//! and the action a click adds — through [`state::Snapshot`] into the project
//! file and back, with no editor involved either way. It is also where the
//! model is *born*: the field owns the one every other handle is taken from.
//!
//! And in front of all of it, the audio path itself: [`block_ctx`] reads the
//! host's `Transport` and the three parameters into the [`engine::BlockCtx`]
//! of one block — with the rule that a block without both a position and a
//! tempo counts as stopped — [`Plugin::activate`] builds the
//! [`engine::Engine`] whose history buffer is sized from the host's sample
//! rate, and [`Plugin::process`] hands it every block: recorded whatever the
//! transport and the parameters say, played back from wherever the selected
//! pattern points, and mixed against the input it came in as.
//!
//! And the waveform summaries that walk writes as it goes: the instance owns
//! one [`scope::Scope`], [`Plugin::activate`] hands it to the engine, and the
//! editor draws from it.

pub mod actions;
pub mod editor;
pub mod engine;
pub mod filter;
pub mod history;
pub mod model;
pub mod notes;
pub mod pattern;
pub mod scope;
pub mod starters;
pub mod state;
pub mod undo;

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use nice_plug::params::persist::PersistentField;
use nice_plug::prelude::*;
use nice_plug_egui::{EguiEditor, EguiEditorState, RepaintNotifier};

use crate::editor::StuttterEditor;
use crate::engine::{BlockCtx, Engine, SelectionChanges};
use crate::model::StutterModel;
use crate::notes::{HeldNotes, Trigger};
use crate::scope::Scope;
use crate::state::{PrefsCell, Snapshot};

/// What the `pattern` parameter reads at its lowest position: no pattern at
/// all, the plugin passing the input through bit for bit.
///
/// Zero rather than a separate flag because a CLAP parameter is one number: a
/// stepped range of `0..=PATTERN_MAX` gives the host the 17 positions it
/// needs, Off being simply the one below pattern 1.
pub const PATTERN_OFF: i32 = 0;
/// The highest pattern the `pattern` parameter selects.
///
/// The `i32` view of [`pattern::PATTERN_COUNT`], which is where that limit
/// lives: the parameter's top position is the last pattern there is, and the
/// two cannot drift apart because this is the other one.
pub const PATTERN_MAX: i32 = pattern::PATTERN_COUNT as i32;
/// Where `pattern` starts: pattern 1, not Off, so a freshly dropped device
/// does something as soon as its grid has blocks in it.
pub const PATTERN_DEFAULT: i32 = 1;

/// Fully wet, which is where `mix` starts: the device is an effect someone
/// reached for, so it is heard, and the dry signal is what Off and `mix` at 0
/// are for.
pub const MIX_DEFAULT: f32 = 1.0;
/// Milliseconds `mix` takes to reach a new value.
///
/// `Linear`, not the `Logarithmic` style a gain wants: this is a crossfade
/// position between two signals, its range includes zero (which logarithmic
/// smoothing cannot cross), and the ends have to be reached *exactly* or
/// `mix` at 0 would not be the input bit for bit.
const MIX_SMOOTHING_MS: f32 = 10.0;

/// The shortest crossfade `smooth` allows, in milliseconds. Not zero: every
/// read-position jump is faded, and a zero-length fade is a click. Short
/// enough that the sample-exact tests can read a jump as near-instant.
pub const SMOOTH_MIN_MS: f32 = 0.5;
/// The longest crossfade `smooth` allows, in milliseconds. Past this the fade
/// starts eating the short slices a 1/32 pattern is made of.
pub const SMOOTH_MAX_MS: f32 = 20.0;
/// Where `smooth` starts: long enough to hide the jumps, short enough that a
/// chop still sounds like a chop.
pub const SMOOTH_DEFAULT_MS: f32 = 3.0;

/// One plugin instance.
///
/// Its parameters, its window and its engine. `Default` is how nice-plug makes
/// an instance — the host calls it before `activate()` and, for a plugin it is
/// only scanning, may never call anything else — so the engine is *not* built
/// here: there is no sample rate to size its history buffer from yet, and
/// "please don't be that plugin that takes twenty seconds to scan" (nice-plug's
/// own `activate` docs) rules out a twelve-megabyte allocation per scanned
/// instance. It arrives in [`activate`](Stuttter::activate), which is this
/// framework's `initialize()`.
pub struct Stuttter {
    /// The host-visible parameters, shared with the host and with the editor.
    params: Arc<StuttterParams>,
    /// The editor window's size and open flag, kept here rather than inside
    /// the editor because the host may close and reopen the window many times
    /// over one instance and what nice-plug-egui writes into this should
    /// survive that.
    editor_state: Arc<EguiEditorState>,
    /// The editor's wake-up handle. Lives alongside `editor_state` for the
    /// same reason, and is cheap to clone (one `Arc<AtomicBool>`).
    ///
    /// Rung from [`process`](Stuttter::process) once there is something
    /// moving with the song to see — the playhead and the waveform: egui
    /// repaints when its input changes, and a transport that advanced is not
    /// input. One relaxed store, so the audio thread may ring it.
    repaint_notifier: RepaintNotifier,
    /// The audio core, or `None` until the host activates the instance: the
    /// recorded past, the two read heads over it and the per-block walk
    /// [`process`](Stuttter::process) hands every block to.
    ///
    /// An `Option` rather than an engine built by `Default`, because the
    /// history buffer's size is the host's sample rate times
    /// [`history::MAX_LOOKBACK_SECONDS`] and that rate first arrives in
    /// `activate()` — which is also the only place the allocation is allowed.
    engine: Option<Engine>,
    /// The sample rate `engine`'s buffer was sized at, so that an `activate`
    /// repeated at the same configuration — which nice-plug warns a host may
    /// do "multiple times in rapid succession" while restoring state — does
    /// not throw the buffer away and allocate another one.
    ///
    /// Meaningless while `engine` is `None`, and never read on its own.
    engine_rate: f32,
    /// The waveform summaries the engine writes and the editor draws:
    /// [`COLUMNS`](scope::COLUMNS) min/max columns of the input and the output
    /// over one pattern cycle, plus the playhead.
    ///
    /// Owned here rather than inside `engine` because both of the threads
    /// that touch it outlive any one engine: the host may open the editor
    /// before it activates the instance, and activating it again at another
    /// sample rate builds a new engine around this same scope, which is what
    /// keeps an open window drawing instead of going dark. The engine holds
    /// a clone of this `Arc` and is the only writer (`crate::scope` §The
    /// one-writer contract).
    scope: Arc<Scope>,
    /// The patterns the notes so far ask for ([`notes`]): the top one plays
    /// over whatever the Pattern parameter says.
    held: HeldNotes,
    /// The Notes setting and Latch switch `held` came under. A change of
    /// either lets every note go: the keys mean something else now, and a
    /// note held under one mode would never be let go under the other.
    notes_mode: (Option<u8>, bool),
    /// Whether the transport was playing in the last block, so the block it
    /// stops in can let every note go — pressing play again then starts
    /// clean instead of carrying on with a latched pattern.
    was_playing: bool,
}

impl Stuttter {
    /// The audio core this instance plays through, or `None` until the host
    /// has activated it ([`Stuttter`]).
    ///
    /// Read-only, and the one door onto the audio path from outside this crate:
    /// "the block ran" is visible as the recorded past ([`Engine::history`])
    /// and the waveform summaries ([`Engine::scope`]) it left behind, and the
    /// wiring audit (`stuttter/tests/wiring.rs`) watches both through here
    /// rather than through a copy of the plugin's insides. Nothing here hands
    /// out a way to *drive* the engine: a block only ever reaches it through
    /// [`process`](Stuttter::process).
    pub fn engine(&self) -> Option<&Engine> {
        self.engine.as_ref()
    }

    /// The block's notes, as the pattern changes they make and the frames
    /// they make them on ([`notes`], [`SelectionChanges`]).
    ///
    /// With the window's Notes setting off (`start` is `None`) every event is
    /// read and dropped. Otherwise:
    ///
    /// - **Held** (`latch` false): a pattern key going down pushes its
    ///   pattern, and its note off, choke or zero-velocity note on (MIDI's
    ///   note off) takes it out; a release for any key (CLAP's wildcard) lets
    ///   go of them all.
    /// - **Latched**: only a note starting counts. A pattern key replaces
    ///   whatever was playing and the Stop key stops it.
    ///
    /// A change is only made when it changes what plays: the top note's
    /// pattern, or Off with none — with Notes on, the Pattern parameter is
    /// not heard at all.
    ///
    /// Allocation-free: the changes are a fixed-size list on the stack.
    fn take_notes(
        &mut self,
        context: &mut impl ProcessContext<Self>,
        (start, latch): (Option<u8>, bool),
        frames: usize,
    ) -> SelectionChanges {
        let mut changes = SelectionChanges::new();
        let last = frames.saturating_sub(1);

        while let Some(event) = context.next_event() {
            let Some(start) = start else {
                continue;
            };
            let (timing, key, down) = match event {
                NoteEvent::NoteOn { timing, key, velocity, .. } => (timing, key, velocity > 0.0),
                NoteEvent::NoteOff { timing, key, .. } => (timing, key, false),
                NoteEvent::Choke { timing, key, .. } => (timing, key, false),
                _ => continue,
            };

            let before = self.held.top();
            let trigger = key.number().and_then(|key| notes::trigger_for_key(key, start));
            match (latch, down, key.number(), trigger) {
                (true, true, _, Some(Trigger::Play(number))) => self.held.latch(number),
                (true, true, _, Some(Trigger::Stop)) => self.held.clear(),
                (false, true, _, Some(Trigger::Play(number))) => self.held.press(number),
                (false, false, _, Some(Trigger::Play(number))) => self.held.release(number),
                (false, false, None, _) => self.held.clear(),
                _ => continue,
            }
            let after = self.held.top();
            if after != before {
                changes.push((timing as usize).min(last), after);
            }
        }

        changes
    }
}

/// Three self-contained parameters and a window that has not opened yet, so
/// this really can be built out of nothing — which is what nice-plug asks of
/// a plugin struct. The engine is the one thing that cannot be
/// ([`Stuttter`]).
impl Default for Stuttter {
    fn default() -> Stuttter {
        Stuttter {
            params: Arc::new(StuttterParams::default()),
            editor_state: editor::default_state(),
            repaint_notifier: RepaintNotifier::new(),
            engine: None,
            engine_rate: 0.0,
            scope: Arc::new(Scope::new()),
            held: HeldNotes::new(),
            notes_mode: (None, false),
            was_playing: false,
        }
    }
}

/// Everything the project saves that is not a parameter, as the host's own
/// `#[persist = "state"]` field: the sixteen patterns and the two GUI prefs.
///
/// A newtype rather than an impl on `Arc<StutterModel>` directly, because the
/// orphan rule forbids that — and because this is where the two worlds meet:
/// `model.rs` and `state.rs` import no nice-plug types, so the trait that
/// hands their snapshots to the framework belongs here in the shell.
///
/// It holds both of a restore's destinations, and they are the instance's
/// own: the model behind this field is the one `process()` reads every block
/// and the one the editor writes through, so a load through
/// [`PersistentField::set`] is visible to the audio thread at its next
/// [`StutterModel::shared`] — there is one set of patterns, not a saved copy
/// of one. Nothing in either direction needs an editor, which is what
/// "restores work headless" means.
pub struct StateField {
    /// The patterns, behind the writer mutex every writer shares.
    model: Arc<StutterModel>,
    /// The prefs the window remembers, in the one cell this instance has.
    prefs: Arc<PrefsCell>,
}

impl StateField {
    /// A field over `model` and `prefs` — the same handles the rest of the
    /// instance holds.
    pub fn new(model: Arc<StutterModel>, prefs: Arc<PrefsCell>) -> StateField {
        StateField { model, prefs }
    }

    /// The patterns: what `process()` reads and what every editor gesture
    /// writes through.
    pub fn model(&self) -> &Arc<StutterModel> {
        &self.model
    }

    /// The prefs: the keep-on-top switch and the action a click on an empty
    /// step adds.
    pub fn prefs(&self) -> &Arc<PrefsCell> {
        &self.prefs
    }
}

/// Both directions are main-thread work that allocates freely; neither is
/// reachable from `process()`.
impl PersistentField<'_, Snapshot> for StateField {
    /// The host handing back a saved project.
    ///
    /// The patterns go in as one write through the writer mutex, so a load
    /// can neither interleave with a GUI edit nor tear the block the audio
    /// thread is already generating: what it publishes is sixteen whole
    /// patterns, every invariant re-established. Every load rule — the
    /// version, the caps, the clamping, the enums this build does not know —
    /// is [`Snapshot::data`]'s on the way in, so a hostile project file
    /// cannot panic the host.
    ///
    /// The undo stacks are dropped afterwards: opening a project is not an
    /// edit, and stepping back into the patterns it replaced would be a
    /// surprise ([`StutterModel::clear_history`]).
    fn set(&self, new_value: Snapshot) {
        self.model.install(new_value.data());
        self.model.clear_history();
        self.prefs.set(new_value.prefs());
    }

    /// The host asking for state: read the live patterns and prefs into a
    /// snapshot — exactly, the Volume row's hidden steps included — and let
    /// the framework serialise it.
    fn map<F, R>(&self, f: F) -> R
    where
        F: Fn(&Snapshot) -> R,
    {
        f(&Snapshot::of(&self.model.data(), self.prefs.get()))
    }
}

/// The plugin's host-visible parameters.
///
/// Three and no more: the patterns themselves are a grid of hundreds of steps,
/// which no fixed-size CLAP parameter list can hold, so they live in the GUI
/// and in the saved state — which is the fourth field here, [`StateField`], not
/// a parameter but the blob the host stores beside them.
///
/// The ids (`pattern`, `mix`, `smooth`) are what a host hashes its parameter
/// ids from and what a saved project refers to: renaming one orphans the
/// automation written against it.
#[derive(Params)]
pub struct StuttterParams {
    /// Which pattern plays, or Off ([`PATTERN_OFF`]). Stepped, so the host
    /// gives it 17 discrete positions rather than a knob. A change takes
    /// effect at the next step boundary, never mid-step.
    #[id = "pattern"]
    pub pattern: IntParam,
    /// Dry/wet, stored as the plain `0..=1` factor the engine multiplies by
    /// — not as a percentage — so there is no conversion on the audio
    /// thread; the host's text is the percentage.
    #[id = "mix"]
    pub mix: FloatParam,
    /// The crossfade length in milliseconds, for both read-position jumps and
    /// Volume-row step changes.
    /// Milliseconds and not samples: it has to mean the same thing at every
    /// sample rate.
    #[id = "smooth"]
    pub smooth: FloatParam,
    /// The patterns and the GUI prefs. Not a parameter but a persistent
    /// field, which the host stores as an opaque blob beside the parameter
    /// values and hands straight back on load ([`StateField`]).
    ///
    /// The key is what a saved project keys the blob on, so renaming it
    /// would lose every existing project's patterns.
    #[persist = "state"]
    pub state: StateField,
    /// The pattern a note is playing (`1..=16`), or 0 with none
    /// ([`notes`]). Not a parameter and not saved: `process()` writes it at
    /// the end of every block and the editor reads it to light the pattern
    /// that is playing ([`StuttterParams::playing`]).
    pub note_pattern: AtomicU8,
}

/// Three parameters and the state they sit beside, all of which this struct
/// is the owner of — so unlike the sibling plugins' params structs this one
/// really can be built out of nothing, and `Default` is how nice-plug makes
/// it, by way of [`Stuttter`].
///
/// The model being born here rather than passed in is the one difference
/// from chopdrop and bitstep, and it is deliberate: there, the plugin struct
/// and the loader hold the model too and a `Default` would quietly build a
/// second one nobody else sees. Here the field is the only owner, and every
/// other handle — `process()`'s, the editor's — is reached through
/// [`StateField::model`], so there is no second model to build.
impl Default for StuttterParams {
    fn default() -> StuttterParams {
        StuttterParams {
            pattern: IntParam::new(
                "Pattern",
                PATTERN_DEFAULT,
                IntRange::Linear { min: PATTERN_OFF, max: PATTERN_MAX },
            )
            .with_value_to_string(Arc::new(|value| {
                if value <= PATTERN_OFF { "Off".to_string() } else { value.to_string() }
            }))
            // So that typing the word back — which is the only text the host
            // ever showed for that position — selects it again.
            .with_string_to_value(Arc::new(|string| {
                if string.eq_ignore_ascii_case("off") {
                    Some(PATTERN_OFF)
                } else {
                    string.parse().ok()
                }
            })),
            mix: FloatParam::new("Mix", MIX_DEFAULT, FloatRange::Linear { min: 0.0, max: 1.0 })
                .with_smoother(SmoothingStyle::Linear(MIX_SMOOTHING_MS))
                .with_unit(" %")
                .with_value_to_string(formatters::v2s_f32_percentage(0))
                .with_string_to_value(formatters::s2v_f32_percentage()),
            // No smoother: this is a length the engine reads once per block
            // to size its fades, not a number anything is multiplied by, so
            // there is nothing here for a zipper noise to ride on.
            smooth: FloatParam::new(
                "Smoothing",
                SMOOTH_DEFAULT_MS,
                FloatRange::Linear { min: SMOOTH_MIN_MS, max: SMOOTH_MAX_MS },
            )
            .with_unit(" ms")
            .with_value_to_string(formatters::v2s_f32_rounded(1)),
            state: StateField::new(
                Arc::new(StutterModel::default()),
                Arc::new(PrefsCell::default()),
            ),
            note_pattern: AtomicU8::new(0),
        }
    }
}

impl StuttterParams {
    /// Which pattern plays — `1..=PATTERN_MAX` — or `None` for Off.
    ///
    /// nice-plug clamps whatever a host drives through a normalized value,
    /// but the plain `i32` is what the parameter stores and what a restored
    /// project writes into it, so the shell clamps here rather than trusting
    /// it: an `as u8` cast would quietly wrap a nonsense value into a
    /// valid-looking pattern.
    pub fn pattern(&self) -> Option<u8> {
        match clamp_pattern(self.pattern.value()) {
            PATTERN_OFF => None,
            selected => Some(selected as u8),
        }
    }

    /// The pattern a note is playing, or `None` with none.
    pub fn note_pattern(&self) -> Option<u8> {
        let number = self.note_pattern.load(Ordering::Relaxed);
        (1..=PATTERN_MAX as u8).contains(&number).then_some(number)
    }

    /// The pattern that is playing, or `None` for Off. What the editor
    /// lights and follows. With Notes on it is a note's or nothing — the
    /// Pattern parameter is not heard then — and with Notes off it is the
    /// parameter's.
    pub fn playing(&self) -> Option<u8> {
        match self.state.prefs().notes() {
            Some(_) => self.note_pattern(),
            None => self.pattern(),
        }
    }

    /// The dry/wet factor at the end of a block of `frames`, smoothed
    /// ([`MIX_SMOOTHING_MS`]).
    ///
    /// One value a block, following chopdrop's `next_master_gain`: the engine
    /// is pure and knows nothing of nice-plug's smoothers, so the smoother is
    /// advanced by the length of the block and the value it lands on is the
    /// block's *target*, which the engine then ramps to across the block a
    /// frame at a time. The mix therefore still moves per frame, and the
    /// jump between two blocks that automation writes never lands as a step
    /// at a block boundary.
    ///
    /// An empty block advances nothing — `Smoother::next_step` wants at least
    /// one step — and reads back the value the last block left.
    pub fn next_mix(&self, frames: usize) -> f32 {
        let steps = u32::try_from(frames).unwrap_or(u32::MAX);
        let mix = if steps == 0 {
            self.mix.smoothed.previous_value()
        } else {
            self.mix.smoothed.next_step(steps)
        };

        clamp_mix(mix)
    }

    /// The crossfade length in milliseconds, clamped to the parameter's own
    /// range: the engine turns this into a sample count and divides by it.
    pub fn smooth_ms(&self) -> f32 {
        clamp_smooth_ms(self.smooth.value())
    }
}

/// Folds any `i32` into `PATTERN_OFF..=PATTERN_MAX`, saturating at both ends.
/// An integer has no NaN, so this one is a plain `clamp`.
fn clamp_pattern(raw: i32) -> i32 {
    raw.clamp(PATTERN_OFF, PATTERN_MAX)
}

/// Folds any `f32` into the dry/wet range, NaN included.
///
/// `max`-then-`min`, not `f32::clamp`: that order turns a NaN from a hostile
/// host into the quiet end of the range, where `clamp` would hand the NaN
/// straight back and silently poison every sample it touched.
///
/// Public because the engine clamps the mix again on the way in
/// ([`engine::BlockCtx::mix`]) and on every frame of the line it walks
/// across a block: the exactness of the two ends is what `mix` at 0 being
/// the input bit for bit rests on, so there is one clamp and both sides use
/// it.
pub fn clamp_mix(raw: f32) -> f32 {
    // The lint wants `clamp`; the order here *is* the guard, see above.
    #[allow(clippy::manual_clamp)]
    raw.max(0.0).min(1.0)
}

/// Folds any `f32` into the Smoothing range, NaN included (see
/// [`clamp_mix`] on the order). A NaN here would land as the *shortest*
/// fade, which clicks but stays finite — unlike a NaN fade length, which
/// would make every faded sample NaN.
fn clamp_smooth_ms(raw: f32) -> f32 {
    // The lint wants `clamp`; the order here *is* the guard, see above.
    #[allow(clippy::manual_clamp)]
    raw.max(SMOOTH_MIN_MS).min(SMOOTH_MAX_MS)
}

/// Reads the host's transport into the engine's view of one block, ported from
/// bitstep's `block_ctx`.
///
/// `num_samples` comes from the buffer rather than from the transport,
/// because that is what the block's samples are actually counted in — and
/// what the plugin has to fill and record whatever the transport says.
///
/// The engine cannot lay the pattern grid over a block without a position
/// *and* a tempo: the first says where in the pattern the block starts and
/// the second how far each sample moves through it. So a transport missing
/// either — a host that reports no position while the transport sits still,
/// a free-running one with no tempo at all — reads as **stopped**, which is
/// live audio out and the history buffer still recording. The block keeps its
/// length and its sample rate even then ([`BlockCtx::stopped`]): those two
/// are known from the buffer, not from the transport.
///
/// `pattern` rides along to the same place, and arrives even on a stopped
/// block: the parameter is read once per block here, so the engine sees one
/// consistent selection for the whole of it rather than re-reading a value
/// the host may be automating underneath it.
///
/// The numbers themselves are not validated here — a nonsense tempo, rate or
/// position is [`BlockCtx`]'s business, and it resolves them to "this block
/// is stopped" rather than to a panic: every host-provided value is clamped,
/// and nothing panics on host data.
///
/// Allocation-free and lock-free: it is `process()`'s second step.
pub fn block_ctx(transport: &Transport, num_samples: usize, pattern: Option<u8>) -> BlockCtx {
    // The `pos_beats()` accessor, not the raw field: CLAP hosts are free to
    // report the position in samples or seconds only, and it reconstructs
    // beats from whichever of the three arrived. A plugin reading
    // `transport.pos_beats` directly would sit silent under such a host.
    let (Some(pos_beats), Some(tempo)) = (transport.pos_beats(), transport.tempo) else {
        return BlockCtx::stopped(transport.sample_rate, num_samples).with_pattern(pattern);
    };

    BlockCtx::new(pos_beats, tempo, transport.sample_rate, num_samples)
        .with_playing(transport.playing)
        .with_pattern(pattern)
}

impl Plugin for Stuttter {
    const NAME: &'static str = "stuttter";
    const VENDOR: &'static str = "pywkt";
    const URL: &'static str = "https://github.com/pywkt/stuttter";
    const EMAIL: &'static str = "bitstep@pywkt.com";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    /// An audio effect: one stereo input, one stereo output, and nothing
    /// else. Stereo on both sides because the effect works on whatever the
    /// track feeds it and must hand back the same shape — a mono source
    /// reaches a stereo track as two equal channels, which is the host's
    /// business, not this plugin's.
    ///
    /// Exactly one layout, so the host has nothing to choose: the first entry
    /// is the default and the only one, which is also what lets `process()`
    /// treat "two channels" as the contract rather than as a hope. No aux
    /// ports: there is no sidechain.
    const AUDIO_IO_LAYOUTS: &'static [AudioIOLayout] = &[AudioIOLayout {
        main_input_channels: NonZeroU32::new(2),
        main_output_channels: NonZeroU32::new(2),
        ..AudioIOLayout::const_default()
    }];

    /// Notes in, so a note clip can play the patterns ([`notes`]) once the
    /// window's Notes setting is on: sixteen keys from the C it names play
    /// patterns 1 to 16, while held or latched. `Basic` is note on, note off
    /// and choke, which is all of it.
    const MIDI_INPUT: MidiConfig = MidiConfig::Basic;
    /// No notes out: declaring a note output would make most hosts stop
    /// passing note data through to the rest of the track's chain.
    const MIDI_OUTPUT: MidiConfig = MidiConfig::None;

    /// The egui window of [`editor`], over the state this instance keeps for
    /// it (this struct's `editor_state`).
    type Editor = EguiEditor<StuttterEditor>;
    type SysExMessage = ();
    type BackgroundTask = ();

    fn params(&self) -> Arc<dyn Params> {
        self.params.clone()
    }

    /// The egui editor, over the window state this instance holds.
    ///
    /// Assembling it opens no window — the host does that later through
    /// `Editor::spawn()` — so this is safe to call with no display, which is
    /// how it is tested here. The parameters go in whole: the top bar drives
    /// Mix and Smoothing through the host and lights the playing pattern from
    /// Pattern. The scope goes with them, because the waveform band draws the
    /// summaries the engine writes — this instance's own, so a window that is
    /// open while the host activates the instance again at another sample rate
    /// keeps drawing from the same one ([`Stuttter::scope`]). The
    /// background-task executor goes unused: the window reads the model
    /// directly and has nothing to hand off (`BackgroundTask = ()`).
    fn editor(&mut self, _async_executor: AsyncExecutor<Self>) -> Option<Self::Editor> {
        editor::create(
            Arc::clone(&self.editor_state),
            self.repaint_notifier.clone(),
            Arc::clone(&self.params),
            Arc::clone(&self.scope),
        )
    }

    /// The host activating the instance for one audio configuration, which is
    /// nice-plug 0.4.2's `initialize()`: the engine, and so the history buffer,
    /// is built here and nowhere else.
    ///
    /// The buffer is [`history::MAX_LOOKBACK_SECONDS`] of the rate the host
    /// reports — ~12 MB at 48 kHz — which is why it is not built with the
    /// instance (see [`Stuttter`]) and absolutely not in `process()`, where
    /// an allocation is a dropout. The rate is the only thing here that sizes
    /// anything: the block size is not, because every buffer on the audio
    /// path is the host's own and the engine walks whatever length it is
    /// handed.
    ///
    /// Repeated calls at the same rate keep the engine they already built. A
    /// host may activate "multiple times in rapid succession" while
    /// restoring a project (nice-plug's own words), and nice-plug calls
    /// [`reset`](Stuttter::reset) after every one of them anyway, so there
    /// is nothing to gain from throwing the buffer away and allocating
    /// another.
    ///
    /// Never `false`: there is no configuration this plugin cannot take.
    /// [`History::new`](history::History::new) clamps a nonsense rate to a
    /// buffer that reads silence rather than panicking, so even a host that
    /// reports 0 Hz gets an instance that passes audio.
    fn activate(
        &mut self,
        _audio_io_layout: &AudioIOLayout,
        buffer_config: &BufferConfig,
        _context: &mut impl ActivateContext<Self>,
    ) -> bool {
        let rate = buffer_config.sample_rate;
        if self.engine.is_none() || self.engine_rate != rate {
            self.engine = Some(Engine::new(rate, Arc::clone(&self.scope)));
            self.engine_rate = rate;
        }

        true
    }

    /// The host saying the audio either side of this point is not continuous
    /// — a relocation, or the start of processing — so the recorded past, the
    /// read heads, the level and the mix all go
    /// ([`Engine::reset`](engine::Engine::reset)).
    ///
    /// Forgetting what was heard is the honest answer rather than a loss: the
    /// material before such a seam is not the material the next block
    /// continues from, and a repeat is better off reading silence than
    /// reading audio from somewhere else in the song. The buffer keeps its
    /// capacity, so this allocates nothing — which it may not, because
    /// nice-plug calls it from the audio thread as well as after
    /// [`activate`](Stuttter::activate).
    ///
    /// Notes go too, as a synth's voices would: whatever was holding them
    /// down is not continuous across the seam either.
    fn reset(&mut self) {
        if let Some(engine) = self.engine.as_mut() {
            engine.reset();
        }
        self.held.clear();
        self.params.note_pattern.store(0, Ordering::Relaxed);
    }

    /// One block of audio: record it, play the pattern over it, hand it back.
    ///
    /// Thin on purpose — where to read, how to smooth the jumps getting there
    /// and how much of the result to mix in are all
    /// [`Engine::run_block`](engine::Engine::run_block)'s, and unit tested
    /// without a host. What is left here is the shell's own four steps, with
    /// recording and reading the context folded together: recording is the
    /// first thing the engine's walk does to a frame, so there is no recording
    /// the block *before* the context the walk needs has been read.
    ///
    /// 1. **The block context**, from the host's transport and the three
    ///    parameters ([`block_ctx`]). All four are read exactly once per
    ///    block, so a parameter the host is automating cannot change its
    ///    answer underneath the walk, and the mix is read as the *target* the
    ///    engine ramps to across the block ([`StuttterParams::next_mix`]).
    /// 2. **The patterns**, as the one `SharedCell` read the audio thread is
    ///    allowed: what comes back is an immutable snapshot that cannot be
    ///    edited underneath the block, and holding it for the block's length is
    ///    what keeps a writer's replacement from being freed while it is being
    ///    played.
    /// 3. **The block itself**, recorded into the history buffer and
    ///    overwritten with what the engine chose — in place, because
    ///    nice-plug has already put the input into the buffer this is handed
    ///    (`wrapper/util/buffer_management.rs` copies the input channels over
    ///    the output ones whenever the host did not hand over the same
    ///    pointers for both). Recording happens inside that same walk, frame
    ///    by frame and before anything reads it, so the plugin **always
    ///    records** — playing or stopped, any pattern, even Off: a repeat
    ///    pressed right after play has the audio from before it. The same
    ///    walk writes the waveform summaries the editor draws, into the
    ///    [`Scope`] this instance owns.
    /// 4. **The editor's wake-up call**, while the window is open and the
    ///    song is moving: the playhead and the waveform follow the transport,
    ///    and a transport that advanced is not input as far as egui is
    ///    concerned, so it repaints every frame only while the transport
    ///    plays and the window is open, not unconditionally. One relaxed
    ///    store, and nothing at all with the window closed.
    ///
    /// A host outside the layout it agreed to — fewer than two channels — is
    /// left its buffer untouched, which for an effect is a bypass rather than
    /// the silence an instrument's shell would write: better a track that
    /// plays dry than a track that goes quiet. Channels past the second are
    /// passed through for the same reason. Neither is reachable from a host
    /// that honours [`Self::AUDIO_IO_LAYOUTS`].
    ///
    /// So is the un-activated case: no engine means the host called
    /// `process()` before `activate()`, and the block is passed through
    /// rather than guessing a sample rate to allocate a buffer from.
    ///
    /// `KeepAlive`, always: this effect records every block, silence
    /// included, so a host that suspended it over a quiet passage would
    /// leave the next repeat with nothing to repeat. `Normal` reads to a
    /// CLAP host as "continue if not quiet", which is exactly the licence
    /// not to call this again. No latency is ever reported, here or
    /// anywhere else: the effect only ever reads the past.
    ///
    /// Allocation-free and lock-free, which the `assert_process_allocs`
    /// feature enforces in debug and test builds: the history buffer was
    /// allocated in `activate()`, the patterns come from a `SharedCell`
    /// rather than the writer mutex, and the parameters, the window's open
    /// flag and the repaint notifier are atomics.
    fn process(
        &mut self,
        buffer: &mut Buffer,
        _aux: &mut AuxiliaryBuffers,
        context: &mut impl ProcessContext<Self>,
    ) -> ProcessStatus {
        // Step 1. `buffer.samples()`, not the transport's idea of the block
        // length: these are the frames that actually exist to be filled.
        let frames = buffer.samples();
        // The Notes setting and Latch, lock-free. Changed since the last
        // block, or the transport stopped, every note is let go before this
        // block reads its own.
        let prefs = self.params.state.prefs();
        let mode = (prefs.notes(), prefs.latch());
        // With Notes on the notes are in charge: no note is Off, whatever
        // the Pattern parameter says ([`StuttterParams::playing`]).
        let knob = match mode.0 {
            Some(_) => None,
            None => self.params.pattern(),
        };
        let playing = context.transport().playing;
        if mode != self.notes_mode || (self.was_playing && !playing) {
            self.held.clear();
            self.notes_mode = mode;
        }
        self.was_playing = playing;
        let ctx = block_ctx(context.transport(), frames, self.held.top().or(knob))
            .with_smooth_ms(self.params.smooth_ms())
            .with_mix(self.params.next_mix(frames));
        let changes = self.take_notes(context, mode, frames);

        if let Some(engine) = self.engine.as_mut() {
            // Step 2.
            let data = self.params.state.model().shared();

            // Step 3.
            if let [left, right, ..] = buffer.as_slice() {
                // Step 3 and, inside the same walk, step 4: the engine
                // writes the scope summaries as it fills the block, which is
                // the only place both the frame that came in and the frame
                // that goes out are in hand.
                engine.run_block_with(&ctx, &data, &changes, left, right);
            } else {
                nice_debug_assert!(false, "the host gave fewer than two audio channels");
            }
        }

        let note_pattern = self.held.top().unwrap_or(0);
        self.params.note_pattern.store(note_pattern, Ordering::Relaxed);

        // Step 4.
        if ctx.is_playing() && self.editor_state.is_open() {
            self.repaint_notifier.request_repaint();
        }

        ProcessStatus::KeepAlive
    }
}

impl ClapPlugin for Stuttter {
    /// The plugin was called bitstutter until 2026-11-17. The id keeps that
    /// name so projects saved before the rename still find the plugin.
    const CLAP_ID: &'static str = "com.bitstutter.bitstutter";
    const CLAP_DESCRIPTION: Option<&'static str> = Some(env!("CARGO_PKG_DESCRIPTION"));
    const CLAP_MANUAL_URL: Option<&'static str> = Some(Self::URL);
    const CLAP_SUPPORT_URL: Option<&'static str> =
        Some("https://github.com/pywkt/stuttter/issues");
    /// `AudioEffect` is the main category — the one every plugin must have at
    /// least one of, and what puts the device in Bitwig's audio-FX browser
    /// rather than among the instruments. `Glitch` and `Stereo` are the
    /// optional hints for what kind of effect it is.
    const CLAP_FEATURES: &'static [ClapFeature] =
        &[ClapFeature::AudioEffect, ClapFeature::Glitch, ClapFeature::Stereo];

    /// The one remote-controls page: section "stuttter", page "Main", the three
    /// host parameters in their fixed order.
    ///
    /// This is what a controller's eight knobs land on when Bitwig maps the
    /// device — three used, five left empty, and no page splitting to worry
    /// about since nice-plug only splits past eight parameters. The grid
    /// itself can never appear here: it is state, not parameters.
    ///
    /// Declared through the trait exactly once, at the host's request during
    /// initialization; the closures run immediately, so nothing is held.
    fn remote_controls(&self, context: &mut impl RemoteControlsContext) {
        context.add_section("stuttter", |section| {
            section.add_page("Main", |page| {
                page.add_param(&self.params.pattern);
                page.add_param(&self.params.mix);
                page.add_param(&self.params.smooth);
            })
        })
    }
}

nice_export_clap!(Stuttter);

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::{BTreeMap, VecDeque};

    use nice_plug::context::process::SendEventError;

    use crate::actions::{MAX_SPEED, SpeedPreset};
    use crate::model::StutterData;
    use crate::pattern::{Action, MAX_STEPS, PATTERN_COUNT, Pattern, Rate};
    use crate::state::Prefs;

    /// An audio effect, not an instrument and not a note effect: one stereo
    /// input and one stereo output. The host picks the first layout, so there
    /// being exactly one — and it being the stereo pair — is the contract
    /// `process()` and the engine are written against.
    #[test]
    fn the_shell_is_an_audio_effect_with_one_stereo_input_and_output() {
        assert_eq!(Stuttter::AUDIO_IO_LAYOUTS.len(), 1);
        let layout = Stuttter::AUDIO_IO_LAYOUTS[0];
        assert_eq!(layout.main_input_channels, NonZeroU32::new(2));
        assert_eq!(layout.main_output_channels, NonZeroU32::new(2));
        assert!(layout.aux_input_ports.is_empty());
        assert!(layout.aux_output_ports.is_empty());
        // The generated layout name is what a host shows in a port-layout
        // menu; two in and two out reads as "Stereo".
        assert_eq!(layout.name(), "Stereo");
    }

    /// Notes in, to play the patterns from a note clip; none out.
    #[test]
    fn notes_come_in_and_none_go_out() {
        assert_eq!(Stuttter::MIDI_INPUT, MidiConfig::Basic);
        assert_eq!(Stuttter::MIDI_OUTPUT, MidiConfig::None);
    }

    /// The identity a host keys on. `CLAP_ID` in particular is what a saved
    /// project looks the plugin up by, so changing it orphans every project
    /// that used the old one — it is pinned here, not only declared above.
    /// It still says bitstutter, the plugin's name before 2026-11-17, on
    /// purpose.
    #[test]
    fn the_identity_is_stuttter_by_pywkt() {
        assert_eq!(Stuttter::NAME, "stuttter");
        assert_eq!(Stuttter::VENDOR, "pywkt");
        assert_eq!(Stuttter::URL, "https://github.com/pywkt/stuttter");
        assert_eq!(Stuttter::EMAIL, "bitstep@pywkt.com");
        assert_eq!(Stuttter::VERSION, env!("CARGO_PKG_VERSION"));

        assert_eq!(Stuttter::CLAP_ID, "com.bitstutter.bitstutter");
        assert_eq!(Stuttter::CLAP_DESCRIPTION, Some("CLAP stutter / tape-stop audio effect"));
        assert_eq!(Stuttter::CLAP_MANUAL_URL, Some(Stuttter::URL));
        assert_eq!(
            Stuttter::CLAP_SUPPORT_URL,
            Some("https://github.com/pywkt/stuttter/issues")
        );
        assert_eq!(
            Stuttter::CLAP_FEATURES,
            &[ClapFeature::AudioEffect, ClapFeature::Glitch, ClapFeature::Stereo]
        );
    }

    /// Exactly the three host parameters, under the ids a saved project refers
    /// to and in the order the host lists them.
    #[test]
    fn the_parameter_list_is_pattern_mix_and_smoothing() {
        let params = StuttterParams::default();
        let map = params.param_map();

        let ids: Vec<&str> = map.iter().map(|(id, _, _)| id.as_str()).collect();
        assert_eq!(ids, vec!["pattern", "mix", "smooth"]);
        // One flat list, no parameter groups: three controls need no tree.
        assert!(map.iter().all(|(_, _, group)| group.is_empty()));
    }

    /// `pattern`: 17 discrete positions — Off, then 1 through 16 — starting
    /// at 1, each with the text the host shows for it.
    #[test]
    fn pattern_steps_through_off_and_sixteen_patterns() {
        let params = StuttterParams::default();
        let pattern = &params.pattern;

        assert_eq!(pattern.name(), "Pattern");
        assert_eq!(pattern.default_plain_value(), PATTERN_DEFAULT);
        assert_eq!(pattern.value(), 1);
        // 16 steps between 17 positions, which is what makes the host step
        // through them rather than draw a continuous knob.
        assert_eq!(pattern.step_count(), Some(16));
        assert_eq!(pattern.preview_plain(0.0), PATTERN_OFF);
        assert_eq!(pattern.preview_plain(1.0), PATTERN_MAX);

        // Every position is reachable from its normalized value and shows
        // its own text — `Off` at the bottom, the plain number above it.
        for value in PATTERN_OFF..=PATTERN_MAX {
            let normalized = pattern.preview_normalized(value);
            assert_eq!(pattern.preview_plain(normalized), value, "position {value} is not exact");

            let expected = if value == PATTERN_OFF { "Off".to_string() } else { value.to_string() };
            assert_eq!(pattern.normalized_value_to_string(normalized, true), expected);
        }

        // And the text goes back in, case and all, so a host that round-trips
        // its own display string lands where it started.
        for (string, expected) in [("Off", PATTERN_OFF), ("off", PATTERN_OFF), ("7", 7)] {
            let normalized =
                pattern.string_to_normalized_value(string).unwrap_or_else(|| panic!("{string}"));
            assert_eq!(pattern.preview_plain(normalized), expected);
        }
        assert_eq!(pattern.string_to_normalized_value("nonsense"), None);
    }

    /// What the engine asks for: the pattern as a number, or `None` at Off.
    #[test]
    fn pattern_reads_as_none_at_off_and_as_a_number_otherwise() {
        let params = StuttterParams::default();
        assert_eq!(params.pattern(), Some(1));

        for value in PATTERN_OFF..=PATTERN_MAX {
            set_pattern(&params, value);
            let expected = if value == PATTERN_OFF { None } else { Some(value as u8) };
            assert_eq!(params.pattern(), expected, "pattern {value}");
        }

        // Host data is untrusted: a plain value from outside the range — what
        // a hand-edited project file holds — saturates rather than wrapping
        // into some other pattern.
        set_pattern(&params, i32::MIN);
        assert_eq!(params.pattern(), None);
        set_pattern(&params, i32::MAX);
        assert_eq!(params.pattern(), Some(PATTERN_MAX as u8));

        assert_eq!(clamp_pattern(i32::MIN), PATTERN_OFF);
        assert_eq!(clamp_pattern(-1), PATTERN_OFF);
        assert_eq!(clamp_pattern(17), PATTERN_MAX);
        assert_eq!(clamp_pattern(i32::MAX), PATTERN_MAX);
    }

    /// `mix`: a continuous 0–100 %, fully wet by default, stored as the
    /// `0..=1` factor the engine multiplies by.
    #[test]
    fn mix_is_a_continuous_percentage_defaulting_to_fully_wet() {
        let params = test_params();
        let mix = &params.mix;

        assert_eq!(mix.name(), "Mix");
        assert_eq!(mix.default_plain_value(), MIX_DEFAULT);
        assert_eq!(mix.value(), 1.0);
        // Continuous, so the host draws a knob instead of stepping.
        assert_eq!(mix.step_count(), None);
        assert_eq!(mix.preview_plain(0.0), 0.0);
        assert_eq!(mix.preview_plain(1.0), 1.0);
        // Linear, so halfway along the control is halfway between dry and
        // wet — which is what "linear dry/wet" means to the ear here.
        assert_eq!(mix.preview_plain(0.5), 0.5);

        assert_eq!(mix.normalized_value_to_string(mix.default_normalized_value(), true), "100 %");
        assert_eq!(mix.normalized_value_to_string(0.0, true), "0 %");
        assert_eq!(mix.normalized_value_to_string(0.5, false), "50");

        let half = mix.string_to_normalized_value("50 %").expect("\"50 %\" is a percentage");
        assert_eq!(mix.preview_plain(half), 0.5);
    }

    /// `smooth`: 0.5–20 ms, 3 ms by default, shown in milliseconds.
    #[test]
    fn smoothing_runs_from_half_a_millisecond_to_twenty() {
        let params = test_params();
        let smooth = &params.smooth;

        assert_eq!(smooth.name(), "Smoothing");
        assert_eq!(smooth.default_plain_value(), SMOOTH_DEFAULT_MS);
        assert_eq!(smooth.value(), 3.0);
        assert_eq!(smooth.step_count(), None);
        assert_eq!(smooth.preview_plain(0.0), SMOOTH_MIN_MS);
        assert_eq!(smooth.preview_plain(1.0), SMOOTH_MAX_MS);

        assert_eq!(
            smooth.normalized_value_to_string(smooth.default_normalized_value(), true),
            "3.0 ms"
        );
        assert_eq!(smooth.normalized_value_to_string(0.0, true), "0.5 ms");
        assert_eq!(smooth.normalized_value_to_string(1.0, true), "20.0 ms");

        // The unit comes back off again, so the host's own display string
        // parses (nice-plug's default parser trims the unit for us).
        let ten = smooth.string_to_normalized_value("10 ms").expect("\"10 ms\" is a time");
        assert!((smooth.preview_plain(ten) - 10.0).abs() < 0.001);

        assert_eq!(params.smooth_ms(), 3.0);
    }

    /// Both float parameters are read back through clamps, because the plain
    /// value a parameter stores is not the normalized one a host drives and
    /// nothing else stands between a hostile project file and the maths.
    /// `max`-then-`min` is what makes a NaN usable — `f32::clamp` would hand
    /// the NaN straight back.
    #[test]
    fn hostile_mix_and_smoothing_values_are_clamped_nan_and_all() {
        assert_eq!(clamp_mix(f32::NAN), 0.0);
        assert_eq!(clamp_mix(f32::NEG_INFINITY), 0.0);
        assert_eq!(clamp_mix(f32::INFINITY), 1.0);
        assert_eq!(clamp_mix(-0.5), 0.0);
        assert_eq!(clamp_mix(2.0), 1.0);
        assert_eq!(clamp_mix(0.25), 0.25);

        assert_eq!(clamp_smooth_ms(f32::NAN), SMOOTH_MIN_MS);
        assert_eq!(clamp_smooth_ms(f32::NEG_INFINITY), SMOOTH_MIN_MS);
        assert_eq!(clamp_smooth_ms(f32::INFINITY), SMOOTH_MAX_MS);
        assert_eq!(clamp_smooth_ms(0.0), SMOOTH_MIN_MS);
        assert_eq!(clamp_smooth_ms(100.0), SMOOTH_MAX_MS);
        assert_eq!(clamp_smooth_ms(3.0), 3.0);
    }

    /// The mix a block is handed: the smoother advanced by the block's own
    /// length, and an empty block advancing nothing.
    #[test]
    fn the_block_mix_follows_the_smoother() {
        let params = test_params();

        // Primed at the default, so the first block is fully wet with no ramp
        // up from zero — which is what the host's `activate()` arranges.
        assert_eq!(params.next_mix(64), 1.0);
        assert_eq!(params.next_mix(0), 1.0);

        // A move to dry takes MIX_SMOOTHING_MS, so a short block lands part
        // of the way and a long one arrives — exactly, or `mix` at 0 would
        // not be the input bit for bit.
        params.mix.smoothed.set_target(RATE, 0.0);
        let partway = params.next_mix(64);
        assert!(partway < 1.0 && partway > 0.0, "{partway}");
        assert_eq!(params.next_mix(0), partway, "an empty block advanced the smoother");
        assert_eq!(params.next_mix((RATE as usize) / 10), 0.0);
        assert!(!params.mix.smoothed.is_smoothing());
    }

    /// The sample rate the parameter tests smooth at. No host, so nothing
    /// else fixes it.
    const RATE: f32 = 48_000.0;

    /// Parameters as the host hands them their first block: defaults, with
    /// the smoothers primed.
    fn test_params() -> StuttterParams {
        let params = StuttterParams::default();
        prime_smoothers(&params);
        params
    }

    /// What the host does before the first block and nothing else does: a
    /// smoother built by `with_smoother` starts at 0.0, and nice-plug's
    /// wrapper resets every one of them to its parameter's value in
    /// `activate()` (`wrapper/clap/wrapper.rs`). Without it a test would be
    /// reading a mix no host ever has.
    pub(super) fn prime_smoothers(params: &StuttterParams) {
        params.mix.smoothed.reset(params.mix.value());
    }

    /// Writes a plain value into `pattern` the way the wrapper does when the
    /// host sends a parameter change or restores a project. nice-plug keeps
    /// the setters `unsafe` to mark them as its own business rather than for
    /// any memory-safety reason, which is also why the accessors under test
    /// cannot assume the value is in range.
    pub(super) fn set_pattern(params: &StuttterParams, value: i32) {
        use nice_plug::params::InternalParamMut;

        // SAFETY: not a memory-safety contract — the method is `unsafe` to
        // keep plugins from setting parameters behind the host's back, which
        // is exactly what a test with no host has to do.
        unsafe {
            params.pattern._internal_set_plain_value(value);
        }
    }

    // ---- the `#[persist = "state"]` field ---------------------------------

    /// Edits an instance's patterns into something no default could produce,
    /// through the writer API and nothing else — which is also what makes
    /// this a headless edit: there is no editor anywhere in it.
    ///
    /// Every pattern gets a rate and a length of its own, a Volume row that
    /// is not flat *past its length as well as inside it* (so a save that
    /// dropped the hidden half of the row would fail the round trip), and two
    /// blocks of the two kinds — one of them a hand-drawn ramp, which is the
    /// case a stored preset label must not be invented for.
    fn fill_with_a_project(model: &StutterModel) {
        for index in 0..PATTERN_COUNT {
            let rate = Rate::ALL[index % Rate::ALL.len()];
            let length = 8 + index as u16;
            let preset = SpeedPreset::ALL[index % SpeedPreset::ALL.len()];

            assert!(model.set_rate(index, rate));
            assert_eq!(model.set_length(index, length), Some(length));
            let row = (0..MAX_STEPS).map(|step| ((step + index) % 5) as f32 / 4.0);
            assert!(model.set_volumes(index, row));
            assert_eq!(model.add_block(index, 0, 2, Action::repeat_at(rate)), Some(0));
            assert_eq!(model.add_block(index, 4, 3, Action::from_preset(preset)), Some(1));
        }

        // One ramp nobody named, in a pattern that is not the first.
        assert!(model.set_block_action(3, 1, Action::ramp(0.3, -0.7)));
    }

    /// Prefs that are all unlike the defaults, so a load that kept any of
    /// them at its default fails rather than passing by accident.
    fn edited_prefs() -> Prefs {
        Prefs {
            keep_on_top: false,
            last_action: Action::from_preset(SpeedPreset::Reverse),
            notes: Some(notes::SUGGESTED_START),
            latch: true,
        }
    }

    /// `map` reads what the instance holds *now*, not a copy taken when the
    /// field was built: the host asking for state twice around an edit must
    /// get two different blobs.
    #[test]
    fn map_reads_the_live_patterns_and_prefs() {
        let params = StuttterParams::default();

        let before = PersistentField::map(&params.state, Snapshot::clone);
        assert_eq!(before, Snapshot::default(), "a fresh instance saves the defaults");
        assert_eq!(before.version, state::SNAPSHOT_VERSION);
        assert_eq!(before.patterns.len(), PATTERN_COUNT, "all sixteen are saved");

        fill_with_a_project(params.state.model());
        params.state.prefs().set(edited_prefs());

        let after = PersistentField::map(&params.state, Snapshot::clone);
        assert_ne!(after, before);
        assert_eq!(after.data(), params.state.model().data());
        assert_eq!(after.prefs(), edited_prefs());
        assert!(!after.keep_on_top, "the switch is saved state, not a parameter");
    }

    /// `set` is a write on the instance's own model, so what the audio thread
    /// reads is the loaded patterns — the point of the field wrapping the live
    /// model rather than owning a copy. The prefs land with them, and the undo
    /// stacks are dropped: opening a project is not an edit.
    #[test]
    fn set_publishes_onto_the_shared_model_and_the_prefs() {
        let saved = {
            let source = StutterModel::default();
            fill_with_a_project(&source);
            Snapshot::of(&source.data(), edited_prefs())
        };

        let params = StuttterParams::default();
        let model = params.state.model();
        assert_eq!(model.set_length(0, 32), Some(32));
        model.commit_edits();
        assert!(model.set_rate(1, Rate::ALL[1]));
        model.commit_edits();
        assert!(model.can_undo(), "two committed gestures are two undo steps");

        PersistentField::set(&params.state, saved.clone());

        // `shared()` is what `process()` reads, and it is this instance's own
        // model — not a copy the field kept to itself.
        assert_eq!(*model.shared(), saved.data());
        assert_eq!(params.state.prefs().get(), saved.prefs());
        assert_eq!(PersistentField::map(&params.state, Snapshot::clone), saved);
        assert!(!model.can_undo(), "opening a project is not an edit to step out of");
        assert!(!model.can_redo());
    }

    /// The host's round trip end to end: `serialize_fields` is the blob a
    /// project file stores under `state`, and `deserialize_fields` on a fresh
    /// instance rebuilds all sixteen patterns and both prefs. The
    /// derive-generated path — `serde_json` either side of the trait methods —
    /// not the trait methods called by hand.
    #[test]
    fn the_state_field_round_trips_through_the_project_file() {
        let saved_params = StuttterParams::default();
        fill_with_a_project(saved_params.state.model());
        saved_params.state.prefs().set(edited_prefs());
        let saved = saved_params.state.model().data();

        let stored = saved_params.serialize_fields();
        assert_eq!(
            stored.keys().collect::<Vec<&String>>(),
            vec!["state"],
            "one blob, under the key every saved project refers to"
        );
        // Named JSON fields, not an opaque encoding: this is what Bitwig
        // writes, so a future build has to be able to read it field by field.
        let blob = &stored["state"];
        assert_eq!(
            serde_json::from_str::<Snapshot>(blob).expect("the stored blob is JSON"),
            Snapshot::of(&saved, edited_prefs())
        );
        assert!(blob.contains(r#""keep_on_top":false"#), "{blob}");
        assert!(blob.contains(r#""looping":null"#), "{blob}");

        let loaded_params = StuttterParams::default();
        assert_ne!(loaded_params.state.model().data(), saved, "the defaults are not the project");
        loaded_params.deserialize_fields(&stored);

        let loaded = loaded_params.state.model().data();
        for (index, (loaded, expected)) in
            loaded.patterns().iter().zip(saved.patterns()).enumerate()
        {
            assert_eq!(loaded, expected, "pattern {index} came back changed");
        }
        assert_eq!(loaded, saved);
        assert_eq!(loaded_params.state.prefs().get(), edited_prefs());
    }

    /// The keep-on-top switch end to end, as far as this file goes: the toggle
    /// on the bar is the only thing that writes the pref
    /// ([`crate::editor::top_bar::toggle_keep_on_top`]), the blob the host
    /// stores carries it, and a fresh instance handed that blob opens with the
    /// switch the user left it at.
    ///
    /// Both values, and each of them loaded over an instance holding the
    /// other: the default is `true`, so a load that quietly did nothing at
    /// all would pass half of this on its own.
    #[test]
    fn the_keep_on_top_switch_round_trips_through_the_project_file() {
        use crate::editor::top_bar::toggle_keep_on_top;

        let params = StuttterParams::default();
        assert!(params.state.prefs().get().keep_on_top, "a new instance opens pinned");

        // Turned off by a click on the bar, saved, and reopened off.
        assert!(!toggle_keep_on_top(params.state.prefs()), "the click turns it off");
        let stored = params.serialize_fields();
        assert!(stored["state"].contains(r#""keep_on_top":false"#), "{}", stored["state"]);

        let reopened = StuttterParams::default();
        reopened.deserialize_fields(&stored);
        assert!(!reopened.state.prefs().get().keep_on_top, "the project's switch came back off");
        assert_eq!(
            reopened.state.prefs().get().last_action,
            Prefs::default().last_action,
            "the pref beside it is untouched"
        );

        // And turned back on, loaded over an instance whose switch is off:
        // the value that is also the default still has to be carried.
        assert!(toggle_keep_on_top(reopened.state.prefs()), "the next click turns it on");
        let stored = reopened.serialize_fields();
        assert!(stored["state"].contains(r#""keep_on_top":true"#), "{}", stored["state"]);

        let again = StuttterParams::default();
        assert!(!toggle_keep_on_top(again.state.prefs()), "so the load has something to move");
        again.deserialize_fields(&stored);
        assert!(again.state.prefs().get().keep_on_top, "the project's switch came back on");
    }

    /// A project that never stored a `state` key — one saved by a build
    /// before this field existed, or a host handing back parameters only —
    /// leaves the patterns alone rather than wiping them to the defaults.
    #[test]
    fn a_project_without_the_state_key_keeps_the_current_state() {
        let params = StuttterParams::default();
        fill_with_a_project(params.state.model());
        params.state.prefs().set(edited_prefs());
        let before = params.state.model().data();

        params.deserialize_fields(&BTreeMap::from([(
            String::from("something_else"),
            String::from("\"ignored\""),
        )]));

        assert_eq!(params.state.model().data(), before);
        assert_eq!(params.state.prefs().get(), edited_prefs());
    }

    /// Garbage under the `state` key never reaches the model: the framework
    /// parses before it calls `set`, so a blob that is not a snapshot is
    /// dropped and the patterns in the instance stand.
    ///
    /// Driven through `deserialize_field` — the exact function the `Params`
    /// derive calls — rather than through `deserialize_fields`, because the
    /// generated code answers a failed parse with `nice_debug_assert_failure!`,
    /// which is a hard panic in `cfg(test)` builds and a logged warning
    /// everywhere else. The branch under test is the one before that.
    #[test]
    fn garbage_under_the_state_key_never_reaches_the_model() {
        use nice_plug::params::persist::deserialize_field;

        let params = StuttterParams::default();
        fill_with_a_project(params.state.model());
        let before = params.state.model().data();

        for blob in ["", "not json at all", "{", "[1, 2, 3]", "\"state\"", "null"] {
            assert!(
                deserialize_field::<Snapshot>(blob).is_err(),
                "{blob:?} must not parse as a snapshot"
            );
            // Nothing was handed to `set`, so nothing moved.
            assert_eq!(params.state.model().data(), before);
            assert_eq!(params.state.prefs().get(), Prefs::default());
        }
    }

    /// JSON that *does* parse is loaded, however little of it there is or
    /// however hostile: `{}` is sixteen fresh patterns, and a file of
    /// impossible numbers opens clamped instead of panicking inside Bitwig.
    /// The rules themselves are `state.rs`'s; what this pins is that the
    /// persist path runs them.
    #[test]
    fn hostile_but_parseable_state_loads_clamped() {
        let params = StuttterParams::default();
        fill_with_a_project(params.state.model());
        params.state.prefs().set(edited_prefs());

        params.deserialize_fields(&BTreeMap::from([(
            String::from("state"),
            String::from("{}"),
        )]));
        assert_eq!(params.state.model().data(), StutterData::new(), "every field defaulted");
        assert_eq!(params.state.prefs().get(), Prefs::default());

        let pattern = r#"{"rate": 200, "length": 60000, "volumes": [1e40, -1e40, 0.5],
            "blocks": [{"start": 900, "len": 9000, "action": {"kind": "freeze"}},
                       {"start": 2, "len": 4, "action": {"kind": "speed",
                        "from": 1e40, "to": -1e40, "preset": 99}},
                       {"start": 3, "len": 4, "action": {"kind": "repeat", "slice": 99}}]}"#;
        let patterns: Vec<&str> = (0..100).map(|_| pattern).collect();
        let hostile = format!(
            r#"{{"version": 9999, "keep_on_top": false, "patterns": [{}]}}"#,
            patterns.join(",")
        );
        params.deserialize_fields(&BTreeMap::from([(String::from("state"), hostile)]));

        let loaded = params.state.model().data();
        assert_eq!(loaded.patterns().len(), PATTERN_COUNT, "a hundred patterns became sixteen");
        for (index, pattern) in loaded.patterns().iter().enumerate() {
            assert!(pattern.length() <= Pattern::MAX_LENGTH, "pattern {index} is too long");
            assert!(pattern.volumes().iter().all(|&level| (0.0..=1.0).contains(&level)));
            for block in pattern.blocks() {
                assert!(block.start() + block.len() <= pattern.length(), "pattern {index}");
                let (from, to) = block.action().speeds();
                assert!(from.abs() <= MAX_SPEED && to.abs() <= MAX_SPEED);
            }
        }
        assert!(!params.state.prefs().get().keep_on_top, "a stored switch is still read");
        // Re-saving the clamped state writes this build's version, so the
        // next load is no longer a file from the future.
        assert_eq!(
            PersistentField::map(&params.state, |snapshot| snapshot.version),
            state::SNAPSHOT_VERSION
        );
    }

    /// A [`RemoteControlsContext`] that records what the plugin declares
    /// instead of building CLAP pages out of it.
    ///
    /// nice-plug's own implementation lives inside the CLAP wrapper and needs
    /// a live host to reach, but the traits are public and the plugin only
    /// ever talks to them — so the declaration is checked by driving it with
    /// this. Each declared page is kept paired with the name of the section
    /// it went into.
    #[derive(Default)]
    struct RecordedPages {
        /// Each page with the name of the section it was declared in.
        pages: Vec<(String, RecordedPage)>,
    }

    /// The section being filled. It owns its pages and `add_section` moves
    /// them out once the closure has run — the associated type has no
    /// lifetime to borrow the recorder with, which is how nice-plug's own
    /// section is built too.
    #[derive(Default)]
    struct RecordedSection {
        pages: Vec<RecordedPage>,
    }

    /// The page being filled. A slot is the parameter's name and identity,
    /// or `None` for a spacer.
    struct RecordedPage {
        name: String,
        params: Vec<Option<(String, ParamPtr)>>,
    }

    impl RemoteControlsContext for RecordedPages {
        type Section = RecordedSection;

        fn add_section(&mut self, name: impl Into<String>, f: impl FnOnce(&mut Self::Section)) {
            let name = name.into();
            let mut section = RecordedSection::default();
            f(&mut section);

            self.pages.extend(section.pages.into_iter().map(|page| (name.clone(), page)));
        }
    }

    impl RemoteControlsSection for RecordedSection {
        type Page = RecordedPage;

        fn add_page(&mut self, name: impl Into<String>, f: impl FnOnce(&mut Self::Page)) {
            let mut page = RecordedPage { name: name.into(), params: Vec::new() };
            f(&mut page);
            self.pages.push(page);
        }
    }

    impl RemoteControlsPage for RecordedPage {
        fn add_param(&mut self, param: &impl Param) {
            self.params.push(Some((param.name().to_string(), param.as_ptr())));
        }

        fn add_spacer(&mut self) {
            self.params.push(None);
        }
    }

    /// The page a controller's knobs land on: one section, one page, the three
    /// parameters in their fixed order. Identities and not just names, because
    /// a page that named the right things but pointed at copies would map
    /// nothing.
    #[test]
    fn the_remote_controls_are_one_main_page_of_the_three_parameters() {
        let plugin = Stuttter::default();
        let mut recorded = RecordedPages::default();

        plugin.remote_controls(&mut recorded);

        assert_eq!(recorded.pages.len(), 1, "one section, one page");
        let (section, page) = &recorded.pages[0];
        assert_eq!(section, "stuttter");
        assert_eq!(page.name, "Main");
        assert_eq!(
            page.params,
            &[
                Some(("Pattern".to_string(), plugin.params.pattern.as_ptr())),
                Some(("Mix".to_string(), plugin.params.mix.as_ptr())),
                Some(("Smoothing".to_string(), plugin.params.smooth.as_ptr())),
            ]
        );
    }

    /// Three parameters, so nice-plug never splits the page — it only does
    /// that past eight — and a host that maps the device gets one page
    /// rather than "Main 1" and "Main 2".
    #[test]
    fn the_page_stays_well_inside_the_eight_slots_clap_allows() {
        let mut recorded = RecordedPages::default();

        Stuttter::default().remote_controls(&mut recorded);

        assert!(recorded.pages[0].1.params.len() <= 8, "a CLAP page holds eight parameters");
    }

    /// Everything a host finds inside `target/bundled/stuttter.clap` reaches
    /// it through the `clap_entry` symbol that `nice_export_clap!` generates,
    /// so the bundle's contract is checked here rather than by inspecting the
    /// built `.clap` file: entry point present, plugin factory reachable under
    /// the CLAP factory ID, and exactly one plugin described in it.
    ///
    /// Deliberately stops short of `create_plugin()` — that needs a real
    /// `clap_host`, which is what `clap-validator validate` on the bundle is
    /// for.
    #[test]
    fn clap_entry_exposes_one_stereo_audio_effect() {
        use nice_plug::wrapper::clap::{CLAP_PLUGIN_FACTORY_ID, clap_plugin_factory};
        use std::ffi::CStr;

        assert_eq!(clap_entry.clap_version.major, 1);

        let get_factory = clap_entry.get_factory.expect("no `get_factory` in `clap_entry`");
        let factory =
            unsafe { get_factory(CLAP_PLUGIN_FACTORY_ID.as_ptr()) } as *const clap_plugin_factory;
        assert!(!factory.is_null(), "plugin factory not exposed");

        // SAFETY: `factory` is the macro's own `static CLAP_PLUGIN_FACTORY`,
        // and its function pointers take that same pointer back.
        unsafe {
            let count = (*factory).get_plugin_count.expect("no `get_plugin_count`")(factory);
            assert_eq!(count, 1, "the bundle must expose exactly one plugin");

            let descriptor =
                (*factory).get_plugin_descriptor.expect("no `get_plugin_descriptor`")(factory, 0);
            assert!(!descriptor.is_null());
            assert_eq!(CStr::from_ptr((*descriptor).id).to_str().unwrap(), Stuttter::CLAP_ID);
            assert_eq!(CStr::from_ptr((*descriptor).name).to_str().unwrap(), Stuttter::NAME);

            // The feature list is an environ-style null-terminated array; an
            // effect that does not advertise `audio-effect` is in no
            // category at all, and Bitwig would not show it among the audio
            // FX.
            let mut features = Vec::new();
            let mut ptr = (*descriptor).features;
            while !(*ptr).is_null() {
                features.push(CStr::from_ptr(*ptr).to_str().unwrap());
                ptr = ptr.add(1);
            }
            assert_eq!(features, vec!["audio-effect", "glitch", "stereo"]);
        }
    }

    /// A stand-in for the host, so `process()` can be driven with no Bitwig
    /// and no audio device (chopdrop's `TestHost`, with its note queues kept:
    /// `ProcessContext` demands the two methods whatever `MIDI_INPUT` says,
    /// and a shell that declares no note ports should be seen to send
    /// nothing).
    pub(super) struct TestHost {
        transport: Transport,
        incoming: VecDeque<PluginNoteEvent<Stuttter>>,
        pub(super) sent: Vec<PluginNoteEvent<Stuttter>>,
    }

    impl TestHost {
        /// A stopped host at 48 kHz with nothing queued for the block.
        fn stopped() -> TestHost {
            TestHost::with_transport(Transport::new(48_000.0))
        }

        /// A host reporting `transport`, with nothing queued for the block —
        /// how the wiring tests put the song position under one block.
        pub(super) fn with_transport(transport: Transport) -> TestHost {
            TestHost { transport, incoming: VecDeque::new(), sent: Vec::new() }
        }

        /// Queues `event` for the next block, after whatever is queued.
        pub(super) fn queue(&mut self, event: PluginNoteEvent<Stuttter>) {
            self.incoming.push_back(event);
        }
    }

    impl ProcessContext<Stuttter> for TestHost {
        fn plugin_api(&self) -> PluginApi {
            PluginApi::Clap
        }

        // `BackgroundTask = ()`, so there is nothing to run and nothing to
        // record: a task that carries no information cannot be asserted on.
        fn execute_background(&self, _task: ()) {}
        fn execute_gui(&self, _task: ()) {}

        fn transport(&self) -> &Transport {
            &self.transport
        }

        fn next_event(&mut self) -> Option<PluginNoteEvent<Stuttter>> {
            self.incoming.pop_front()
        }

        fn try_send_event(
            &mut self,
            event: PluginNoteEvent<Stuttter>,
        ) -> Result<(), (PluginNoteEvent<Stuttter>, SendEventError)> {
            self.sent.push(event);
            Ok(())
        }

        // Three the shell has no business touching. `unreachable!` rather
        // than a no-op so that the day one of them starts being called, a
        // test says so instead of the call vanishing: stuttter reads only
        // the past and so never reports latency, an effect asking the host to
        // restart it would be a bug, and voice capacity may only be set with
        // `CLAP_POLY_MODULATION_CONFIG`, which this plugin does not set.
        fn set_latency_samples(&self, _samples: u32) {
            unreachable!("stuttter reports no latency");
        }

        fn request_restart(&self) {
            unreachable!("stuttter never asks the host to restart it");
        }

        fn set_current_voice_capacity(&self, _capacity: u32) {
            unreachable!("stuttter sets no polyphonic modulation config");
        }
    }

    /// The other half of a host: what `activate()` is handed. Same three
    /// refusals as the process half — an effect that reads only the past
    /// reports no latency, and voice capacity needs a poly-modulation config
    /// this plugin does not set — and `execute` has nothing to run, because
    /// `BackgroundTask = ()`.
    impl ActivateContext<Stuttter> for TestHost {
        fn plugin_api(&self) -> PluginApi {
            PluginApi::Clap
        }

        fn execute(&self, _task: ()) {}

        fn set_latency_samples(&self, _samples: u32) {
            unreachable!("stuttter reports no latency");
        }

        fn set_current_voice_capacity(&self, _capacity: u32) {
            unreachable!("stuttter sets no polyphonic modulation config");
        }
    }

    /// Takes `plugin` through the lifecycle a host does before the first
    /// block, at `rate`: `activate()`, then the `reset()` nice-plug always
    /// calls after it, then the smoother priming its wrapper does.
    ///
    /// Everything downstream of this is the real plugin: the engine it holds
    /// afterwards is the one `process()` drives, sized from the rate given
    /// here.
    pub(super) fn activate_at(plugin: &mut Stuttter, rate: f32) {
        let config = BufferConfig {
            sample_rate: rate,
            min_buffer_size: None,
            // The largest block the tests below hand over. Nothing in this
            // plugin is sized from it — the engine walks whatever length it
            // is given — but a host always reports one.
            max_buffer_size: 1024,
            process_mode: ProcessMode::Realtime,
        };
        let mut host = TestHost::stopped();

        assert!(
            plugin.activate(&Stuttter::AUDIO_IO_LAYOUTS[0], &config, &mut host),
            "the plugin refused the stereo layout at {rate} Hz"
        );
        plugin.reset();
        prime_smoothers(&plugin.params);
    }

    /// Runs one block through `process()` over the two channels given, and
    /// hands back the status, the channels as `process()` left them, and the
    /// host — which is a stopped one, so this is the "nothing is playing"
    /// block.
    ///
    /// The channels go in holding the input, which is where nice-plug's
    /// in-place wrapper leaves it before the call.
    fn render_block(
        plugin: &mut Stuttter,
        channels: &mut [Vec<f32>; 2],
    ) -> (ProcessStatus, TestHost) {
        let mut host = TestHost::stopped();
        let status = render_block_on(plugin, channels, &mut host);

        (status, host)
    }

    /// The same block against a host the caller built — how the wiring tests
    /// move the song position from block to block, and keep one plugin
    /// instance across the whole run.
    pub(super) fn render_block_on(
        plugin: &mut Stuttter,
        channels: &mut [Vec<f32>; 2],
        host: &mut TestHost,
    ) -> ProcessStatus {
        let frames = channels[0].len();
        assert_eq!(channels[1].len(), frames, "both channels are the same length");

        let mut buffer = Buffer::default();
        // SAFETY: `channels` outlives `buffer`, which is dropped at the end
        // of this scope, and both slices are `frames` long as `set_slices`
        // requires.
        unsafe {
            buffer.set_slices(frames, |slices| {
                let (left, right) = channels.split_at_mut(1);
                *slices = vec![&mut left[0], &mut right[0]];
            });
        }

        let mut aux = AuxiliaryBuffers { inputs: &mut [], outputs: &mut [] };
        let status = plugin.process(&mut buffer, &mut aux, host);
        drop(buffer);

        status
    }

    /// A stopped transport is live audio: the block the host handed over comes
    /// back bit for bit, in both channels, through the whole engine — recorded,
    /// read at `d = 0`, levelled at unity and mixed fully wet. A `process()`
    /// that silenced the buffer — which is what an instrument's shell does —
    /// would mute the track it is dropped on, and one that let the interpolator
    /// touch the dry path would not come back bit for bit.
    #[test]
    fn process_passes_the_block_through_untouched() {
        let mut plugin = Stuttter::default();
        activate_at(&mut plugin, 48_000.0);
        let left: Vec<f32> = (0..512).map(|n| n as f32 / 512.0).collect();
        let right: Vec<f32> = left.iter().map(|sample| -sample).collect();
        let mut channels = [left.clone(), right.clone()];

        let (status, host) = render_block(&mut plugin, &mut channels);

        assert!(matches!(status, ProcessStatus::KeepAlive), "{status:?}");
        assert_eq!(channels[0], left, "the left channel came back changed");
        assert_eq!(channels[1], right, "the right channel came back changed");
        assert!(host.sent.is_empty(), "an effect with no note output sent an event");
    }

    /// `KeepAlive` always: an effect that must record every block, silence
    /// included, cannot let the host suspend it while nothing is audible.
    /// Empty and odd-sized blocks are both legal, and neither may panic.
    #[test]
    fn process_keeps_the_plugin_alive_through_any_block_size() {
        let mut plugin = Stuttter::default();
        activate_at(&mut plugin, 48_000.0);

        for frames in [0usize, 1, 3, 64, 513] {
            let mut channels = [vec![0.25; frames], vec![-0.25; frames]];
            let (status, _host) = render_block(&mut plugin, &mut channels);
            assert!(matches!(status, ProcessStatus::KeepAlive), "{frames} frames: {status:?}");
            assert!(channels[0].iter().all(|&sample| sample == 0.25));
            assert!(channels[1].iter().all(|&sample| sample == -0.25));
        }
    }

    /// The block length the transport tests read a context at.
    const BLOCK: usize = 512;

    /// The transport a host reports while playing at `pos_beats`, with
    /// everything the engine needs present: 120 BPM at 48 kHz, where one
    /// beat is exactly 24 000 frames.
    fn playing_at(pos_beats: f64) -> Transport {
        let mut transport = Transport::new(RATE);
        transport.playing = true;
        transport.tempo = Some(120.0);
        transport.pos_beats = Some(pos_beats);
        transport
    }

    /// A playing host with a position and a tempo maps straight onto a
    /// playing block, carrying the pattern the parameter asked for.
    #[test]
    fn a_playing_transport_maps_onto_a_playing_block() {
        let ctx = block_ctx(&playing_at(8.5), BLOCK, Some(3));

        assert_eq!(ctx, BlockCtx::new(8.5, 120.0, RATE, BLOCK).with_pattern(Some(3)));
        assert!(ctx.is_playing());
        assert_eq!(ctx.start_beat(), 8.5);
        assert_eq!(ctx.tempo(), 120.0);
        assert_eq!(ctx.num_samples(), BLOCK);
        assert_eq!(ctx.pattern_index(), Some(2));
    }

    /// A transport sitting still keeps the position it reports — the engine
    /// plays live over it, and an open editor can still show where playback
    /// broke off — but it does not count as playing.
    #[test]
    fn a_stopped_transport_keeps_the_position_it_reports() {
        let mut transport = playing_at(16.0);
        transport.playing = false;

        let ctx = block_ctx(&transport, BLOCK, Some(1));

        assert!(!ctx.is_playing());
        assert_eq!(ctx.start_beat(), 16.0);
        assert_eq!(ctx.tempo(), 120.0);
        assert_eq!(ctx.num_samples(), BLOCK);
    }

    /// The missing-transport rule: a host reporting no position cannot have
    /// the grid laid over it, so the block reads as stopped however loudly
    /// the transport claims to be playing. Same for a missing tempo — without
    /// one there is no telling how long the block is in beats. Either way the
    /// block keeps the length and the rate it must still record at, and the
    /// pattern selection arrives all the same.
    #[test]
    fn missing_transport_data_reads_as_stopped() {
        let no_position = Transport { pos_beats: None, ..playing_at(4.0) };
        let no_tempo = Transport { tempo: None, ..playing_at(4.0) };

        for transport in [no_position, no_tempo] {
            let ctx = block_ctx(&transport, BLOCK, Some(5));

            assert!(!ctx.is_playing(), "{transport:?} must read as stopped");
            assert_eq!(ctx, BlockCtx::stopped(RATE, BLOCK).with_pattern(Some(5)));
            assert_eq!(ctx.num_samples(), BLOCK, "a stopped block still has its frames");
            assert_eq!(ctx.sample_rate(), f64::from(RATE));
            assert_eq!(ctx.beat_at(BLOCK), 0.0, "nothing advances while stopped");
            assert_eq!(ctx.pattern_index(), Some(4));
        }
    }

    /// Pattern Off rides through as Off, on a playing block and a stopped
    /// one alike: there is no pattern to index, and the engine's answer to
    /// that is the dry input.
    #[test]
    fn pattern_off_reaches_the_block_as_no_pattern() {
        let playing = block_ctx(&playing_at(1.0), BLOCK, None);
        let no_transport = block_ctx(&Transport::new(RATE), BLOCK, None);

        for ctx in [playing, no_transport] {
            assert_eq!(ctx.pattern(), None);
            assert_eq!(ctx.pattern_index(), None);
        }

        // And the parameter is where that `None` comes from.
        let params = test_params();
        set_pattern(&params, PATTERN_OFF);
        assert_eq!(block_ctx(&playing_at(1.0), BLOCK, params.pattern()).pattern(), None);
        set_pattern(&params, 7);
        assert_eq!(block_ctx(&playing_at(1.0), BLOCK, params.pattern()).pattern(), Some(7));
    }

    /// CLAP hosts may report the position in samples or seconds instead of
    /// beats, so the reading goes through `Transport`'s accessor rather than
    /// its raw field — a plugin that read `transport.pos_beats` directly
    /// would sit silent under such a host.
    #[test]
    fn a_position_reported_in_samples_is_still_a_position() {
        // 2 beats at 120 BPM is one second, which is 48 000 frames.
        let mut in_samples = playing_at(0.0);
        in_samples.pos_beats = None;
        in_samples.pos_samples = Some(48_000);

        let ctx = block_ctx(&in_samples, BLOCK, Some(1));
        assert!(ctx.is_playing());
        assert_eq!(ctx.start_beat(), 2.0);

        // And in seconds the same way: half a second is one beat.
        let mut in_seconds = playing_at(0.0);
        in_seconds.pos_beats = None;
        in_seconds.pos_seconds = Some(0.5);

        assert_eq!(block_ctx(&in_seconds, BLOCK, Some(1)).start_beat(), 1.0);
    }

    /// The beat of a sample inside the block, which is the one number the
    /// rest of the engine is driven from: the position in beats is honoured
    /// and the tempo carries it forward frame by frame.
    #[test]
    fn the_blocks_beats_run_from_the_reported_position() {
        let ctx = block_ctx(&playing_at(8.5), BLOCK, Some(1));

        assert_eq!(ctx.beat_at(0), 8.5);
        assert_eq!(ctx.beat_at(12_000), 9.0);
        assert_eq!(ctx.beat_at(24_000), 9.5);
        assert_eq!(ctx.end_beat(), 8.5 + BLOCK as f64 * ctx.beats_per_sample());
    }
}

/// The shell driving the real engine: `process()` over several blocks of one
/// plugin instance, with the song moving underneath it.
///
/// The **ramp trick**, run through the host's own entry point rather than
/// through [`engine::Engine::run_block`]: the input frame at absolute position
/// `n` holds the value `n`, so every output sample *is* the position the plugin
/// read at, and what the whole chain did — the parameters, the transport, the
/// published patterns, the history buffer, the read heads and the mix — can be
/// asserted frame by frame with `assert_eq!` on `f32`. Nothing here is a
/// stand-in for part of the plugin: the only stand-in is the host.
///
/// A module of its own, like `engine.rs`'s two, because the rate and tempo
/// here are chosen to make the arithmetic exact rather than to look like a
/// session's (see [`RATE`]), where the tests above are written at the 48 kHz
/// a host actually reports.
#[cfg(test)]
mod wiring_tests {
    use super::tests::{TestHost, activate_at, prime_smoothers, render_block_on, set_pattern};
    use super::*;

    use crate::actions::SpeedPreset;
    use crate::pattern::{Action, Rate};
    use crate::scope::{COLUMNS, Column, FIRST_GENERATION};

    use nice_plug::midi::{Channel, Key, VoiceID};

    /// 32 768 Hz — a rate no host offers, chosen because it makes the
    /// arithmetic exact rather than nearly so (the same fixture rate
    /// `engine.rs`'s driver tests use, and for the same reason): at
    /// [`TEMPO`] one beat is exactly 16 384 frames, so every beat the
    /// transport reports is a dyadic rational, every offset derived from one
    /// is exact, and every read position lands on a whole frame — where the
    /// interpolator returns the recorded sample itself and the ramp trick
    /// reads back exactly.
    const RATE: f32 = 32_768.0;
    /// 120 BPM — see [`RATE`].
    const TEMPO: f64 = 120.0;
    /// Frames in one beat at [`RATE`] and [`TEMPO`].
    const BEAT: usize = 16_384;
    /// Frames in one step of the fixture pattern: 1/16 at [`RATE`] and
    /// [`TEMPO`], so a quarter of a beat.
    const STEP: usize = BEAT / 4;
    /// Steps in the fixture pattern: eight, so it spans two beats.
    const STEPS: u16 = 8;
    /// Frames in one cycle of the fixture pattern.
    const CYCLE: usize = STEPS as usize * STEP;
    /// The block length the runs below hand the plugin: 512 frames, which
    /// divides neither a step nor a beat into one block — so every assertion
    /// below is also an assertion that the walk reads straight through a
    /// block seam.
    const BLOCK: usize = 512;
    /// Frames to leave unasserted after a read-position jump: the Smoothing
    /// parameter's default 3 ms is 99 frames at [`RATE`], and inside that
    /// window the output is two positions mixed by design.
    const FADED: usize = 128;

    /// The fixture, published onto the instance's **pattern 1**: [`STEPS`]
    /// steps of 1/16 with one block of `action` over `steps`.
    ///
    /// Written through the model's writer API and nothing else, which is both
    /// the only way anything reaches `process()` and a headless edit — there is
    /// no editor anywhere in it.
    fn publish(plugin: &Stuttter, action: Action, steps: std::ops::Range<u16>) {
        let model = plugin.params.state.model();

        assert!(model.set_rate(0, Rate::Sixteenth));
        assert_eq!(model.set_length(0, STEPS), Some(STEPS));
        assert_eq!(model.add_block(0, steps.start, steps.end - steps.start, action), Some(0));
    }

    /// The transport a host reports while playing, with the song position at
    /// absolute frame `frame` of the run — which is also the frame the input
    /// ramp's value is taken from, so "the beat" and "the recorded past" are
    /// the same clock.
    fn playing_at(frame: usize) -> Transport {
        let mut transport = Transport::new(RATE);
        transport.playing = true;
        transport.tempo = Some(TEMPO);
        transport.pos_beats = Some(frame as f64 / BEAT as f64);

        transport
    }

    /// A transport that says nothing: no position, no tempo, not playing —
    /// a block that counts as stopped.
    fn stopped() -> Transport {
        Transport::new(RATE)
    }

    /// Runs the absolute frames `frames` of the ramp input through
    /// `plugin`'s own `process()`, in [`BLOCK`]-sized blocks, with the
    /// transport `transport_at` reports for the block's first frame.
    ///
    /// Returns the two output channels. The right channel carries `−n`, so a
    /// channel copied or swapped over the other shows up.
    fn run(
        plugin: &mut Stuttter,
        frames: std::ops::Range<usize>,
        transport_at: impl Fn(usize) -> Transport,
    ) -> (Vec<f32>, Vec<f32>) {
        let mut left = Vec::with_capacity(frames.len());
        let mut right = Vec::with_capacity(frames.len());

        let mut start = frames.start;
        while start < frames.end {
            let block = BLOCK.min(frames.end - start);
            let mut channels = [
                (start..start + block).map(|n| n as f32).collect::<Vec<f32>>(),
                (start..start + block).map(|n| -(n as f32)).collect::<Vec<f32>>(),
            ];
            let mut host = TestHost::with_transport(transport_at(start));

            let status = render_block_on(plugin, &mut channels, &mut host);

            assert!(matches!(status, ProcessStatus::KeepAlive), "frame {start}: {status:?}");
            assert!(host.sent.is_empty(), "an effect with no note output sent an event");
            left.extend_from_slice(&channels[0]);
            right.extend_from_slice(&channels[1]);
            start += block;
        }

        (left, right)
    }

    /// An instance as a host has it just before the first block: activated
    /// at [`RATE`], smoothers primed, and pattern 1 selected with `action`
    /// over `steps` of it.
    fn instance_with(action: Action, steps: std::ops::Range<u16>) -> Stuttter {
        let mut plugin = Stuttter::default();
        activate_at(&mut plugin, RATE);
        publish(&plugin, action, steps);
        set_pattern(&plugin.params, 1);

        plugin
    }

    /// **The wiring, end to end**: the real plugin, driven block by block
    /// through `process()` over two cycles of a pattern, plays the Repeat the
    /// pattern asks for — not the input it was handed.
    ///
    /// The fixture is the classic stutter: one Repeat block of 1/16 slices over
    /// the whole eight-step pattern, which is read at `d = p − (p mod ℓ)`. So
    /// the first slice of every cycle plays live and the seven after it replay
    /// that same slice, and the whole second cycle of the output is the 4096
    /// frames of its first step over and over — which is exactly what 128
    /// blocks of the ramp come back as here, to the last bit.
    #[test]
    fn process_plays_the_pattern_the_model_holds_over_many_blocks() {
        let mut plugin = instance_with(Action::repeat_at(Rate::Sixteenth), 0..STEPS);

        let (left, right) = run(&mut plugin, 0..2 * CYCLE, playing_at);

        let mut asserted = 0;
        for frame in CYCLE..2 * CYCLE {
            // The slice boundary this frame is past, where `d` jumps by one
            // slice and the two heads cross over.
            if frame % STEP < FADED {
                continue;
            }

            // The read: back to the first step of the cycle the frame is in.
            let position = CYCLE + frame % STEP;
            assert_eq!(left[frame], position as f32, "left, frame {frame}");
            assert_eq!(right[frame], -(position as f32), "right, frame {frame}");
            asserted += 1;
        }

        // Every frame of every slice outside the fades, so a run that
        // skipped its way to green cannot pass.
        assert_eq!(asserted, STEPS as usize * (STEP - FADED));

        // And it is the *engine's* output, not the block that came in: only
        // the first slice of the cycle is live.
        let replayed = CYCLE + STEP + FADED;
        assert_ne!(left[replayed], replayed as f32, "frame {replayed} came back as the input");
        assert_eq!(left[CYCLE + FADED], (CYCLE + FADED) as f32, "the first slice is live");
    }

    /// **Always record**: the history buffer takes every frame of every block,
    /// transport stopped and pattern Off included — so audio from before the
    /// song started is there to be played the moment it is asked for.
    ///
    /// Two runs of one instance. The first is a whole cycle with nothing
    /// playing and `pattern` at Off, which must come back bit for bit — and
    /// records all of it. The second presses play on a Reverse block, whose
    /// `d = 2p` reads back through the cycle before the block: a plugin that
    /// only recorded while playing would answer that read with silence,
    /// where this one answers with the ramp it heard while stopped, frame for
    /// frame and with no fade anywhere in it (a reverse opens at `d = 0` and
    /// turns the head round, which is a corner and not a jump).
    #[test]
    fn the_history_records_while_the_transport_is_stopped_and_the_pattern_is_off() {
        let mut plugin = instance_with(Action::from_preset(SpeedPreset::Reverse), 0..STEPS / 2);
        set_pattern(&plugin.params, PATTERN_OFF);

        let (dry_left, dry_right) = run(&mut plugin, 0..CYCLE, |_| stopped());

        for frame in 0..CYCLE {
            assert_eq!(dry_left[frame], frame as f32, "Off and stopped is not live, frame {frame}");
            assert_eq!(dry_right[frame], -(frame as f32), "frame {frame}");
        }

        // Play from the top of the next cycle, where the Reverse block is.
        set_pattern(&plugin.params, 1);
        let (left, right) = run(&mut plugin, CYCLE..CYCLE + BEAT, playing_at);

        for frame in 0..BEAT {
            // `d = 2p`, so the read walks backwards from the block's first
            // frame at one frame per frame — into the stopped cycle.
            let position = CYCLE - frame;
            assert_eq!(left[frame], position as f32, "left, frame {frame} of the reverse");
            assert_eq!(right[frame], -(position as f32), "right, frame {frame} of the reverse");
        }
    }

    /// An empty pattern is live, playing or not, and live is the input bit for
    /// bit even fully wet — the Volume row sits at unity and `d = 0` reads the
    /// frame just recorded, so nothing in the chain can round it.
    #[test]
    fn an_empty_pattern_plays_live_bit_for_bit() {
        let mut plugin = Stuttter::default();
        activate_at(&mut plugin, RATE);
        set_pattern(&plugin.params, 1);

        let (left, right) = run(&mut plugin, 0..4 * BLOCK, playing_at);

        for frame in 0..4 * BLOCK {
            assert_eq!(left[frame], frame as f32, "frame {frame}");
            assert_eq!(right[frame], -(frame as f32), "frame {frame}");
        }
    }

    /// The history buffer is allocated in `activate()` and sized from the
    /// host's sample rate — [`history::MAX_LOOKBACK_SECONDS`] of it — and never
    /// in `process()`.
    ///
    /// Also what a repeated `activate()` at the same configuration does:
    /// nothing. The recorded past surviving it is how that is visible, and
    /// is why the plugin does not hand a host restoring a project a fresh
    /// twelve-megabyte buffer per call.
    #[test]
    fn the_history_buffer_is_built_in_activate_and_sized_from_the_host_rate() {
        let mut plugin = Stuttter::default();
        assert!(plugin.engine.is_none(), "an unactivated instance holds no buffer");

        let mut host = TestHost::with_transport(stopped());
        let config = |rate: f32| BufferConfig {
            sample_rate: rate,
            min_buffer_size: None,
            max_buffer_size: 1024,
            process_mode: ProcessMode::Realtime,
        };
        let layout = &Stuttter::AUDIO_IO_LAYOUTS[0];

        assert!(plugin.activate(layout, &config(RATE), &mut host));
        let engine = plugin.engine.as_ref().expect("activate built no engine");
        let capacity = engine.history().capacity();
        assert_eq!(capacity, (f64::from(RATE) * history::MAX_LOOKBACK_SECONDS) as usize);

        // One block recorded, so that keeping the buffer can be told from
        // quietly replacing it.
        prime_smoothers(&plugin.params);
        run(&mut plugin, 0..BLOCK, |_| stopped());
        let recorded = plugin.engine.as_ref().unwrap().history().recorded();
        assert_eq!(recorded, BLOCK as u64, "process() recorded the block it was handed");

        assert!(plugin.activate(layout, &config(RATE), &mut host));
        let engine = plugin.engine.as_ref().unwrap();
        assert_eq!(engine.history().capacity(), capacity, "the same rate resized the buffer");
        assert_eq!(engine.history().recorded(), recorded, "the same rate replaced the buffer");

        // A new rate is a new buffer, and an empty one.
        assert!(plugin.activate(layout, &config(96_000.0), &mut host));
        let engine = plugin.engine.as_ref().unwrap();
        let wider = (96_000.0 * history::MAX_LOOKBACK_SECONDS) as usize;
        assert_eq!(engine.history().capacity(), wider, "a new rate kept the old buffer");
        assert_eq!(engine.history().recorded(), 0);
    }

    /// `reset()` is the host saying the audio either side of this point is
    /// not continuous, so the recorded past goes with it — a repeat right
    /// after a relocation must not play material from where the song used to
    /// be. It allocates nothing, which is why the buffer keeps its capacity.
    #[test]
    fn reset_forgets_the_recorded_past_and_keeps_the_buffer() {
        let mut plugin = Stuttter::default();
        activate_at(&mut plugin, RATE);

        run(&mut plugin, 0..BLOCK, |_| stopped());
        let capacity = plugin.engine.as_ref().unwrap().history().capacity();
        assert_eq!(plugin.engine.as_ref().unwrap().history().recorded(), BLOCK as u64);

        plugin.reset();

        let engine = plugin.engine.as_ref().unwrap();
        assert_eq!(engine.history().recorded(), 0, "reset kept the recorded past");
        assert_eq!(engine.history().capacity(), capacity, "reset reallocated the buffer");
    }

    /// A host that calls `process()` before `activate()` is outside the
    /// contract, and gets a bypass rather than a guessed sample rate to
    /// allocate a buffer from on the audio thread — and rather than the
    /// silence that would mute the track.
    #[test]
    fn a_block_before_activate_is_passed_through() {
        let mut plugin = Stuttter::default();
        set_pattern(&plugin.params, 1);

        let (left, right) = run(&mut plugin, 0..BLOCK, playing_at);

        assert!(plugin.engine.is_none(), "process() built an engine");
        for frame in 0..BLOCK {
            assert_eq!(left[frame], frame as f32, "frame {frame}");
            assert_eq!(right[frame], -(frame as f32), "frame {frame}");
        }
    }

    /// **The scope through the shell**: the summaries the waveform band is
    /// drawn from are written by the real `process()` path — the instance's own
    /// [`Scope`], handed to the engine in `activate()` and written frame by
    /// frame as the block is filled.
    ///
    /// One cycle of the classic stutter, so the two halves of the view
    /// differ: the input is the ramp, column by column, and the output is
    /// the block's first slice replayed over the seven steps after it.
    #[test]
    fn process_writes_the_scope_summaries() {
        /// Frames in one column of this fixture's cycle, which divides
        /// exactly: 64.
        const COLUMN: usize = CYCLE / COLUMNS;

        let mut plugin = instance_with(Action::repeat_at(Rate::Sixteenth), 0..STEPS);
        assert_eq!(COLUMN * COLUMNS, CYCLE, "the fixture's cycle stopped dividing");

        run(&mut plugin, 0..CYCLE, playing_at);

        let scope = plugin.engine.as_ref().expect("activate built no engine").scope();

        // One cycle and no wrap, so every column was written in the first
        // generation and the whole view is current.
        assert_eq!(scope.generation(), FIRST_GENERATION);
        assert_eq!(scope.column(), Some(COLUMNS - 1));
        assert_eq!(scope.playhead(), (CYCLE - 1) as f32 / CYCLE as f32);

        for column in 0..COLUMNS {
            let last = (column * COLUMN + COLUMN - 1) as f32;
            let ramp = Column { min: -last, max: last };

            assert_eq!(scope.written_in(column), FIRST_GENERATION, "column {column}");
            assert_eq!(scope.age(column), 0, "column {column}");
            assert_eq!(scope.input(column), ramp, "input column {column}");
        }

        // And the output half is the engine's, not the input: a column in
        // the middle of the second slice replays the matching column of the
        // first, which is where `d = p − (p mod ℓ)` sent it.
        let slice = STEP / COLUMN;
        let replayed = slice + slice / 2;
        assert_eq!(scope.output(replayed), scope.input(replayed - slice), "the replayed slice");
        assert_ne!(scope.output(replayed), scope.input(replayed), "that column is live");
        // The first slice itself is live, so there the two halves agree.
        assert_eq!(scope.output(slice / 2), scope.input(slice / 2), "the live first slice");
    }

    // ---- notes playing the patterns (`crate::notes`) ----------------------

    /// The Notes setting the tests below switch on: C6, the one a new user
    /// is offered first.
    const START: u8 = notes::SUGGESTED_START;

    /// A note starting at absolute frame `frame` of the run on `key`.
    fn note_on(frame: usize, key: u8) -> (usize, PluginNoteEvent<Stuttter>) {
        let (voice_id, channel, key) = (VoiceID::Wildcard, Channel::Number(0), Key::Number(key));
        (frame, NoteEvent::NoteOn { timing: 0, voice_id, channel, key, velocity: 0.8 })
    }

    /// A note ending at absolute frame `frame` of the run on `key`.
    fn note_off(frame: usize, key: u8) -> (usize, PluginNoteEvent<Stuttter>) {
        let (voice_id, channel, key) = (VoiceID::Wildcard, Channel::Number(0), Key::Number(key));
        (frame, NoteEvent::NoteOff { timing: 0, voice_id, channel, key, velocity: 0.0 })
    }

    /// [`run`] with `notes` handed to `process()` in the blocks their frames
    /// fall in, each timed from the start of its block — so the run also
    /// proves a note lands on its own frame rather than on a block's.
    fn run_with_notes(
        plugin: &mut Stuttter,
        frames: std::ops::Range<usize>,
        notes: &[(usize, PluginNoteEvent<Stuttter>)],
    ) -> Vec<f32> {
        let mut left = Vec::with_capacity(frames.len());
        let mut start = frames.start;
        while start < frames.end {
            let block = BLOCK.min(frames.end - start);
            let mut channels = [
                (start..start + block).map(|n| n as f32).collect::<Vec<f32>>(),
                (start..start + block).map(|n| -(n as f32)).collect::<Vec<f32>>(),
            ];
            let mut host = TestHost::with_transport(playing_at(start));
            let in_block = |(frame, _): &&(usize, _)| (start..start + block).contains(frame);
            for &(frame, event) in notes.iter().filter(in_block) {
                let timing = (frame - start) as u32;
                host.queue(match event {
                    NoteEvent::NoteOn { voice_id, channel, key, velocity, .. } => {
                        NoteEvent::NoteOn { timing, voice_id, channel, key, velocity }
                    }
                    NoteEvent::NoteOff { voice_id, channel, key, velocity, .. } => {
                        NoteEvent::NoteOff { timing, voice_id, channel, key, velocity }
                    }
                    other => other,
                });
            }

            render_block_on(plugin, &mut channels, &mut host);
            assert!(host.sent.is_empty(), "an effect with no note output sent an event");
            left.extend_from_slice(&channels[0]);
            start += block;
        }

        left
    }

    /// **Held (the default): a note plays its pattern for exactly as long as
    /// it is held**, from the frame it starts on — mid-block and mid-step,
    /// without waiting for the step to end the way the Pattern parameter
    /// does — and nothing plays on the frame it ends.
    ///
    /// The Pattern parameter is set to the very pattern the note plays, and
    /// is not heard either side of it: with Notes on, no note is Off.
    #[test]
    fn a_held_note_plays_its_pattern_from_the_frame_it_starts_to_the_frame_it_ends() {
        let mut plugin = instance_with(Action::repeat_at(Rate::Sixteenth), 0..STEPS);
        set_pattern(&plugin.params, 1);
        plugin.params.state.prefs().set_notes(Some(START));
        assert!(!plugin.params.state.prefs().latch(), "held is the default");
        let down = CYCLE + STEP + STEP / 3;
        let up = CYCLE + 4 * STEP + STEP / 2 + 100;
        assert!(!down.is_multiple_of(BLOCK) && !up.is_multiple_of(BLOCK));

        let left = run_with_notes(
            &mut plugin,
            0..2 * CYCLE,
            &[note_on(down, START), note_off(up, START)],
        );

        assert_repeats_between(&left, down, up);
        assert_eq!(plugin.params.note_pattern(), None, "the note was let go");
    }

    /// Held: the newest held note plays, letting it go hands back to the one
    /// held before, the Stop key does nothing, and a reset lets every note go
    /// — leaving nothing playing, whatever the parameter says.
    #[test]
    fn the_playing_pattern_follows_the_held_notes() {
        let mut plugin = instance_with(Action::repeat_at(Rate::Sixteenth), 0..STEPS);
        set_pattern(&plugin.params, 2);
        plugin.params.state.prefs().set_notes(Some(START));

        let (fifth, ninth) = (START + 4, START + 8);
        let notes = [note_on(10, fifth), note_on(20, ninth), note_on(30, stop_key())];
        run_with_notes(&mut plugin, 0..BLOCK, &notes);
        assert_eq!(plugin.params.playing(), Some(9), "the newest held note plays");
        run_with_notes(&mut plugin, BLOCK..2 * BLOCK, &[note_off(BLOCK + 10, ninth)]);
        assert_eq!(plugin.params.playing(), Some(5), "back to the one held before");
        assert_eq!(plugin.engine().and_then(Engine::selection), Some(4));

        plugin.reset();
        assert_eq!(plugin.params.note_pattern(), None);
        assert_eq!(plugin.params.playing(), None, "no note, nothing plays");

        // With Notes off the parameter is back in charge.
        plugin.params.state.prefs().set_notes(None);
        assert_eq!(plugin.params.playing(), Some(2));
    }

    /// What every note test checks: live before `from` and after `to`, the
    /// cycle's first slice over and over between them (outside the fades).
    fn assert_repeats_between(left: &[f32], from: usize, to: usize) {
        let mut repeated = 0;
        for (frame, &sample) in left.iter().enumerate() {
            let fading = (frame >= from && frame < from + FADED)
                || (frame >= to && frame < to + FADED)
                || (frame > from && frame < to && frame % STEP < FADED);
            if fading {
                continue;
            }
            let expected = match frame >= from && frame < to {
                true => {
                    repeated += 1;
                    CYCLE + frame % STEP
                }
                false => frame,
            };
            assert_eq!(sample, expected as f32, "frame {frame}");
        }
        assert!(repeated > 2 * STEP, "too little of the pattern was checked: {repeated}");
    }

    /// **Latched: a note starts its pattern and the pattern keeps going** —
    /// from the frame the note starts on, mid-block and mid-step, without
    /// waiting for the step to end the way the Pattern parameter does — past
    /// the end of the note, until the Stop key stops it on its own frame. The
    /// parameter, on that same pattern, is not heard either side.
    ///
    /// C6 is pattern 1, which holds the classic Repeat: from the note on,
    /// every frame reads back to the cycle's first slice; either side of it
    /// the input comes back as it went in.
    #[test]
    fn a_latched_note_starts_its_pattern_and_the_stop_key_ends_it() {
        let mut plugin = instance_with(Action::repeat_at(Rate::Sixteenth), 0..STEPS);
        set_pattern(&plugin.params, 1);
        plugin.params.state.prefs().set_notes(Some(START));
        plugin.params.state.prefs().set_latch(true);
        // Both mid-block (BLOCK does not divide them) and mid-step; the
        // note itself is a drum hit's, over long before the stop.
        let down = CYCLE + STEP + STEP / 3;
        let stop = CYCLE + 4 * STEP + STEP / 2 + 100;
        assert!(!down.is_multiple_of(BLOCK) && !stop.is_multiple_of(BLOCK));

        let left = run_with_notes(
            &mut plugin,
            0..2 * CYCLE,
            &[
                note_on(down, START),
                note_off(down + 50, START),
                note_on(stop, notes::stop_key(START)),
                note_off(stop + 50, notes::stop_key(START)),
            ],
        );

        assert_repeats_between(&left, down, stop);
        assert_eq!(plugin.params.note_pattern(), None, "the stop key ended it");
    }

    /// The editor is told which pattern the last note started, and nothing
    /// before it whatever the parameter says; the note ending changes
    /// nothing; the next note takes over; keys outside the seventeen do
    /// nothing; and Stop and a reset leave nothing playing.
    #[test]
    fn a_latched_pattern_follows_the_last_note() {
        let mut plugin = instance_with(Action::repeat_at(Rate::Sixteenth), 0..STEPS);
        set_pattern(&plugin.params, 2);
        plugin.params.state.prefs().set_notes(Some(START));
        plugin.params.state.prefs().set_latch(true);
        assert_eq!(plugin.params.playing(), None, "the parameter is not heard");

        let fifth = START + 4;
        let hits = [note_on(10, fifth), note_off(15, fifth), note_on(20, 36)];
        run_with_notes(&mut plugin, 0..BLOCK, &hits);
        assert_eq!(plugin.params.note_pattern(), Some(5), "a short note still counts");
        assert_eq!(plugin.params.playing(), Some(5));
        assert_eq!(plugin.engine().and_then(Engine::selection), Some(4));

        run_with_notes(&mut plugin, BLOCK..2 * BLOCK, &[note_on(BLOCK + 10, START + 6)]);
        assert_eq!(plugin.params.playing(), Some(7), "the next note takes over");

        run_with_notes(&mut plugin, 2 * BLOCK..3 * BLOCK, &[note_on(2 * BLOCK, stop_key())]);
        assert_eq!(plugin.params.note_pattern(), None);
        assert_eq!(plugin.params.playing(), None, "stop leaves nothing playing");

        run_with_notes(&mut plugin, 3 * BLOCK..4 * BLOCK, &[note_on(3 * BLOCK, START)]);
        plugin.reset();
        assert_eq!(plugin.params.note_pattern(), None);
        assert_eq!(plugin.params.playing(), None);
    }

    /// The transport stopping forgets a latched pattern, so pressing play
    /// again starts clean rather than carrying on with whatever the song
    /// ended on; and switching Latch off lets go of it too.
    #[test]
    fn stopping_the_transport_forgets_the_latched_pattern() {
        let mut plugin = instance_with(Action::repeat_at(Rate::Sixteenth), 0..STEPS);
        set_pattern(&plugin.params, PATTERN_OFF);
        plugin.params.state.prefs().set_notes(Some(START));
        plugin.params.state.prefs().set_latch(true);

        run_with_notes(&mut plugin, 0..BLOCK, &[note_on(10, START + 3)]);
        assert_eq!(plugin.params.playing(), Some(4));
        run_with_notes(&mut plugin, BLOCK..2 * BLOCK, &[]);
        assert_eq!(plugin.params.playing(), Some(4), "it keeps playing with no note");

        let mut channels = [vec![0.0; BLOCK], vec![0.0; BLOCK]];
        render_block_on(&mut plugin, &mut channels, &mut TestHost::with_transport(stopped()));
        assert_eq!(plugin.params.playing(), None, "stopped, it is forgotten");

        run_with_notes(&mut plugin, 0..BLOCK, &[note_on(10, START + 3)]);
        plugin.params.state.prefs().set_latch(false);
        run_with_notes(&mut plugin, BLOCK..2 * BLOCK, &[]);
        assert_eq!(plugin.params.playing(), None, "switching modes lets it go");
    }

    /// With the Notes setting off — a new device — notes do nothing at all:
    /// a drum machine in front of the device is not switching its patterns
    /// on every hit. Switched on, only the sixteen keys from its C count;
    /// and changing it lets go of whatever was held.
    #[test]
    fn notes_play_nothing_until_the_setting_is_on_and_only_in_its_range() {
        let mut plugin = instance_with(Action::repeat_at(Rate::Sixteenth), 0..STEPS);
        set_pattern(&plugin.params, PATTERN_OFF);
        assert_eq!(plugin.params.state.prefs().notes(), None, "a new device ignores notes");

        // A kick and a snare, as a drum machine passes them on.
        let drums = [note_on(10, 36), note_on(20, 38), note_on(30, START)];
        let left = run_with_notes(&mut plugin, 0..BLOCK, &drums);
        assert_eq!(plugin.params.playing(), None);
        assert!(left.iter().enumerate().all(|(frame, &sample)| sample == frame as f32));

        plugin.params.state.prefs().set_notes(Some(START));
        run_with_notes(&mut plugin, BLOCK..2 * BLOCK, &[note_on(BLOCK + 10, 36)]);
        assert_eq!(plugin.params.playing(), None, "C1 is outside C6–D#7");
        run_with_notes(&mut plugin, 2 * BLOCK..3 * BLOCK, &[note_on(2 * BLOCK + 10, START + 2)]);
        assert_eq!(plugin.params.playing(), Some(3));

        plugin.params.state.prefs().set_notes(Some(notes::LOWEST_START));
        run_with_notes(&mut plugin, 3 * BLOCK..4 * BLOCK, &[]);
        assert_eq!(plugin.params.playing(), None, "it went with the old setting");
    }

    /// The Stop key with the tests' setting.
    fn stop_key() -> u8 {
        notes::stop_key(START)
    }
}
