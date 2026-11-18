//! The concurrency stress test: every thread that writes to the model
//! writing at once, while the audio thread renders block after block against
//! what they publish.
//!
//! > GUI thread swapping pattern snapshots through the writer API while the
//! > audio thread runs blocks: no tearing, no allocation on the audio side,
//! > snapshots observed atomically.
//!
//! Three threads, which are exactly the three a loaded instance has:
//!
//! - **this one, as the audio thread**: [`Engine::run_block`] in a loop, one
//!   `SharedCell::get` a block, never reaching for the writer mutex;
//! - **a GUI thread**: the gestures a user makes — a rate menu, a length
//!   drag, a volume drawn across the row, blocks added, resized, moved from
//!   either edge, re-actioned, duplicated and deleted, patterns copied and
//!   cleared, undo, redo — each through the writer mutex, each followed by
//!   the editor's per-frame collect;
//! - **the host's state door**: `Snapshot` out through serde and back in
//!   through [`StutterModel::install`], which is what Bitwig does when a
//!   project is opened. It is the writer whose timing nothing in the GUI
//!   controls, which is the point of having it here: its whole-model
//!   replacement lands between a gesture and the block that renders it.
//!   Every fourth load is a **hostile** snapshot, so the clamping the audio
//!   thread relies on is being done under contention rather than in a quiet
//!   unit test.
//!
//! What that asks this to look for reads in this file as:
//!
//! - *no tearing* — the audio thread fingerprints the snapshot it is holding
//!   at the top of its block and again at the bottom, after a block's worth
//!   of writers have been and gone ([`Probe`]). A published `StutterData` is
//!   immutable by contract, so the two readings must be identical; a writer
//!   that edited in place instead of publishing a replacement would show up
//!   here as a torn block.
//! - *snapshots observed atomically* — that same walk counts every breach of
//!   `pattern.rs`'s four invariants in the data the block is actually playing
//!   (sorted non-overlapping blocks inside the length, volumes in `0..=1`,
//!   speeds inside ±[`MAX_SPEED`] with a truthful preset label). A snapshot
//!   caught half-written would break one of them; the tally must be zero over
//!   every block of the run, hostile loads included.
//! - *no allocation on the audio side* — the whole render loop runs inside
//!   `nice_assert_no_alloc::assert_no_alloc`, so the retired snapshot whose
//!   last reference this thread drops has to be *queued* for the collector
//!   rather than freed here (`tests/no_alloc.rs`'s control case is what proves
//!   that guard bites at all; an abort out of this test — reported as a failed
//!   allocation rather than as a failed assertion — means the audio thread
//!   allocated or freed something).
//! - *and the collector drains* — once the writers are joined and the loop has
//!   let go of its last snapshot, installing a fresh model and collecting must
//!   leave exactly one live allocation: the snapshot the instance is
//!   publishing. Every retired `StutterData` of the run is then accounted for.
//!
//! Deterministic by construction rather than by timing: fixed seeds, fixed
//! counts, no sleep anywhere, and no assertion that depends on who won a race.
//! The threads wait for each other instead. Each writer publishes a monotonic
//! counter of its own progress, and the audio thread counts a block as
//! *contended* when a given writer's counter moved while it rendered. Each
//! writer keeps cycling its script until there have been enough of those, and
//! the audio thread keeps rendering until both writers are done. Every
//! condition is met by either side making progress, so neither can wait on the
//! other for ever, and "the patterns really were rewritten under a running
//! block" is something the test establishes rather than hopes for.

use std::f64::consts::TAU;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};

use nice_assert_no_alloc::assert_no_alloc;

use stuttter::actions::{MAX_SPEED, SpeedPreset};
use stuttter::engine::{BlockCtx, Engine};
use stuttter::model::{StutterData, StutterModel};
use stuttter::pattern::{
    Action, MAX_BLOCKS, MAX_STEPS, NoteValue, PATTERN_COUNT, Pattern, Rate, Source,
};
use stuttter::scope::Scope;
use stuttter::state::{ActionSnapshot, BlockSnapshot, PatternSnapshot, Prefs, Snapshot};

/// The host rate every block renders at.
const RATE: f32 = 48_000.0;

/// The longest block the run renders, and so the length the buffers are
/// allocated at — Bitwig's default.
const BLOCK_FRAMES: usize = 512;

/// The tempo the blocks are placed at: fast, as in `tests/no_alloc.rs`, so
/// that a few hundred blocks cover whole pattern cycles and the walk is
/// crossing step boundaries while the writers rewrite the steps.
const TEMPO: f64 = 480.0;

/// How many blocks one `pattern` selection is held for: sixteen, which is two
/// whole cycles of [`block_frames`] and so 4138 frames — longer than the 3000
/// of the slowest step [`Rate::ALL`] offers at [`TEMPO`] and [`RATE`].
///
/// That is what makes the walk's own selection count evidence rather than
/// luck: a change arriving mid-step waits for a step boundary
/// ([`Engine::selection`]), and a stretch longer than the longest step always
/// contains one.
const SLOT_BLOCKS: usize = 16;

/// Blocks the audio loop renders before the writers are let go, with
/// [`starting_patterns`] published and nothing touching it.
///
/// Why there is a solo stretch at all: it is the only part of the run whose
/// output is a function of the fixture alone, so the "this really played
/// audio" assertions can be made without depending on any interleaving.
const SOLO_BLOCKS: usize = 16;

/// The fewest blocks the contended loop renders, for the case of writer
/// threads that get through their whole scripts while this one is descheduled:
/// without a floor the loop would end having rendered one.
const MIN_BLOCKS: usize = 128;

/// The fewest blocks that must have been rendered *while the GUI thread was
/// writing* before the GUI is allowed to stop — a block during which its
/// gesture counter moved.
///
/// This is what makes the overlap a fact rather than a likelihood, and it is
/// counted from the two threads' own progress counters rather than from
/// snapshot addresses, so nothing about it depends on the allocator.
const MIN_GUI_CONTENDED: usize = 192;

/// The same for the state door, whose writes are far heavier (sixteen patterns
/// through serde and back), so fewer of them are needed to prove the overlap.
const MIN_LOAD_CONTENDED: usize = 24;

/// The gestures the GUI thread makes at least — it keeps cycling the script
/// past this while [`MIN_GUI_CONTENDED`] is unmet. Enough that every kind of
/// gesture happens many times over, and small enough that the suite stays
/// quick (under two minutes for all of it).
const GESTURES: usize = 600;

/// How many kinds of gesture [`gesture`] cycles through.
const GESTURE_KINDS: usize = 12;

/// The project loads the state door makes at least.
const LOADS: usize = 48;

/// The GUI thread's seed. Fixed, so a failure is reproducible to the gesture.
const GUI_SEED: u64 = 0x5BE1_57A1_1C3D_9F01;

/// The state door's seed, likewise.
const LOAD_SEED: u64 = 0x1234_5678_9ABC_DEF0;

/// The two frequencies of the input, in Hz — the smooth, non-constant signal
/// `tests/click.rs` and `tests/no_alloc.rs` feed.
const FREQUENCIES: [f64; 2] = [97.0, 173.0];

/// A deterministic xorshift64*, as the other integration tests use: no `rand`
/// dependency for a few bits of variety.
fn next(state: &mut u64) -> u64 {
    *state ^= *state >> 12;
    *state ^= *state << 25;
    *state ^= *state >> 27;
    state.wrapping_mul(0x2545_F491_4F6C_DD1D)
}

/// How long block `block` is, in frames: from one frame to a full
/// [`BLOCK_FRAMES`], because a host may hand over any length, the seam between
/// two blocks is not a boundary the read head knows anything about, and a
/// crossfade started inside a short block is still running at the end of it —
/// which is how this file sees that the walk was crossfading at all.
fn block_frames(block: usize) -> usize {
    match block % 8 {
        0 | 3 | 6 => BLOCK_FRAMES,
        1 => 1,
        2 => 64,
        4 => 7,
        5 => 128,
        _ => 333,
    }
}

/// The input at frame `frame`: [`FREQUENCIES`] at half scale each.
fn input(frame: usize) -> f32 {
    let seconds = frame as f64 / f64::from(RATE);
    let sum: f64 = FREQUENCIES.iter().map(|hz| 0.5 * (TAU * hz * seconds).sin()).sum();

    sum as f32
}

/// Every action a block can run — repeats from the block start and from
/// fixed spots, the five named presets
/// and hand-drawn ramps to both ends of ±[`MAX_SPEED`].
fn actions() -> Vec<Action> {
    let top = MAX_SPEED as f32;
    let mut actions = vec![
        Action::repeat(NoteValue::ThirtySecond),
        Action::repeat(NoteValue::Sixteenth).with_source(Source::Spot { step: 1, passes: 0 }),
        Action::repeat(NoteValue::EighthTriplet).with_source(Source::Spot { step: 6, passes: 2 }),
        Action::ramp(top, -top),
        Action::ramp(-top, top),
    ];
    actions.extend(SpeedPreset::ALL.map(Action::from_preset));

    actions
}

/// The fixture the solo blocks render: all sixteen patterns given a rate, a
/// length, a Volume row that moves at every step and a Time row tiled with
/// [`actions`], with a gap every third block so the walk comes out into live
/// audio and goes back in.
///
/// Built through the writer API, so the blocks render against a snapshot the
/// GUI and the state load really produce.
fn starting_patterns(model: &StutterModel) {
    let actions = actions();

    for pattern in 0..PATTERN_COUNT {
        let length = 4 + (pattern as u16 * 3) % 29;
        assert!(model.set_rate(pattern, Rate::ALL[pattern % Rate::ALL.len()]));
        assert_eq!(model.set_length(pattern, length), Some(length));
        assert!(model.set_volumes(
            pattern,
            (0..length).map(|step| [1.0, 0.0, 0.5, 0.75][(pattern + step as usize) % 4])
        ));

        let (mut step, mut index) = (0, 0);
        while step < length {
            let len = 1 + (pattern as u16 + step) % 3;
            if index % 3 != 2 {
                let action = actions[index % actions.len()];
                assert!(model.add_block(pattern, step, len, action).is_some());
            }
            step += len;
            index += 1;
        }
    }
    model.commit_edits();
}

/// What the audio thread reads off the snapshot it is holding, in one walk:
/// a fingerprint of every byte of it that matters, and a count of every breach
/// of `pattern.rs`'s invariants in it.
///
/// Both halves are here rather than in two walks because this runs twice per
/// block on the real-time path: it is arithmetic over at most sixteen patterns
/// of sixty-four steps, it allocates nothing, and it reads nothing but the
/// snapshot this block was handed.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Probe {
    /// Rates, lengths, every block's span and action, and every step of every
    /// Volume row, folded into one number. Two readings of one snapshot must
    /// agree; two readings of different snapshots almost certainly will not.
    fingerprint: f64,
    /// How many of [the four invariants](stuttter::pattern) this snapshot
    /// breaks. Must be zero, always — it is what lets the audio thread read a
    /// published pattern without checking anything.
    violations: usize,
    /// How many blocks the whole snapshot holds, so a run can show it was
    /// playing patterns with something drawn on them.
    blocks: usize,
}

/// [`Probe`] of one snapshot.
fn probe(data: &StutterData) -> Probe {
    let mut out = Probe::default();

    for (index, pattern) in data.patterns().iter().enumerate() {
        let length = pattern.length();
        out.fingerprint += (index as f64 + 1.0) * (f64::from(pattern.rate().to_u8()) + 1.0);
        out.fingerprint += f64::from(length) * 7.0;
        if !(1..=Pattern::MAX_LENGTH).contains(&length) {
            out.violations += 1;
        }
        if pattern.blocks().len() > MAX_BLOCKS {
            out.violations += 1;
        }

        // Invariant 2: sorted, `len >= 1`, no overlap, inside `0..length`.
        let mut cursor = 0;
        for block in pattern.blocks() {
            out.fingerprint += f64::from(block.start()) * 13.0 + f64::from(block.len()) * 17.0;
            if block.start() < cursor || block.len() == 0 || block.end() > length {
                out.violations += 1;
            }
            cursor = block.end();

            // Invariant 4: both endpoints inside ±MAX_SPEED.
            let action = block.action();
            if let Some(length) = action.looping {
                out.fingerprint += f64::from(length.to_u8()) * 19.0;
            }
            out.fingerprint += match action.source {
                Source::BlockStart => 1.0,
                Source::Spot { step, passes } => {
                    2.0 + f64::from(step) + f64::from(passes) * 100.0
                }
                Source::Continue => 3.0,
            };
            let (from, to) = action.speeds();
            out.fingerprint += from * 23.0 + to * 29.0;
            let finite = from.is_finite() && to.is_finite();
            if !finite || from.abs() > MAX_SPEED || to.abs() > MAX_SPEED {
                out.violations += 1;
            }
            out.blocks += 1;
        }

        // Invariant 3: every volume a real number in `0.0..=1.0` — the hidden
        // steps past the length included, because they are published too.
        for (step, &volume) in pattern.volumes().iter().enumerate() {
            out.fingerprint += volume as f64 * (step as f64 + 2.0);
            if !(0.0..=1.0).contains(&volume) {
                out.violations += 1;
            }
        }
    }

    out
}

/// The counters the three threads agree through. Nothing here is data: it is
/// how far each side has got, which is what lets both sides wait for the other
/// to have seen them without either waiting for ever.
#[derive(Debug, Default)]
struct Handshake {
    /// Gestures the GUI thread has made.
    gui: AtomicUsize,
    /// Projects the state door has loaded.
    load: AtomicUsize,
    /// Blocks rendered while [`Handshake::gui`] moved, as the audio thread
    /// counted them.
    gui_contended: AtomicUsize,
    /// Blocks rendered while [`Handshake::load`] moved.
    load_contended: AtomicUsize,
    /// Writer threads still running. The audio loop renders while this is
    /// above zero.
    writers: AtomicUsize,
}

/// Decrements [`Handshake::writers`], whatever happens to the thread holding
/// it.
///
/// On `Drop` and not at the end of each writer, because a writer that panics
/// must also stop the render loop: the loop would otherwise spin until the
/// harness timed the suite out, and a hang says far less about what broke than
/// the panic this way re-raises in `join`.
struct DoneOnDrop(Arc<Handshake>);

impl Drop for DoneOnDrop {
    fn drop(&mut self) {
        self.0.writers.fetch_sub(1, Ordering::Release);
    }
}

/// What the GUI thread did, so the audio thread can assert afterwards that the
/// run was as busy as it claims.
#[derive(Debug, Default)]
struct Gestures {
    /// Gestures made: [`GESTURES`] or more.
    made: usize,
    /// Writes that landed on the Volume row or the rate and length.
    rows: usize,
    /// Structural changes to a Time row: blocks added, resized, moved,
    /// re-actioned, duplicated or deleted.
    blocks: usize,
    /// Whole patterns copied or cleared.
    patterns: usize,
    /// Undo and redo steps taken.
    steps: usize,
}

/// One gesture, through the writer mutex the way the editor's frame callback
/// takes it: write, commit when the gesture is over, collect.
///
/// `n` chooses the kind and `roll` the details, so the script is a fixed
/// sequence that still touches different patterns, blocks and values each time
/// around. Nothing here asserts that a gesture *took*: a block the state door
/// has since replaced, or a pattern a project load has shortened, is exactly
/// the race this test exists for, and a writer that lands on nothing is the
/// model doing its job.
fn gesture(n: usize, roll: u64, model: &StutterModel, counts: &mut Gestures) {
    let actions = actions();
    let pattern = roll as usize % PATTERN_COUNT;
    // What the editor draws from: the published snapshot, read without the
    // writer lock. Whatever it says may be stale by the next line, which is
    // the point.
    let (length, drawn_blocks) = {
        let drawn = model.shared();
        let seen = drawn.pattern(pattern).expect("every index names a pattern");

        (seen.length(), seen.blocks().len())
    };
    let index = (roll >> 8) as usize % drawn_blocks.max(1);
    let step = (roll >> 16) as u16 % Pattern::MAX_LENGTH;

    match n % GESTURE_KINDS {
        0 => {
            model.set_rate(pattern, Rate::ALL[(roll >> 24) as usize % Rate::ALL.len()]);
            counts.rows += 1;
        }
        1 => {
            // A length drag, anywhere in the parameter's range — including
            // lengths that drop or truncate the blocks already drawn.
            if model.set_length(pattern, 1 + step % Pattern::MAX_LENGTH).is_some() {
                counts.rows += 1;
            }
        }
        2 => {
            // One step of the Volume row, with a NaN every so often: a GUI
            // setter is a boundary and clamps, and the audio thread's probe
            // is what notices if it ever did not.
            let volume = if roll.is_multiple_of(16) {
                f32::NAN
            } else {
                (roll >> 32) as u8 as f32 / 255.0
            };
            if model.set_volume(pattern, step, volume) {
                counts.rows += 1;
            }
        }
        3 => {
            // A level drawn across the whole row, hidden steps and all.
            let base = (roll >> 40) as u8 as f32 / 255.0;
            if model.set_volumes(
                pattern,
                (0..MAX_STEPS).map(|step| base * (step % 2) as f32),
            ) {
                counts.rows += 1;
            }
        }
        4 | 5 => {
            // A block drawn, which is the republish this test is named for:
            // the audio thread may be rendering the step it lands on.
            let action = actions[(roll >> 48) as usize % actions.len()];
            if model.add_block(pattern, step % length.max(1), 1 + step % 4, action).is_some() {
                counts.blocks += 1;
            }
        }
        6 => {
            if model.resize_block(pattern, index, 1 + step % 8).is_some() {
                counts.blocks += 1;
            }
        }
        7 => {
            if model.move_block(pattern, index, step).is_some() {
                counts.blocks += 1;
            }
        }
        8 => {
            // The left-edge drag: both edges at once.
            if model.set_block_span(pattern, index, step, 1 + step % 6).is_some() {
                counts.blocks += 1;
            }
        }
        9 => {
            let action = actions[(roll >> 52) as usize % actions.len()];
            if model.set_block_action(pattern, index, action) {
                counts.blocks += 1;
            }
            if model.duplicate_block(pattern, index).is_some() {
                counts.blocks += 1;
            }
        }
        10 => {
            // Shrink — deleting a block the walk may be inside, which is the
            // use-after-free case: the block being played belongs to the
            // snapshot the audio thread holds, not to the model.
            if model.delete_block(pattern, index) {
                counts.blocks += 1;
            }
        }
        _ => {
            // Two whole patterns at once: one copied over another, and one
            // emptied. The first is the biggest single republish the GUI can
            // make.
            let other = (roll >> 56) as usize % PATTERN_COUNT;
            if model.copy_pattern(pattern, other) {
                counts.patterns += 1;
            }
            if model.clear_pattern(other) {
                counts.patterns += 1;
            }
        }
    }

    // The editor commits between gestures, which is what makes a drag one undo
    // step; undo and redo are on strides of their own so they fall across
    // every kind of gesture above rather than always after the same one.
    model.commit_edits();
    if n.is_multiple_of(7) && model.undo() {
        counts.steps += 1;
    }
    if n.is_multiple_of(11) && model.redo() {
        counts.steps += 1;
    }

    // The editor's per-frame collect, which is what finally frees a snapshot
    // the audio thread was still playing when a writer replaced it.
    model.collect();
}

/// The GUI thread: the gesture script, cycling while fewer than
/// [`MIN_GUI_CONTENDED`] blocks have been rendered against a pattern set it
/// was in the middle of rewriting.
fn gui_thread(model: Arc<StutterModel>, hands: Arc<Handshake>, start: Arc<Barrier>) -> Gestures {
    let _done = DoneOnDrop(Arc::clone(&hands));
    let mut counts = Gestures::default();
    let mut state = GUI_SEED;

    start.wait();
    let mut n = 0;
    while n < GESTURES || hands.gui_contended.load(Ordering::Acquire) < MIN_GUI_CONTENDED {
        let roll = next(&mut state);
        gesture(n, roll, &model, &mut counts);
        n += 1;
        hands.gui.store(n, Ordering::Release);
    }
    counts.made = n;

    counts
}

/// A snapshot no instance would ever write: a version from the future, forty
/// patterns of nine thousand steps, blocks that overlap and run off the end,
/// a slice and a repeat source that are not any this build knows, speeds at
/// 10³⁰ and NaN under a preset label that does not name them, and a Volume row
/// outside `0..=1` in both directions.
///
/// It goes in through the same door a real project does
/// ([`StutterModel::install`] of [`Snapshot::data`]), so what the audio thread
/// then plays is the clamping a snapshot load does —
/// under contention, which is the only part of it this file is testing.
fn hostile_snapshot() -> Snapshot {
    let pattern = PatternSnapshot {
        rate: 200,
        length: 9_000,
        blocks: vec![
            BlockSnapshot {
                start: 0,
                len: 9_000,
                action: ActionSnapshot {
                    source: 2,
                    step: 9_000,
                    passes: 200,
                    looping: Some(77),
                    from: 1e30,
                    to: f32::NAN,
                },
            },
            BlockSnapshot { start: 1, len: 0, action: ActionSnapshot::default() },
            BlockSnapshot {
                start: 60_000,
                len: 7,
                action: ActionSnapshot {
                    source: 3,
                    step: 60_000,
                    passes: 1,
                    looping: Some(200),
                    from: f32::NEG_INFINITY,
                    to: 3.0,
                },
            },
        ],
        volumes: vec![2.0, -3.0, f32::NAN, f32::INFINITY, 0.5],
        // A real filter, so the audio thread plays one over levels that
        // arrived as nonsense.
        filter: 1,
        cutoff: vec![f32::NAN, 2.0, -1.0, f32::INFINITY],
        resonance: vec![9.0, f32::NAN, -5.0],
    };

    Snapshot {
        version: u32::MAX,
        patterns: vec![pattern; 40],
        keep_on_top: false,
        notes: Some(u8::MAX),
        latch: true,
        last_action: ActionSnapshot {
            source: 3,
            step: 99,
            passes: 99,
            ..ActionSnapshot::default()
        },
    }
}

/// The host's state door, run as its own thread: read the live patterns into a
/// snapshot, through serde and back, and install the result over whatever the
/// GUI has meanwhile made of them — with every fourth load hostile instead.
///
/// The undo stacks are dropped after each load, as `StateField::set` drops
/// them: opening a project is not an edit.
fn load_thread(model: Arc<StutterModel>, hands: Arc<Handshake>, start: Arc<Barrier>) -> usize {
    let _done = DoneOnDrop(Arc::clone(&hands));
    let mut state = LOAD_SEED;

    start.wait();
    let mut n = 0;
    while n < LOADS || hands.load_contended.load(Ordering::Acquire) < MIN_LOAD_CONTENDED {
        let snapshot = if next(&mut state).is_multiple_of(4) {
            hostile_snapshot()
        } else {
            // The real round trip: what the host stores is JSON, so that is
            // what a load has to come back through.
            let saved = Snapshot::of(&model.data(), Prefs::default());
            let blob = serde_json::to_string(&saved).expect("a snapshot serialises");
            let parsed: Snapshot = serde_json::from_str(&blob).expect("and parses back");
            assert_eq!(parsed, saved, "the stored blob round-trips exactly");
            parsed
        };

        model.install(snapshot.data());
        model.clear_history();
        model.collect();
        n += 1;
        hands.load.store(n, Ordering::Release);
    }

    n
}

/// What the audio loop counted, so every assertion about it can be made after
/// the no-alloc guard has come down: a failing assertion allocates, and inside
/// the guard that would abort the process instead of failing the test.
#[derive(Debug, Default)]
struct Stats {
    /// Blocks rendered.
    blocks: usize,
    /// Frames rendered.
    frames: usize,
    /// Blocks rendered while the GUI thread's counter was moving.
    gui_contended: usize,
    /// Blocks rendered while the state door's counter was moving.
    load_contended: usize,
    /// How many times the published snapshot changed under the loop. A lower
    /// bound: it compares the snapshot's address with the last one's, and a
    /// retired snapshot's memory can be handed straight back out for its
    /// replacement. The contended counts are what the handshake uses; this one
    /// is direct evidence that a block held data a writer had already retired.
    swaps: usize,
    /// The address of the last one.
    last: usize,
    /// Blocks whose snapshot read differently at the end of the block than at
    /// the start of it. Must be zero: a published `StutterData` is immutable.
    torn: usize,
    /// Breaches of `pattern.rs`'s invariants seen in the data actually played.
    /// Must be zero.
    violations: usize,
    /// The most blocks one snapshot held.
    drawn: usize,
    /// Which patterns the walk actually played, one bit each.
    selections: u32,
    /// The loudest sample the blocks produced.
    peak: f32,
    /// Blocks that ended with a crossfade still in flight.
    faded: usize,
}

/// What one block does on the audio thread, start to finish: the snapshot, the
/// fingerprint, the render, and a second fingerprint of that same snapshot —
/// which is the tearing probe.
///
/// Every line of this is on the real-time path and allocates nothing: the
/// `get` is a refcount bump, the probe is arithmetic over the snapshot, and
/// the drop of the handle at the end *queues* a retired snapshot for the
/// collector rather than freeing it.
fn audio_block(
    engine: &mut Engine,
    model: &StutterModel,
    ctx: &BlockCtx,
    left: &mut [f32],
    right: &mut [f32],
    hands: &Handshake,
    stats: &mut Stats,
) {
    let held = model.shared();
    let snapshot: &StutterData = &held;
    let address = std::ptr::from_ref(snapshot) as usize;
    if address != stats.last {
        stats.last = address;
        stats.swaps += 1;
    }
    let gui = hands.gui.load(Ordering::Acquire);
    let load = hands.load.load(Ordering::Acquire);

    let before = probe(&held);
    engine.run_block(ctx, &held, left, right);
    // The probe: the data this block started with, read again now that a
    // block's worth of writers has been and gone. Nothing about it may have
    // changed — a writer publishes a replacement and never edits what is
    // published — and every pattern of it must still be here to read, which is
    // what basedrop's deferred collection buys and what a collect on this
    // thread would take away.
    let after = probe(&held);
    if before != after {
        stats.torn += 1;
    }
    stats.violations += before.violations + after.violations;
    stats.drawn = stats.drawn.max(before.blocks);

    for sample in left.iter().chain(right.iter()) {
        stats.peak = stats.peak.max(sample.abs());
    }
    if let Some(pattern) = engine.selection() {
        stats.selections |= 1 << pattern;
    }
    if engine.reader().is_fading() {
        stats.faded += 1;
    }

    if hands.gui.load(Ordering::Acquire) != gui {
        stats.gui_contended += 1;
    }
    if hands.load.load(Ordering::Acquire) != load {
        stats.load_contended += 1;
    }
    stats.blocks += 1;
    stats.frames += left.len();
}

/// Fills `left`/`right` with the block of [`input`] starting at `frame`, as
/// nice-plug hands the input over in the buffer the output is written into.
fn fill(frame: usize, left: &mut [f32], right: &mut [f32]) {
    for (offset, (left, right)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
        *left = input(frame + offset);
        *right = -*left;
    }
}

/// The headline of this file.
///
/// Reads top to bottom as the life of an instance: build it, play it alone for
/// a few blocks, then let the GUI and the host's state door rewrite it under
/// the render loop until every side has seen enough of the others, join them,
/// check what is left, and hand the memory back.
#[test]
fn the_gui_and_the_state_door_rewrite_the_patterns_while_the_engine_renders() {
    let model = Arc::new(StutterModel::new(StutterData::new()));
    starting_patterns(&model);

    let scope = Arc::new(Scope::new());
    let mut engine = Engine::new(RATE, Arc::clone(&scope));
    let mut left = vec![0.0f32; BLOCK_FRAMES];
    let mut right = vec![0.0f32; BLOCK_FRAMES];
    let hands = Arc::new(Handshake::default());
    let start = Arc::new(Barrier::new(3));

    // One block before any of the counting, as `tests/no_alloc.rs` does it, so
    // that a one-time lazy init inside *std* cannot be reported as the audio
    // path's allocation. The engine itself holds no lazily-built state.
    fill(0, &mut left, &mut right);
    engine.run_block(
        &BlockCtx::new(0.0, TEMPO, RATE, BLOCK_FRAMES).with_pattern(Some(1)),
        &model.shared(),
        &mut left,
        &mut right,
    );
    engine.reset();

    hands.writers.store(2, Ordering::Release);
    let writers = {
        let (gui_model, gui_hands, gui_start) =
            (Arc::clone(&model), Arc::clone(&hands), Arc::clone(&start));
        let (load_model, load_hands, load_start) =
            (Arc::clone(&model), Arc::clone(&hands), Arc::clone(&start));
        (
            std::thread::Builder::new()
                .name(String::from("stuttter-stress-gui"))
                .spawn(move || gui_thread(gui_model, gui_hands, gui_start))
                .expect("the GUI thread spawns"),
            std::thread::Builder::new()
                .name(String::from("stuttter-stress-load"))
                .spawn(move || load_thread(load_model, load_hands, load_start))
                .expect("the state door's thread spawns"),
        )
    };

    // The context for block `block`: the song running on from where the last
    // one left it, with a `pattern` selection held for [`SLOT_BLOCKS`] blocks
    // and Off every seventh slot — the *last* of each cycle, so that slot 0,
    // which is the whole solo stretch, is one that plays a pattern.
    let ctx_of = |frame: usize, block: usize| {
        let slot = block / SLOT_BLOCKS;
        let pattern = match slot % 7 {
            6 => None,
            _ => Some(1 + (slot * 5 % PATTERN_COUNT) as u8),
        };
        let beat = frame as f64 * TEMPO / 60.0 / f64::from(RATE);

        BlockCtx::new(beat, TEMPO, RATE, block_frames(block)).with_pattern(pattern)
    };

    // The solo stretch: the writers are parked on the barrier, so these blocks
    // are a function of `starting_patterns` alone.
    let mut solo = Stats::default();
    assert_no_alloc(|| {
        for block in 0..SOLO_BLOCKS {
            let frames = block_frames(block);
            fill(solo.frames, &mut left[..frames], &mut right[..frames]);
            audio_block(
                &mut engine,
                &model,
                &ctx_of(solo.frames, block),
                &mut left[..frames],
                &mut right[..frames],
                &hands,
                &mut solo,
            );
        }
    });
    assert_eq!(solo.blocks, SOLO_BLOCKS);
    assert_eq!(solo.swaps, 1, "nothing was writing, so there was one snapshot");
    assert_eq!(solo.gui_contended, 0, "and no gesture had been made yet");
    assert_eq!(solo.load_contended, 0, "nor any project loaded");
    assert_eq!(solo.torn, 0);
    assert_eq!(solo.violations, 0);
    assert!(solo.drawn > 0, "the fixture published blocks to play: {}", solo.drawn);
    assert!(solo.peak > 0.0, "the fixture rendered audible audio, peak {}", solo.peak);
    assert!(solo.selections.count_ones() > 0, "the walk played a pattern");

    // And the contended stretch: from here until both writers are done, every
    // block renders against whatever they have most recently published.
    start.wait();
    let mut stats = Stats { frames: solo.frames, last: solo.last, ..Stats::default() };
    assert_no_alloc(|| {
        while hands.writers.load(Ordering::Acquire) > 0 || stats.blocks < MIN_BLOCKS {
            let block = SOLO_BLOCKS + stats.blocks;
            let frames = block_frames(block);
            fill(stats.frames, &mut left[..frames], &mut right[..frames]);
            audio_block(
                &mut engine,
                &model,
                &ctx_of(stats.frames, block),
                &mut left[..frames],
                &mut right[..frames],
                &hands,
                &mut stats,
            );
            hands.gui_contended.store(stats.gui_contended, Ordering::Release);
            hands.load_contended.store(stats.load_contended, Ordering::Release);
        }
    });

    let (gui, load) = writers;
    let counts = gui.join().expect("the GUI thread finished without panicking");
    let loads = load.join().expect("the state door's thread finished without panicking");

    // What the run must not have done, whatever the interleaving was.
    assert_eq!(
        stats.torn, 0,
        "{} of {} blocks saw the snapshot they were holding change under them — a published \
         StutterData must be replaced, never edited",
        stats.torn, stats.blocks,
    );
    assert_eq!(
        stats.violations, 0,
        "{} breaches of pattern.rs's invariants were published to the audio thread",
        stats.violations,
    );

    // And what it must have done: the overlap is guaranteed by the handshake
    // rather than hoped for, and the rest follows from the scripts the writers
    // completed.
    assert!(
        stats.gui_contended >= MIN_GUI_CONTENDED,
        "{} of {} blocks rendered while the GUI was writing, wanted {MIN_GUI_CONTENDED}",
        stats.gui_contended,
        stats.blocks,
    );
    assert!(
        stats.load_contended >= MIN_LOAD_CONTENDED,
        "{} of {} blocks rendered while a project was being loaded, wanted \
         {MIN_LOAD_CONTENDED}",
        stats.load_contended,
        stats.blocks,
    );
    assert!(stats.blocks >= MIN_BLOCKS, "blocks rendered: {}", stats.blocks);
    assert!(stats.swaps > 1, "the published snapshot changed under the loop: {}", stats.swaps);
    assert!(stats.peak > 0.0, "the contended blocks rendered audio too, peak {}", stats.peak);
    assert!(stats.drawn > 0, "the blocks were playing patterns with a Time row: {}", stats.drawn);
    assert!(
        stats.selections.count_ones() >= 4,
        "several patterns were walked while they were being rewritten: {:016b}",
        stats.selections,
    );
    assert!(stats.faded > 0, "crossfades ran while the patterns changed: {}", stats.faded);

    assert!(counts.made >= GESTURES, "gestures made: {}", counts.made);
    assert!(counts.rows > 0, "the GUI wrote rates, lengths and volumes: {}", counts.rows);
    assert!(counts.blocks > 0, "the GUI rewrote Time rows: {}", counts.blocks);
    assert!(counts.patterns > 0, "whole patterns were copied and cleared: {}", counts.patterns);
    assert!(counts.steps > 0, "undo and redo ran as well: {}", counts.steps);
    assert!(loads >= LOADS, "projects loaded under the blocks: {loads}");

    // Nothing in all of that may have broken the model, and what the writers
    // left behind is still sixteen whole patterns.
    let left_behind = model.data();
    assert_eq!(left_behind.patterns().len(), PATTERN_COUNT);
    assert_eq!(probe(&left_behind).violations, 0, "the model's invariants survived the run");
    assert_eq!(
        probe(&left_behind).fingerprint,
        probe(&model.shared()).fingerprint,
        "what the writers left is what the audio thread would be handed",
    );

    // And the collector drains. Both writers are joined and the render loop has
    // dropped its last snapshot, so the model holds the last handle on
    // everything the run made: installing a fresh set retires the lot, and one
    // collect must leave exactly the snapshot the instance is publishing.
    drop(engine);
    model.install(StutterData::new());
    model.collect();
    assert_eq!(
        model.live_allocs(),
        1,
        "only the live snapshot is left — every retired StutterData of the run has been \
         handed back",
    );
}
