//! The per-block driver: what the host said about this block, and the audio
//! that comes out of it.
//!
//! This is the module `process()` hands a block to, and the one place the
//! song position turns into the numbers the rest of the engine is written in
//! terms of. It follows bitstep's `engine.rs` pattern: the framework stops at
//! the shell, and everything here is driven through a plain context struct
//! ([`BlockCtx`]) that a test can build by hand — so the maths is unit-tested
//! without a host, a transport or an audio device.
//!
//! # Following Bitwig
//!
//! The transport reading: [`BlockCtx`], built from the host's transport by
//! [`crate::block_ctx`] and read through the accessors below. Its two rules:
//!
//! - **Position and tempo, or the block counts as stopped.** A host that
//!   reports no position, no tempo, or numbers that cannot be believed is a
//!   host no block can be placed against, and live audio is the answer to
//!   it: if either is missing the block counts as **stopped**.
//! - **The beat at sample `i` is `pos_beats + i × tempo / 60 / sample_rate`**
//!   ([`BlockCtx::beat_at`]), which is the beat `Pattern::position_at` folds
//!   into the pattern — from beat 0 of the project, never from wherever the
//!   pattern was switched on.
//!
//! The driver itself: [`Engine`], which records the block's input, asks
//! [`crate::pattern::Pattern::position_at`] where in the grid each frame
//! sits, [`crate::actions`] how far behind that frame should read, and
//! [`crate::history::Reader`] for the frames themselves — crossfading
//! wherever the read position jumps.
//!
//! And the Volume row over that walk: [`VolumeRamp`], the step level the wet
//! signal is multiplied by, ramped over the Smoothing time rather than
//! stepped — "a level change between steps ramps over the Smoothing time,
//! not instantly".
//!
//! And which pattern that walk is over: [`Selector`], which holds the
//! `pattern` parameter back until the step the grid is playing has finished
//! — "takes effect at the next step boundary, never mid-step".
//!
//! And the transport's own edges, which are not code of their own but
//! properties of that walk: the history records every frame handed to it
//! whether the transport runs or not, a block the transport cannot be
//! followed through plays live and crossfades back to it from wherever the
//! read had got to, and a loop jump or a relocation is faded exactly where it
//! moves the read position. [`Engine`] §Transport edges has the whole of it.
//!
//! And over the whole of it, the dry/wet mix: the wet signal the walk makes
//! is blended against the frame that came in, by a mix that arrives as one
//! value a block ([`crate::StuttterParams::next_mix`]) and is walked to
//! across the block a frame at a time ([`MixRamp`]). The two ends of the
//! parameter are exact — `mix` at 0 is the input bit for bit and `mix` at 1
//! is the wet path untouched, a subnormal input apart: those are flushed to
//! a zero of the same sign at both ends of the per-frame work ([`flush`]).
//!
//! And last, what the editor sees of all of it: the walk writes min/max
//! column summaries of the frame that came in and the frame that went out
//! into the instance's [`Scope`], indexed by where in the pattern cycle the
//! frame sat — the waveform view's whole input, written from here because
//! here is the only place that knows both halves (`crate::scope` has the
//! rules the two threads share).
//!
//! Nothing here allocates, locks or blocks, and nothing that joins it may
//! either.

use std::sync::Arc;

use crate::{MIX_DEFAULT, SMOOTH_DEFAULT_MS, clamp_mix};
use crate::actions::{BlockPath, MAX_SPEED};
use crate::filter::{FilterStage, FilterTarget};
use crate::history::{History, MAX_SAMPLE_RATE, Reader, fade_frames_for};
use crate::model::StutterData;
use crate::pattern::{Lane, PATTERN_COUNT, Pattern};
use crate::scope::Scope;

/// The slowest tempo a block is believed at, in BPM.
///
/// Not a musical limit but a sanity one: below this a single beat is more
/// than the history buffer holds, so a "tempo" under it is a host reporting
/// nonsense (or nothing yet) rather than a project anyone wrote. Such a
/// block counts as stopped and plays live, which is the safe reading — see
/// [`BlockCtx::tempo`].
pub const MIN_TEMPO: f64 = 1.0;
/// The fastest tempo a block is believed at, in BPM. Half again as fast as
/// the fastest any host offers (Bitwig stops at 666), so nothing legitimate
/// comes near it; past it, see [`MIN_TEMPO`].
pub const MAX_TEMPO: f64 = 1000.0;

/// What the host said about one block: whether it is playing, where it
/// starts, how fast the song runs, how long the block is, and which pattern
/// the `pattern` parameter has asked for.
///
/// Built once per `process()` call by [`crate::block_ctx`] and then read-only
/// for the rest of the block — v1 has no mid-block tempo ramp, so one tempo
/// and one start beat describe the whole of it.
///
/// The fields are private and every accessor sanitises, because all six
/// numbers come from the host: the raw values are kept exactly as reported
/// (so a GUI could show where a stopped transport sits) and the clamping
/// happens on the way out, once, where the engine reads them. A value that
/// cannot be believed resolves to "this block is stopped" rather than to a
/// panic or a NaN travelling into the output: clamp all host numbers, and
/// never panic on host data.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BlockCtx {
    /// Whether the host says the transport is running. See
    /// [`BlockCtx::is_playing`], which is the question the engine actually
    /// asks.
    playing: bool,
    /// The song position at the block's first sample, in beats, as reported.
    pos_beats: f64,
    /// The song tempo in BPM, as reported.
    tempo: f64,
    /// The sample rate in Hz, as reported.
    sample_rate: f32,
    /// The block's length in frames — from the buffer, not the transport,
    /// because that is what the samples are actually counted in.
    num_samples: usize,
    /// The `pattern` parameter: the pattern to play, or `None` for Off.
    pattern: Option<u8>,
    /// The `smooth` parameter in milliseconds: how long every crossfade this
    /// block starts takes. Read once per block like the rest, and turned
    /// into a frame count by [`BlockCtx::fade_frames`].
    smooth_ms: f32,
    /// The `mix` parameter at the **end** of this block, already through
    /// nice-plug's smoother ([`crate::StuttterParams::next_mix`]): the
    /// dry/wet position the block is heading for, which the engine walks to
    /// across it rather than stepping to at its first frame ([`MixRamp`]).
    mix: f32,
}

impl BlockCtx {
    /// A stopped block of no particular length at no particular rate — what
    /// a test compares against, and what [`Default`] is.
    ///
    /// The shell's own stopped case is [`BlockCtx::stopped`], which keeps the
    /// block's length and sample rate: those two are known even when the
    /// transport is not.
    pub const STOPPED: BlockCtx = BlockCtx {
        playing: false,
        pos_beats: 0.0,
        tempo: 0.0,
        sample_rate: 0.0,
        num_samples: 0,
        pattern: None,
        smooth_ms: SMOOTH_DEFAULT_MS,
        mix: MIX_DEFAULT,
    };

    /// A playing block of `num_samples` frames starting at `pos_beats`, with
    /// pattern Off and Smoothing and Mix at their parameters' defaults;
    /// [`BlockCtx::with_playing`], [`BlockCtx::with_pattern`],
    /// [`BlockCtx::with_smooth_ms`] and [`BlockCtx::with_mix`] add the rest.
    pub const fn new(pos_beats: f64, tempo: f64, sample_rate: f32, num_samples: usize) -> BlockCtx {
        BlockCtx {
            playing: true,
            pos_beats,
            tempo,
            sample_rate,
            num_samples,
            pattern: None,
            smooth_ms: SMOOTH_DEFAULT_MS,
            mix: MIX_DEFAULT,
        }
    }

    /// A stopped block of `num_samples` frames at `sample_rate`: the host
    /// reported no usable position or tempo, but the block is still that
    /// many frames long and the plugin still records every one of them
    /// (a stopped transport outputs live audio, and the history buffer keeps
    /// recording).
    ///
    /// The length matters — an engine handed [`BlockCtx::STOPPED`] for a real
    /// block would believe it had no samples to fill.
    pub const fn stopped(sample_rate: f32, num_samples: usize) -> BlockCtx {
        BlockCtx {
            playing: false,
            pos_beats: 0.0,
            tempo: 0.0,
            sample_rate,
            num_samples,
            pattern: None,
            smooth_ms: SMOOTH_DEFAULT_MS,
            mix: MIX_DEFAULT,
        }
    }

    /// The same block with the host's play flag set to `playing`.
    pub const fn with_playing(mut self, playing: bool) -> BlockCtx {
        self.playing = playing;
        self
    }

    /// The same block with `pattern` selected (`None` being Off).
    pub const fn with_pattern(mut self, pattern: Option<u8>) -> BlockCtx {
        self.pattern = pattern;
        self
    }

    /// The same block with the Smoothing parameter at `milliseconds`.
    pub const fn with_smooth_ms(mut self, milliseconds: f32) -> BlockCtx {
        self.smooth_ms = milliseconds;
        self
    }

    /// The same block with the Mix parameter heading for `mix`, `0.0` being
    /// fully dry and `1.0` fully wet.
    pub const fn with_mix(mut self, mix: f32) -> BlockCtx {
        self.mix = mix;
        self
    }

    /// Whether this block plays a pattern at all: the host says the transport
    /// is running **and** the numbers it said that with are usable.
    ///
    /// The two halves are deliberately one question. A host claiming to play
    /// while reporting a NaN position, no tempo or an impossible sample rate
    /// cannot have a step placed against it, so the engine treats it exactly
    /// as it treats a stopped transport — live audio, history still
    /// recording — instead of guessing a position for it.
    pub fn is_playing(&self) -> bool {
        self.playing && self.pos_beats.is_finite() && self.beats_per_sample() > 0.0
    }

    /// The block's tempo in BPM, or `0.0` when the host reported one outside
    /// [`MIN_TEMPO`]…[`MAX_TEMPO`] (a NaN included, which fails the range
    /// test and so lands here too).
    ///
    /// Zero rather than a clamp into the range: a believable tempo is left
    /// alone, and an unbelievable one is not quietly replaced by a different
    /// speed — it makes the block stopped, and stopped plays the input. The
    /// alternative, playing a 0-BPM project at 1 BPM, would freeze the read
    /// head somewhere past the end of the history buffer and output silence.
    pub fn tempo(&self) -> f64 {
        match (MIN_TEMPO..=MAX_TEMPO).contains(&self.tempo) {
            true => self.tempo,
            false => 0.0,
        }
    }

    /// The sample rate in Hz, or `0.0` when the host reported one that cannot
    /// be used: zero, negative, a NaN, or above [`MAX_SAMPLE_RATE`].
    pub fn sample_rate(&self) -> f64 {
        let rate = f64::from(self.sample_rate);
        match rate > 0.0 && rate <= MAX_SAMPLE_RATE {
            true => rate,
            false => 0.0,
        }
    }

    /// The block's length in frames.
    pub const fn num_samples(&self) -> usize {
        self.num_samples
    }

    /// The pattern the `pattern` parameter asks for, or `None` for Off.
    pub const fn pattern(&self) -> Option<u8> {
        self.pattern
    }

    /// The index into the model's patterns that [`BlockCtx::pattern`] names,
    /// i.e. one less, or `None` for Off.
    ///
    /// Folded into the sixteen that exist, so a selection from outside the
    /// parameter's range — only reachable from a hand-built context, since
    /// the shell clamps on the way in — can still only name a real pattern
    /// and the engine never has to check the index it is handed.
    pub fn pattern_index(&self) -> Option<usize> {
        pattern_index(self.pattern)
    }

    /// The Smoothing parameter as the host reported it, in milliseconds —
    /// raw, like every other field here. [`BlockCtx::fade_frames`] is where
    /// it is believed.
    pub const fn smooth_ms(&self) -> f32 {
        self.smooth_ms
    }

    /// The dry/wet position this block is heading for, folded into
    /// `0.0..=1.0` ([`crate::clamp_mix`], whose `max`-then-`min` order sends
    /// a NaN to the **dry** end: a mix that cannot be believed bypasses the
    /// effect rather than filling the output with NaNs).
    ///
    /// The clamp is what lets the blend trust the two ends exactly: `0.0`
    /// means the frame that came in and `1.0` means the wet path, bit for
    /// bit either way ([`blend`]).
    pub fn mix(&self) -> f32 {
        clamp_mix(self.mix)
    }

    /// How long a crossfade started in this block lasts, in output frames:
    /// the Smoothing parameter at this block's sample rate
    /// ([`crate::history::fade_frames_for`], which does the clamping — a
    /// nonsense rate or a NaN length comes back as the shortest fade there
    /// is, never as zero frames and never as a NaN gain).
    pub fn fade_frames(&self) -> u32 {
        fade_frames_for(self.smooth_ms, self.sample_rate())
    }

    /// How many beats one frame advances the song: `tempo / 60 /
    /// sample_rate`, and `0.0` when either of those is unusable — which is
    /// what makes every derived beat below collapse onto the block's start
    /// rather than wander off.
    pub fn beats_per_sample(&self) -> f64 {
        let rate = self.sample_rate();
        match rate > 0.0 {
            true => self.tempo() / 60.0 / rate,
            false => 0.0,
        }
    }

    /// How many frames one beat lasts — the conversion a read offset `d` in
    /// beats goes through to reach a frame position. `0.0` when the
    /// transport is unusable, as above.
    pub fn samples_per_beat(&self) -> f64 {
        let tempo = self.tempo();
        match tempo > 0.0 {
            true => 60.0 / tempo * self.sample_rate(),
            false => 0.0,
        }
    }

    /// The song position of the block's first sample, in beats — the host's
    /// own, or `0.0` if it reported one that is not a finite number.
    pub fn start_beat(&self) -> f64 {
        match self.pos_beats.is_finite() {
            true => self.pos_beats,
            false => 0.0,
        }
    }

    /// The song position of frame `frame` of this block, in beats:
    /// `pos_beats + frame × tempo / 60 / sample_rate`, exactly.
    ///
    /// `frame` need not be inside the block — `beat_at(num_samples)` is the
    /// beat the *next* block starts at, which is [`BlockCtx::end_beat`] and
    /// what makes a loop jump detectable. On a stopped or unusable block
    /// every frame lands on the start beat, since nothing is advancing.
    pub fn beat_at(&self, frame: usize) -> f64 {
        self.start_beat() + frame as f64 * self.beats_per_sample()
    }

    /// The block's length in beats, `0.0` when it is stopped or unusable.
    pub fn span_beats(&self) -> f64 {
        self.num_samples as f64 * self.beats_per_sample()
    }

    /// The beat just past this block — the one the next block should start
    /// at if the host simply plays on. Half-open as every range here is: the
    /// block covers `[start_beat(), end_beat())`.
    pub fn end_beat(&self) -> f64 {
        self.beat_at(self.num_samples)
    }
}

/// A block with nothing in it, which is the honest reading of a host that has
/// said nothing yet.
impl Default for BlockCtx {
    fn default() -> BlockCtx {
        BlockCtx::STOPPED
    }
}

/// The furthest the read head may travel in one output frame and still be
/// *stepping* rather than jumping, in frames.
///
/// Jumps have to be told apart from steps because a step is read straight
/// through and a jump is crossfaded ([`Reader`]), and the engine's own
/// position is the only thing that knows which this frame is. The test is
/// **position continuity**, not the shape of the grid, and that is
/// deliberate:
///
/// - Inside any one stretch of the maths the read head moves at the speed of
///   the action over it — 1 frame per frame for live audio and for a Repeat
///   inside its slice, `v(p)` for a ramp — and `|v| ≤ `[`MAX_SPEED`]. So
///   nothing continuous can move further than that in one frame, and one
///   frame of slack on top covers the rounding of the beats-to-frames
///   conversion.
/// - Every jump the engine fades moves it very much further:
///   the shortest is a Repeat wrap of one slice, which is 165 frames even for
///   a 1/64 slice at 1000 BPM and 44.1 kHz. There is no middle ground to get
///   wrong.
/// - And the joins that are continuous *stay* continuous, which structural
///   detection would get wrong: a Reverse or a tape stop opens at `d = 0` and
///   double speed closes there ("ends exactly on live, so the next step joins
///   without a jump"). Fading those would mix a head with itself, which at
///   the middle of an equal-power ramp is +3 dB, not silence.
///
/// A corner is not a jump: live into Reverse turns the read head round
/// without moving it, and that is a change of slope the output follows
/// exactly, with nothing to fade.
pub const MAX_READ_STEP: f64 = MAX_SPEED + 1.0;

/// The audio core: the recorded past, the two heads reading it, and the walk
/// that turns one block of input into one block of output.
///
/// One instance per plugin instance, built in `initialize()` where its
/// buffer may be allocated ([`Engine::new`]) and driven from `process()`
/// through [`Engine::run_block`] — the only method the audio thread calls,
/// and one that allocates, locks and blocks nowhere. The framework stops at
/// the shell: what arrives here is a [`BlockCtx`], a published
/// [`StutterData`] and two slices of `f32`, all of which a test can build by
/// hand.
///
/// What it does per frame:
///
/// 1. **Record** the input frame, always — playing or stopped, pattern Off
///    or not, before anything else can read it. So "live" is the frame just
///    recorded, and a block starting on the next step already has its
///    material behind it.
/// 2. **Choose the pattern** it plays: what the `pattern` parameter asks
///    for, from the next step boundary on rather than at once ([`Selector`]).
/// 3. **Place** the frame in that grid: the beat is
///    [`BlockCtx::beat_at`] and the step, block and phases under it are
///    [`Pattern::position_at`]'s, measured from beat 0 of the project.
/// 4. **Choose `d`**, the beats behind "now" to read: `0` where no block
///    covers the frame, and otherwise the block's solved [`BlockPath`] (source,
///    loop and speed ramp, `crate::actions`).
/// 5. **Read** at `now − d × samples_per_beat` through the [`Reader`], after
///    telling it whether that position is a jump ([`MAX_READ_STEP`]).
/// 6. **Level** it: multiply by the Volume row's level for that step,
///    ramped by [`VolumeRamp`] rather than stepped.
/// 7. **Mix** it against the frame that came in: `dry × (1 − mix) + wet ×
///    mix`, with both ends of the parameter exact ([`blend`]) and the mix
///    itself walked across the block instead of stepping at its first frame
///    ([`MixRamp`]).
/// 8. **Summarise** the frame that came in and the frame that goes out into
///    the [`Scope`] column its place in the cycle names ([`cycle_phase`]) —
///    the editor's waveform view, which is drawn from nothing else.
///
/// Step 6 is the wet signal and step 7 is the output; step 8 reads both and
/// changes neither. Steps 1 to 6 run whatever the mix is, including at `mix`
/// 0: the recorded past, the read heads, the fades and the level ramp are
/// all *where the song is*, not functions of how much of the result is
/// heard, so turning the mix back up plays the pattern the song has reached
/// rather than restarting it somewhere. What `mix` 0 does is pick the dry
/// frame at the end — the wet one, interpolator and all, is computed and
/// dropped.
///
/// # Transport edges
///
/// The three things a host can do that are not "play the next block" all
/// fall out of that walk rather than being special cases inside it:
///
/// - **Stopped, or a transport that cannot be followed.** Step 1 happens
///   anyway — "the history buffer keeps recording", every frame of every
///   block, which is what makes the audio from a stopped stretch material a
///   later block can play. Steps 2 to 4 collapse onto live
///   ([`Selector::reset`] and `d = 0`), so the output is the input, and the
///   next frame that *can* be followed plays the selected pattern without
///   waiting for a step boundary it has no way to find. The read position
///   coming back to live is a jump like any other, which is what "crossfading
///   back if the read was offset" means — from *any* offset, because the fade
///   is chosen by how far the position moved and not by what moved it.
/// - **A loop jump or a relocation.** This is a host position discontinuity
///   against the position the engine predicted from the last block, and what
///   it asks for is a crossfade. This engine notices it one step further on,
///   in the read position that discontinuity produces ([`MAX_READ_STEP`]),
///   and that is the tighter test of the two: what a frame plays is its read
///   position and nothing else, so a jump the grid lines up with — a loop
///   whose length is a whole number of pattern cycles, which is the ordinary
///   case — leaves the output continuous and is read straight through, while
///   one that moves the read is faded however small the move in beats was.
///   Fading the former would be worse than not fading it: two heads over the
///   same material sum to +3 dB in the middle of an equal-power ramp.
/// - **The step a jump lands on** is a boundary for [`Selector`] either way,
///   because the step number it compares is counted from beat 0 of the
///   project rather than from the position in the cycle — so a held-back
///   pattern change is released by a loop jump instead of waiting out a step
///   the song has already left.
pub struct Engine {
    /// Everything heard so far. The one allocation this module makes, made
    /// once, by [`Engine::new`].
    history: History,
    /// The two read heads over that history, and the crossfade between them.
    reader: Reader,
    /// Where the last output frame read, or `None` before the first one and
    /// after a [`Engine::reset`]: what the next frame's position is compared
    /// against to tell a step from a jump ([`MAX_READ_STEP`]).
    ///
    /// Kept across blocks, because the seam between two blocks is not a
    /// boundary the read head knows anything about — a block edge in the
    /// middle of a Repeat slice must read straight through it.
    read_position: Option<f64>,
    /// The Volume row's level, where the ramp to the playing step's level
    /// has got to. Kept across blocks for the same reason as
    /// `read_position`: a ramp started one frame before a block ends has to
    /// finish in the next block.
    volume: VolumeRamp,
    /// The pattern's filter on the effect signal, with the playing step's
    /// cutoff and resonance ([`FilterStage`]). Kept across blocks like the
    /// level, for the same reason.
    filter: FilterStage,
    /// Which pattern is playing, and the step grid that decides when the
    /// `pattern` parameter's latest answer is allowed to replace it. Kept
    /// across blocks because a change arriving mid-step waits for a boundary
    /// that is usually in some later block ([`Selector`]).
    selection: Selector,
    /// The dry/wet position the last block was heading for, or `None` before
    /// the first block and after a [`Engine::reset`]: where this block's
    /// [`MixRamp`] starts from, so that a parameter the host moves between
    /// two blocks arrives as a line and not as a step at the block boundary.
    mix: Option<f32>,
    /// The waveform summaries the walk writes and the editor draws
    /// ([`Scope`]), shared with the plugin instance that handed them over —
    /// the one thing here the audio thread does not own alone, and an `Arc`
    /// for exactly that reason (the editor outlives any one engine: a
    /// re-`activate()` at another sample rate builds a new engine around the
    /// same scope).
    ///
    /// Written per frame, and only for a frame that has a place in a
    /// pattern cycle to be written at — see [`Engine::run_block`].
    scope: Arc<Scope>,
}

impl Engine {
    /// An engine whose history holds [`crate::history::MAX_LOOKBACK_SECONDS`]
    /// at `sample_rate`, with nothing recorded, no heads running and fades at
    /// the Smoothing parameter's default length.
    ///
    /// The allocating call, and the only one: `initialize()` is where it
    /// belongs, because the sample rate is what sizes the buffer and
    /// `process()` may not allocate. Every later block re-reads the
    /// Smoothing parameter off its [`BlockCtx`], so the length chosen here
    /// only covers a fade starting before the first block does.
    ///
    /// `scope` is the plugin instance's own, written by the walk and read by
    /// its editor: it is handed in rather than made here because the editor
    /// may be opened before the host activates the instance, and because
    /// activating it again at another sample rate builds another engine —
    /// the summaries the window is drawing must survive both.
    pub fn new(sample_rate: f32, scope: Arc<Scope>) -> Engine {
        Engine {
            history: History::new(sample_rate),
            reader: Reader::new(fade_frames_for(SMOOTH_DEFAULT_MS, f64::from(sample_rate))),
            read_position: None,
            volume: VolumeRamp::new(),
            filter: FilterStage::new(sample_rate),
            selection: Selector::new(),
            mix: None,
            scope,
        }
    }

    /// The recorded past, for the tests to look at.
    pub fn history(&self) -> &History {
        &self.history
    }

    /// The waveform summaries this engine writes — the editor's view of the
    /// walk, and a test's.
    pub fn scope(&self) -> &Scope {
        &self.scope
    }

    /// The read heads, for a test to ask how many are running and whether a
    /// fade is in flight.
    pub fn reader(&self) -> &Reader {
        &self.reader
    }

    /// Where the last output frame read, in absolute frames — `None` before
    /// the first one. The offset maths of a whole block, as one number a
    /// test can read back.
    pub fn read_position(&self) -> Option<f64> {
        self.read_position
    }

    /// The Volume row's gain as the last output frame was multiplied by it —
    /// `None` before the first frame and after a [`Engine::reset`], where no
    /// level has been established to ramp from.
    pub fn volume_gain(&self) -> Option<f32> {
        self.volume.gain()
    }

    /// Which pattern is playing, as an index into the model's patterns, or
    /// `None` for live — Off, a stopped transport, or a selection that has
    /// arrived but is still waiting for a step boundary ([`Selector`]).
    ///
    /// Not the same question as "what does the `pattern` parameter say": that
    /// one is the host's and is on the [`BlockCtx`]. This is the answer the
    /// audio is actually being made from.
    pub fn selection(&self) -> Option<usize> {
        self.selection.current()
    }

    /// Forget the recorded past, both heads, the level, the mix and the
    /// waveform summaries: what the plugin's `reset()` wants, where the host
    /// has said the audio either side of this point is not continuous —
    /// there is no frame before the seam for a ramp to start from, so the
    /// next block takes its level and its mix as they are.
    ///
    /// The scope goes with the history rather than staying on screen: its
    /// columns summarise a past that has just been forgotten, and the
    /// material after the seam is from somewhere else in the song.
    ///
    /// Allocates nothing and keeps the buffer's capacity, so it is safe on
    /// the audio thread, which is where nice-plug calls `reset()`.
    pub fn reset(&mut self) {
        self.history.reset();
        self.reader.reset();
        self.read_position = None;
        self.volume.reset();
        self.filter.reset();
        self.selection.reset();
        self.mix = None;
        self.scope.clear();
    }

    /// Records one block of input and writes the block's output over it.
    ///
    /// In place, because that is how nice-plug hands a buffer over (the
    /// wrapper has already put the input into it): every frame is read out
    /// of the slices, recorded, and then overwritten with what the engine
    /// chose to play. `left` and `right` are one stereo block — this plugin
    /// has exactly that layout — and the number of frames is the shorter of
    /// the two, not the transport's idea of it, because those are the frames
    /// that actually exist.
    ///
    /// Off, a stopped or unusable transport, a pattern the model does not
    /// hold, and a pattern with nothing over this frame all reduce to the
    /// same thing: `d = 0`, which reads the frame just recorded and so
    /// passes the input through bit for bit (the dry path must not pass
    /// through the interpolator, and at `d = 0` it does not: the
    /// interpolator's constant term *is* the frame).
    /// The Volume row leaves that bit-exact too wherever it sits at unity,
    /// which is its default and what Off itself asks for: multiplying an
    /// `f32` by exactly `1.0` returns it unchanged.
    ///
    /// `mix` at 0 is the fourth of those cases and the only one that does
    /// not go through the reads at all: whatever the pattern was playing,
    /// the frame handed back is the frame handed in ([`blend`]). The walk
    /// still happens underneath it — see [`Engine`] step 7 — so the mix can
    /// be turned up again without the pattern having lost its place.
    ///
    /// All four of them are bit-exact over every input but one: a subnormal
    /// frame is flushed to a zero of the same sign on its way in, which is
    /// inaudible, is what keeps denormals out of the arithmetic, and is what
    /// `clap-validator`'s fuzzer requires of the output ([`flush`]).
    ///
    /// The `pattern` parameter is read off the context once, but the pattern
    /// the walk is over is chosen per frame: a change the host made in the
    /// middle of a step takes effect at the step boundary, which may be in
    /// this block or in a later one ([`Selector`]).
    ///
    /// The [`Scope`] is written for every frame that a pattern covers, and
    /// for none of the live cases above — there is no place in a cycle to
    /// index a summary by when no cycle is playing, so the view holds still
    /// and ages instead (`crate::scope`). `mix` at 0 is not one of those
    /// cases: the frame still has a place, and what the column then holds is
    /// the input twice over, which is the truth of a block the plugin played
    /// dry.
    ///
    /// Allocation-free, lock-free and panic-free on every input, host-shaped
    /// or hostile: the positions it computes are clamped by [`BlockCtx`] and
    /// [`Pattern`] on the way in, and a read outside the recorded past is
    /// [`History`]'s silence rather than an index out of range.
    pub fn run_block(
        &mut self,
        ctx: &BlockCtx,
        data: &StutterData,
        left: &mut [f32],
        right: &mut [f32],
    ) {
        self.run_block_with(ctx, data, &SelectionChanges::new(), left, right);
    }

    /// [`Engine::run_block`] with pattern changes that land on exact frames
    /// of the block — the notes that started or ended in it
    /// ([`SelectionChanges`]). Each takes effect on its frame without
    /// waiting for a step boundary, and the selection it makes stands until
    /// the next change or the next block's context.
    pub fn run_block_with(
        &mut self,
        ctx: &BlockCtx,
        data: &StutterData,
        changes: &SelectionChanges,
        left: &mut [f32],
        right: &mut [f32],
    ) {
        let frames = left.len().min(right.len());

        // The Smoothing parameter, once per block: a fade already running
        // keeps the length it started with ([`Reader::set_fade_frames`]),
        // and so does a level ramp ([`VolumeRamp::next`]).
        let fade_frames = ctx.fade_frames();
        self.reader.set_fade_frames(fade_frames);

        // What the `pattern` parameter asks for, once per block: the
        // selection the walk moves *to*, which is not the one it is playing
        // until a step boundary says so ([`Selector`]). Read here so that an
        // automated parameter cannot change answer underneath the walk.
        let mut selected = ctx.pattern_index();
        let mut changes = changes.as_slice().iter().peekable();
        let playing = ctx.is_playing();
        let samples_per_beat = ctx.samples_per_beat();
        // The Mix parameter, once per block too — but as a line rather than
        // a number, walked from wherever the last block left the mix to the
        // value this one is heading for ([`MixRamp`]).
        let target_mix = ctx.mix();
        let mix = match self.mix {
            Some(previous) => MixRamp::across(previous, target_mix, frames),
            None => MixRamp::flat(target_mix),
        };
        self.mix = Some(target_mix);
        // One `BlockPath` solved per block of the Time row the walk enters,
        // not per frame — see [`PathCache`].
        let mut paths = PathCache::new();
        // How close to the oldest recorded frame a read may come before its
        // level starts fading out: one fade's worth of travel at the fastest
        // the read and the buffer's far end can close on each other, plus the
        // interpolator's reach. So the fade is over before the data runs out.
        let margin = f64::from(fade_frames) * MAX_READ_STEP + 4.0;

        for frame in 0..frames {
            // The frame as it came in, kept before anything is written over
            // it: the dry signal of step 7, and what `mix` at 0 hands back
            // untouched. Flushed, so that no denormal reaches the ring
            // buffer, the interpolator, the fades or the output
            // ([`flush`]) — the one thing the frame does not come back as
            // bit for bit.
            let (dry_left, dry_right) = (flush(left[frame]), flush(right[frame]));
            self.history.record(dry_left, dry_right);
            // `recorded` is one past the newest, so the frame just recorded
            // — the one `d = 0` reads — is one below it.
            let now = self.history.recorded().saturating_sub(1) as f64;
            let beat = ctx.beat_at(frame);

            // The pattern this frame plays. A stopped or unusable transport
            // has no grid to be mid-step in, so it both plays live and
            // leaves nothing in effect for the first playing frame to wait
            // behind.
            let in_effect = self.selection.current();
            // A note's change lands on its own frame, and is in effect from
            // there without waiting for the step to end.
            while let Some(&(_, pattern)) = changes.next_if(|change| change.0 <= frame) {
                selected = pattern_index(pattern);
                self.selection.adopt(selected);
            }
            let pattern = match playing {
                true => self.selection.resolve(selected, data, beat),
                false => {
                    self.selection.reset();
                    None
                }
            };
            if self.selection.current() != in_effect {
                // The Time-row indexes a solved ramp is keyed by belong to
                // the pattern that was playing, and this frame is another's.
                paths.clear();
            }

            let target = match pattern {
                Some(pattern) => FrameTarget::of(pattern, beat, &mut paths),
                None => FrameTarget::LIVE,
            };
            let position = now - target.offset * samples_per_beat;

            if self.is_jump(position) {
                self.reader.jump();
            }
            self.read_position = Some(position);

            // Per frame, and after the read: the level is a property of the
            // step the song is on, not of the material the offset found, so
            // it rides on top of whatever the reader handed back — a
            // crossfade included.
            let level = match self.has_material(position, now, margin) {
                true => target.volume,
                false => 0.0,
            };
            let gain = self.volume.next(level, fade_frames);

            // The filter on what was read, before the level: the Volume row
            // chops the filtered sound, so a gate stays as sharp as it was
            // drawn rather than ringing on through the filter.
            let read = self.reader.read(&self.history, position);
            let (out_left, out_right) = self.filter.process(read, target.filter, fade_frames);
            // One mix for both channels, so the stereo image cannot move
            // with the parameter, and read per frame so a block the host
            // automated across is a ramp and not a step.
            // Flushed again on the way out, because the blend and the gain
            // can make a subnormal of two normals ([`flush`]).
            let frame_mix = mix.at(frame);
            left[frame] = flush(blend(dry_left, gain * out_left, frame_mix));
            right[frame] = flush(blend(dry_right, gain * out_right, frame_mix));

            // Step 8, after the output exists because the output is half of
            // what it summarises. Only where the frame has a place in a
            // cycle to be summarised at: `None` is Off, a stopped or
            // unusable transport, or a pattern the model does not hold, and
            // the scope holds still through all of them rather than drawing
            // a waveform against a grid it has no relation to
            // (`crate::scope` §What is not in it).
            if let Some(phase) = pattern.and_then(|pattern| cycle_phase(pattern, beat)) {
                self.scope.write(phase, (dry_left, dry_right), (left[frame], right[frame]));
            }
        }
    }

    /// Whether reading at `position` next is a jump rather than a step —
    /// [`MAX_READ_STEP`] has the why of the one comparison.
    /// Whether a read at `position` still has recorded audio around it, with
    /// `margin` frames to spare before the oldest frame the buffer holds.
    ///
    /// A read near "now" always has (the frames there were just recorded),
    /// which keeps live audio — and a block's live first slice — exactly as
    /// it came in. Anything else running towards the far end of the buffer,
    /// or back past the first frame ever recorded, answers `false` early
    /// enough for the level ramp to fade it out rather than drop to silence
    /// in one sample.
    fn has_material(&self, position: f64, now: f64, margin: f64) -> bool {
        if position >= now - MAX_READ_STEP {
            return true;
        }

        position - margin >= self.history.oldest() as f64
    }

    fn is_jump(&self, position: f64) -> bool {
        let Some(previous) = self.read_position else {
            // Nothing has been read yet, so there is no step to be too big
            // for and no head to fade out either.
            return false;
        };

        // A step that is not a number is a jump, and tested for as its own
        // case rather than left to fall out of a comparison a NaN always
        // fails: a head coasting onto a NaN position would read silence for
        // the rest of the block with nothing crossfading it.
        let step = (position - previous).abs();

        !step.is_finite() || step > MAX_READ_STEP
    }
}

/// The index into the model's patterns that pattern number `pattern` names
/// (`1..=16` → `0..16`), or `None` for Off — folded into the sixteen that
/// exist, so the engine never has to check the index it is handed.
fn pattern_index(pattern: Option<u8>) -> Option<usize> {
    // `PATTERN_COUNT` is 16, so the cast is exact; the clamp is what makes
    // the subtraction below safe for any `u8`.
    let selected = pattern?.clamp(1, PATTERN_COUNT as u8);
    Some(usize::from(selected - 1))
}

/// How many changes one block can carry in a [`SelectionChanges`]. Far more
/// than notes a block ever holds; past it, the last change takes the place of
/// the one before, so the block still ends on the right pattern.
pub const MAX_SELECTION_CHANGES: usize = 64;

/// Changes of pattern that land on an exact frame of a block and take effect
/// there and then — what a note starting or ending is
/// ([`crate::notes`]).
///
/// Unlike the `pattern` parameter, which waits for the playing step to end
/// ([`Selector`]), a change here does not wait: a note says when, and its
/// pattern starts on the frame it said, at the place in the song grid that
/// frame is at. The read-position jump that may be is crossfaded like any
/// other.
///
/// Fixed capacity, so building one on the audio thread allocates nothing.
#[derive(Clone, Copy, Debug)]
pub struct SelectionChanges {
    /// `(frame, pattern)` pairs, frames in rising order; only `..len` is used.
    items: [(usize, Option<u8>); MAX_SELECTION_CHANGES],
    /// How many of `items` are in use.
    len: usize,
}

impl SelectionChanges {
    /// No changes: the block plays whatever its context selects.
    pub const fn new() -> SelectionChanges {
        SelectionChanges { items: [(0, None); MAX_SELECTION_CHANGES], len: 0 }
    }

    /// From frame `frame` on, play `pattern` (`None` being Off).
    ///
    /// A frame earlier than the last one pushed is moved up to it, so the
    /// list stays in order whatever it is handed. A second change on the same
    /// frame replaces the first, and so does one that finds the list full.
    pub fn push(&mut self, frame: usize, pattern: Option<u8>) {
        if let Some(last) = self.len.checked_sub(1) {
            let frame = frame.max(self.items[last].0);
            if frame == self.items[last].0 || self.len == MAX_SELECTION_CHANGES {
                self.items[last] = (frame, pattern);
                return;
            }
            self.items[self.len] = (frame, pattern);
        } else {
            self.items[0] = (frame, pattern);
        }
        self.len += 1;
    }

    /// The changes, frames in rising order.
    pub fn as_slice(&self) -> &[(usize, Option<u8>)] {
        &self.items[..self.len]
    }
}

impl Default for SelectionChanges {
    fn default() -> SelectionChanges {
        SelectionChanges::new()
    }
}

/// Which pattern is playing, and when the `pattern` parameter is allowed to
/// change that: a change of the `pattern` parameter "takes effect at the
/// next step boundary, never mid-step".
///
/// The host can move the parameter at any sample it likes — a knob, an
/// automation lane, a controller — and the shell reads it once per block, so
/// without this the grid under the walk would change wherever a block
/// happened to begin: a Repeat cut off a third of the way through its slice,
/// a tape stop abandoned mid-fall. So the parameter's answer is kept as a
/// *pending* selection and only becomes the playing one at a step boundary,
/// which is the one place in the grid where the material is expected to
/// change anyway.
///
/// # Which step boundary
///
/// The grid that decides is the **playing** pattern's, not the arriving
/// one's: it is the step being played that must not be cut in half, and only
/// the pattern playing it knows how long it is. The boundary is found on the
/// step grid *unfolded* from beat 0 ([`step_number`]) rather than from the
/// step index inside the cycle, so that a loop jump landing on the same step
/// of another cycle is a boundary too — the material either side of it is
/// not continuous, which is exactly the case a held-back change should stop
/// being held back for.
///
/// # Where it does not wait
///
/// A change takes effect at the frame it arrives when there is no step being
/// played to protect:
///
/// - **Nothing in effect** — the parameter's first frame, the first frame
///   after a [`Engine::reset`], and the first frame after a stopped or
///   unusable block (which resets this). Pressing play is not a parameter
///   change and must not cost a step.
/// - **Off in effect.** Off is the bypass, not a pattern with a grid: there
///   are no steps running to be mid-way through, so a pattern selected over
///   it joins at once, at the current place in the bar. The read-position
///   jump that entry may be is crossfaded like any other (`Off→pattern` is
///   one of the boundaries the click test checks for a click).
///
/// Going the other way *does* wait: Off arriving mid-step is a change to the
/// grid that is playing, so the step it interrupts is played out first and
/// the bypass starts on the boundary.
///
/// Two `Option`s, compared and divided once per frame — nothing here
/// allocates, locks or looks anything up beyond the one pattern the index
/// names.
struct Selector {
    /// The pattern in effect, as an index into the model's patterns, or
    /// `None` for live.
    current: Option<usize>,
    /// Which step of the project the last frame sat on, in `current`'s own
    /// grid ([`step_number`]) — or `None` where there was no grid to be
    /// mid-step in, which is what makes the next change immediate.
    step: Option<f64>,
}

impl Selector {
    /// Nothing in effect and no step established: the next selection asked
    /// for is taken as it is.
    const NONE: Selector = Selector { current: None, step: None };

    /// Nothing in effect, which is [`Selector::NONE`].
    fn new() -> Selector {
        Selector::NONE
    }

    /// The pattern in effect, as an index into the model's patterns.
    fn current(&self) -> Option<usize> {
        self.current
    }

    /// Forget both the selection and the step: what [`Engine::reset`] wants,
    /// and what a block the transport cannot be followed through leaves
    /// behind, so that the first frame that can be followed plays what the
    /// parameter says without waiting for a boundary it has no way to find.
    fn reset(&mut self) {
        *self = Selector::NONE;
    }

    /// Put `selected` in effect now, without waiting for a step boundary:
    /// what a note asks for ([`SelectionChanges`]).
    fn adopt(&mut self, selected: Option<usize>) {
        self.current = selected;
        self.step = None;
    }

    /// The pattern project beat `beat` plays, given that the parameter is
    /// asking for `selected`.
    ///
    /// Called once per output frame: it adopts `selected` if this frame is
    /// where it is allowed to take effect, and then answers with whatever is
    /// in effect — so the caller never has to know which of the two it got.
    fn resolve<'d>(
        &mut self,
        selected: Option<usize>,
        data: &'d StutterData,
        beat: f64,
    ) -> Option<&'d Pattern> {
        if selected != self.current && self.at_step_boundary(data, beat) {
            self.current = selected;
        }

        let pattern = self.current.and_then(|index| data.pattern(index));
        // Carried to the next frame as the step a boundary is noticed
        // against — in the grid of whatever is in effect *now*, which after
        // an adoption is the new pattern's and not the old one's.
        self.step = pattern.and_then(|pattern| step_number(pattern, beat));

        pattern
    }

    /// Whether beat `beat` is where the playing pattern's current step ends
    /// — and so where a pending selection may take over.
    ///
    /// `true` wherever there is no step to wait for the end of: nothing in
    /// effect, Off in effect, a selection the model has no pattern for, or a
    /// beat that is not a number (a host reporting nonsense, which has no
    /// position in any grid and so cannot be mid-step in one).
    fn at_step_boundary(&self, data: &StutterData, beat: f64) -> bool {
        let Some(step) = self.step else {
            return true;
        };
        let playing = self.current.and_then(|index| data.pattern(index));

        match playing.and_then(|pattern| step_number(pattern, beat)) {
            Some(now) => now != step,
            None => true,
        }
    }
}

/// Which step of the project `beat` sits on in `pattern`'s grid, counted
/// from beat 0 — or `None` for a beat that is not a finite number.
///
/// The pattern's steps are `pattern.step_beats()` long and are laid out from
/// beat 0 of the project (the position is `b mod P` and nothing else, and
/// `P` is a whole number of steps), so this is simply which of those the
/// beat is in — negative for a count-in, and continuing to count up across
/// pattern cycles rather than wrapping with them. Two beats share a number
/// exactly when they are in the same step of the same cycle, which is what
/// [`Selector`] needs of it.
///
/// A whole number as an `f64` rather than an integer: beats are `f64` and
/// `floor` of one is exact, so nothing is gained by a cast that would have
/// to saturate.
fn step_number(pattern: &Pattern, beat: f64) -> Option<f64> {
    // The step length is positive and finite by `Pattern`'s invariants, so
    // the only unfinished number that can come out of the division is one
    // that went in.
    beat.is_finite().then(|| (beat / pattern.step_beats()).floor())
}

/// Where project beat `beat` sits in one cycle of `pattern`, as a fraction
/// of the cycle — `(b mod P) / P`, which is the scope's column index before
/// the multiplication.
///
/// [`Pattern::phase_at`] is the fold, so the scope's idea of where the song
/// is in the pattern is the same one [`Pattern::position_at`] plays it from
/// and cannot drift from it. `None` for a beat that is not a number — a host
/// reporting nonsense, which has no place in a cycle any more than it has a
/// step.
fn cycle_phase(pattern: &Pattern, beat: f64) -> Option<f64> {
    // Positive and finite by `Pattern`'s invariants, so the division is
    // neither by zero nor by a NaN, and the quotient is in `0.0..1.0` for
    // every phase the fold can hand back.
    let span = pattern.span_beats();

    Some(pattern.phase_at(beat)? / span)
}

/// What the grid asks of one output frame: how far behind "now" it reads,
/// and how loud it plays — the Time row's answer and the Volume row's, which
/// are the pattern's two rows and are read together because they come from
/// one [`crate::pattern::Position`].
///
/// The level is a *target*, not the gain the frame is multiplied by: the step
/// it comes from changes at a step boundary, and [`VolumeRamp`] is what turns
/// that change into a ramp.
#[derive(Clone, Copy, Debug, PartialEq)]
struct FrameTarget {
    /// The read offset `d`, in beats: how far behind the frame to read.
    offset: f64,
    /// The Volume row's level for the step the frame sits on, `0.0..=1.0`.
    volume: f32,
    /// The pattern's filter with that step's Cutoff and Res levels.
    filter: FilterTarget,
}

impl FrameTarget {
    /// Live audio at full level: the frame as it came in.
    ///
    /// What every case with nothing behind it reduces to — Off, a stopped or
    /// unusable transport, a pattern the model does not hold — and what
    /// keeps those bit-exact.
    const LIVE: FrameTarget = FrameTarget {
        offset: 0.0,
        volume: Pattern::DEFAULT_VOLUME,
        filter: FilterTarget::BYPASS,
    };

    /// What `pattern` asks of the frame at project beat `beat`.
    ///
    /// The offset is `0` — live — for every case where there is nothing to
    /// read behind: a beat that is not a number (a host reporting nonsense,
    /// which [`Pattern::position_at`] turns away), a step with no block drawn
    /// over it, and a block index the Time row does not hold, which cannot
    /// happen from a position this pattern answered but is still not worth a
    /// panic on the audio thread.
    ///
    /// The level is the row's, read at [`crate::pattern::Position::step`] and
    /// so **independent of the Time row**: the Volume row is "the gate and
    /// chop", and it gates a bare step's live audio exactly as it gates a
    /// block's read. Only a beat with no position at all falls back to unity,
    /// because there is no step to read a level from.
    fn of(pattern: &Pattern, beat: f64, paths: &mut PathCache) -> FrameTarget {
        let Some(position) = pattern.position_at(beat) else {
            return FrameTarget::LIVE;
        };
        let step = position.step();
        let volume = pattern.volume(step);
        // Like the level, the filter belongs to the step and not to the Time
        // row: a bare step's live audio is filtered too.
        let filter = FilterTarget {
            filter: pattern.filter(),
            cutoff: pattern.level(Lane::Cutoff, step),
            resonance: pattern.level(Lane::Resonance, step),
        };
        // The step's level with live audio under it — what a bare step plays
        // and what a Time row the position cannot be resolved in falls back
        // to.
        let live = FrameTarget { offset: 0.0, volume, filter };

        let Some(index) = position.block() else {
            return live;
        };
        let Some(path) = paths.of(pattern, index) else {
            return live;
        };

        FrameTarget { offset: path.offset(position.block_phase()), volume, filter }
    }
}

/// The [`BlockPath`] of the block the walk is inside, solved once and kept
/// until the walk leaves that block — the anchor shift and a `Continue`
/// chain are worked out on entry, never per sample.
///
/// Keyed by the block's index in the Time row, which identifies it for as
/// long as the Time row is the one being played: one [`Engine::run_block`]
/// call, inside which the published snapshot cannot change. The one thing
/// that *can* change inside a call is which pattern those indexes belong to
/// ([`Selector`]), and [`Engine::run_block`] drops the cache
/// ([`PathCache::clear`]) where it does.
struct PathCache {
    /// The Time-row index `path` was solved for, or `None`.
    block: Option<usize>,
    /// The path for that block; the live path while `block` is `None`.
    path: BlockPath,
}

impl PathCache {
    /// An empty cache: the next block the walk meets is solved for.
    fn new() -> PathCache {
        PathCache { block: None, path: BlockPath::live() }
    }

    /// Forget the solved block: the same index in another Time row is
    /// another block.
    fn clear(&mut self) {
        self.block = None;
    }

    /// The path for block `index` of `pattern`, solving it if this is not
    /// the block the cache already holds.
    fn of(&mut self, pattern: &Pattern, index: usize) -> Option<&BlockPath> {
        if self.block != Some(index) {
            self.path = pattern.block_path(index)?;
            self.block = Some(index);
        }

        Some(&self.path)
    }
}

/// The Volume row's level, on its way to the level of the step that is
/// playing: "a level change between steps ramps over the Smoothing time,
/// not instantly".
///
/// A step boundary moves the target in one frame, and the gain that reaches
/// the output is this ramp's, which takes the Smoothing parameter's worth of
/// frames to get there — the same count the crossfade between two read heads
/// takes ([`crate::history::fade_frames_for`]), because it is the same
/// parameter and the same click it is there to avoid. A row drawn from unity
/// to silence and back is then a gate with edges rather than a square wave
/// cut into the audio.
///
/// **Linear in the gain**, where the crossfade is equal-power, and the
/// difference is not an inconsistency: equal-power exists to hold the
/// *loudness* of two uncorrelated signals constant while one replaces the
/// other. Here there is one signal and the level it is asked for is the
/// level it must arrive at, so the straight line between the two is the ramp
/// that has no overshoot and no detour — and the level a GUI bar shows is
/// then the level the audio ends up at.
///
/// Three properties the ramp holds, which are what its tests assert:
///
/// - **Both ends are exact.** The last frame of a ramp is assigned the
///   target rather than accumulated onto it, so a row at unity multiplies by
///   exactly `1.0` (which leaves every sample bit for bit) and a row at zero
///   multiplies by exactly `0.0`.
/// - **A change mid-ramp restarts from the current gain**, as a jump
///   arriving mid-crossfade restarts from the current mix ([`Reader`] §A jump
///   during a fade): the gain the output hears never steps, however fast the
///   targets arrive — a 1/64 row at 20 ms of Smoothing simply never reaches
///   its levels.
/// - **The first level ever asked for is taken as it is**, with no ramp from
///   anywhere, exactly as the first read position is not a jump: there is no
///   level before the first frame to come from, and ramping up from unity
///   would put a level on the output that no step asked for.
///
/// Nothing here allocates or branches on anything but its own four numbers:
/// [`VolumeRamp::next`] is called once per output frame from the audio
/// thread.
pub struct VolumeRamp {
    /// The gain the last frame was multiplied by, or `None` before the first
    /// one and after a [`VolumeRamp::reset`].
    gain: Option<f32>,
    /// The level being ramped to — the step level last asked for, which is
    /// what a change is noticed against.
    target: f32,
    /// How much of the way to `target` one frame moves, as a gain
    /// difference. Meaningless while `remaining` is zero.
    step: f32,
    /// Frames of the running ramp still to render, `0` when the gain is
    /// sitting on its target.
    ///
    /// Counted down in frames for the reason [`crate::history::Reader`]
    /// counts its fade up in them: "the ramp is over" is then an integer
    /// fact rather than the far end of a float accumulation, and the frame
    /// it becomes true on is the one that gets the target exactly.
    remaining: u32,
}

impl VolumeRamp {
    /// A ramp with no level yet: the first level [`VolumeRamp::next`] is
    /// asked for is taken as it is.
    pub fn new() -> VolumeRamp {
        VolumeRamp {
            gain: None,
            target: Pattern::DEFAULT_VOLUME,
            step: 0.0,
            remaining: 0,
        }
    }

    /// The gain the last frame was multiplied by — `None` before the first
    /// one.
    pub fn gain(&self) -> Option<f32> {
        self.gain
    }

    /// Forget the level: the next frame takes whatever it is asked for. What
    /// the plugin's `reset()` wants, where the audio either side of the seam
    /// is not continuous and so there is nothing to ramp from.
    pub fn reset(&mut self) {
        *self = VolumeRamp::new();
    }

    /// The gain for this output frame, moving toward `target` over `frames`
    /// frames from wherever the gain is now.
    ///
    /// Called every frame with the playing step's level: a level that has not
    /// changed costs the comparison and nothing else, and a level that has
    /// starts (or restarts) the ramp. `frames` is the Smoothing parameter's
    /// frame count, read once per block by the caller; a ramp already running
    /// keeps the length it started with, as a running crossfade does
    /// ([`Reader::set_fade_frames`]) and for the same reason — rescaling a
    /// ramp halfway through would move the gain it is holding.
    ///
    /// `target` is a level off the Volume row, which `pattern.rs` guarantees
    /// is a real number in `0.0..=1.0` (its invariant 3, NaN clamped to zero
    /// at the setter), so there is no NaN to guard here: one would otherwise
    /// compare unequal to itself and restart the ramp on every frame.
    pub fn next(&mut self, target: f32, frames: u32) -> f32 {
        let Some(gain) = self.gain else {
            // Nothing has played yet, so there is no level to ramp from —
            // and nothing a ramp would smooth over, since no frame has been
            // heard at the old gain.
            self.target = target;
            self.gain = Some(target);
            self.remaining = 0;
            return target;
        };

        if target != self.target {
            // A fade of no frames is a cut, which is what this exists to
            // avoid; `fade_frames_for` already guarantees at least one, and
            // the `max` is what makes that a local fact.
            let frames = frames.max(1);
            self.target = target;
            self.step = (target - gain) / frames as f32;
            self.remaining = frames;
        }

        if self.remaining == 0 {
            return gain;
        }

        self.remaining -= 1;
        // The last frame of the ramp lands *on* the target rather than one
        // accumulation short of it.
        let gain = match self.remaining {
            0 => self.target,
            _ => gain + self.step,
        };
        self.gain = Some(gain);

        gain
    }
}

/// A ramp with no level yet, which is [`VolumeRamp::new`].
impl Default for VolumeRamp {
    fn default() -> VolumeRamp {
        VolumeRamp::new()
    }
}

/// A subnormal sample flushed to zero, with its sign kept; everything else
/// — normals, both zeroes, the infinities and the NaNs — handed back
/// untouched.
///
/// Subnormals are the `f32`s below `f32::MIN_POSITIVE` (1.18e−38, about
/// −757 dBFS), so nothing here is audible and nothing here is a signal; what
/// they are is two real problems on this path, and nice-plug sets no
/// flush-to-zero flags for us:
///
/// - `clap-validator`'s fuzzer feeds whole blocks of denormal audio and
///   fails any plugin whose output holds a subnormal value — an effect that
///   passes its input through hands them straight back. Measured before this
///   flush: nine `FAILED` seeds, every one of them "the sample written to
///   output port 0 ... is subnormal", the values spread evenly across the
///   subnormal range and always at sample index 0, which is a denormal block
///   in and the same block out.
/// - Denormal arithmetic is several times slower than normal arithmetic on
///   x86, and this engine multiplies and adds its way through four history
///   frames per read ([`crate::history`]) plus a crossfade. That is what
///   `clap-validator`'s `process-audio-denormals` warning was measuring.
///
/// So the engine flushes at both ends of its per-frame work: the frame as it
/// comes in, which keeps denormals out of the ring buffer and so out of the
/// interpolator and the fades, and the frame as it goes out, which catches
/// what the arithmetic itself can make — `dry × (1 − mix) + wet × mix` can
/// cancel two normal terms down into the subnormal range, and so can a
/// volume ramp or a fade multiplying a small sample by a small factor.
///
/// The sign is kept because that is what the hardware's own flush-to-zero
/// does, and because a `−0.0` turned `+0.0` is the sign bit [`blend`] goes
/// out of its way not to lose.
///
/// # What this costs the bit-exactness contract
///
/// The engine promises that with pattern Off, an empty pattern, or `mix` 0
/// the output is the input **bit for bit**. That still holds for every input
/// a signal can be made of, and the contract says so explicitly: a subnormal
/// input is the one exception, and it comes back as a zero of the same sign.
#[inline]
fn flush(sample: f32) -> f32 {
    if sample.is_subnormal() {
        return 0.0_f32.copysign(sample);
    }

    sample
}

/// One output frame of the mix: `dry × (1 − mix) + wet × mix`, with both
/// ends of the parameter exact.
///
/// The two ends are branches rather than cases of the formula because the
/// promise is that "with pattern Off, or an empty pattern, or mix 0, the
/// output equals the input **bit for bit**". Arithmetic alone does not give
/// that, for two separate reasons —
///
/// - `dry × 1.0 + wet × 0.0` is `dry + 0.0`, which is `+0.0` when `dry` is
///   `−0.0`: a sign bit lost on every silent frame of a dry signal, which is
///   not bit-exact even though it sounds the same.
/// - `wet × 0.0` is a NaN when `wet` is infinite and a NaN when it is a NaN.
///   Nothing in this engine produces either — the history holds what the
///   host sent and the interpolator is a polynomial over four of its frames
///   — but a host *can* send an infinity, and at `mix` 0 it must come back
///   out as itself rather than poisoning the dry path it is not even in.
///
/// Both hold mirrored at `mix` 1, where it is the wet path that must
/// survive untouched: a dry `−0.0` would otherwise flip the sign of a wet
/// `−0.0`, and a dry infinity would make a NaN of a wet frame it is not in.
///
/// So the ends are taken as they are, and the formula is used only where the
/// two signals are genuinely being added. Exact float comparisons on
/// purpose: [`crate::clamp_mix`] guarantees a real number in `0.0..=1.0`, so
/// these two tests are "is the parameter at its end" and nothing subtler,
/// and `MixRamp::at` reaches both ends exactly on the frames where it should
/// (don't "fix" exact float comparisons on this path with epsilons).
fn blend(dry: f32, wet: f32, mix: f32) -> f32 {
    if mix == 0.0 {
        return dry;
    }
    if mix == 1.0 {
        return wet;
    }

    dry * (1.0 - mix) + wet * mix
}

/// The dry/wet mix over one block: where it starts, and how far it moves per
/// frame.
///
/// Ported from chopdrop's `MasterRamp`, for the problem it solves there: the
/// mix reaches the audio thread once a block —
/// [`crate::StuttterParams::next_mix`] steps nice-plug's smoother by the
/// whole block and hands over the value it landed on. One number a block is
/// fine for a knob drag, whose own smoothing (`MIX_SMOOTHING_MS`, 10 ms) is
/// longer than any usual buffer, but a host can automate the parameter from
/// one end to the other between two blocks, and a mix that far apart on two
/// neighbouring frames is a click. So the mix arrives here as a *line*: the
/// engine remembers what the last block was heading for and walks from there
/// to this block's value one frame at a time.
///
/// A line and not a per-frame buffer of mixes, because the real-time rule
/// leaves nowhere to put a buffer: two `f32`s and a multiply-add per frame.
///
/// Both ends stay exact, which is what [`blend`] rests on:
///
/// - A mix that is not moving is a **flat** line whose step is exactly
///   `0.0`, and `start + 0.0 × frame` is `start` for every frame of the
///   block. So a project nobody is automating sits on the parameter's value
///   itself — `0.0` and `1.0` included, which is where the bit-exactness
///   lives.
/// - A line **reaches `to` at frame `frames`**, which is the *next* block's
///   frame 0 — where the mix is flat again at that value. So two blocks in a
///   row join up without repeating a value or skipping one, and the frame
///   that lands on an end of the parameter lands on it exactly.
#[derive(Clone, Copy, Debug, PartialEq)]
struct MixRamp {
    /// The mix at frame 0 of the block, already through
    /// [`crate::clamp_mix`].
    start: f32,
    /// How far the mix moves per frame, exactly `0.0` for a flat line.
    step: f32,
}

impl MixRamp {
    /// A mix that does not move: every frame of the block is `mix`.
    fn flat(mix: f32) -> MixRamp {
        MixRamp { start: clamp_mix(mix), step: 0.0 }
    }

    /// The line from `from` to `to` across `frames` frames.
    ///
    /// Both ends are clamped before the step is taken, so the line between
    /// them is finite whatever the host drove. A block of no frames has no
    /// line to draw and two equal ends have none to take: both are
    /// [`MixRamp::flat`] at `to`, which keeps the steady state — every block
    /// of a project whose Mix nobody is automating — exactly as cheap and
    /// exactly as precise as the single value it replaced.
    fn across(from: f32, to: f32, frames: usize) -> MixRamp {
        let from = clamp_mix(from);
        let to = clamp_mix(to);
        if frames == 0 || from == to {
            return MixRamp::flat(to);
        }

        MixRamp { start: from, step: (to - from) / frames as f32 }
    }

    /// The mix at frame `frame` of the block.
    ///
    /// `start + step × frame`, not a running sum: a sum carries its own
    /// rounding error forward frame by frame, and the closed form costs the
    /// same multiply-add. Clamped once more because the frame count is the
    /// caller's — a block rendered longer than the line was drawn for walks
    /// past `to`, and the clamp is what stops it walking out of the range
    /// [`blend`] accepts.
    fn at(self, frame: usize) -> f32 {
        clamp_mix(self.start + self.step * frame as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rate and block length the assertions below are written at: 48 kHz
    /// and 512 frames, with 120 BPM as the tempo, which makes one beat
    /// exactly 24 000 frames and every beat in these tests exact in `f64`.
    const SAMPLE_RATE: f32 = 48_000.0;
    const BLOCK: usize = 512;
    const TEMPO: f64 = 120.0;
    /// Frames in one beat at [`TEMPO`] and [`SAMPLE_RATE`].
    const BEAT_FRAMES: usize = 24_000;

    /// A playing block starting at `pos_beats`, with everything the engine
    /// reads present and believable.
    fn playing_at(pos_beats: f64) -> BlockCtx {
        BlockCtx::new(pos_beats, TEMPO, SAMPLE_RATE, BLOCK)
    }

    /// The believable case: every number is handed back as reported, and the
    /// two conversions the rest of the engine runs on are each other's
    /// inverse.
    #[test]
    fn a_playing_block_reports_what_the_host_said() {
        let ctx = playing_at(8.5);

        assert!(ctx.is_playing());
        assert_eq!(ctx.tempo(), TEMPO);
        assert_eq!(ctx.sample_rate(), f64::from(SAMPLE_RATE));
        assert_eq!(ctx.num_samples(), BLOCK);
        assert_eq!(ctx.start_beat(), 8.5);
        assert_eq!(ctx.samples_per_beat(), BEAT_FRAMES as f64);
        assert_eq!(ctx.beats_per_sample() * BEAT_FRAMES as f64, 1.0);
    }

    /// The formula, sampled: the beat at frame `i` is the start plus
    /// `i × tempo / 60 / sample_rate`, so a beat lands every 24 000 frames
    /// at 120 BPM and 48 kHz — and the position in beats is honoured, not
    /// rounded to a step or a bar.
    #[test]
    fn the_beat_of_a_frame_advances_with_the_tempo() {
        let ctx = playing_at(8.5);

        assert_eq!(ctx.beat_at(0), 8.5);
        assert_eq!(ctx.beat_at(BEAT_FRAMES / 2), 9.0);
        assert_eq!(ctx.beat_at(BEAT_FRAMES), 9.5);
        assert_eq!(ctx.beat_at(BEAT_FRAMES * 4), 12.5);

        // A fractional start beat is kept as it is: the grid is laid over the
        // project from beat 0, so the engine needs the position the host
        // gave, not a tidied one.
        assert_eq!(playing_at(0.125).beat_at(BEAT_FRAMES), 1.125);
        assert_eq!(playing_at(-0.25).beat_at(0), -0.25);
    }

    /// The block's own span, and the beat the next block is expected at: the
    /// range it covers is `[start, end)`, with `end` one block's worth of
    /// beats on.
    #[test]
    fn a_blocks_span_is_its_length_in_beats() {
        let whole_beat = BlockCtx::new(4.0, TEMPO, SAMPLE_RATE, BEAT_FRAMES);
        assert_eq!(whole_beat.span_beats(), 1.0);
        assert_eq!(whole_beat.end_beat(), 5.0);

        let ctx = playing_at(8.5);
        assert_eq!(ctx.span_beats(), BLOCK as f64 * ctx.beats_per_sample());
        assert_eq!(ctx.end_beat(), ctx.beat_at(BLOCK));
    }

    /// The missing-tempo rule, in its "reported but unbelievable" form: zero,
    /// negative, NaN and absurd tempos all make the block stopped, and a
    /// stopped block's beats do not advance — every frame of it sits on the
    /// position the host reported, which is what plays live audio.
    #[test]
    fn an_unusable_tempo_counts_as_stopped() {
        for tempo in [0.0, -120.0, f64::NAN, f64::INFINITY, MIN_TEMPO / 2.0, MAX_TEMPO * 2.0] {
            let ctx = BlockCtx::new(8.5, tempo, SAMPLE_RATE, BLOCK);

            assert!(!ctx.is_playing(), "tempo {tempo} must read as stopped");
            assert_eq!(ctx.tempo(), 0.0, "tempo {tempo}");
            assert_eq!(ctx.beats_per_sample(), 0.0, "tempo {tempo}");
            assert_eq!(ctx.samples_per_beat(), 0.0, "tempo {tempo}");
            assert_eq!(ctx.beat_at(BLOCK), 8.5, "tempo {tempo}");
            assert_eq!(ctx.span_beats(), 0.0, "tempo {tempo}");
        }

        // The ends of the range are believed, so the clamp cannot be read as
        // "only ordinary tempos play".
        for tempo in [MIN_TEMPO, MAX_TEMPO] {
            let ctx = BlockCtx::new(8.5, tempo, SAMPLE_RATE, BLOCK);
            assert!(ctx.is_playing(), "tempo {tempo} must play");
            assert_eq!(ctx.tempo(), tempo);
        }
    }

    /// The same for the sample rate: without a usable one there is no
    /// frame-to-beat conversion, so the block is stopped rather than
    /// dividing by zero.
    #[test]
    fn an_unusable_sample_rate_counts_as_stopped() {
        for rate in [0.0, -48_000.0, f32::NAN, f32::INFINITY, (MAX_SAMPLE_RATE * 2.0) as f32] {
            let ctx = BlockCtx::new(8.5, TEMPO, rate, BLOCK);

            assert!(!ctx.is_playing(), "rate {rate} must read as stopped");
            assert_eq!(ctx.sample_rate(), 0.0, "rate {rate}");
            assert_eq!(ctx.beats_per_sample(), 0.0, "rate {rate}");
            assert_eq!(ctx.samples_per_beat(), 0.0, "rate {rate}");
            assert_eq!(ctx.beat_at(BLOCK), 8.5, "rate {rate}");
        }
    }

    /// A position that is not a number is not a position: the block is
    /// stopped and its beats fall to zero, so nothing downstream is handed a
    /// NaN to fold into the pattern.
    #[test]
    fn a_non_finite_position_falls_to_beat_zero() {
        for pos in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let ctx = BlockCtx::new(pos, TEMPO, SAMPLE_RATE, BLOCK);

            assert!(!ctx.is_playing(), "position {pos} must read as stopped");
            assert_eq!(ctx.start_beat(), 0.0, "position {pos}");
            assert_eq!(ctx.beat_at(0), 0.0, "position {pos}");
            // The tempo is still believable, so the frame maths still works —
            // it is only the origin that was replaced.
            assert_eq!(ctx.beat_at(BEAT_FRAMES), 1.0, "position {pos}");
        }
    }

    /// The host's play flag is the other half of the question: a transport
    /// sitting still at a perfectly good position is stopped, and a stopped
    /// block still has its frames and its rate to record them at.
    #[test]
    fn a_stopped_block_keeps_its_length_and_its_rate() {
        let paused = playing_at(16.0).with_playing(false);
        assert!(!paused.is_playing());
        assert_eq!(paused.start_beat(), 16.0);
        assert_eq!(paused.num_samples(), BLOCK);

        // The shell's own form, for a host that reported no transport at all:
        // no position and no tempo, but the block is still 512 frames of
        // audio at a known rate.
        let unknown = BlockCtx::stopped(SAMPLE_RATE, BLOCK);
        assert!(!unknown.is_playing());
        assert_eq!(unknown.num_samples(), BLOCK);
        assert_eq!(unknown.sample_rate(), f64::from(SAMPLE_RATE));
        assert_eq!(unknown.tempo(), 0.0);
        assert_eq!(unknown.start_beat(), 0.0);
        assert_eq!(unknown.beat_at(BLOCK), 0.0);
        assert_eq!(unknown.span_beats(), 0.0);
    }

    /// The pattern selection rides along untouched, and resolves to an index
    /// the model can be read with — Off staying `None` all the way through,
    /// since there is no pattern to index.
    #[test]
    fn the_pattern_selection_resolves_to_a_pattern_index() {
        assert_eq!(playing_at(0.0).pattern(), None);
        assert_eq!(playing_at(0.0).pattern_index(), None);

        for selected in 1..=PATTERN_COUNT as u8 {
            let ctx = playing_at(0.0).with_pattern(Some(selected));
            assert_eq!(ctx.pattern(), Some(selected));
            assert_eq!(ctx.pattern_index(), Some(usize::from(selected) - 1));
        }

        // Only reachable from a hand-built context, and still a real
        // pattern: saturating at both ends.
        assert_eq!(playing_at(0.0).with_pattern(Some(0)).pattern_index(), Some(0));
        let highest = playing_at(0.0).with_pattern(Some(u8::MAX));
        assert_eq!(highest.pattern_index(), Some(PATTERN_COUNT - 1));
    }

    /// The Mix parameter rides along like the rest and is believed on the
    /// way out: fully wet unless the shell said otherwise, folded into
    /// `0.0..=1.0`, and a NaN landing on the **dry** end rather than
    /// travelling into the output.
    #[test]
    fn the_block_carries_the_mix_the_shell_read() {
        // A block nobody set a mix on is fully wet, which is where the
        // parameter starts (`MIX_DEFAULT`) — so every test written before
        // the mix existed still reads the wet path.
        assert_eq!(playing_at(0.0).mix(), MIX_DEFAULT);
        assert_eq!(playing_at(0.0).mix(), 1.0);
        assert_eq!(BlockCtx::stopped(SAMPLE_RATE, BLOCK).mix(), 1.0);
        assert_eq!(BlockCtx::STOPPED.mix(), 1.0);

        for mix in [0.0, 0.25, 0.5, 1.0] {
            assert_eq!(playing_at(0.0).with_mix(mix).mix(), mix, "mix {mix}");
        }

        // Hostile values, as `clamp_mix` resolves them: the ends saturate
        // and a NaN bypasses the effect.
        assert_eq!(playing_at(0.0).with_mix(-0.5).mix(), 0.0);
        assert_eq!(playing_at(0.0).with_mix(2.0).mix(), 1.0);
        assert_eq!(playing_at(0.0).with_mix(f32::INFINITY).mix(), 1.0);
        assert_eq!(playing_at(0.0).with_mix(f32::NEG_INFINITY).mix(), 0.0);
        assert_eq!(playing_at(0.0).with_mix(f32::NAN).mix(), 0.0);
    }

    /// Nothing is the stopped block of nothing: the default a `Transport`
    /// that has said nothing maps onto.
    #[test]
    fn the_default_block_is_the_stopped_one() {
        let ctx = BlockCtx::default();

        assert_eq!(ctx, BlockCtx::STOPPED);
        assert!(!ctx.is_playing());
        assert_eq!(ctx.num_samples(), 0);
        assert_eq!(ctx.pattern(), None);
        assert_eq!(ctx.span_beats(), 0.0);
    }
}

/// The driver's own tests: the **ramp trick**, which is what makes every
/// action's offset maths exactly checkable.
///
/// Feed the input `x[n] = n` — the frame recorded at absolute position `n`
/// holds the value `n` — and the output *is* the position the engine read
/// at. So every identity of where the engine reads can be asserted frame by
/// frame with `assert_eq!` on `f32`, not with a tolerance, provided three
/// things hold: the beats are exact, the interpolator's four taps all land
/// on the ramp, and no crossfade is mixing two positions into the frame.
/// [`SAMPLE_RATE`] and [`TEMPO`] see to the first, [`Run::assert_reads`] to
/// the other two.
///
/// These live in their own module because they are written at a different
/// rate and tempo from the transport tests above: those check what the host
/// said, these check what comes out of it.
#[cfg(test)]
mod driver_tests {
    use std::cmp::Ordering;

    use super::*;
    use crate::{SMOOTH_MAX_MS, SMOOTH_MIN_MS};
    use crate::actions::SpeedPreset;
    use crate::pattern::{Action, NoteValue, Rate, Source};
    use crate::scope::{COLUMNS, Column, FIRST_GENERATION};

    /// 32 768 Hz — a rate no host offers, chosen because it makes the
    /// arithmetic exact rather than nearly so.
    ///
    /// With [`TEMPO`] at 120 BPM one beat is `60 / 120 × 32 768` = 16 384
    /// frames, a power of two, so **one frame is exactly `2⁻¹⁴` beats**:
    /// every beat [`BlockCtx::beat_at`] computes is a dyadic rational, every
    /// offset the dyadic actions derive from one is exact, and every read
    /// position those land on is a whole frame or an exact binary fraction
    /// of one. At 48 kHz a beat is 24 000 frames and `1 / 24 000` is not
    /// representable, so a phase lands a hair either side of a slice
    /// boundary and the frame *at* the boundary could read either slice —
    /// a wobble in the test, not in the engine, and one worth designing
    /// out of the fixture.
    const SAMPLE_RATE: f32 = 32_768.0;
    /// 120 BPM — see [`SAMPLE_RATE`].
    const TEMPO: f64 = 120.0;
    /// Frames in one beat at [`SAMPLE_RATE`] and [`TEMPO`].
    const BEAT_FRAMES: f64 = 16_384.0;

    /// The fixture pattern's rate: 1/16, so a step is 0.25 beats = 4096
    /// frames.
    const RATE: Rate = Rate::Sixteenth;
    /// Beats in one step at [`RATE`].
    const STEP_BEATS: f64 = 0.25;
    /// Frames in one step at [`SAMPLE_RATE`] and [`TEMPO`]: 4096, so a step
    /// boundary always lands on a whole frame.
    const STEP_FRAMES: usize = (STEP_BEATS * BEAT_FRAMES) as usize;
    /// The fixture pattern's length in steps: eight, so it spans two beats
    /// and a run of [`FRAMES`] covers exactly two cycles of it.
    const STEPS: u16 = 8;
    /// Beats in one cycle of the fixture pattern.
    const SPAN_BEATS: f64 = STEPS as f64 * STEP_BEATS;
    /// Frames in one cycle of the fixture pattern.
    const CYCLE_FRAMES: usize = 32_768;

    /// The step the fixture's one Time-row block starts on: step 2, so the
    /// block opens half a beat into the cycle with live audio before it —
    /// which is what a Reverse or a BeforeBlock repeat reads.
    const BLOCK_STEP: u16 = 2;
    /// How many steps that block covers: four, so `B` is one whole beat and
    /// a 1/16 slice repeats four times inside it.
    const BLOCK_STEPS: u16 = 4;
    /// The beat of the cycle the block starts on.
    const BLOCK_START_BEATS: f64 = BLOCK_STEP as f64 * STEP_BEATS;
    /// The block's length `B` in beats.
    const BLOCK_BEATS: f64 = BLOCK_STEPS as f64 * STEP_BEATS;

    /// How long every run is: two cycles of the fixture pattern.
    ///
    /// The first cycle is the lead-in — it fills the history with ramp, so
    /// that the second cycle's reads have somewhere to land instead of the
    /// silence before the first recorded frame — and the assertions are all
    /// made over the second ([`SECOND_CYCLE`]).
    const FRAMES: usize = 2 * CYCLE_FRAMES;
    /// The frames the assertions are made over: the second cycle.
    const SECOND_CYCLE: std::ops::Range<usize> = CYCLE_FRAMES..FRAMES;

    /// The Smoothing the runs use: the parameter's shortest fade, which is
    /// "Smoothing at its minimum" — the crossfade windows the assertions
    /// step around are then as narrow as the plugin allows.
    const SMOOTH_MS: f32 = SMOOTH_MIN_MS;
    /// How many frames that fade lasts at [`SAMPLE_RATE`]: 0.5 ms of
    /// 32 768 Hz, rounded up.
    const FADE_FRAMES: usize = 17;

    /// The frame the block starts on, in the cycle the assertions are made
    /// over.
    const BLOCK_START: usize = CYCLE_FRAMES + (BLOCK_START_BEATS * BEAT_FRAMES) as usize;
    /// One frame past the end of that block — the first live frame after it.
    const BLOCK_END: usize = BLOCK_START + (BLOCK_BEATS * BEAT_FRAMES) as usize;

    /// The ramp trick's input at frame `frame`: the frame's own absolute
    /// position, exact in `f32` to 2²⁴ and so to every frame of every run
    /// here.
    fn input(frame: usize) -> f32 {
        frame as f32
    }

    /// The fixture: sixteen patterns, the first of them [`STEPS`] steps of
    /// [`RATE`] with one block of `action` over steps
    /// `BLOCK_STEP .. BLOCK_STEP + BLOCK_STEPS`.
    fn data_with(action: Action) -> StutterData {
        data_leveled(Some(action), &[])
    }

    /// The same fixture with both rows drawn on: `action` on the Time row as
    /// [`data_with`] puts it (or nothing, for a pattern that plays live
    /// throughout), and `volumes` written onto the Volume row from step 0 —
    /// the steps past its end keeping [`Pattern::DEFAULT_VOLUME`].
    fn data_leveled(action: Option<Action>, volumes: &[f32]) -> StutterData {
        let mut pattern = Pattern::new();
        pattern.set_rate(RATE);
        pattern.set_length(STEPS);
        if let Some(action) = action {
            assert_eq!(pattern.add_block(BLOCK_STEP, BLOCK_STEPS, action), Some(0));
        }
        for (step, &volume) in volumes.iter().enumerate() {
            pattern.set_volume(step as u16, volume);
        }

        StutterData::from_patterns([pattern])
    }

    /// An engine at the fixture's rate, with a scope of its own to write —
    /// [`Engine::scope`] is how the runs below read it back.
    fn new_engine() -> Engine {
        Engine::new(SAMPLE_RATE, Arc::new(Scope::new()))
    }

    /// The context for the `frames` frames starting at `frame`, at the
    /// fixture's rate, tempo and Smoothing, with `pattern` selected.
    fn ctx_at(frame: usize, frames: usize, pattern: Option<u8>) -> BlockCtx {
        BlockCtx::new(frame as f64 / BEAT_FRAMES, TEMPO, SAMPLE_RATE, frames)
            .with_pattern(pattern)
            .with_smooth_ms(SMOOTH_MS)
    }

    /// One run's output, and which of its frames a crossfade touched.
    struct Run {
        /// The left channel out, one entry per frame.
        left: Vec<f32>,
        /// The right channel out, which carries `−n` so that a channel
        /// swapped or copied over the other shows up.
        right: Vec<f32>,
        /// Whether a fade was running on either side of the frame — see
        /// [`run`].
        faded: Vec<bool>,
        /// Where the frame read, in absolute frames
        /// ([`Engine::read_position`]) — the offset maths' own answer,
        /// before the interpolator and the crossfade touch it.
        position: Vec<f64>,
        /// Which pattern the frame was played from
        /// ([`Engine::selection`]), as an index into the model.
        selection: Vec<Option<usize>>,
    }

    impl Run {
        /// Asserts that frame `frame` of the output is the history read
        /// `offset` beats behind it, and returns 1 if it asserted and 0 if
        /// the frame had to be skipped — so a caller can prove it checked a
        /// real number of frames rather than skipping the lot.
        ///
        /// Three kinds of frame are skipped:
        ///
        /// - **Inside a crossfade**, where the output is a mix of two
        ///   positions by design, so the reads are asserted away from the
        ///   crossfade windows.
        /// - **Before the first recorded frame**, which reads silence rather
        ///   than a continuation of the ramp.
        /// - **Within two frames of the newest recorded frame, at a
        ///   fractional position.** The interpolator reaches one frame
        ///   before and two frames after the position, and past the newest
        ///   those taps are silence, not the straight line the trick reads
        ///   the position off. A *whole* position needs no interpolation —
        ///   the polynomial's constant term is the frame itself — so those
        ///   stay asserted right up to live.
        fn assert_reads(&self, frame: usize, offset: f64) -> usize {
            if self.faded[frame] {
                return 0;
            }

            // The frame just recorded is the one `d = 0` reads, so "now" is
            // the frame's own position and the read is that many frames
            // behind it.
            let position = frame as f64 - offset * BEAT_FRAMES;
            if position < 0.0 {
                return 0;
            }
            if position.fract() != 0.0 && (position < 2.0 || position > frame as f64 - 2.0) {
                return 0;
            }

            assert_eq!(self.left[frame], position as f32, "left, frame {frame}");
            assert_eq!(self.right[frame], -(position as f32), "right, frame {frame}");
            1
        }
    }

    /// Runs [`FRAMES`] frames of the ramp input through the engine **one
    /// frame at a time**, from project beat 0 with `pattern` selected.
    ///
    /// One frame per block is what makes the crossfade windows visible: the
    /// reader is asked whether a fade is running on both sides of every
    /// frame, so a frame a fade touched is *known* rather than guessed at.
    /// It also proves the walk is per frame and not per block — and
    /// [`the_block_size_does_not_change_a_single_sample`] proves the
    /// real-sized blocks come out identical.
    fn run(data: &StutterData, pattern: Option<u8>) -> Run {
        run_selected(data, |_| pattern)
    }

    /// The same run with the `pattern` parameter moving underneath it:
    /// frame `frame` is played with `select(frame)` selected, which is how
    /// the switching tests put a parameter change in the middle of a step.
    fn run_selected(data: &StutterData, select: impl Fn(usize) -> Option<u8>) -> Run {
        run_contexts(data, |frame| ctx_at(frame, 1, select(frame)))
    }

    /// The same run with the **transport** moving underneath it: frame
    /// `frame` is played against `ctx_of(frame)`, a whole context rather
    /// than a selection, which is how the transport-edge tests stop the
    /// transport, start it again and jump the song position about.
    fn run_contexts(data: &StutterData, ctx_of: impl Fn(usize) -> BlockCtx) -> Run {
        let mut engine = new_engine();
        let mut out = Run {
            left: Vec::with_capacity(FRAMES),
            right: Vec::with_capacity(FRAMES),
            faded: Vec::with_capacity(FRAMES),
            position: Vec::with_capacity(FRAMES),
            selection: Vec::with_capacity(FRAMES),
        };

        for frame in 0..FRAMES {
            let fading_before = engine.reader().is_fading();
            let mut left = [input(frame)];
            let mut right = [-input(frame)];

            engine.run_block(&ctx_of(frame), data, &mut left, &mut right);

            // The invariant the whole crossfade rests on, checked on every
            // frame of every run in this module: never a third head.
            assert!(engine.reader().active_heads() <= 2, "frame {frame}: a third head");
            assert_eq!(engine.history().recorded(), frame as u64 + 1, "frame {frame}");

            out.left.push(left[0]);
            out.right.push(right[0]);
            out.faded.push(fading_before || engine.reader().is_fading());
            out.position.push(engine.read_position().expect("the frame read somewhere"));
            out.selection.push(engine.selection());
        }

        out
    }

    /// The same run with the Mix parameter at `mix` for every frame of it —
    /// the rest exactly as [`run`] drives it.
    fn run_mixed(data: &StutterData, pattern: Option<u8>, mix: f32) -> Run {
        run_contexts(data, |frame| ctx_at(frame, 1, pattern).with_mix(mix))
    }

    /// The same input run in blocks of `block` frames, returning the left
    /// channel only — the fade state is not visible per frame here, which is
    /// why [`run`] exists.
    fn run_in_blocks(data: &StutterData, pattern: Option<u8>, block: usize) -> Vec<f32> {
        run_in_blocks_with(data, block, |frame, frames| ctx_at(frame, frames, pattern))
    }

    /// The same, with a whole context per block: `ctx_of(frame, frames)` is
    /// the block starting at `frame` and `frames` long, which is how the mix
    /// tests move a parameter **between** blocks rather than inside one.
    fn run_in_blocks_with(
        data: &StutterData,
        block: usize,
        ctx_of: impl Fn(usize, usize) -> BlockCtx,
    ) -> Vec<f32> {
        let mut engine = new_engine();
        let mut out = Vec::with_capacity(FRAMES);
        let mut frame = 0;

        while frame < FRAMES {
            let frames = block.min(FRAMES - frame);
            let mut left: Vec<f32> = (frame..frame + frames).map(input).collect();
            let mut right: Vec<f32> = left.iter().map(|sample| -sample).collect();

            engine.run_block(&ctx_of(frame, frames), data, &mut left, &mut right);

            out.extend_from_slice(&left);
            frame += frames;
        }

        out
    }

    /// Where frame `frame` sits inside the fixture's block, in beats, or
    /// `None` where it plays live.
    ///
    /// Worked out from the grid's own numbers rather than from
    /// `pattern.rs`, so that the engine's placement and the fixture's have
    /// to agree.
    fn block_phase(frame: usize) -> Option<f64> {
        block_phase_at(frame as f64 / BEAT_FRAMES)
    }

    /// The same, for a frame played at project beat `beat` rather than at
    /// the beat its own position implies — which is every frame of a run
    /// whose transport was stopped or jumped underneath it.
    fn block_phase_at(beat: f64) -> Option<f64> {
        let phase = beat.rem_euclid(SPAN_BEATS) - BLOCK_START_BEATS;

        (0.0..BLOCK_BEATS).contains(&phase).then_some(phase)
    }

    /// Asserts the whole of `offsets` frame by frame over [`SECOND_CYCLE`],
    /// and that it did so for all but a handful of the cycle's frames —
    /// the handful being the crossfade windows and the frames whose read
    /// sits inside the interpolator's reach of live
    /// ([`Run::assert_reads`]).
    fn assert_cycle(run: &Run, allowed_skips: usize, offsets: impl Fn(usize) -> f64) {
        let mut checked = 0;
        for frame in SECOND_CYCLE {
            checked += run.assert_reads(frame, offsets(frame));
        }

        let skipped = CYCLE_FRAMES - checked;
        assert!(skipped <= allowed_skips, "{skipped} frames skipped, {checked} asserted");
    }

    /// Off is the bypass: every frame out is the frame in, bit for bit, and
    /// nothing ever fades because the read never leaves live.
    #[test]
    fn pattern_off_passes_the_input_through_bit_for_bit() {
        let run = run(&data_with(Action::repeat_at(RATE)), None);

        for frame in 0..FRAMES {
            assert_eq!(run.left[frame], input(frame), "frame {frame}");
            assert_eq!(run.right[frame], -input(frame), "frame {frame}");
            assert!(!run.faded[frame], "frame {frame} faded with nothing to fade");
        }
    }

    /// A pattern with nothing drawn on it is the same bypass: a step with no
    /// block over it is `d = 0`, and sixteen of those are a whole pattern of
    /// live audio.
    #[test]
    fn an_empty_pattern_plays_live_bit_for_bit() {
        let run = run(&StutterData::new(), Some(1));

        for frame in 0..FRAMES {
            assert_eq!(run.left[frame], input(frame), "frame {frame}");
            assert_eq!(run.right[frame], -input(frame), "frame {frame}");
            assert!(!run.faded[frame], "frame {frame} faded with nothing to fade");
        }
    }

    /// Repeat, source BlockStart: `d = p − (p mod ℓ)` — zero through the
    /// first slice, which therefore plays live, then one slice further back
    /// per slice.
    #[test]
    fn a_repeat_block_loops_the_slice_at_its_start() {
        let slice = NoteValue::Sixteenth;
        let action = Action::repeat(slice);
        let run = run(&data_with(action), Some(1));

        assert_cycle(&run, 5 * FADE_FRAMES, |frame| match block_phase(frame) {
            Some(phase) => phase - phase % slice.beats(),
            None => 0.0,
        });

        // The first slice is live, and joining it is not a jump: `d` is 0 on
        // both sides of the block's first frame, so there is nothing to
        // crossfade and the fixture's own input comes straight back.
        assert!(!run.faded[BLOCK_START], "the block's first frame should not fade");
        assert_eq!(run.left[BLOCK_START], input(BLOCK_START));
    }

    /// The periodicity of a slice loop, read straight off the output: with
    /// the slice loop running, a frame and the frame one slice earlier read
    /// the *same* position, so they are the same sample.
    #[test]
    fn a_repeats_output_is_periodic_with_the_slice() {
        let slice = NoteValue::Sixteenth;
        let action = Action::repeat(slice);
        let run = run(&data_with(action), Some(1));
        let period = (slice.beats() * BEAT_FRAMES) as usize;

        let mut checked = 0;
        for frame in BLOCK_START + period..BLOCK_END {
            if run.faded[frame] || run.faded[frame - period] {
                continue;
            }

            assert_eq!(run.left[frame], run.left[frame - period], "frame {frame}");
            checked += 1;
        }

        assert!(checked > period * 2, "only {checked} frames asserted");
    }

    /// Repeat from a spot one step (one slice) before the block: the same
    /// loop one slice further back,
    /// `d = ℓ + p − (p mod ℓ)`, so the block opens on the material that
    /// played just before it rather than on live audio.
    #[test]
    fn a_repeat_block_can_loop_the_slice_before_it() {
        let slice = NoteValue::Sixteenth;
        let spot = Source::Spot { step: BLOCK_STEP - 1, passes: 0 };
        let action = Action::repeat(slice).with_source(spot);
        let run = run(&data_with(action), Some(1));

        assert_cycle(&run, 6 * FADE_FRAMES, |frame| match block_phase(frame) {
            Some(phase) => slice.beats() + phase - phase % slice.beats(),
            None => 0.0,
        });

        // Here the block's first frame *is* a jump — a whole slice back —
        // so it is faded, unlike BlockStart's.
        assert!(run.faded[BLOCK_START], "the block's first frame should fade");
    }

    /// Half speed: `A = 0`, `d = p / 2`. An octave down, ending `B / 2`
    /// behind.
    #[test]
    fn half_speed_reads_at_half_the_phase() {
        let run = run(&data_with(Action::from_preset(SpeedPreset::HalfSpeed)), Some(1));

        assert_cycle(&run, 2 * FADE_FRAMES + 8, |frame| match block_phase(frame) {
            Some(phase) => phase / 2.0,
            None => 0.0,
        });
    }

    /// Double speed: `A = B`, `d = B − p`. It plays the `B` beats before the
    /// block and the block's own at twice the rate, and **ends exactly on
    /// live** — so the step after it joins without a jump, which is the one
    /// assertion here that is about the crossfade rather than the offset.
    #[test]
    fn double_speed_ends_exactly_on_live() {
        let run = run(&data_with(Action::from_preset(SpeedPreset::DoubleSpeed)), Some(1));

        assert_cycle(&run, 2 * FADE_FRAMES, |frame| match block_phase(frame) {
            Some(phase) => BLOCK_BEATS - phase,
            None => 0.0,
        });

        // The read head arrives at live as the block ends: the last frame
        // inside it reads one frame behind, the first frame after it reads
        // live, and a step of one frame is no jump.
        assert_eq!(run.left[BLOCK_END - 1], input(BLOCK_END - 2));
        assert_eq!(run.left[BLOCK_END], input(BLOCK_END));
        assert!(!run.faded[BLOCK_END], "double speed should join the next step without a fade");
    }

    /// Reverse: `A = 0`, `d = 2p` — the `B` beats before the block, played
    /// backwards at pitch. The read head starts at live and walks back two
    /// frames for every frame the song moves forward.
    #[test]
    fn reverse_plays_the_beats_before_the_block_backwards() {
        let run = run(&data_with(Action::from_preset(SpeedPreset::Reverse)), Some(1));

        assert_cycle(&run, 2 * FADE_FRAMES, |frame| match block_phase(frame) {
            Some(phase) => 2.0 * phase,
            None => 0.0,
        });

        // Backwards at pitch, in the plainest possible form: `d` grows by
        // two frames for every frame the song advances, so the read head
        // walks back through the recording one frame at a time —
        // consecutive output frames are consecutive input frames in the
        // other order.
        let inside = BLOCK_START + FADE_FRAMES + 1;
        assert_eq!(run.left[inside + 1], run.left[inside] - 1.0);
    }

    /// Tape stop: `A = 0`, `d = p² / (2B)` — starts seamlessly from live
    /// and winds down to frozen, ending `B / 2` behind.
    #[test]
    fn tape_stop_winds_down_from_live() {
        let run = run(&data_with(Action::from_preset(SpeedPreset::TapeStop)), Some(1));

        // The first frames of the block read within two frames of live,
        // where the interpolator has no ramp to work on: `d` only passes
        // two frames at `p² / (2B) × 16 384 ≥ 2`, i.e. 256 frames in.
        assert_cycle(&run, 2 * FADE_FRAMES + 256, |frame| match block_phase(frame) {
            Some(phase) => phase * phase / (2.0 * BLOCK_BEATS),
            None => 0.0,
        });

        // It opens on live, which is what "starts seamlessly" means, and so
        // needs no fade to get there.
        assert!(!run.faded[BLOCK_START], "a tape stop opens on live");
        assert_eq!(run.left[BLOCK_START], input(BLOCK_START));
    }

    /// Tape start: `A = 0`, `d = p − p² / (2B)` — starts frozen and winds
    /// up to speed, ending `B / 2` behind.
    #[test]
    fn tape_start_winds_up_to_speed() {
        let run = run(&data_with(Action::from_preset(SpeedPreset::TapeStart)), Some(1));

        assert_cycle(&run, 2 * FADE_FRAMES + 8, |frame| match block_phase(frame) {
            Some(phase) => phase - phase * phase / (2.0 * BLOCK_BEATS),
            None => 0.0,
        });

        assert!(!run.faded[BLOCK_START], "a tape start opens on live");
        assert_eq!(run.left[BLOCK_START], input(BLOCK_START));
    }

    /// A hand-drawn ramp whose vertex falls inside the block: 1.5 → −0.5,
    /// where `V(p) − p` peaks at `p* = B / 4` and the shift is `A = B / 16`.
    ///
    /// The preset identities all have `A` at an endpoint, so this is the one
    /// case that proves the engine uses the solved shift rather than a
    /// special case — and that it solves it once per block, through
    /// [`PathCache`], with the same answer every frame.
    #[test]
    fn a_free_ramp_carries_the_shift_that_keeps_it_behind_live() {
        let run = run(&data_with(Action::ramp(1.5, -0.5)), Some(1));

        // `d = p − V(p) + A` with `V(p) = 1.5p − p² / B` and `A = B / 16`,
        // which reduces to `(p − B/4)² / B`: a parabola sitting *on* zero at
        // the vertex. So the read passes within the interpolator's two
        // frames of live for the 362 frames around it — `(i − 4096)² /
        // 16 384 < 2` — and those are skipped, which is most of the
        // allowance here.
        assert_cycle(&run, 2 * FADE_FRAMES + 400, |frame| match block_phase(frame) {
            Some(phase) => {
                let travelled = 1.5 * phase - phase * phase / BLOCK_BEATS;
                phase - travelled + BLOCK_BEATS / 16.0
            }
            None => 0.0,
        });

        // The shift is the *smallest* one that works, so the ramp touches
        // live exactly once — at its vertex, a quarter of the way in.
        let vertex = BLOCK_START + (BLOCK_BEATS / 4.0 * BEAT_FRAMES) as usize;
        assert_eq!(run.left[vertex], input(vertex));
    }

    /// Every jump is crossfaded, and the fade is as long as Smoothing says:
    /// the three slice wraps inside a Repeat block and the drop back to live
    /// at its end are faded, and the frames either side of each window are
    /// not.
    ///
    /// That no jump ever runs a third head is asserted on every frame of
    /// every run in this module, inside [`run`].
    #[test]
    fn a_jump_crossfades_for_the_smoothing_time_and_no_longer() {
        let slice = NoteValue::Sixteenth;
        let action = Action::repeat(slice);
        let run = run(&data_with(action), Some(1));
        let period = (slice.beats() * BEAT_FRAMES) as usize;

        for jump in [BLOCK_START + period, BLOCK_START + 2 * period, BLOCK_END] {
            assert!(run.faded[jump], "frame {jump} is a jump and should fade");
            assert!(!run.faded[jump - 1], "frame {jump} faded a frame early");
            assert!(run.faded[jump + FADE_FRAMES - 1], "frame {jump}'s fade ended early");
            assert!(!run.faded[jump + FADE_FRAMES], "frame {jump}'s fade ran long");
        }
    }

    /// The block size is the host's business and nothing of the engine's:
    /// the same input in blocks of 1, 7, 64, 512 or 1024 frames comes out
    /// bit for bit the same, because the walk carries its read position
    /// across the seam and every beat is computed from the project position
    /// rather than from how far into a block a frame sits.
    #[test]
    fn the_block_size_does_not_change_a_single_sample() {
        let data = data_with(Action::from_preset(SpeedPreset::TapeStop));
        let frame_by_frame = run(&data, Some(1));

        for block in [7, 64, 512, 1024] {
            let blocked = run_in_blocks(&data, Some(1), block);
            assert_eq!(blocked, frame_by_frame.left, "blocks of {block} frames");
        }
    }

    /// A stopped transport plays live and the history keeps recording — and
    /// so does a playing one the host reported nonsense about, which is the
    /// same case. Nothing here may panic on a number that came from the
    /// host.
    #[test]
    fn a_block_with_no_usable_transport_plays_live_and_still_records() {
        let data = data_with(Action::from_preset(SpeedPreset::Reverse));
        let frames = 64;
        let contexts = [
            BlockCtx::stopped(SAMPLE_RATE, frames),
            ctx_at(0, frames, Some(1)).with_playing(false),
            BlockCtx::new(1.0, f64::NAN, SAMPLE_RATE, frames),
            BlockCtx::new(1.0, 0.0, SAMPLE_RATE, frames),
            BlockCtx::new(f64::NAN, TEMPO, SAMPLE_RATE, frames),
            BlockCtx::new(1.0, TEMPO, f32::NAN, frames),
        ];

        for (index, ctx) in contexts.into_iter().enumerate() {
            let mut engine = new_engine();
            let mut left: Vec<f32> = (0..frames).map(input).collect();
            let mut right: Vec<f32> = left.iter().map(|sample| -sample).collect();

            engine.run_block(&ctx.with_pattern(Some(1)), &data, &mut left, &mut right);

            assert_eq!(engine.history().recorded(), frames as u64, "context {index}");
            for frame in 0..frames {
                assert_eq!(left[frame], input(frame), "context {index}, frame {frame}");
                assert_eq!(right[frame], -input(frame), "context {index}, frame {frame}");
            }
        }
    }

    /// The last read position of a block is readable afterwards, and it is
    /// the offset maths' own answer — the number the scope and the tests can
    /// hold the walk to without going through the interpolator.
    #[test]
    fn the_engine_reports_where_it_last_read() {
        let data = data_with(Action::from_preset(SpeedPreset::Reverse));
        let mut engine = new_engine();
        assert_eq!(engine.read_position(), None);

        // Up to the frame half a beat into the block, where `d = 2p` is one
        // beat and the read is a beat's worth of frames behind.
        let frames = BLOCK_START + (0.5 * BEAT_FRAMES) as usize + 1;
        let mut left: Vec<f32> = (0..frames).map(input).collect();
        let mut right: Vec<f32> = left.iter().map(|sample| -sample).collect();
        engine.run_block(&ctx_at(0, frames, Some(1)), &data, &mut left, &mut right);

        let last = frames - 1;
        assert_eq!(engine.read_position(), Some(last as f64 - BEAT_FRAMES));

        // The Volume row's gain reads back the same way, and the fixture's
        // row is at unity — so the wet path was multiplied by exactly one.
        assert_eq!(engine.volume_gain(), Some(1.0));

        // And a reset forgets it, with the history and the heads.
        engine.reset();
        assert_eq!(engine.read_position(), None);
        assert_eq!(engine.volume_gain(), None);
        assert_eq!(engine.history().recorded(), 0);
        assert!(!engine.reader().is_fading());
    }

    /// Runs [`FRAMES`] frames of **constant** input through the engine one
    /// frame at a time at `smooth_ms` of Smoothing, returning the gain the
    /// Volume row applied to each.
    ///
    /// The ramp trick's sibling: with an input of exactly 1.0 and nothing on
    /// the Time row, every frame reads live — which is the frame just
    /// recorded, through the interpolator's constant term, so exactly 1.0 —
    /// and the output *is* the gain, frame by frame and bit for bit.
    fn run_gains(data: &StutterData, smooth_ms: f32) -> Vec<f32> {
        let mut engine = new_engine();
        let mut gains = Vec::with_capacity(FRAMES);

        for frame in 0..FRAMES {
            let mut left = [1.0];
            let mut right = [-1.0];

            let ctx = ctx_at(frame, 1, Some(1)).with_smooth_ms(smooth_ms);
            engine.run_block(&ctx, data, &mut left, &mut right);

            // The gain is one number applied to both channels, not two.
            assert_eq!(right[0], -left[0], "frame {frame}: the channels disagree");
            assert_eq!(engine.volume_gain(), Some(left[0]), "frame {frame}");
            gains.push(left[0]);
        }

        gains
    }

    /// The Volume row's own question ("a level change between steps ramps
    /// over the Smoothing time, not instantly"): a Volume row going from
    /// unity on step 0 to a quarter on step 1 is a straight line between the
    /// two, [`FADE_FRAMES`] long.
    ///
    /// Asserted at four places, which together are what "ramps, not steps"
    /// means: the frame before the boundary is still at the old level, the
    /// frame *on* the boundary has moved but nowhere near arrived, every
    /// frame of the window is one `0.75 / 17` stride on from the last, and
    /// the window's last frame is on the new level exactly — after which the
    /// gain sits there for the rest of the step.
    #[test]
    fn a_step_level_change_ramps_over_the_smoothing_time() {
        let data = data_leveled(None, &[1.0, 0.25]);
        let gains = run_gains(&data, SMOOTH_MS);

        // The start of step 1 in the cycle the assertions are made over.
        let boundary = CYCLE_FRAMES + STEP_FRAMES;
        let stride = (0.25 - 1.0) / FADE_FRAMES as f32;

        assert_eq!(gains[boundary - 1], 1.0, "the level moved before the step did");
        assert!(gains[boundary] < 1.0, "the level did not move at the boundary");
        assert!(gains[boundary] > 0.25, "the level arrived instantly");

        for step in 0..FADE_FRAMES {
            let frame = boundary + step;
            let walked = gains[frame] - gains[frame - 1];
            assert!(
                (walked - stride).abs() < 1e-6,
                "frame {frame}: the gain walked {walked}, not {stride}"
            );
        }

        // The far end is exact, and it is reached at the Smoothing time and
        // not before: one frame earlier the gain is still above the level.
        assert!(gains[boundary + FADE_FRAMES - 2] > 0.25, "the ramp arrived early");
        assert_eq!(gains[boundary + FADE_FRAMES - 1], 0.25, "the ramp missed its level");
        assert_eq!(gains[boundary + FADE_FRAMES], 0.25, "the ramp ran past its level");
        assert_eq!(gains[boundary + STEP_FRAMES - 1], 0.25, "the level did not hold");

        // And step 2 is back at the row's default, so the gain ramps up
        // again — the same line the other way.
        let next = boundary + STEP_FRAMES;
        assert!(gains[next] > 0.25, "the level did not move at the next boundary");
        assert_eq!(gains[next + FADE_FRAMES - 1], 1.0, "the ramp back missed unity");
    }

    /// The length of that ramp is the Smoothing parameter's, across its
    /// whole range: the gain lands on the new level at
    /// [`fade_frames_for`]'s frame and is still on its way one frame before
    /// — and a level of zero lands on exact silence, not near it.
    #[test]
    fn the_smoothing_parameter_sets_the_level_ramps_length() {
        let data = data_leveled(None, &[1.0, 0.0]);
        let boundary = CYCLE_FRAMES + STEP_FRAMES;

        for smooth_ms in [SMOOTH_MIN_MS, SMOOTH_DEFAULT_MS, SMOOTH_MAX_MS] {
            let frames = fade_frames_for(smooth_ms, f64::from(SAMPLE_RATE)) as usize;
            let gains = run_gains(&data, smooth_ms);

            assert_eq!(gains[boundary - 1], 1.0, "{smooth_ms} ms");
            assert!(gains[boundary + frames - 2] > 0.0, "{smooth_ms} ms: the ramp arrived early");
            assert_eq!(gains[boundary + frames - 1], 0.0, "{smooth_ms} ms: not exactly silent");
            assert_eq!(gains[boundary + STEP_FRAMES - 1], 0.0, "{smooth_ms} ms: silence held");
        }
    }

    /// The row levels the wet path, whatever the Time row put on it: a step
    /// at zero inside a Reverse block is silence, where the frames before it
    /// are the block's read.
    ///
    /// The Volume row is "the gate and chop" and is read per step from
    /// [`crate::pattern::Position::step`], so it is independent of the
    /// blocks: this is the half of that independence the Time row is drawn
    /// over, and [`a_step_level_change_ramps_over_the_smoothing_time`] is
    /// the half with nothing drawn at all.
    #[test]
    fn the_volume_row_levels_a_blocks_read_too() {
        let action = Action::from_preset(SpeedPreset::Reverse);
        // Step 3 — the second of the four the block covers — silenced.
        let data = data_leveled(Some(action), &[1.0, 1.0, 1.0, 0.0]);
        let run = run(&data, Some(1));

        let silent = CYCLE_FRAMES + 3 * STEP_FRAMES;

        // Reverse reads two beats back by the end of the step before, so the
        // frames leading into the gate are material and not silence already.
        assert!(run.left[silent - 1] != 0.0, "the gate had nothing to silence");

        for frame in silent + FADE_FRAMES..silent + STEP_FRAMES {
            assert_eq!(run.left[frame], 0.0, "frame {frame} is inside the gate");
            assert_eq!(run.right[frame], 0.0, "frame {frame} is inside the gate");
        }

        // And the step after it plays the block again, at the row's default.
        let after = silent + STEP_FRAMES + FADE_FRAMES;
        assert!(run.left[after] != 0.0, "the gate did not lift");
    }

    /// **The filter acts like the Volume row**: a pattern with a low-pass
    /// down at 20 Hz silences a tone well above it — on bare steps as on a
    /// block's read — while the pattern plays, and nothing is filtered once
    /// it stops; a pattern whose filter is off plays the tone untouched,
    /// bit for bit.
    #[test]
    fn a_patterns_filter_works_while_it_plays_and_not_after() {
        let tone = |n: usize| if n.is_multiple_of(2) { 0.5 } else { -0.5 };
        let mut filtered = Pattern::new();
        filtered.set_rate(RATE);
        filtered.set_length(STEPS);
        filtered.choose_filter(crate::pattern::Filter::LowPass);
        filtered.set_lane(Lane::Cutoff, [0.0; crate::pattern::MAX_STEPS]);
        let mut plain = filtered.clone();
        plain.choose_filter(crate::pattern::Filter::Off);
        let data = StutterData::from_patterns([filtered, plain]);

        // Pattern 1 for a cycle, then Off for a cycle, then pattern 2.
        let mut engine = new_engine();
        let block = 256;
        let total = 3 * CYCLE_FRAMES;
        let mut out = Vec::with_capacity(total);
        for start in (0..total).step_by(block) {
            let pattern = match start / CYCLE_FRAMES {
                0 => Some(1),
                1 => None,
                _ => Some(2),
            };
            let mut left: Vec<f32> = (start..start + block).map(tone).collect();
            let mut right = left.clone();
            engine.run_block(&ctx_at(start, block, pattern), &data, &mut left, &mut right);
            out.extend_from_slice(&left);
        }

        let settled = CYCLE_FRAMES / 2..CYCLE_FRAMES;
        let loudest = out[settled].iter().fold(0.0f32, |most, &y| most.max(y.abs()));
        assert!(loudest < 0.001, "the low-pass let {loudest} through");
        // Off: back to the tone exactly, once the fade out is over.
        let off = CYCLE_FRAMES + 4 * FADE_FRAMES..2 * CYCLE_FRAMES;
        assert!(off.clone().all(|n| out[n] == tone(n)), "Off still filtered");
        // A pattern with its filter off: the tone exactly, from the start.
        let plain = 2 * CYCLE_FRAMES + 1..3 * CYCLE_FRAMES;
        assert!(plain.clone().all(|n| out[n] == tone(n)), "the filter was off");
    }

    /// A level change arriving while the ramp is still running **restarts
    /// from the current gain**, as a jump mid-crossfade restarts from the
    /// current mix: the gain never steps, however fast the levels arrive.
    ///
    /// Run at [`MAX_TEMPO`], where a 1/16 step is 15 ms and so shorter than
    /// the 20 ms the Smoothing parameter can be set to: every ramp of this
    /// alternating row is interrupted before it arrives, which is the case
    /// the restart exists for. The levels are then never reached — the proof
    /// that the interruption really happened — and the output is a wobble
    /// around the middle rather than a square wave.
    #[test]
    fn a_level_change_mid_ramp_restarts_from_the_current_gain() {
        let data = data_leveled(None, &[1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0, 0.0]);
        let fade = fade_frames_for(SMOOTH_MAX_MS, f64::from(SAMPLE_RATE));
        let beats_per_frame = MAX_TEMPO / 60.0 / f64::from(SAMPLE_RATE);
        // Two cycles of the row, as every run in this module does.
        let frames = (2.0 * SPAN_BEATS / beats_per_frame) as usize;

        let mut engine = new_engine();
        let mut gains = Vec::with_capacity(frames);

        for frame in 0..frames {
            let mut left = [1.0];
            let mut right = [-1.0];
            let ctx = BlockCtx::new(frame as f64 * beats_per_frame, MAX_TEMPO, SAMPLE_RATE, 1)
                .with_pattern(Some(1))
                .with_smooth_ms(SMOOTH_MAX_MS);

            engine.run_block(&ctx, &data, &mut left, &mut right);
            gains.push(left[0]);
        }

        // No step bigger than one frame of the longest stride a ramp can
        // take, which is a whole unit of gain over the fade.
        let stride = 1.0 / fade as f32;
        for frame in 1..frames {
            let walked = gains[frame] - gains[frame - 1];
            assert!(
                walked.abs() <= stride + 1e-6,
                "frame {frame}: the gain stepped by {walked}, more than {stride}"
            );
        }

        // Every gain is a level the row could have asked for, and none of
        // them is a level it did: from the first boundary on, the ramps are
        // chasing each other and arrive nowhere.
        let chasing = &gains[(STEP_BEATS / beats_per_frame).ceil() as usize..];
        assert!(chasing.iter().all(|&gain| (0.0..=1.0).contains(&gain)), "a gain left the row");
        assert!(chasing.iter().all(|&gain| gain > 0.0), "a ramp reached silence");
        assert!(chasing.iter().all(|&gain| gain < 1.0), "a ramp reached unity");
    }

    /// The slice the switching fixture's **second** pattern repeats: 1/8,
    /// twice the fixture's step, taken from before a block that covers the
    /// whole cycle — so pattern 2 reads at least half a beat behind on
    /// every frame of the cycle and pattern 1 never does, whichever step
    /// the walk is on ([`data_pair`]).
    const OTHER_SLICE: NoteValue = NoteValue::Eighth;

    /// The frame the switching tests move the `pattern` parameter on: the
    /// middle of step 4 of the cycle the assertions are made over, which is
    /// the middle of a step and also the middle of pattern 1's block — a
    /// frame where there is something to interrupt.
    const SWITCH: usize = CYCLE_FRAMES + 4 * STEP_FRAMES + STEP_FRAMES / 2;
    /// The step boundary that change has to wait for: the start of step 5,
    /// half a step after [`SWITCH`].
    const BOUNDARY: usize = CYCLE_FRAMES + 5 * STEP_FRAMES;

    /// The fixture for a pattern change: two patterns that disagree about
    /// where to read on **every** frame of the cycle, so that which one is
    /// in effect is visible frame by frame.
    ///
    /// - Pattern 1 is [`data_with`]'s: one Repeat block of 1/16 slices over
    ///   steps `BLOCK_STEP .. BLOCK_STEP + BLOCK_STEPS`, live either side of
    ///   it, so `d` walks 0, 0, 0, ¼, ½, ¾, 0, 0 across the eight steps.
    /// - Pattern 2 has one Repeat block of [`OTHER_SLICE`] slices over all
    ///   [`STEPS`] steps, taken from *before* the block, so `d` walks ½, ½,
    ///   1, 1, 1½, 1½, 2, 2 — a different number on every step, and never
    ///   less than half a beat, which is 8192 frames and so far more than
    ///   [`MAX_READ_STEP`]: every switch between the two is a jump and must
    ///   be crossfaded.
    ///
    /// Patterns 3 to 16 are left as [`Pattern::new`] — nothing drawn on
    /// them, which is the empty-pattern bypass
    /// [`a_change_to_an_empty_pattern_passes_the_input_through_bit_for_bit`]
    /// switches to.
    fn data_pair() -> StutterData {
        let mut one = Pattern::new();
        one.set_rate(RATE);
        one.set_length(STEPS);
        let slice = NoteValue::Sixteenth;
        let inside = Action::repeat(slice);
        assert_eq!(one.add_block(BLOCK_STEP, BLOCK_STEPS, inside), Some(0));

        let mut two = Pattern::new();
        two.set_rate(RATE);
        two.set_length(STEPS);
        // Two steps (one 1/8) back from step 0: the end of the pass before.
        let spot = Source::Spot { step: STEPS - 2, passes: 0 };
        let before = Action::repeat(OTHER_SLICE).with_source(spot);
        assert_eq!(two.add_block(0, STEPS, before), Some(0));

        StutterData::from_patterns([one, two])
    }

    /// Where a frame reads when it is `offset` beats behind "now", in
    /// absolute frames — what [`Engine::read_position`] holds, and the one
    /// number that says which grid a frame was played from.
    fn read_at(frame: usize, offset: f64) -> f64 {
        frame as f64 - offset * BEAT_FRAMES
    }

    /// Pattern 1 of [`data_pair`]'s offset at `frame`: `d = p − (p mod ℓ)`
    /// inside its block, live either side of it.
    fn offset_one(frame: usize) -> f64 {
        repeat_offset_at(frame as f64 / BEAT_FRAMES)
    }

    /// Pattern 2 of [`data_pair`]'s offset at `frame`: `d = ℓ + p −
    /// (p mod ℓ)`, with `p` the phase in a block that covers the whole
    /// cycle and so is the pattern phase itself.
    fn offset_two(frame: usize) -> f64 {
        let slice = OTHER_SLICE.beats();
        let phase = frame as f64 / BEAT_FRAMES % SPAN_BEATS;

        slice + phase - phase % slice
    }

    /// The selector's own question (a change of the `pattern` parameter
    /// "takes effect at the next step boundary, never mid-step"): the
    /// parameter moves from pattern 1 to pattern 2 half way through step 4,
    /// and the walk goes on reading pattern 1's grid until step 5 begins.
    ///
    /// Asserted on [`Engine::read_position`] rather than on the output,
    /// because that is the offset maths' own answer and the two grids
    /// disagree about it on every frame of the wait — which the test checks,
    /// so that "still pattern 1's" cannot pass by the two happening to
    /// agree.
    #[test]
    fn a_pattern_change_mid_step_waits_for_the_next_step_boundary() {
        let data = data_pair();
        let run = run_selected(&data, |frame| Some(if frame < SWITCH { 1 } else { 2 }));

        for frame in SWITCH..BOUNDARY {
            assert_ne!(offset_one(frame), offset_two(frame), "frame {frame}: the grids agree");
            assert_eq!(run.selection[frame], Some(0), "frame {frame}: pattern 2 cut in early");
            assert_eq!(run.position[frame], read_at(frame, offset_one(frame)), "frame {frame}");
        }

        // And from the boundary on it is pattern 2's grid, for the rest of
        // the run — the change is not just late, it arrives.
        for frame in BOUNDARY..FRAMES {
            assert_eq!(run.selection[frame], Some(1), "frame {frame}");
            assert_eq!(run.position[frame], read_at(frame, offset_two(frame)), "frame {frame}");
        }

        // The switch moves the read head by a beat and a quarter, so it is
        // a jump and is crossfaded like any other — on the boundary frame and
        // not before it.
        assert!(run.faded[BOUNDARY], "the switch was not crossfaded");
        assert!(!run.faded[BOUNDARY - 1], "something faded before the boundary");
        assert!(!run.faded[BOUNDARY + FADE_FRAMES], "the switch's fade ran long");
    }

    /// A note's change does **not** wait: handed to the engine as a
    /// [`SelectionChanges`] on the frame it happens, pattern 2 is in effect
    /// from that very frame — half way through step 4, the frame the
    /// parameter's change above had to sit out until the boundary.
    #[test]
    fn a_note_change_takes_effect_on_its_own_frame() {
        let data = data_pair();
        let mut engine = new_engine();

        for frame in 0..FRAMES {
            let mut left = [input(frame)];
            let mut right = [-input(frame)];
            // What the shell hands over: the block's context says what was
            // playing when the block began, the change says what the note
            // made it, and the blocks after carry the held note.
            let mut changes = SelectionChanges::new();
            let held = match frame.cmp(&SWITCH) {
                Ordering::Less => 1,
                Ordering::Equal => {
                    changes.push(0, Some(2));
                    1
                }
                Ordering::Greater => 2,
            };
            let ctx = ctx_at(frame, 1, Some(held));
            engine.run_block_with(&ctx, &data, &changes, &mut left, &mut right);

            let (selection, offset) = match frame < SWITCH {
                true => (Some(0), offset_one(frame)),
                false => (Some(1), offset_two(frame)),
            };
            assert_eq!(engine.selection(), selection, "frame {frame}");
            assert_eq!(engine.read_position(), Some(read_at(frame, offset)), "frame {frame}");
        }
    }

    /// The change list keeps its frames in order, lets a second change on a
    /// frame replace the first, and when full lets the newest replace the
    /// last — so a block always ends on what its last note asked for.
    #[test]
    fn selection_changes_stay_ordered_and_bounded() {
        let mut changes = SelectionChanges::new();
        assert!(changes.as_slice().is_empty());

        changes.push(5, Some(1));
        changes.push(5, Some(2));
        changes.push(3, None);
        assert_eq!(changes.as_slice(), &[(5, None)]);

        changes.push(9, Some(4));
        assert_eq!(changes.as_slice(), &[(5, None), (9, Some(4))]);

        for frame in 10..10 + 2 * MAX_SELECTION_CHANGES {
            changes.push(frame, Some(3));
        }
        changes.push(1000, Some(7));
        assert_eq!(changes.as_slice().len(), MAX_SELECTION_CHANGES);
        assert_eq!(changes.as_slice().last(), Some(&(1000, Some(7))));
        assert!(changes.as_slice().windows(2).all(|pair| pair[0].0 < pair[1].0));
    }

    /// Off is a change like any other: asked for half way through step 4 it
    /// plays that step out and starts the bypass on the boundary, after
    /// which the input comes back **bit for bit**.
    #[test]
    fn a_change_to_off_waits_for_the_boundary_and_then_passes_through() {
        let data = data_pair();
        let run = run_selected(&data, |frame| (frame < SWITCH).then_some(1));

        for frame in SWITCH..BOUNDARY {
            assert_eq!(run.selection[frame], Some(0), "frame {frame}: Off cut in early");
            assert_eq!(run.position[frame], read_at(frame, offset_one(frame)), "frame {frame}");
            // The step it is playing out is a repeat of material a quarter
            // beat or more behind, so there is something for the bypass to
            // be visibly different from.
            assert!(run.position[frame] < frame as f64, "frame {frame} was already live");
        }

        for frame in BOUNDARY..FRAMES {
            assert_eq!(run.selection[frame], None, "frame {frame}");
            assert_eq!(run.position[frame], frame as f64, "frame {frame} did not go live");
        }

        // Bit for bit from the end of the crossfade off the old head: a
        // whole read position is the interpolator's constant term, and the
        // Volume row is at unity, so nothing is left to round.
        for frame in BOUNDARY + FADE_FRAMES..FRAMES {
            assert_eq!(run.left[frame], input(frame), "frame {frame}");
            assert_eq!(run.right[frame], -input(frame), "frame {frame}");
        }
    }

    /// An empty pattern is the same bypass as Off, and it arrives the same
    /// way: pattern 3 of the fixture has nothing drawn on it, and from the
    /// step boundary on the input comes back bit for bit.
    ///
    /// Note which grid decided the boundary — pattern 1's, the one playing.
    /// Pattern 3 is [`Pattern::new`] and so a *different* length and rate
    /// (sixteen steps of 1/16 against the fixture's eight), which is
    /// exactly why the arriving pattern's grid cannot be the one that
    /// decides: the step that must not be cut in half is the one being
    /// played.
    #[test]
    fn a_change_to_an_empty_pattern_passes_the_input_through_bit_for_bit() {
        let data = data_pair();
        let run = run_selected(&data, |frame| Some(if frame < SWITCH { 1 } else { 3 }));

        for frame in SWITCH..BOUNDARY {
            assert_eq!(run.selection[frame], Some(0), "frame {frame}: the change cut in early");
            assert_eq!(run.position[frame], read_at(frame, offset_one(frame)), "frame {frame}");
        }

        for frame in BOUNDARY..FRAMES {
            assert_eq!(run.selection[frame], Some(2), "frame {frame}");
            assert_eq!(run.position[frame], frame as f64, "frame {frame} did not go live");
        }

        for frame in BOUNDARY + FADE_FRAMES..FRAMES {
            assert_eq!(run.left[frame], input(frame), "frame {frame}");
            assert_eq!(run.right[frame], -input(frame), "frame {frame}");
        }
    }

    /// Off has no steps of its own to be mid-way through, so a pattern
    /// selected over it joins at the frame it arrives, at whatever place in
    /// the bar that is — the entry being a jump, which is crossfaded
    /// ([`Selector`] §Where it does not wait).
    #[test]
    fn off_takes_a_pattern_at_the_frame_it_arrives() {
        let data = data_pair();
        let run = run_selected(&data, |frame| (frame >= SWITCH).then_some(2));

        // Off up to then: live, bit for bit, nothing in effect.
        for frame in 0..SWITCH {
            assert_eq!(run.selection[frame], None, "frame {frame}");
            assert_eq!(run.position[frame], frame as f64, "frame {frame}");
            assert_eq!(run.left[frame], input(frame), "frame {frame}");
        }

        // And pattern 2 on that very frame, half a step before the next
        // boundary — that the switch is mid-step at all is a fact about the
        // two constants, and so checked where they are known.
        const { assert!(!SWITCH.is_multiple_of(STEP_FRAMES), "the switch is on a boundary") };
        for frame in SWITCH..FRAMES {
            assert_eq!(run.selection[frame], Some(1), "frame {frame}");
            assert_eq!(run.position[frame], read_at(frame, offset_two(frame)), "frame {frame}");
        }
        assert!(run.faded[SWITCH], "the entry was not crossfaded");
    }

    /// Pressing play is not a parameter change: a stopped transport leaves
    /// nothing in effect ([`Selector::reset`]), so the first frame that can
    /// be followed plays the selected pattern without waiting for a
    /// boundary — which it could not find anyway, there being no previous
    /// step to have left.
    #[test]
    fn starting_the_transport_takes_the_selection_without_waiting() {
        let data = data_pair();
        let mut engine = new_engine();

        // Stopped, with pattern 2 selected all along: live out, bit for
        // bit, and the history filling up behind it.
        for frame in 0..SWITCH {
            let mut left = [input(frame)];
            let mut right = [-input(frame)];
            let ctx = ctx_at(frame, 1, Some(2)).with_playing(false);

            engine.run_block(&ctx, &data, &mut left, &mut right);

            assert_eq!(engine.selection(), None, "frame {frame}");
            assert_eq!(left[0], input(frame), "frame {frame}");
            assert_eq!(right[0], -input(frame), "frame {frame}");
        }

        // The first playing frame is in the middle of a step and plays the
        // pattern there and then.
        let mut left = [input(SWITCH)];
        let mut right = [-input(SWITCH)];
        engine.run_block(&ctx_at(SWITCH, 1, Some(2)), &data, &mut left, &mut right);

        assert_eq!(engine.selection(), Some(1));
        assert_eq!(engine.read_position(), Some(read_at(SWITCH, offset_two(SWITCH))));
    }

    /// A switch mid-block changes which Time row the block indexes name, so
    /// the [`BlockPath`] solved for the block the walk was in is dropped
    /// with it: both patterns here have their one block at index 0, and the
    /// walk must read the new one's ramp from the boundary on.
    ///
    /// Held against a run of the second pattern alone, frame for frame:
    /// `d` is a function of the beat and the grid, so once the switch has
    /// happened the two runs can only disagree if something was kept that
    /// should not have been. A stale Reverse ramp over half speed's phase
    /// would read 2½ beats behind where the run reads ⅝ of one.
    #[test]
    fn a_switch_mid_block_solves_the_new_patterns_ramp() {
        let mut one = Pattern::new();
        one.set_rate(RATE);
        one.set_length(STEPS);
        let reverse = Action::from_preset(SpeedPreset::Reverse);
        assert_eq!(one.add_block(BLOCK_STEP, BLOCK_STEPS, reverse), Some(0));

        let mut two = Pattern::new();
        two.set_rate(RATE);
        two.set_length(STEPS);
        let half = Action::from_preset(SpeedPreset::HalfSpeed);
        assert_eq!(two.add_block(0, STEPS, half), Some(0));

        let data = StutterData::from_patterns([one, two]);
        let switched = run_selected(&data, |frame| Some(if frame < SWITCH { 1 } else { 2 }));
        let only_two = run(&data, Some(2));
        let only_one = run(&data, Some(1));

        for frame in SWITCH..BOUNDARY {
            assert_eq!(switched.position[frame], only_one.position[frame], "frame {frame}");
        }
        for frame in BOUNDARY..FRAMES {
            assert_ne!(only_one.position[frame], only_two.position[frame], "frame {frame}");
            assert_eq!(switched.position[frame], only_two.position[frame], "frame {frame}");
        }
    }

    /// Off is the bypass for the Volume row too: a pattern whose row is all
    /// silence gates nothing while the parameter says Off, because Off does
    /// not play that pattern — the input comes back bit for bit, which is
    /// the bypass's promise and would not survive a gain of anything but
    /// exactly one.
    #[test]
    fn pattern_off_ignores_the_volume_row() {
        let run = run(&data_leveled(None, &[0.0; STEPS as usize]), None);

        for frame in 0..FRAMES {
            assert_eq!(run.left[frame], input(frame), "frame {frame}");
            assert_eq!(run.right[frame], -input(frame), "frame {frame}");
        }
    }

    /// The frame the stop/start tests press stop on: half a beat into the
    /// fixture's block, in the cycle the assertions are made over, where a
    /// Reverse is reading a whole beat behind live — the "any offset" of
    /// "crossfading back if the read was offset" — and
    /// far enough from the block's own edges that the only jump anywhere
    /// near it is the transport's.
    const STOP: usize = BLOCK_START + (0.5 * BEAT_FRAMES) as usize;
    /// How long they leave it stopped: half a beat, which is long enough
    /// for the fade back to live to finish inside it and for the frames it
    /// records to be read back after it
    /// ([`the_history_records_while_the_transport_is_stopped`]).
    const STOPPED_FRAMES: usize = (0.5 * BEAT_FRAMES) as usize;
    /// The frame they press play again on.
    const RESUME: usize = STOP + STOPPED_FRAMES;

    /// The transport the stop/start tests drive: playing from beat 0 up to
    /// [`STOP`], stopped over [`STOP`]`..`[`RESUME`] with the song position
    /// frozen where it stopped, and playing on from that same position
    /// afterwards — which is what pressing stop and then play does.
    ///
    /// The position a stopped host reports is read by nothing (the block is
    /// stopped whatever it says), but freezing it is what makes the restart
    /// land mid-step, where the grid has something to be interrupted in.
    fn stopped_between(frame: usize) -> BlockCtx {
        match frame {
            frame if frame < STOP => ctx_at(frame, 1, Some(1)),
            frame if frame < RESUME => ctx_at(STOP, 1, Some(1)).with_playing(false),
            frame => ctx_at(frame - STOPPED_FRAMES, 1, Some(1)),
        }
    }

    /// The fixture Reverse block's offset at project beat `beat`: `d = 2p`
    /// inside the block, live either side of it.
    fn reverse_offset_at(beat: f64) -> f64 {
        match block_phase_at(beat) {
            Some(phase) => 2.0 * phase,
            None => 0.0,
        }
    }

    /// The fixture Repeat block's offset at project beat `beat`, for a slice
    /// of one step: `d = p − (p mod ℓ)` inside the block, live either side.
    fn repeat_offset_at(beat: f64) -> f64 {
        match block_phase_at(beat) {
            Some(phase) => phase - phase % STEP_BEATS,
            None => 0.0,
        }
    }

    /// Where the walk should read at frame `frame` of a [`stopped_between`]
    /// run of the Reverse fixture: the frame itself while the transport is
    /// stopped — live, which is the frame just recorded — and `d = 2p` at
    /// the beat the host reported while it is playing.
    fn stopped_read_at(frame: usize) -> f64 {
        let ctx = stopped_between(frame);

        match ctx.is_playing() {
            true => read_at(frame, reverse_offset_at(ctx.start_beat())),
            false => frame as f64,
        }
    }

    /// "**Stopped transport:** output live audio (crossfading back if the
    /// read was offset). The history buffer keeps recording."
    ///
    /// Stopped half a beat into a Reverse block, so the read is a whole beat
    /// behind live and moving backwards when the transport goes away. All
    /// three halves of the rule are asserted: the walk is on live from the
    /// stop frame on, it got there through one crossfade of the Smoothing
    /// parameter's length (whose first frame is the outgoing head alone,
    /// bit for bit, equal power starting at `cos 0`), and once that fade is
    /// over the output is the input bit for bit — a stopped plugin is a
    /// bypassed one. The recording is asserted on every frame by [`run`]'s
    /// runner, which holds `recorded` to the frame count whatever the
    /// transport said.
    #[test]
    fn stopping_the_transport_crossfades_back_to_live() {
        let data = data_with(Action::from_preset(SpeedPreset::Reverse));
        let run = run_contexts(&data, stopped_between);

        // Reading a beat behind live and nothing fading: the last played
        // frame of the block.
        // One frame short of half a beat into the block, so `d = 2p` is one
        // frame short of a whole beat and the read is that far behind.
        let played = stopped_read_at(STOP - 1);
        let behind = 2.0 * (0.5 * BEAT_FRAMES - 1.0);
        assert_eq!(played, STOP as f64 - 1.0 - behind, "the fixture moved");
        assert_eq!(run.position[STOP - 1], played, "frame {}", STOP - 1);
        assert!(!run.faded[STOP - 1], "the stop faded a frame early");

        // The stop frame reads live, and the fade from where the block had
        // got to is one Smoothing time long and no longer.
        assert_eq!(run.position[STOP], STOP as f64, "the stop did not go to live");
        assert!(run.faded[STOP], "the stop did not crossfade");
        assert!(run.faded[STOP + FADE_FRAMES - 1], "the stop's fade ended early");
        assert!(!run.faded[STOP + FADE_FRAMES], "the stop's fade ran long");

        // Its first frame is the Reverse head coasting on at its own speed,
        // which is one frame backwards per output frame, at a fade-in of
        // exactly zero.
        assert_eq!(run.left[STOP], (played - 1.0) as f32, "the fade did not start on the head");
        assert_eq!(run.right[STOP], -((played - 1.0) as f32), "frame {STOP}");

        // Every stopped frame is live, is played from no pattern at all, and
        // is the input bit for bit once the fade is done with.
        for frame in STOP..RESUME {
            assert_eq!(run.position[frame], frame as f64, "frame {frame} left live");
            assert_eq!(run.selection[frame], None, "frame {frame} played a pattern");
        }
        for frame in STOP + FADE_FRAMES..RESUME {
            assert_eq!(run.left[frame], input(frame), "frame {frame}");
            assert_eq!(run.right[frame], -input(frame), "frame {frame}");
        }
    }

    /// The other side of the same seam: pressing play again puts the grid
    /// back under the walk at the frame it arrives — there is no step to
    /// wait out, the stopped stretch having left nothing in effect — and the
    /// jump back to the block's offset is crossfaded exactly as the jump to
    /// live was.
    ///
    /// Asserted frame by frame over the whole of the restarted block, not
    /// just at the seam: the beats the host reports after the restart are
    /// behind the frames they are played on (the song stood still for half
    /// a beat while the plugin kept recording), so a walk that had kept any
    /// of its own position across the stop would read the wrong material
    /// for the rest of the block.
    #[test]
    fn starting_the_transport_crossfades_back_into_the_pattern() {
        let data = data_with(Action::from_preset(SpeedPreset::Reverse));
        let run = run_contexts(&data, stopped_between);

        // Live before, and a beat behind live after — the offset the frozen
        // position puts the walk back at.
        let resumed = stopped_read_at(RESUME);
        assert_eq!(resumed, RESUME as f64 - BEAT_FRAMES, "the fixture moved");
        assert_eq!(run.position[RESUME - 1], RESUME as f64 - 1.0, "frame {}", RESUME - 1);
        assert_eq!(run.position[RESUME], resumed, "frame {RESUME}");
        assert_eq!(run.selection[RESUME], Some(0), "the restart waited for a boundary");

        assert!(!run.faded[RESUME - 1], "the restart faded a frame early");
        assert!(run.faded[RESUME], "the restart did not crossfade");
        assert!(run.faded[RESUME + FADE_FRAMES - 1], "the restart's fade ended early");
        assert!(!run.faded[RESUME + FADE_FRAMES], "the restart's fade ran long");

        // And the rest of the block is the Reverse read of the beats the
        // host is now reporting, frame for frame.
        let mut checked = 0;
        for frame in RESUME..FRAMES {
            let beat = stopped_between(frame).start_beat();
            checked += run.assert_reads(frame, reverse_offset_at(beat));
        }

        let skipped = FRAMES - RESUME - checked;
        assert!(skipped <= FADE_FRAMES + 1, "{skipped} frames skipped, {checked} asserted");
    }

    /// "The history buffer keeps recording", in the form that matters: the
    /// frames a stopped transport recorded are material the next block can
    /// play.
    ///
    /// The restart lands half way through the fixture's Repeat block, where
    /// `d` is half a beat — which is exactly the half-beat the transport was
    /// stopped for. So the first slice after the restart replays the stopped
    /// stretch frame by frame, and the ramp trick makes that readable
    /// straight off the output: frame `RESUME + k` must come back as the
    /// sample that was recorded while nothing was playing at `STOP + k`.
    #[test]
    fn the_history_records_while_the_transport_is_stopped() {
        let slice = NoteValue::Sixteenth;
        let action = Action::repeat(slice);
        let run = run_contexts(&data_with(action), stopped_between);
        let slice_frames = (slice.beats() * BEAT_FRAMES) as usize;

        for offset in FADE_FRAMES..slice_frames {
            let frame = RESUME + offset;
            let read = STOP + offset;

            // What makes this test the one it claims to be: the material is
            // inside the stretch the transport was stopped for.
            assert!((STOP..RESUME).contains(&read), "frame {frame} read outside the stop");
            assert_eq!(run.position[frame], read as f64, "frame {frame}");
            assert_eq!(run.left[frame], input(read), "frame {frame}");
            assert_eq!(run.right[frame], -input(read), "frame {frame}");
        }
    }

    /// The frame the jump tests move the song position on: the middle of the
    /// second slice of the fixture's block, in the cycle the assertions are
    /// made over — a frame where the walk is reading behind live and its own
    /// next wrap is half a step away, so a fade seen there is the
    /// transport's and nothing else's.
    const JUMP_AT: usize = BLOCK_START + STEP_FRAMES + STEP_FRAMES / 2;
    /// The beat the song has got to by then.
    const JUMP_BEAT: f64 = JUMP_AT as f64 / BEAT_FRAMES;

    /// The transport of a run the host moves the song position in: beat 0
    /// onwards up to [`JUMP_AT`], and `to` onwards from there.
    ///
    /// A loop jump when `to` is behind where the song had got to and a
    /// relocation when it is ahead — the same discontinuity to the engine
    /// either way, which is why the tests below run the same assertions
    /// over both.
    fn jumped_to(to: f64) -> impl Fn(usize) -> BlockCtx {
        move |frame| {
            let beat = match frame < JUMP_AT {
                true => frame as f64 / BEAT_FRAMES,
                false => to + (frame - JUMP_AT) as f64 / BEAT_FRAMES,
            };

            BlockCtx::new(beat, TEMPO, SAMPLE_RATE, 1)
                .with_pattern(Some(1))
                .with_smooth_ms(SMOOTH_MS)
        }
    }

    /// "**Loop jumps and relocations** (host position discontinuity vs. the
    /// position the engine predicted from last block): treat as a jump —
    /// crossfade".
    ///
    /// Both directions, over the same Repeat block: back to the block's
    /// first slice (a loop whose end the song ran into) and forward to its
    /// last (a click in the arranger). Both land on a slice the walk was not
    /// reading, so `d` moves by more than [`MAX_READ_STEP`] and the fade is
    /// the jump's own; and from the fade on, every frame of the rest of the
    /// run reads the grid at the beat the host is *now* reporting, which is
    /// what proves the walk followed the jump rather than coasting through
    /// it.
    #[test]
    fn a_loop_jump_or_relocation_crossfades_and_follows_the_new_position() {
        let slice = NoteValue::Sixteenth;
        let action = Action::repeat(slice);
        let data = data_with(action);

        // A quarter of a step into the block's first slice, where the repeat
        // plays live, and three quarters of the way into its last, where it
        // reads three quarters of a beat behind.
        let back = SPAN_BEATS + BLOCK_START_BEATS + STEP_BEATS / 4.0;
        let forward = SPAN_BEATS + BLOCK_START_BEATS + 2.75 * STEP_BEATS;

        for to in [back, forward] {
            let transport = jumped_to(to);
            let run = run_contexts(&data, &transport);

            let before = read_at(JUMP_AT - 1, repeat_offset_at(JUMP_BEAT));
            let after = read_at(JUMP_AT, repeat_offset_at(to));
            assert_eq!(run.position[JUMP_AT - 1], before, "to {to}: before the jump");
            assert_eq!(run.position[JUMP_AT], after, "to {to}: after the jump");
            assert!((after - before).abs() > MAX_READ_STEP, "to {to}: the read did not move");

            assert!(!run.faded[JUMP_AT - 1], "to {to}: faded a frame early");
            assert!(run.faded[JUMP_AT], "to {to}: the jump did not crossfade");
            assert!(run.faded[JUMP_AT + FADE_FRAMES - 1], "to {to}: the fade ended early");
            assert!(!run.faded[JUMP_AT + FADE_FRAMES], "to {to}: the fade ran long");

            // The jump frame is the head the jump cut loose, coasting on at
            // the speed it had — one frame forward per output frame inside a
            // slice — at a fade-in of exactly zero.
            assert_eq!(run.left[JUMP_AT], (before + 1.0) as f32, "to {to}: the fade's first frame");

            // And the rest of the run is the grid the host jumped into.
            let mut checked = 0;
            for frame in JUMP_AT..FRAMES {
                let beat = transport(frame).start_beat();
                checked += run.assert_reads(frame, repeat_offset_at(beat));
            }

            // Six fades' worth of slack: the jump itself, and at most five
            // wraps or block edges in the 1.1 beats of grid left after it.
            let skipped = FRAMES - JUMP_AT - checked;
            assert!(skipped <= 6 * (FADE_FRAMES + 1), "to {to}: {skipped} skipped, {checked} on");
        }
    }

    /// The other half of that rule, which is where this engine reads it more
    /// tightly than the words: a jump the grid lines up with is **not**
    /// faded, because nothing moved to fade.
    ///
    /// The host jumps back exactly one pattern cycle — what a loop whose
    /// length is a whole number of cycles does at its end, which is the
    /// ordinary case in a project — so the phase, the step and `d` are all
    /// the ones the walk already had. The read position carries straight on
    /// one frame at a time and the output carries the input's ramp with it,
    /// unbroken. Fading here would be worse than not: two heads over the
    /// same material sum to +3 dB half way through an equal-power ramp
    /// ([`MAX_READ_STEP`]).
    #[test]
    fn a_loop_jump_the_grid_lines_up_with_plays_straight_through() {
        let slice = NoteValue::Sixteenth;
        let action = Action::repeat(slice);
        let transport = jumped_to(JUMP_BEAT - SPAN_BEATS);
        let run = run_contexts(&data_with(action), &transport);

        assert!(!run.faded[JUMP_AT], "a jump the grid lines up with was crossfaded");
        assert!(!run.faded[JUMP_AT + 1], "the jump started a fade a frame late");
        assert_eq!(
            run.position[JUMP_AT],
            run.position[JUMP_AT - 1] + 1.0,
            "the read position moved across a jump that changed nothing"
        );
        assert_eq!(run.left[JUMP_AT], run.left[JUMP_AT - 1] + 1.0, "the output stepped");
        assert_eq!(run.right[JUMP_AT], run.right[JUMP_AT - 1] - 1.0, "the output stepped");

        // And it goes on reading the grid at the beats the host now reports,
        // which after the jump are a cycle behind the frames they play on.
        let mut checked = 0;
        for frame in JUMP_AT..FRAMES {
            let beat = transport(frame).start_beat();
            checked += run.assert_reads(frame, repeat_offset_at(beat));
        }

        let skipped = FRAMES - JUMP_AT - checked;
        assert!(skipped <= 6 * (FADE_FRAMES + 1), "{skipped} frames skipped, {checked} asserted");
    }

    /// The block size the mix-automation test runs at, and the frame the
    /// parameter moves on: a whole number of blocks in, and inside the
    /// fixture's Time-row block (its second slice, so the wet path is a
    /// slice behind the dry one and the two are never equal).
    const MIX_BLOCK: usize = 512;
    const MIX_MOVE_AT: usize = BLOCK_START + STEP_FRAMES;

    /// `mix` at 0 is the input **bit for bit**, over a pattern that is very
    /// much not playing the input: a Repeat looping its slices with a Volume
    /// row cutting levels out of them.
    ///
    /// Both rows are drawn on deliberately. A Volume row left at unity would
    /// let the dry frame come back through a gain of exactly `1.0` by
    /// accident (which is what makes Off bit-exact), and this has to be the
    /// mix doing it and not the rest of the walk.
    #[test]
    fn mix_at_zero_is_the_input_bit_for_bit() {
        let slice = NoteValue::Sixteenth;
        let action = Action::repeat(slice);
        let data = data_leveled(Some(action), &[1.0, 0.5, 0.25, 0.75]);
        let dry = run_mixed(&data, Some(1), 0.0);
        let wet = run_mixed(&data, Some(1), 1.0);

        let mut differed = 0;
        for frame in 0..FRAMES {
            // On the bits, not on the value: `==` reads `−0.0` as `+0.0`,
            // and the sign of the zeroes is part of "bit for bit" — frame 0
            // of the right channel is a `−0.0`, which the arithmetic form of
            // the blend would hand back positive ([`blend`]).
            assert_eq!(dry.left[frame].to_bits(), input(frame).to_bits(), "left, frame {frame}");
            assert_eq!(dry.right[frame].to_bits(), (-input(frame)).to_bits(), "right, {frame}");
            differed += usize::from(wet.left[frame] != dry.left[frame]);
        }

        // Not vacuous: over most of the run the wet path had something else
        // to say, and the mix is the only reason none of it was heard.
        assert!(differed > CYCLE_FRAMES / 2, "only {differed} frames were not the input anyway");
    }

    /// `mix` at 1 is the wet path, and "the wet path" is pinned the way
    /// every other test in this module pins it — the ramp trick, frame by
    /// frame ([`assert_cycle`]): the output *is* the history position the
    /// offset maths chose, not the input and not a blend of the two.
    ///
    /// Reverse rather than a Repeat, because a Repeat's first slice reads
    /// live and would pass the input through on its own.
    #[test]
    fn mix_at_one_is_the_wet_path() {
        let data = data_with(Action::from_preset(SpeedPreset::Reverse));
        let wet = run_mixed(&data, Some(1), 1.0);

        assert_cycle(&wet, 2 * FADE_FRAMES, |frame| match block_phase(frame) {
            Some(phase) => 2.0 * phase,
            None => 0.0,
        });

        // Inside the block that is the recording played backwards, which is
        // nothing like the input it was recorded from.
        let inside = BLOCK_START + FADE_FRAMES + 1;
        assert_eq!(wet.left[inside + 1], wet.left[inside] - 1.0);
        assert!(wet.left[inside] != input(inside), "the wet path was the input after all");

        // And it is the same signal the parameter's own default plays, since
        // that default is fully wet (`MIX_DEFAULT`).
        let unmixed = run(&data, Some(1));
        assert_eq!(wet.left, unmixed.left);
        assert_eq!(wet.right, unmixed.right);
    }

    /// Between the ends it is the formula and nothing else: `dry ×
    /// (1 − mix) + wet × mix`, with the dry signal the fixture's input and
    /// the wet signal the same run at `mix` 1 — bit for bit, because the
    /// mix the engine holds over a block of one steady value is that value
    /// exactly ([`MixRamp`] §flat) and the arithmetic is then the same
    /// arithmetic.
    #[test]
    fn a_mix_between_the_ends_blends_the_two_paths() {
        let data = data_with(Action::from_preset(SpeedPreset::Reverse));
        let wet = run_mixed(&data, Some(1), 1.0);

        for mix in [0.25_f32, 0.5, 0.75] {
            let run = run_mixed(&data, Some(1), mix);

            for frame in 0..FRAMES {
                let dry = input(frame);
                let expected = dry * (1.0 - mix) + wet.left[frame] * mix;
                assert_eq!(run.left[frame], expected, "mix {mix}, left, frame {frame}");

                let expected = -dry * (1.0 - mix) + wet.right[frame] * mix;
                assert_eq!(run.right[frame], expected, "mix {mix}, right, frame {frame}");
            }
        }
    }

    /// `mix` at 0 is an output choice, not a bypass of the engine: the walk
    /// underneath it runs exactly as it does at `mix` 1 — the same read
    /// positions, the same crossfades, the same selection, and the history
    /// recording every frame ([`run_contexts`] checks that one on every
    /// frame of both runs).
    ///
    /// Which is what makes turning the mix back up play the pattern where
    /// the song has got to, instead of starting it again from wherever the
    /// knob stopped being 0.
    #[test]
    fn mix_at_zero_keeps_the_pattern_playing_underneath() {
        let data = data_with(Action::from_preset(SpeedPreset::TapeStop));
        let dry = run_mixed(&data, Some(1), 0.0);
        let wet = run_mixed(&data, Some(1), 1.0);

        assert_eq!(dry.position, wet.position, "the read head moved differently at mix 0");
        assert_eq!(dry.faded, wet.faded, "the crossfades differed at mix 0");
        assert_eq!(dry.selection, wet.selection, "the pattern selection differed at mix 0");
    }

    /// A mix the host moves between two blocks arrives as a line across the
    /// block it lands in, not as a step at its first frame ([`MixRamp`]).
    ///
    /// The parameter goes from 0 to 1 at a block boundary, and all three
    /// stretches are exact: dry bit for bit before it, the line across the
    /// block it lands in — whose own first frame is still fully dry, which
    /// is the join the ramp exists for — and the wet path bit for bit from
    /// the next block on, because the line reaches `to` at the frame the
    /// next block starts.
    #[test]
    fn a_mix_move_between_blocks_ramps_across_the_block() {
        assert_eq!(MIX_MOVE_AT % MIX_BLOCK, 0, "the move must land on a block boundary");

        let slice = NoteValue::Sixteenth;
        let action = Action::repeat(slice);
        let data = data_with(action);
        let wet = run_in_blocks(&data, Some(1), MIX_BLOCK);
        let moved = run_in_blocks_with(&data, MIX_BLOCK, |frame, frames| {
            let mix = match frame < MIX_MOVE_AT {
                true => 0.0,
                false => 1.0,
            };
            ctx_at(frame, frames, Some(1)).with_mix(mix)
        });

        for (frame, &out) in moved.iter().take(MIX_MOVE_AT).enumerate() {
            assert_eq!(out, input(frame), "frame {frame}, before the move");
        }

        let mut blended = 0;
        for offset in 0..MIX_BLOCK {
            let frame = MIX_MOVE_AT + offset;
            // The line: `(to − from) / frames × offset` with both ends at
            // the parameter's own, which at a block of 512 frames is an
            // exact binary fraction on every frame of it.
            let mix = offset as f32 / MIX_BLOCK as f32;
            let dry = input(frame);
            let expected = if mix == 0.0 { dry } else { dry * (1.0 - mix) + wet[frame] * mix };

            assert_eq!(moved[frame], expected, "frame {frame}, {offset} frames into the ramp");
            blended += usize::from(wet[frame] != dry);
        }

        // Spelled out, because it is the point: the ramp's first frame is
        // where the *last* block left the mix, so the boundary itself does
        // not step — and there was a wet path to step to.
        assert_eq!(moved[MIX_MOVE_AT], input(MIX_MOVE_AT), "the mix stepped at the boundary");
        assert!(blended > MIX_BLOCK / 2, "only {blended} frames of the ramp had a wet path");

        for frame in MIX_MOVE_AT + MIX_BLOCK..FRAMES {
            assert_eq!(moved[frame], wet[frame], "frame {frame}, after the ramp");
        }
    }

    /// [`MixRamp`]'s two promises on their own: a mix that is not moving is
    /// the value itself on every frame, and a line reaches its far end
    /// exactly at the frame the next block starts.
    #[test]
    fn the_mix_ramp_is_a_line_that_lands_on_both_ends() {
        for mix in [0.0, 0.5, 1.0] {
            let flat = MixRamp::flat(mix);
            for frame in [0, 1, MIX_BLOCK, 100_000] {
                assert_eq!(flat.at(frame), mix, "flat at {mix}, frame {frame}");
            }
        }

        let line = MixRamp::across(0.0, 1.0, MIX_BLOCK);
        assert_eq!(line.at(0), 0.0);
        assert_eq!(line.at(MIX_BLOCK / 2), 0.5);
        assert_eq!(line.at(MIX_BLOCK), 1.0);
        // Past the end it is held, never walked out of the range `blend`
        // trusts.
        assert_eq!(line.at(2 * MIX_BLOCK), 1.0);

        let down = MixRamp::across(1.0, 0.0, 4);
        let frames = [down.at(0), down.at(1), down.at(2), down.at(3), down.at(4)];
        assert_eq!(frames, [1.0, 0.75, 0.5, 0.25, 0.0]);

        // Nothing to draw: equal ends, and a block of no frames.
        assert_eq!(MixRamp::across(0.25, 0.25, MIX_BLOCK), MixRamp::flat(0.25));
        assert_eq!(MixRamp::across(0.0, 1.0, 0), MixRamp::flat(1.0));

        // Hostile ends are clamped before the step is taken, so the line
        // between them is finite and inside the range.
        let hostile = MixRamp::across(f32::NAN, 2.0, 8);
        assert_eq!(hostile.at(0), 0.0);
        assert_eq!(hostile.at(8), 1.0);
        assert_eq!(MixRamp::across(-1.0, f32::NAN, 8), MixRamp::flat(0.0));
    }

    /// [`blend`]'s own contract: the formula in the middle, and the two ends
    /// taken as they are rather than computed — down to the sign of a zero
    /// and through values the arithmetic would have poisoned.
    #[test]
    fn blending_keeps_both_ends_exact() {
        assert_eq!(blend(1.0, 3.0, 0.5), 2.0);
        assert_eq!(blend(0.0, 1.0, 0.25), 0.25);
        assert_eq!(blend(2.0, 0.0, 0.75), 0.5);

        assert_eq!(blend(7.0, -3.0, 0.0), 7.0);
        assert_eq!(blend(7.0, -3.0, 1.0), -3.0);

        // `dry × 1.0 + wet × 0.0` is `dry + 0.0`, which is `+0.0` for a dry
        // `−0.0`: a sign bit lost on every silent frame.
        assert_eq!(blend(-0.0, 1.0, 0.0).to_bits(), (-0.0_f32).to_bits());
        assert_eq!(blend(1.0, -0.0, 1.0).to_bits(), (-0.0_f32).to_bits());

        // And the signal that is not heard stays out of the output, where a
        // multiply by zero would have made a NaN of it.
        assert_eq!(blend(7.0, f32::INFINITY, 0.0), 7.0);
        assert_eq!(blend(7.0, f32::NAN, 0.0), 7.0);
        assert_eq!(blend(f32::NAN, -3.0, 1.0), -3.0);
    }

    /// [`flush`]'s own contract: the subnormals go, with their sign, and
    /// nothing else is touched at all.
    #[test]
    fn flushing_takes_the_subnormals_and_leaves_the_rest() {
        // The ends of the subnormal range and one in the middle of it, both
        // signs, every one of them a zero out with its sign kept.
        for sample in [f32::from_bits(1), 5.4e-39, 1.1754942e-38] {
            assert!(sample.is_subnormal(), "{sample:e} is not a subnormal");
            assert_eq!(flush(sample).to_bits(), 0.0_f32.to_bits(), "{sample:e}");
            assert_eq!(flush(-sample).to_bits(), (-0.0_f32).to_bits(), "-{sample:e}");
        }

        // The smallest normal is not a subnormal and does not move, and
        // neither does anything above it.
        assert_eq!(flush(f32::MIN_POSITIVE), f32::MIN_POSITIVE);
        assert_eq!(flush(-f32::MIN_POSITIVE), -f32::MIN_POSITIVE);
        assert_eq!(flush(0.25), 0.25);
        assert_eq!(flush(f32::MAX), f32::MAX);

        // Both zeroes keep their sign — `is_subnormal` is false for them,
        // so they are not even the flushing case.
        assert_eq!(flush(0.0).to_bits(), 0.0_f32.to_bits());
        assert_eq!(flush(-0.0).to_bits(), (-0.0_f32).to_bits());

        // And what a host can send that is not a number at all is passed
        // on as it is, the way `blend`'s two ends pass it on.
        assert_eq!(flush(f32::INFINITY), f32::INFINITY);
        assert_eq!(flush(f32::NEG_INFINITY), f32::NEG_INFINITY);
        assert!(flush(f32::NAN).is_nan());
    }

    /// The fuzzer's case: `clap-validator fuzz` feeds whole blocks of
    /// denormal audio and fails a plugin whose output holds a subnormal
    /// value. An effect that passes its input through hands them straight
    /// back, so the engine flushes — nothing it writes may be subnormal at
    /// any mix, with the Volume row and a Repeat's reads underneath it, and
    /// `mix` at 0 answers a denormal frame with a zero of that frame's own
    /// sign.
    #[test]
    fn denormal_input_never_comes_back_subnormal() {
        // Spread across the subnormal range and alternating in sign, which
        // is the shape the nine seeds that failed at 6.3 came in: a random
        // mantissa over a zero exponent.
        let denormal = |frame: usize| {
            let bits = 1 + (frame as u32).wrapping_mul(7919) % 0x007f_fffe;
            let sample = f32::from_bits(bits);
            match frame % 2 {
                0 => sample,
                _ => -sample,
            }
        };

        let data = data_leveled(Some(Action::repeat_at(RATE)), &[0.0, 0.5, 1.0, 0.25]);
        for mix in [0.0, 0.25, 0.5, 1.0] {
            let mut engine = new_engine();
            for frame in 0..FRAMES {
                let dry = denormal(frame);
                assert!(dry.is_subnormal(), "frame {frame}: the fixture is not denormal");
                let (mut left, mut right) = ([dry], [-dry]);
                let ctx = ctx_at(frame, 1, Some(1)).with_mix(mix);
                engine.run_block(&ctx, &data, &mut left, &mut right);

                for (side, out) in [("left", left[0]), ("right", right[0])] {
                    assert!(!out.is_subnormal(), "{side}, mix {mix}, frame {frame}: {out:e}");
                    assert!(out.is_finite(), "{side}, mix {mix}, frame {frame}: {out}");
                }

                // The dry path at `mix` 0: the flush is the whole of what
                // happened to the frame, so the sign survives it.
                if mix == 0.0 {
                    let zero = 0.0_f32.copysign(dry);
                    assert_eq!(left[0].to_bits(), zero.to_bits(), "left, frame {frame}");
                    assert_eq!(right[0].to_bits(), (-zero).to_bits(), "right, frame {frame}");
                }
            }
        }
    }

    /// Frames in one scope column of the fixture's cycle: [`CYCLE_FRAMES`]
    /// over [`COLUMNS`], which divides exactly here — 64 — so every column
    /// spans a whole number of frames and the summaries below are exact
    /// rather than nearly so. The runs assert the division as well, since a
    /// fixture that stopped dividing would make every assertion softer
    /// without failing.
    const COLUMN_FRAMES: usize = CYCLE_FRAMES / COLUMNS;

    /// The summary the ramp input leaves in `column` of cycle `cycle`.
    ///
    /// The ramp rises through the column and the right channel carries `−n`,
    /// so the column spans the last frame of it either side of zero: a
    /// `Column` the scope can be compared against with `assert_eq!`, because
    /// nothing on the way in rounded it.
    fn ramp_column(cycle: usize, column: usize) -> Column {
        let last = (cycle * CYCLE_FRAMES + column * COLUMN_FRAMES + COLUMN_FRAMES - 1) as f32;

        Column { min: -last, max: last }
    }

    /// Runs `frames` frames of the ramp input through one engine, a frame at
    /// a time against `ctx_of(frame)`, and hands the **engine** back rather
    /// than its output — so that the scope it wrote can be read off it
    /// ([`Engine::scope`]).
    fn run_scope(
        frames: usize,
        data: &StutterData,
        ctx_of: impl Fn(usize) -> BlockCtx,
    ) -> Engine {
        let mut engine = new_engine();

        for frame in 0..frames {
            let mut left = [input(frame)];
            let mut right = [-input(frame)];

            engine.run_block(&ctx_of(frame), data, &mut left, &mut right);
        }

        engine
    }

    /// **The scope**: the walk summarises the frame that came in and the
    /// frame that went out into the column its place in the cycle names, and
    /// the playhead follows it round.
    ///
    /// The input half is asserted against the ramp a column at a time —
    /// every column of the cycle, exactly, which is `(b mod P) / P × 512`
    /// checked 512 times. The output half is asserted against the *input*
    /// half: live where no block covers the column, and the block's first
    /// slice replayed where the Repeat wraps onto it, which is the read
    /// offset `d = p − (p mod ℓ)` seen a column at a time instead of a frame
    /// at a time.
    #[test]
    fn the_scope_summarises_both_halves_of_every_frame_the_pattern_plays() {
        assert_eq!(COLUMN_FRAMES * COLUMNS, CYCLE_FRAMES, "the fixture's cycle stopped dividing");
        let data = data_with(Action::repeat_at(Rate::Sixteenth));

        let engine = run_scope(FRAMES, &data, |frame| ctx_at(frame, 1, Some(1)));
        let scope = engine.scope();

        // Two cycles, so one wrap: every column holds the second cycle's
        // audio, written in the second generation, and none of it is old.
        assert_eq!(scope.generation(), FIRST_GENERATION + 1);
        for column in 0..COLUMNS {
            assert_eq!(scope.input(column), ramp_column(1, column), "input column {column}");
            assert_eq!(scope.age(column), 0, "column {column}");
        }

        // The playhead is where the last frame of the run sat: the far end
        // of the last column.
        assert_eq!(scope.column(), Some(COLUMNS - 1));
        assert_eq!(scope.playhead(), (CYCLE_FRAMES - 1) as f32 / CYCLE_FRAMES as f32);

        let slice = STEP_FRAMES / COLUMN_FRAMES;
        let block = (BLOCK_STEP as usize * slice)..((BLOCK_STEP + BLOCK_STEPS) as usize * slice);
        let mut asserted = 0;
        for column in 0..COLUMNS {
            // The columns a crossfade touched, where the output is two read
            // positions mixed by design: the first of each slice the Repeat
            // wraps *into* (its first slice opens at `d = 0`, which is where
            // the live audio before it already was), and the first live one
            // after the block, where `d` falls back to zero.
            let wrapped = block.contains(&column)
                && column != block.start
                && (column - block.start) % slice == 0;
            if wrapped || column == block.end {
                continue;
            }

            let read = match block.contains(&column) {
                true => block.start + (column - block.start) % slice,
                false => column,
            };
            assert_eq!(scope.output(column), scope.input(read), "output column {column}");
            asserted += 1;
        }

        // Every column but the four a fade touched, so a run that skipped
        // its way to green cannot pass.
        assert_eq!(asserted, COLUMNS - BLOCK_STEPS as usize, "columns asserted");
        // And the output half really is the output: over the replayed slices
        // it is not the input.
        let replayed = block.start + slice + slice / 2;
        assert_ne!(scope.output(replayed), scope.input(replayed), "column {replayed} is live");
    }

    /// The output half is **what left the plugin**, not the wet path: at
    /// `mix` 0 the walk still plays the pattern underneath ([`Engine`] step
    /// 7) and what the view shows is the dry frame, because that is what the
    /// track heard.
    #[test]
    fn the_scope_shows_what_left_the_plugin_rather_than_the_wet_path() {
        let data = data_with(Action::repeat_at(Rate::Sixteenth));

        let engine = run_scope(FRAMES, &data, |frame| ctx_at(frame, 1, Some(1)).with_mix(0.0));
        let scope = engine.scope();

        for column in 0..COLUMNS {
            assert_eq!(scope.input(column), ramp_column(1, column), "input column {column}");
            assert_eq!(scope.output(column), scope.input(column), "output column {column}");
        }
    }

    /// The generation counts up as the playhead wraps, which is what lets
    /// the GUI dim the columns ahead of it: a run of a cycle and a half
    /// leaves the first half of the view current and the rest a cycle old,
    /// still holding the audio of the pass before.
    #[test]
    fn the_scopes_generation_bumps_when_the_cycle_wraps() {
        // An empty pattern: every frame plays live, so there is no fade
        // anywhere in the run and the columns are the ramp exactly.
        let data = data_leveled(None, &[]);

        let engine = run_scope(CYCLE_FRAMES + CYCLE_FRAMES / 2, &data, |frame| {
            ctx_at(frame, 1, Some(1))
        });
        let scope = engine.scope();

        assert_eq!(scope.generation(), FIRST_GENERATION + 1);
        assert_eq!(scope.column(), Some(COLUMNS / 2 - 1));
        for column in 0..COLUMNS {
            let written = column < COLUMNS / 2;
            assert_eq!(scope.age(column), u32::from(!written), "column {column}");

            // And each column holds its own last pass, not a merge of the
            // two: the second cycle where the playhead has been, the first
            // where it has not.
            let cycle = usize::from(!written);
            assert_eq!(scope.input(column), ramp_column(1 - cycle, column), "column {column}");
        }
    }

    /// A frame with no pattern over it has no place in a cycle to be
    /// summarised at, so the scope holds still: pattern Off and a stopped
    /// transport both leave the view exactly as it was, ageing rather than
    /// filling with a waveform drawn against a grid it has no relation to.
    #[test]
    fn the_scope_holds_still_where_no_pattern_is_playing() {
        let data = data_with(Action::repeat_at(Rate::Sixteenth));

        let off = run_scope(FRAMES, &data, |frame| ctx_at(frame, 1, None));
        let stopped =
            run_scope(FRAMES, &data, |frame| ctx_at(frame, 1, Some(1)).with_playing(false));

        for engine in [off, stopped] {
            let scope = engine.scope();
            assert_eq!(scope.generation(), FIRST_GENERATION);
            assert_eq!(scope.column(), None);
            assert_eq!(scope.playhead(), 0.0);
            for column in 0..COLUMNS {
                assert_eq!(scope.input(column), Column::SILENT, "input column {column}");
                assert_eq!(scope.output(column), Column::SILENT, "output column {column}");
                assert_eq!(scope.age(column), 1, "column {column}");
            }
        }
    }

    /// `reset()` takes the summaries with the recorded past they summarise:
    /// the host has said the audio either side of the seam is not
    /// continuous, so a view still drawing the old shapes would be drawing
    /// somewhere else in the song.
    #[test]
    fn reset_clears_the_scope_with_the_past_it_summarised() {
        let data = data_with(Action::repeat_at(Rate::Sixteenth));
        let mut engine = run_scope(FRAMES, &data, |frame| ctx_at(frame, 1, Some(1)));
        assert_ne!(engine.scope().input(0), Column::SILENT, "the run wrote nothing");

        engine.reset();

        let scope = engine.scope();
        assert_eq!(scope.generation(), FIRST_GENERATION);
        assert_eq!(scope.column(), None);
        assert_eq!(scope.playhead(), 0.0);
        for column in 0..COLUMNS {
            assert_eq!(scope.input(column), Column::SILENT, "input column {column}");
            assert_eq!(scope.output(column), Column::SILENT, "output column {column}");
        }
    }

    // ---- block recipes: sources, loops with speed, continuing ----

    /// The module's eight-step 1/16 pattern with these blocks on it.
    fn pattern_of(blocks: &[(u16, u16, Action)]) -> StutterData {
        let mut pattern = Pattern::new();
        pattern.set_rate(RATE);
        pattern.set_length(STEPS);
        for &(start, len, action) in blocks {
            assert!(pattern.add_block(start, len, action).is_some(), "block at {start}");
        }
        StutterData::from_patterns([pattern])
    }

    /// The position in the cycle, in beats, of `frame`.
    fn cycle_beat(frame: usize) -> f64 {
        (frame as f64 / BEAT_FRAMES).rem_euclid(SPAN_BEATS)
    }

    /// Three repeats of a half beat, then a tape stop set to continue them:
    /// the stop opens on the first frame of the looped half beat — the
    /// fourth time round — and slows it down from there.
    #[test]
    fn a_tape_stop_that_continues_a_repeat_slows_the_repeated_beat() {
        let stop = Action::from_preset(SpeedPreset::TapeStop).with_source(Source::Continue);
        let data = pattern_of(&[(0, 6, Action::repeat(NoteValue::Eighth)), (6, 2, stop)]);
        let run = run(&data, Some(1));
        let (tail_start, tail) = (1.5, 0.5);

        assert_cycle(&run, 5 * FADE_FRAMES, |frame| {
            let beat = cycle_beat(frame);
            if beat < tail_start {
                beat - beat % 0.5
            } else {
                let phase = beat - tail_start;
                tail_start + phase * phase / (2.0 * tail)
            }
        });

        let first = CYCLE_FRAMES + (tail_start * BEAT_FRAMES) as usize;
        assert_eq!(run.position[first], CYCLE_FRAMES as f64, "the stop opens on the looped beat");
    }

    /// A loop whose speed ramps 1 → 2 across the block: every repeat starts
    /// from the one anchor, plays at the speed of its moment in the block,
    /// and the anchor is pulled back just far enough that the first repeat
    /// never reads the future.
    #[test]
    fn a_rising_repeat_restarts_on_one_anchor_at_a_rising_speed() {
        let slice = 0.25;
        let rising = Action { from: 1.0, to: 2.0, ..Action::repeat(NoteValue::Sixteenth) };
        let run = run(&data_with(rising), Some(1));

        // V(p) = p + p²/2B with B = 1 beat; the shift is V(ℓ) − ℓ = ℓ²/2.
        let travelled = |phase: f64| phase + phase * phase / (2.0 * BLOCK_BEATS);
        let shift = slice * slice / 2.0;
        assert_cycle(&run, 12 * FADE_FRAMES, |frame| match block_phase(frame) {
            Some(phase) => {
                let restart = phase - phase % slice;
                phase - (travelled(phase) - travelled(restart)) + shift
            }
            None => 0.0,
        });

        // Every restart reads the same frame: the anchor.
        let anchor = (BLOCK_START as f64 - shift * BEAT_FRAMES).round();
        for lap in 1..4 {
            let restart = BLOCK_START + lap * STEP_FRAMES;
            assert!((run.position[restart] - anchor).abs() < 1e-6, "lap {lap}");
        }
    }

    /// A read that runs off the far end of the recorded past fades out over
    /// the Smoothing time instead of dropping to silence in one sample.
    ///
    /// A whole-pattern Reverse of 64 steps of 1/8 (32 beats) at 30 BPM reads
    /// 64 beats back by its end — 128 s, twice what the buffer keeps — so
    /// the far half of it has nothing to read. The buffer is primed full
    /// first, as it would be on a track that has been playing.
    #[test]
    fn a_read_running_out_of_recording_fades_rather_than_clicks() {
        let rate = 1_000.0;
        let tempo = 30.0;
        let mut pattern = Pattern::new();
        pattern.set_rate(Rate::Eighth);
        pattern.set_length(Pattern::MAX_LENGTH);
        pattern.add_block(0, Pattern::MAX_LENGTH, Action::from_preset(SpeedPreset::Reverse));
        let data = StutterData::from_patterns([pattern]);

        let mut engine = Engine::new(rate, Arc::new(Scope::new()));
        let capacity = engine.history().capacity();
        let chunk = 50;
        let mut left = vec![0.5_f32; chunk];
        let mut right = vec![0.5_f32; chunk];
        for _ in 0..capacity / chunk {
            left.fill(0.5);
            right.fill(0.5);
            let stopped = BlockCtx::stopped(rate, chunk).with_smooth_ms(20.0);
            engine.run_block(&stopped, &data, &mut left, &mut right);
        }

        let cycle_frames = 64_000;
        let mut previous = 0.5_f32;
        let mut worst = 0.0_f32;
        let mut quietest = 1.0_f32;
        for start in (0..cycle_frames).step_by(chunk) {
            left.fill(0.5);
            right.fill(0.5);
            let beat = start as f64 * tempo / 60.0 / f64::from(rate);
            let ctx = BlockCtx::new(beat, tempo, rate, chunk)
                .with_pattern(Some(1))
                .with_smooth_ms(20.0);
            engine.run_block(&ctx, &data, &mut left, &mut right);
            for &sample in &left {
                worst = worst.max((sample - previous).abs());
                quietest = quietest.min(sample.abs());
                previous = sample;
            }
        }

        assert_eq!(quietest, 0.0, "the reverse should have run out of recording");
        assert!(worst < 0.05, "a step of {worst} where the recording ran out");
    }
}
