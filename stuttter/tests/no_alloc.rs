//! The real-time rule, enforced: a deliberately busy run of blocks through
//! [`Engine::run_block`] inside `nice_assert_no_alloc::assert_no_alloc`,
//! which aborts the process if the audio path allocates or frees a single
//! byte.
//!
//! **Why this test lives in `tests/` and not in `engine.rs`.** nice-plug
//! installs `nice_assert_no_alloc::AllocDisabler` as the *global* allocator
//! (its `wrapper/util.rs`, behind `debug_assertions` + our
//! `assert_process_allocs` feature) but keeps its own `assert_no_alloc`
//! wrapper crate-private, so the forbid counter has to be raised by calling
//! the dev-dependency directly — and that counter is a thread-local of the
//! *one* copy of the crate cargo resolved, which is why `Cargo.toml` pins it
//! to nice-plug's own requirement. The guard therefore only bites in a
//! binary that really links nice-plug: a test that touched none of
//! `stuttter` would raise a counter no allocator reads and silently pass
//! without checking anything. Everything here goes through the `stuttter`
//! library, which links nice-plug, and
//! [`the_guard_bites_a_deliberate_allocation`] *runs* the control case
//! rather than asserting the linkage in prose.
//!
//! What "busy" means here — many jumps, long reverse, pattern changes, with
//! the rest of the audio path's per-block work around it:
//!
//! - **Every action**, drawn over sixteen patterns of all five rates (the
//!   triplets included) and lengths from one step to sixty-four: 1/32 repeats
//!   from both sources, so a block wraps on nearly every step; the five
//!   presets, the long reverse among them; and hand-drawn ramps that run the
//!   read head to both ends of ±[`MAX_SPEED`].
//! - **A gap in the Time row every few steps**, so the walk leaves a block
//!   for live audio and comes back — a crossfade in both directions.
//! - **A Volume row that moves at nearly every step**, silence included, so a
//!   level ramp is almost always in flight.
//! - **Pattern changes between blocks**, Off among them: the `Selector`
//!   holding a change back to a step boundary and releasing it there.
//! - **A stopped stretch, the start after it, two blocks the host reports
//!   nothing at all for, and two loop jumps** — the position discontinuities
//!   of a transport, each one a crossfade back to live or across to
//!   somewhere else in the recording.
//! - **Mix and Smoothing moving per block**, from the dry end (where the
//!   blend hands the input straight back) to the wet one, and from the
//!   shortest fade the parameter offers to the longest.
//! - **Block lengths from one frame to five hundred and twelve**, because the
//!   seam between two blocks is not a boundary the read head knows anything
//!   about, and a one-frame block is the worst case for it.
//! - **The `SharedCell` read** `process()` makes per block, and the drop of
//!   that handle at the end of it — which must *queue* a retired snapshot for
//!   the collector rather than free it here.
//! - **`reset()` twice**, which nice-plug calls on the audio thread as well
//!   as after `activate()`, and which must therefore keep its buffer rather
//!   than reallocate it.
//!
//! The editor's wake-up call is in here too ([`RepaintNotifier`], rung from
//! `process()` while the transport plays and the window is open). It is one
//! relaxed store into an `Arc<AtomicBool>` and so allocates nothing, but it
//! is a call the audio thread makes on a type from outside this crate, which
//! is exactly the kind of claim the real-time rule wants checked rather than
//! asserted in prose.

use std::f64::consts::TAU;
use std::io::Write;
use std::ops::Range;
use std::process::Command;
use std::sync::Arc;

use nice_assert_no_alloc::assert_no_alloc;
use nice_plug_egui::RepaintNotifier;

use stuttter::actions::{MAX_SPEED, SpeedPreset};
use stuttter::engine::{BlockCtx, Engine, SelectionChanges};
use stuttter::model::{StutterData, StutterModel};
use stuttter::notes::{self, Trigger};
use stuttter::pattern::{Action, Filter, Lane, NoteValue, PATTERN_COUNT, Pattern, Rate, Source};
use stuttter::scope::{FIRST_GENERATION, Scope};
use stuttter::{SMOOTH_DEFAULT_MS, SMOOTH_MAX_MS, SMOOTH_MIN_MS};

/// The host rate every block renders at.
const RATE: f32 = 48_000.0;

/// The longest block the run renders, and so the length the buffers are
/// allocated at — before the guard goes up, as `initialize()` allocates
/// everything the audio path needs.
const BLOCK_FRAMES: usize = 512;

/// How many blocks the guarded run renders. Many, because some of the audio
/// path is only reached on a later block: a fade that outlives the block that
/// started it, a pattern change released a boundary after it arrived, a scope
/// whose columns have wrapped.
///
/// They are grouped into [`SLOTS`] stretches of [`SLOT_BLOCKS`], which is
/// what the `pattern` parameter changes on.
const BLOCKS: usize = SLOTS.len() * SLOT_BLOCKS;

/// The tempo the blocks are placed at. Fast, so that [`BLOCKS`] blocks cover
/// hundreds of steps and dozens of whole pattern cycles instead of a fraction
/// of one — what makes the run busy is boundaries, and boundaries are steps.
const TEMPO: f64 = 480.0;

/// How many blocks one `pattern` selection is held for: sixteen, which is
/// 4138 frames of [`block_frames`] and so longer than the 3000 frames of the
/// slowest step [`Rate::ALL`] offers (a 1/8 at [`TEMPO`] and [`RATE`]).
///
/// That is what makes the sweep below a fact rather than a hope: a change
/// arriving mid-step waits for a step boundary ([`Engine::selection`]), and a
/// stretch longer than the longest step always contains one.
const SLOT_BLOCKS: usize = 16;

/// What the `pattern` parameter asks for, slot by slot.
///
/// Slots 1…16 are the sweep: every one of the sixteen patterns, in order, each
/// held long enough to be adopted — which is what
/// [`a_busy_run_allocates_nothing`] asserts, and what would otherwise be a
/// coincidence of where the boundaries fell. The three `None`s are Off, which
/// is also what lets the pattern after one join without waiting. The rest is
/// a second pass in a different order, and it is deliberately the half of the
/// run the stops, the loop jumps and the resets land in, so that every one of
/// those happens while a pattern is playing rather than during the sweep.
const SLOTS: [Option<u8>; 32] = [
    None,
    Some(1),
    Some(2),
    Some(3),
    Some(4),
    Some(5),
    Some(6),
    Some(7),
    Some(8),
    Some(9),
    Some(10),
    Some(11),
    Some(12),
    Some(13),
    Some(14),
    Some(15),
    Some(16),
    None,
    Some(16),
    Some(1),
    Some(15),
    Some(2),
    Some(14),
    Some(3),
    Some(13),
    Some(4),
    None,
    Some(12),
    Some(5),
    Some(11),
    Some(6),
    Some(10),
];

/// The blocks the host reports a stopped transport for: a stop in the middle
/// of a pattern, a stretch of live audio with the history still recording,
/// and the start after it.
const STOPPED: Range<usize> = 300..316;

/// The blocks the host reports nothing usable at all for —
/// [`BlockCtx::stopped`], the missing-position and missing-tempo case.
const NO_TRANSPORT: [usize; 2] = [330, 331];

/// The blocks the song position jumps backwards on: an arranger loop coming
/// round, which moves the read head by a distance no step could.
const LOOP_JUMPS: [usize; 2] = [360, 440];

/// How far back a loop jump goes, in beats.
const LOOP_BEATS: f64 = 4.0;

/// The blocks `reset()` is called before, as nice-plug calls it on the audio
/// thread when the host says the audio either side of this point is not
/// continuous.
const RESETS: [usize; 2] = [390, 470];

/// The two frequencies of the input, in Hz — a sum of two sines, as
/// `tests/click.rs` feeds: smooth, non-constant, and aligned with none of the
/// boundary spacings in the run.
const FREQUENCIES: [f64; 2] = [97.0, 173.0];

/// Whether nice-plug installed the guard's allocator for this build: its
/// `#[global_allocator]` is behind `debug_assertions` and our
/// `assert_process_allocs` feature, and `nice-assert-no-alloc`'s own
/// `disable_release` default no-ops `assert_no_alloc` outside debug. With
/// either off there is nothing to check — a plain `cargo test` has both on.
const GUARD_ACTIVE: bool = cfg!(debug_assertions) && cfg!(feature = "assert_process_allocs");

/// The input at frame `frame`: [`FREQUENCIES`] at half scale each, so the sum
/// spans `-1.0 ..= 1.0`.
fn input(frame: usize) -> f32 {
    let seconds = frame as f64 / f64::from(RATE);
    let sum: f64 = FREQUENCIES.iter().map(|hz| 0.5 * (TAU * hz * seconds).sin()).sum();

    sum as f32
}

/// Every kind of block recipe, which is what the patterns are tiled with.
///
/// Loops from the block start and from fixed spots at two lengths — the 1/32
/// so a block wraps on nearly every step — loops whose speed ramps, the five
/// named presets, hand-drawn ramps that take the read head to both ends of
/// ±[`MAX_SPEED`] and through zero, and blocks that continue the one before
/// them, so chains are resolved inside the guard.
fn actions() -> Vec<Action> {
    let top = MAX_SPEED as f32;
    let spot = |length: NoteValue, step: u16, passes: u8| {
        Action::repeat(length).with_source(Source::Spot { step, passes })
    };
    let continuing = |action: Action| action.with_source(Source::Continue);
    let mut actions = vec![
        Action::repeat(NoteValue::ThirtySecond),
        spot(NoteValue::ThirtySecond, 1, 0),
        spot(NoteValue::EighthTriplet, 5, 1),
        Action::repeat(NoteValue::Quarter),
        Action { from: 1.0, to: top, ..Action::repeat(NoteValue::Sixteenth) },
        continuing(Action { from: -top, to: 0.5, ..Action::repeat(NoteValue::Whole) }),
        Action::ramp(top, -top),
        continuing(Action::ramp(-top, top)),
        Action::ramp(0.0, top),
        continuing(Action::from_preset(SpeedPreset::TapeStop)),
    ];
    actions.extend(SpeedPreset::ALL.map(Action::from_preset));

    actions
}

/// How long each pattern is, in steps: one to sixty-four, mostly primes and
/// odd numbers, so no pattern's cycle lines up with another's or with the
/// block lengths the run renders at.
const LENGTHS: [u16; PATTERN_COUNT] =
    [4, 7, 1, 16, 3, 11, 64, 5, 9, 32, 13, 2, 48, 6, 17, 23];

/// The level the Volume row cycles through, step by step: unity, silence and
/// two levels between, so a level ramp is in flight at nearly every step
/// boundary and the bit-exact unity case is still in the run.
const LEVELS: [f32; 4] = [1.0, 0.0, 0.5, 0.75];

/// The model the guarded run plays, built through the writer API — before the
/// guard, as the GUI and the state load build it and `process()` never does.
///
/// Sixteen patterns, each a different rate and [`LENGTHS`] long, their Time
/// rows tiled with [`actions`] and a gap left every fourth block so the walk
/// comes out of a block into live audio and goes back in.
fn busy_model() -> StutterModel {
    let model = StutterModel::new(StutterData::new());
    let actions = actions();

    for pattern in 0..PATTERN_COUNT {
        let length = LENGTHS[pattern];
        assert!(model.set_rate(pattern, Rate::ALL[pattern % Rate::ALL.len()]));
        assert_eq!(model.set_length(pattern, length), Some(length));
        assert!(model.set_volumes(
            pattern,
            (0..length).map(|step| LEVELS[(pattern + step as usize) % LEVELS.len()])
        ));
        // Every shape of filter, swept step by step, so the walk tunes it on
        // the audio thread and fades it in and out between patterns.
        assert!(model.choose_filter(pattern, Filter::ALL[pattern % Filter::ALL.len()]));
        for (lane, offset) in [(Lane::Cutoff, 1), (Lane::Resonance, 2)] {
            let levels = (0..length).map(|step| LEVELS[(pattern + offset + step as usize) % 3]);
            assert!(model.set_lane(pattern, lane, levels));
        }

        let mut step = 0;
        let mut index = 0;
        while step < length {
            let len = 1 + (pattern as u16 + step) % 3;
            if index % 4 != 3 {
                let action = actions[(pattern + index) % actions.len()];
                assert!(model.add_block(pattern, step, len, action).is_some());
            }
            step += len;
            index += 1;
        }
    }
    assert!(
        model.shared().patterns().iter().all(|pattern| !pattern.blocks().is_empty()),
        "every pattern has a Time row to walk",
    );

    model
}

/// How long block `block` is, in frames: from one frame to a full
/// [`BLOCK_FRAMES`], because a host may hand over any length and the stitch
/// between two blocks has to read straight through whatever it hands over.
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

/// The `pattern` parameter for block `block`: [`SLOTS`], a stretch of
/// [`SLOT_BLOCKS`] each.
fn pattern_for(block: usize) -> Option<u8> {
    SLOTS[block / SLOT_BLOCKS]
}

/// What the guarded run counted, so that every assertion about how busy it
/// was can be made *after* the guard has come down: a failing assertion
/// formats a message, which allocates, and inside the guard that would abort
/// the process instead of failing the test.
#[derive(Debug, Default)]
struct Stats {
    /// Blocks rendered.
    blocks: usize,
    /// Frames rendered.
    frames: usize,
    /// Frames rendered since the last `reset()` — what the history must hold.
    since_reset: u64,
    /// Blocks the engine took to be playing a pattern.
    playing: usize,
    /// Blocks the host reported no usable transport for.
    stopped: usize,
    /// Blocks that ended with a crossfade still in flight.
    faded: usize,
    /// Blocks that ended with both read heads running.
    two_heads: usize,
    /// Which patterns the walk actually played, one bit each.
    selections: u32,
    /// Blocks whose walk was live — Off, stopped, or a change still waiting.
    live: usize,
    /// Frames that came out as something other than what went in.
    wet: usize,
    /// The loudest sample the run produced.
    peak: f32,
    /// Repaints asked of the editor.
    repaints: usize,
    /// `reset()` calls made inside the guard.
    resets: usize,
    /// The highest scope generation seen: how many times the waveform view
    /// wrapped round a pattern cycle.
    generation: u32,
}

/// The test itself: [`BLOCKS`] busy blocks, start to finish, with nothing
/// allocated or freed on the way.
///
/// Everything the run needs exists before the guard goes up, as `lib.rs`
/// builds it in `initialize()`/`activate()` and never in `process()`: the
/// model and its patterns, the engine and its history buffer, the scope, the
/// buffers, the repaint notifier. What happens inside is what `process()`
/// does per block — read the transport and the parameters into a context,
/// take the published patterns, run the block, ring the editor.
#[test]
fn a_busy_run_allocates_nothing() {
    let model = busy_model();
    let scope = Arc::new(Scope::new());
    let mut engine = Engine::new(RATE, Arc::clone(&scope));
    // Cloned before the guard, as `lib.rs` clones it when the editor is
    // built and never on this path.
    let repaint = RepaintNotifier::new();
    let mut source = vec![0.0f32; BLOCK_FRAMES];
    let mut left = vec![0.0f32; BLOCK_FRAMES];
    let mut right = vec![0.0f32; BLOCK_FRAMES];

    // One block outside the guard first, then back to an empty history. Not
    // to excuse the audio path anything — it allocates on no block, which is
    // what the run below asserts — but so that a one-time lazy init inside
    // *std* (a thread-local, a float formatting table) cannot be reported as
    // ours. The engine itself holds no lazily-built state: `Engine::new`
    // allocates the history and nothing after it ever does.
    engine.run_block(
        &BlockCtx::new(0.0, TEMPO, RATE, BLOCK_FRAMES).with_pattern(Some(1)),
        &model.shared(),
        &mut left,
        &mut right,
    );
    engine.reset();

    let mut stats = Stats::default();
    let mut beat = 0.0f64;
    let mut triggered: Option<u8> = None;

    assert_no_alloc(|| {
        for block in 0..BLOCKS {
            if RESETS.contains(&block) {
                engine.reset();
                stats.resets += 1;
                stats.since_reset = 0;
            }
            if LOOP_JUMPS.contains(&block) {
                beat -= LOOP_BEATS;
            }

            let frames = block_frames(block);
            for (offset, slot) in source[..frames].iter_mut().enumerate() {
                *slot = input(stats.frames + offset);
            }
            left[..frames].copy_from_slice(&source[..frames]);
            for (slot, dry) in right[..frames].iter_mut().zip(&source[..frames]) {
                // The right channel is the left inverted, as everywhere else
                // in this project's tests: the same slope, told apart by eye.
                *slot = -dry;
            }

            // Step 1 of `process()`: the transport and the three parameters,
            // read once per block.
            let ctx = if NO_TRANSPORT.contains(&block) {
                BlockCtx::stopped(RATE, frames)
            } else {
                BlockCtx::new(beat, TEMPO, RATE, frames)
                    .with_playing(!STOPPED.contains(&block))
                    .with_pattern(pattern_for(block))
                    .with_smooth_ms([SMOOTH_MIN_MS, SMOOTH_DEFAULT_MS, SMOOTH_MAX_MS][block % 3])
                    .with_mix([1.0, 0.0, 0.5, 0.85][block % 4])
            };

            // Step 2: the one `SharedCell` read the audio thread is allowed,
            // held for the block and dropped at the end of it.
            let data = model.shared();

            // The block's notes, as `process()` turns them into changes on
            // exact frames: every fifth block a pattern note a third of the
            // way in, and a Stop note two thirds of the way in every other
            // time.
            let mut changes = SelectionChanges::new();
            if block % 5 == 2 {
                let start = notes::SUGGESTED_START;
                let keys = [start + (block % PATTERN_COUNT) as u8, notes::stop_key(start)];
                let count = if block % 10 == 2 { 2 } else { 1 };
                for (n, &key) in keys[..count].iter().enumerate() {
                    triggered = match notes::trigger_for_key(key, start) {
                        Some(Trigger::Play(number)) => Some(number),
                        _ => None,
                    };
                    changes.push((n + 1) * frames / 3, triggered.or(pattern_for(block)));
                }
            }

            // Step 3: the block itself.
            engine.run_block_with(&ctx, &data, &changes, &mut left[..frames], &mut right[..frames]);

            // What `lib.rs` does with the block afterwards is step 4 alone;
            // the rest of this is the accounting, which is arithmetic on the
            // stack and so belongs inside the guard as much as anything else
            // the audio thread does.
            for (out, dry) in left[..frames].iter().zip(&source[..frames]) {
                stats.peak = stats.peak.max(out.abs());
                if out != dry {
                    stats.wet += 1;
                }
            }
            if ctx.is_playing() {
                stats.playing += 1;
                beat += frames as f64 * ctx.beats_per_sample();
            } else {
                stats.stopped += 1;
            }
            if engine.reader().is_fading() {
                stats.faded += 1;
            }
            if engine.reader().active_heads() == 2 {
                stats.two_heads += 1;
            }
            match engine.selection() {
                Some(pattern) => stats.selections |= 1 << pattern,
                None => stats.live += 1,
            }
            stats.generation = stats.generation.max(scope.generation());

            // Step 4: the editor's wake-up call, with the window open.
            if ctx.is_playing() {
                repaint.request_repaint();
                stats.repaints += 1;
            }

            stats.blocks += 1;
            stats.frames += frames;
            stats.since_reset += frames as u64;
        }
    });

    // The run has to have been as busy as it claims, or a green guard means
    // nothing.
    assert_eq!(stats.blocks, BLOCKS);
    assert_eq!(stats.stopped, STOPPED.len() + NO_TRANSPORT.len(), "the transport stopped");
    assert_eq!(stats.playing, BLOCKS - stats.stopped);
    assert_eq!(stats.repaints, stats.playing, "every playing block woke the editor");
    assert_eq!(stats.resets, RESETS.len());
    assert_eq!(
        engine.history().recorded(),
        stats.since_reset,
        "every frame of every block was recorded, playing or stopped",
    );
    assert_eq!(
        stats.selections.count_ones() as usize,
        PATTERN_COUNT,
        "all sixteen patterns were walked, not just the first: {:016b}",
        stats.selections,
    );
    assert!(stats.live > 0, "some blocks played live audio — Off, stopped or waiting");
    assert!(stats.faded > 0, "crossfades were in flight at block edges: {}", stats.faded);
    assert!(stats.two_heads > 0, "both read heads ran at once: {}", stats.two_heads);
    assert!(stats.peak > 0.0, "the run rendered audible audio, peak {}", stats.peak);
    assert!(
        stats.wet * 4 > stats.frames,
        "{} of {} frames came out changed — the walk was reading the past, not passing \
         the input through",
        stats.wet,
        stats.frames,
    );
    assert!(
        stats.generation > FIRST_GENERATION,
        "the scope wrapped whole pattern cycles, generation {}",
        stats.generation,
    );
}

/// The control case: a single `Vec` push inside the same guard, which must
/// kill the process.
///
/// Documented *and* run, because "the guard is active" is the one assumption
/// the test above cannot check itself — a `nice_assert_no_alloc` that is not
/// the copy nice-plug installed, a build without `assert_process_allocs`, or
/// a test binary that fails to link nice-plug at all would all leave
/// `assert_no_alloc` a no-op and make a busy run pass without proving
/// anything. This is the "demonstrably links nice-plug through the crate"
/// half of the guard's test.
///
/// A violation calls `std::alloc::handle_alloc_error`, which aborts rather
/// than unwinding, so the allocating half cannot run in this process and be
/// caught: it runs in a child copy of this very test binary, asked for by
/// name with [`CONTROL_ENV`] set. The parent then asserts the child **died**.
/// If the guard were inert the child would instead print [`INERT_MARKER`] and
/// exit 0, which is why the assertion is on the exit status and not merely on
/// a failure to pass.
#[test]
fn the_guard_bites_a_deliberate_allocation() {
    if !GUARD_ACTIVE {
        eprintln!("skipped: this build installs no AllocDisabler, so the guard is a no-op");
        return;
    }
    if std::env::var_os(CONTROL_ENV).is_some() {
        allocate_under_the_guard();
        return;
    }

    let exe = std::env::current_exe().expect("a test binary knows its own path");
    let output = Command::new(exe)
        .args(["--exact", CONTROL_TEST, "--nocapture", "--test-threads=1"])
        .env(CONTROL_ENV, "1")
        .output()
        .expect("the control case re-runs this test binary");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stdout.contains(ENTER_MARKER),
        "the child never reached the guard, so its exit says nothing \
         (status {:?})\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}",
        output.status
    );
    assert!(
        !output.status.success(),
        "allocating inside assert_no_alloc did not kill the child: the no-alloc guard is \
         INERT and `a_busy_run_allocates_nothing` is checking nothing. Likely a second copy \
         of nice-assert-no-alloc (its forbid counter is a thread-local of one copy — keep \
         the dev-dependency pinned to nice-plug's), or a test binary not linking \
         nice-plug.\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
}

/// Env var that tells a child copy of this binary to be the allocating half
/// of the control case instead of spawning another one.
const CONTROL_ENV: &str = "STUTTTER_NO_ALLOC_CONTROL";

/// The control case's own test name, for the child's `--exact`.
const CONTROL_TEST: &str = "the_guard_bites_a_deliberate_allocation";

/// Printed by the child before it allocates, so the parent can tell an abort
/// from the guard apart from a child that never started.
const ENTER_MARKER: &str = "stuttter control case: allocating under the guard";

/// Printed by the child if it survives — i.e. if the guard is inert.
const INERT_MARKER: &str = "stuttter control case: survived, the guard is inert";

/// The allocating half, run only in the child process: grow a `Vec` by one
/// element inside the guard — the textbook breach of the real-time rule
/// (fixed-capacity buffers on that path).
///
/// A `Pattern`, because that is the shape this plugin would be tempted to
/// build on the audio thread; `Vec::new` reserves nothing, so the single
/// `push` must call the allocator. `black_box` keeps the optimiser from
/// deciding the vector is unused.
fn allocate_under_the_guard() {
    let mut patterns: Vec<Pattern> = Vec::new();

    println!("{ENTER_MARKER}");
    std::io::stdout().flush().expect("the marker reaches the parent before the abort");

    assert_no_alloc(|| {
        patterns.push(std::hint::black_box(Pattern::new()));
        std::hint::black_box(&patterns);
    });

    // Unreachable while the guard is live: the push above aborts the process.
    println!("{INERT_MARKER}");
}
