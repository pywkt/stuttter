//! The click test: the acceptance test for the two-head crossfade and the
//! volume ramp, against this requirement:
//!
//! > Across **every** action boundary (block start, block end, repeat wrap,
//! > pattern change, Off→pattern, stop/start, a simulated loop jump): feed a
//! > smooth signal (e.g. a low-frequency sine), run with default Smoothing,
//! > assert no adjacent-sample output delta exceeds a threshold derived from
//! > the signal's own maximum slope times a small factor.
//!
//! Every other engine test asks *where* the walk read; this one asks only
//! whether what came out can be listened to. A jump read straight through is
//! a step in the output — the click the fade exists to avoid — and a step is
//! visible in the output alone, without knowing anything about the offset
//! maths that caused it. So the whole of this file is one measurement —
//! [`worst_step`], made over a signal smooth enough that any step bigger
//! than the signal's own slope must have come from the engine.
//!
//! An integration test rather than a module in `engine.rs`, because it needs
//! nothing the plugin does not already make public: an [`Engine`], a
//! [`StutterData`] and a [`BlockCtx`] per block.
//!
//! # What makes it meaningful rather than vacuous
//!
//! A threshold test can pass for three uninteresting reasons, so each one is
//! closed off by an assertion of its own:
//!
//! - **The threshold is too wide.** It is not chosen: it is the signal's
//!   measured slope times [`SLOPE_FACTOR`], and
//!   [`the_threshold_is_derived_from_the_signals_own_slope`] holds it above
//!   the worst the engine's own smoothing may legitimately produce and well
//!   below a fraction of full scale.
//! - **It has no teeth.** [`the_threshold_catches_the_cuts_the_fades_hide`]
//!   runs the same boundaries with Smoothing at zero — a one-frame fade,
//!   which is the cut — and nearly all of them trip the threshold, the
//!   loudest by more than half of full scale.
//! - **The boundary never happened.** Every run also counts the frames where
//!   the read position moved further than [`MAX_READ_STEP`], which is the
//!   engine's own definition of a jump, and asserts the boundary under test
//!   is among them ([`assert_no_click`]).

use std::f64::consts::{PI, SQRT_2, TAU};
use std::ops::Range;
use std::sync::{Arc, LazyLock};

use stuttter::SMOOTH_DEFAULT_MS;
use stuttter::actions::{MAX_SPEED, SpeedPreset};
use stuttter::engine::{BlockCtx, Engine, MAX_READ_STEP};
use stuttter::history::fade_frames_for;
use stuttter::model::StutterData;
use stuttter::pattern::{Action, NoteValue, Pattern, Rate, Source};
use stuttter::scope::Scope;

/// 48 kHz — a rate a host really offers, because what this file measures is
/// a ratio between the fade's length in frames and the signal's slope per
/// frame, and both of those are what they are at a real rate.
const SAMPLE_RATE: f32 = 48_000.0;
/// 240 BPM, which makes one beat exactly 12 000 frames at [`SAMPLE_RATE`]:
/// fast, so that a run covers whole pattern cycles in few frames, and exact,
/// so that every step boundary lands on a whole frame and the constants
/// below are the frames they say they are.
const TEMPO: f64 = 240.0;
/// Frames in one beat at [`TEMPO`] and [`SAMPLE_RATE`].
const BEAT_FRAMES: usize = 12_000;

/// The fixture pattern's rate: 1/16 steps, so a step is 0.25 beats.
const RATE: Rate = Rate::Sixteenth;
/// Beats in one step at [`RATE`].
const STEP_BEATS: f64 = 0.25;
/// Frames in one step: 3000, twenty times [`fade_frames`] — every
/// boundary in this file is therefore far enough from the next that no fade
/// here is ever restarted mid-ramp. That case is bounded but not silent
/// (`history.rs` [`Reader`] §A jump during a fade: the dropped head's gain
/// is at most `√2/2`), and a test that mixed it in would be measuring the
/// design's known worst case rather than the smoothing's ordinary job.
///
/// [`Reader`]: stuttter::history::Reader
const STEP_FRAMES: usize = (STEP_BEATS * BEAT_FRAMES as f64) as usize;
/// The fixture pattern's length in steps: eight, so a cycle is two beats.
const STEPS: u16 = 8;
/// Beats in one cycle of the fixture pattern.
const SPAN_BEATS: f64 = STEPS as f64 * STEP_BEATS;
/// Frames in one cycle of the fixture pattern.
const CYCLE_FRAMES: usize = STEPS as usize * STEP_FRAMES;

/// The step the fixture's one Time-row block starts on: step 2, so there are
/// two steps of live audio before it — the material a Reverse or a
/// `BeforeBlock` repeat reaches back into.
const BLOCK_STEP: u16 = 2;
/// How many steps it covers: four, so `B` is one whole beat.
const BLOCK_STEPS: u16 = 4;
/// Frames from the start of a cycle to the start of that block.
const BLOCK_OFFSET: usize = BLOCK_STEP as usize * STEP_FRAMES;

/// How many cycles of lead-in every run plays before the assertions start.
///
/// Two, which is what makes the asserted cycle honest. A read reaching
/// before the first recorded frame is [`History`]'s silence, and the step
/// from the signal into silence is a click the engine never caused — so the
/// buffer has to be full of signal to the depth of the deepest read in the
/// file. That depth is two beats — a Reverse reads `d = 2p` and so ends its
/// block twice `B` behind live — and two cycles is four.
///
/// [`History`]: stuttter::history::History
const LEAD_IN_CYCLES: usize = 2;
/// How long every run is: the lead-in plus the one cycle that is asserted.
const FRAMES: usize = (LEAD_IN_CYCLES + 1) * CYCLE_FRAMES;
/// The frames the assertions are made over: the cycle after the lead-in.
///
/// The delta *into* the first of them is asserted too — the pair
/// `(start − 1, start)` straddles the seam — so the window has no silent
/// edge of its own.
const ASSERTED: Range<usize> = LEAD_IN_CYCLES * CYCLE_FRAMES..FRAMES;

/// The two frequencies of the input, in Hz: both well below the rate, so the
/// signal is smooth, and neither a divisor of any boundary spacing in the
/// file.
///
/// A single sine would be the requirement's example and a weaker test. Every
/// jump here moves the read head by a whole number of frames, and for one
/// sine there are jump sizes that are a whole number of its own periods — a
/// 1/16 slice at this tempo is 3000 frames, which is exactly 25 periods of
/// 400 Hz — so a cut at such a boundary reads the same value twice and
/// produces *no step at all*. A test built on a signal that aligned with its
/// own boundaries would pass with the crossfade deleted.
///
/// Two frequencies do not make that impossible — the two heads can still
/// meet at the same point of both waves, and
/// [`the_threshold_catches_the_cuts_the_fades_hide`] measures four of the
/// thirty-three cuts doing it — but they make it rare instead of systematic,
/// which is what lets that test ask for a majority and get 29 of 33.
const FREQUENCIES: [f64; 2] = [97.0, 173.0];

/// The factor the threshold is the signal's own slope times.
///
/// Small, in the sense the requirement means: a few times the slope, not a
/// few tenths of full scale. It has to clear the largest step the engine's
/// *own* smoothing can take — [`derived_bound`] computes that from the
/// fade's length and the top read speed, and the threshold test asserts the
/// factor covers it — and it is still ten times smaller than the loudest cut
/// at these boundaries, which is what the teeth test measures.
const SLOPE_FACTOR: f64 = 6.0;

/// One block of the input, in frames, for the runs that use realistic
/// blocks rather than one frame at a time.
const HOST_BLOCK: usize = 512;

/// The input at frame `frame`: two sines of [`FREQUENCIES`], half scale
/// each, so the pair spans `-1.0 ..= 1.0` and starts at zero.
///
/// The right channel is the left negated, as everywhere else in this
/// project's tests — a channel's *identity* is pinned by `engine.rs`, and
/// what this file needs of the right channel is only that it is a smooth
/// signal of the same slope, so that the gain, the blend and the fade are
/// each measured twice.
fn input(frame: usize) -> (f32, f32) {
    let seconds = frame as f64 / f64::from(SAMPLE_RATE);
    let sum: f64 = FREQUENCIES.iter().map(|hz| 0.5 * (TAU * hz * seconds).sin()).sum();

    (sum as f32, -sum as f32)
}

/// The signal's own maximum slope: the largest step the **input** takes
/// between two adjacent frames, measured over the whole run rather than
/// derived from the frequencies, so that the threshold is a fact about the
/// samples the engine was actually handed.
static MAX_SLOPE: LazyLock<f64> = LazyLock::new(|| {
    (1..FRAMES)
        .map(|frame| (f64::from(input(frame).0) - f64::from(input(frame - 1).0)).abs())
        .fold(0.0, f64::max)
});

/// The threshold itself: [`MAX_SLOPE`] times [`SLOPE_FACTOR`].
static THRESHOLD: LazyLock<f64> = LazyLock::new(|| SLOPE_FACTOR * *MAX_SLOPE);

/// How many frames the default Smoothing fades for at [`SAMPLE_RATE`]: 144,
/// and asked of the plugin's own function rather than written down, because
/// "default Smoothing" is the requirement's instruction and the default is
/// the parameter's to change.
fn fade_frames() -> usize {
    fade_frames_for(SMOOTH_DEFAULT_MS, f64::from(SAMPLE_RATE)) as usize
}

/// The largest step the engine's own smoothing may take in one frame, which
/// is what [`SLOPE_FACTOR`] has to cover. Three terms, each of them a bound
/// and not a measurement:
///
/// - **Two heads moving.** Inside a fade both heads walk the history at up
///   to [`MAX_SPEED`] frames per frame, so each carries up to `MAX_SPEED`
///   times the signal's slope, and the equal-power gains sum to at most
///   `√2`.
/// - **The fade's own ramp.** The gains move by at most `(π/2)/N` per frame
///   and multiply a difference of at most full scale either way, so the
///   envelope contributes `π/N`.
/// - **The volume ramp.** A level change of at most 1 spread over `N`
///   frames, on a signal of at most full scale: `1/N`.
fn derived_bound() -> f64 {
    let fade = fade_frames() as f64;

    SQRT_2 * MAX_SPEED * *MAX_SLOPE + PI / fade + 1.0 / fade
}

/// A pattern of [`STEPS`] steps of [`RATE`], with `action` (if any) over
/// steps `BLOCK_STEP .. BLOCK_STEP + BLOCK_STEPS` and `volumes` written onto
/// the Volume row from step 0 — the steps past its end keeping unity.
fn pattern_with(action: Option<Action>, volumes: &[f32]) -> Pattern {
    let mut pattern = Pattern::new();
    pattern.set_rate(RATE);
    pattern.set_length(STEPS);
    if let Some(action) = action {
        assert_eq!(pattern.add_block(BLOCK_STEP, BLOCK_STEPS, action), Some(0));
    }
    for (step, &volume) in volumes.iter().enumerate() {
        pattern.set_volume(step as u16, volume);
    }

    pattern
}

/// The one-pattern fixture: pattern 1 is [`pattern_with`]'s block of
/// `action`, and the other fifteen are empty.
fn data_with(action: Action) -> StutterData {
    StutterData::from_patterns([pattern_with(Some(action), &[])])
}

/// The two-pattern fixture the switching tests change between: pattern 1 a
/// Reverse over the middle of the cycle, pattern 2 a half speed over the
/// whole of it.
///
/// Half speed rather than another Repeat, because the two have to read
/// **different** material at the frame the change lands on, and two shapes
/// over the same grid can agree there by accident: a Repeat over the whole
/// cycle reads `d = p` at every slice boundary, and a Reverse three quarters
/// of the way through its block reads `d = 2p` — which at [`LEAVE_BOUNDARY`]
/// are the same number, so the change there would move nothing and the test
/// would be measuring a boundary that never happened. Against half speed's
/// `d = p/2` the two are 0.875 beats apart at that frame, which is the jump
/// [`a_pattern_change_lands_without_a_click`] asserts.
fn data_pair() -> StutterData {
    let mut second = Pattern::new();
    second.set_rate(RATE);
    second.set_length(STEPS);
    assert_eq!(second.add_block(0, STEPS, Action::from_preset(SpeedPreset::HalfSpeed)), Some(0));

    let first = pattern_with(Some(Action::from_preset(SpeedPreset::Reverse)), &[]);

    StutterData::from_patterns([first, second])
}

/// An engine at the fixture's rate, with a scope of its own.
fn new_engine() -> Engine {
    Engine::new(SAMPLE_RATE, Arc::new(Scope::new()))
}

/// The context for the `frames` frames starting at `frame`, with `pattern`
/// selected and **everything else at the plugin's defaults** — Smoothing
/// included, which is what the requirement asks this test to run at.
fn ctx_at(frame: usize, frames: usize, pattern: Option<u8>) -> BlockCtx {
    BlockCtx::new(frame as f64 / BEAT_FRAMES as f64, TEMPO, SAMPLE_RATE, frames)
        .with_pattern(pattern)
}

/// One run's output, and what the walk was doing while it was made.
struct Run {
    /// The left channel out, one entry per frame.
    left: Vec<f32>,
    /// The right channel out.
    right: Vec<f32>,
    /// Where each frame read, in absolute frames: the engine's own answer,
    /// which is how a boundary is proved to have been a discontinuity.
    position: Vec<f64>,
    /// Whether a fade was running on either side of the frame.
    faded: Vec<bool>,
}

impl Run {
    /// How many frames of the asserted cycle the read position jumped on,
    /// by the engine's own test for it ([`MAX_READ_STEP`]) — the boundaries
    /// the crossfade had to hide.
    fn jumps(&self) -> usize {
        self.position
            .windows(2)
            .skip(ASSERTED.start - 1)
            .filter(|pair| {
                let step = (pair[1] - pair[0]).abs();
                !step.is_finite() || step > MAX_READ_STEP
            })
            .count()
    }

    /// How many frames of the asserted cycle a fade touched.
    fn fades(&self) -> usize {
        self.faded[ASSERTED].iter().filter(|&&faded| faded).count()
    }

    /// How many frames of the asserted cycle came out as something other
    /// than the input — the non-vacuity of a run whose boundaries are not
    /// read-position jumps at all (a Volume row on live audio).
    fn changed(&self) -> usize {
        ASSERTED.filter(|&frame| self.left[frame] != input(frame).0).count()
    }
}

/// Runs [`FRAMES`] frames of [`input`] through the engine **one frame at a
/// time**, playing frame `frame` against `ctx_of(frame)`.
///
/// One frame per block is what makes the fade state visible per frame, and
/// it is how `engine.rs`'s own driver tests put a parameter or a transport
/// change on an exact frame. That the same input in host-sized blocks comes
/// out as smooth is [`the_block_size_does_not_introduce_a_click`].
fn run(data: &StutterData, ctx_of: impl Fn(usize) -> BlockCtx) -> Run {
    let mut engine = new_engine();
    let mut out = Run {
        left: Vec::with_capacity(FRAMES),
        right: Vec::with_capacity(FRAMES),
        position: Vec::with_capacity(FRAMES),
        faded: Vec::with_capacity(FRAMES),
    };

    for frame in 0..FRAMES {
        let fading_before = engine.reader().is_fading();
        let (left_in, right_in) = input(frame);
        let mut left = [left_in];
        let mut right = [right_in];

        engine.run_block(&ctx_of(frame), data, &mut left, &mut right);

        out.left.push(left[0]);
        out.right.push(right[0]);
        out.position.push(engine.read_position().expect("the frame read somewhere"));
        out.faded.push(fading_before || engine.reader().is_fading());
    }

    out
}

/// The same input in blocks of `block` frames, returning the output only —
/// the fade state is not visible per frame here, which is why [`run`] exists.
fn run_in_blocks(data: &StutterData, pattern: Option<u8>, block: usize) -> (Vec<f32>, Vec<f32>) {
    let mut engine = new_engine();
    let mut out = (Vec::with_capacity(FRAMES), Vec::with_capacity(FRAMES));
    let mut frame = 0;

    while frame < FRAMES {
        let frames = block.min(FRAMES - frame);
        let mut left: Vec<f32> = (frame..frame + frames).map(|frame| input(frame).0).collect();
        let mut right: Vec<f32> = (frame..frame + frames).map(|frame| input(frame).1).collect();

        engine.run_block(&ctx_at(frame, frames, pattern), data, &mut left, &mut right);

        out.0.extend_from_slice(&left);
        out.1.extend_from_slice(&right);
        frame += frames;
    }

    out
}

/// The largest adjacent-sample step in the asserted cycle, and the frame it
/// landed on — the one measurement this whole file is made of.
fn worst_step(samples: &[f32]) -> (f64, usize) {
    let mut worst = (0.0, ASSERTED.start);

    for (pair_index, pair) in samples.windows(2).enumerate().skip(ASSERTED.start - 1) {
        let step = (f64::from(pair[1]) - f64::from(pair[0])).abs();
        if step > worst.0 {
            worst = (step, pair_index + 1);
        }
    }

    worst
}

/// Asserts that neither channel of `left`/`right` steps by more than
/// [`THRESHOLD`] anywhere in the asserted cycle, naming the frame and the
/// step of the cycle it failed on — which is what tells a reader whether the
/// click was at a block edge, a wrap, or the boundary the scenario moved.
#[track_caller]
fn assert_smooth(label: &str, left: &[f32], right: &[f32]) {
    for (channel, samples) in [("left", left), ("right", right)] {
        let (worst, at) = worst_step(samples);

        assert!(
            worst <= *THRESHOLD,
            "{label}: the {channel} channel stepped {worst:.5} into frame {at} — step {} of \
             the cycle, {} frames in — past the {:.5} a signal of slope {:.5} may",
            (at - ASSERTED.start) / STEP_FRAMES,
            at - ASSERTED.start,
            *THRESHOLD,
            *MAX_SLOPE,
        );
    }
}

/// The whole assertion of this file, in one call: the run came out smooth,
/// and the boundary it was written for really did happen.
///
/// `least_jumps` is how many read-position discontinuities the scenario is
/// *supposed* to contain. It is what keeps a scenario from passing because
/// the engine quietly played live audio through it — the fade cannot be
/// credited with hiding a jump that was never there.
#[track_caller]
fn assert_no_click(label: &str, run: &Run, least_jumps: usize) {
    assert_smooth(label, &run.left, &run.right);

    let jumps = run.jumps();
    assert!(
        jumps >= least_jumps,
        "{label}: only {jumps} of the expected {least_jumps} read-position jumps happened in \
         the asserted cycle — the boundary under test did not happen, so nothing was smoothed",
    );
    if least_jumps > 0 {
        assert!(run.fades() > 0, "{label}: a jump happened and nothing crossfaded it");
    }
}

/// Asserts that the boundary at `frame` was a read-position discontinuity
/// and that the reader crossfaded it.
///
/// What turns [`assert_no_click`]'s count into a statement about *this*
/// boundary: a scenario is only evidence about the pattern change (or the
/// stop, or the jump) it was written for if the read position actually moved
/// at the frame that change took effect on.
#[track_caller]
fn assert_jumped_at(label: &str, run: &Run, frame: usize) {
    let step = (run.position[frame] - run.position[frame - 1]).abs();

    assert!(
        step > MAX_READ_STEP,
        "{label}: the read moved {step} frames at frame {frame}, which is not a jump — \
         the boundary under test changed nothing, so nothing was smoothed",
    );
    assert!(run.faded[frame], "{label}: the jump at frame {frame} was not crossfaded");
}

/// Every action, with the name it fails under.
///
/// The five presets, both repeat sources, three slice lengths, and two
/// hand-drawn ramps that run the read head to both ends of [`MAX_SPEED`].
/// Between them they put a jump at every place a block has one:
///
/// - **At the start**: double speed opens one `B` behind live, a
///   `BeforeBlock` repeat one slice behind it, and a ramp whose speed starts
///   above 1 opens at the shift that keeps it from passing "now".
/// - **Inside**: a repeat's wraps, three or seven of them per block here.
/// - **At the end**: every action but double speed, which is the one that
///   ends exactly on live and so joins the next step without a jump.
fn every_action() -> Vec<(&'static str, Action)> {
    let top = MAX_SPEED as f32;
    let mut actions = vec![
        ("repeat 1/16 from the block start", Action::repeat_at(RATE)),
        (
            "repeat 1/8 from the block start",
            Action::repeat(NoteValue::Eighth),
        ),
        (
            "repeat 1/32 from the block start",
            Action::repeat(NoteValue::ThirtySecond),
        ),
        (
            "repeat 1/16 from a fixed spot a pass back",
            Action::repeat(NoteValue::Sixteenth).with_source(Source::Spot { step: 0, passes: 1 }),
        ),
        ("a hand ramp from -2 to 2", Action::ramp(-top, top)),
        ("a hand ramp from 2 to -2", Action::ramp(top, -top)),
    ];
    for preset in SpeedPreset::ALL {
        actions.push((
            match preset {
                SpeedPreset::HalfSpeed => "half speed",
                SpeedPreset::DoubleSpeed => "double speed",
                SpeedPreset::Reverse => "reverse",
                SpeedPreset::TapeStop => "a tape stop",
                SpeedPreset::TapeStart => "a tape start",
            },
            Action::from_preset(preset),
        ));
    }

    actions
}

/// **Block start, block end, repeat wrap** — every action's own boundaries,
/// played straight through with nothing else moving.
///
/// One asserted cycle of each action covers all three: the two steps of live
/// audio before the block, the entry, the inside of it (where a repeat
/// wraps and a ramp turns round), the exit back to live, and the two live
/// steps after it.
#[test]
fn every_action_plays_its_block_edges_without_a_click() {
    for (name, action) in every_action() {
        let run = run(&data_with(action), |frame| ctx_at(frame, 1, Some(1)));

        // Every action in the list jumps at least once per cycle: at the
        // entry, at the exit, or at a wrap. Which one is the action's own
        // business and is pinned by `engine.rs`; that one happened at all is
        // what makes this run worth measuring.
        assert_no_click(name, &run, 1);
    }
}

/// **Repeat wrap**, on its own and counted: a 1/16 slice over a four-step
/// block wraps three times inside the asserted cycle and jumps once more on
/// the way out, and every one of those is a step of a quarter of a beat in
/// the read position that the output must not show.
#[test]
fn a_repeat_wraps_without_a_click() {
    for (name, slice, wraps) in [
        ("a 1/16 slice", NoteValue::Sixteenth, 3),
        ("a 1/32 slice", NoteValue::ThirtySecond, 7),
    ] {
        let action = Action::repeat(slice);
        let run = run(&data_with(action), |frame| ctx_at(frame, 1, Some(1)));

        // The wraps, plus the jump back to live at the block's end. The
        // entry is not one: a `BlockStart` repeat's first slice *is* live.
        assert_no_click(name, &run, wraps + 1);

        // And each wrap is where the grid says it is, which is what makes
        // the count above a statement about wraps rather than about jumps in
        // general.
        let slice_frames = (slice.beats() * BEAT_FRAMES as f64) as usize;
        for wrap in 1..=wraps {
            assert_jumped_at(name, &run, BLOCK_START + wrap * slice_frames);
        }
    }
}

/// The **Volume row**, which this file's opening names alongside the
/// crossfade as what this test accepts: a level change at a step boundary is
/// a jump in the gain, not in the read position, and the ramp is the only
/// thing between it and a click.
///
/// Both halves of it: a pattern with no Time row at all, where the level is
/// the only thing happening and the material is live audio; and the same row
/// over a Repeat, where a level change and a wrap are a step apart.
#[test]
fn the_volume_row_changes_level_without_a_click() {
    const LEVELS: [f32; STEPS as usize] = [1.0, 0.0, 0.75, 0.0, 1.0, 0.25, 0.5, 1.0];

    let live = StutterData::from_patterns([pattern_with(None, &LEVELS)]);
    let live = run(&live, |frame| ctx_at(frame, 1, Some(1)));
    // No read-position jump anywhere — live audio throughout — so the count
    // that proves this run did something is the gain's.
    assert_no_click("a volume row over live audio", &live, 0);
    assert_eq!(live.jumps(), 0, "the volume row moved the read position");
    assert!(live.changed() > CYCLE_FRAMES / 2, "the volume row changed nothing");

    let repeat = StutterData::from_patterns([pattern_with(Some(Action::repeat_at(RATE)), &LEVELS)]);
    let repeat = run(&repeat, |frame| ctx_at(frame, 1, Some(1)));
    assert_no_click("a volume row over a repeat", &repeat, 4);
}

/// Where the fixture's one block starts, in the asserted cycle.
const BLOCK_START: usize = ASSERTED.start + BLOCK_OFFSET;

/// The frame the parameter moves on where the change takes effect **at
/// once**: Off→pattern, which does not wait for a step boundary, and the
/// transport edges, which have no boundary to wait for.
///
/// A third of a step into the fourth step of the block — ten twelfths of the
/// way through it. Two things make that frame and not another:
///
/// - **Every action is reading somewhere else by then.** A `BlockStart`
///   repeat plays its first slice live (`d = 0`), and the longest slice here
///   is half the block, so anywhere in the block's first half there are
///   actions with nothing to crossfade. Past the half way point every action
///   in [`every_action`] is offset.
/// - **It is not on anybody's grid line.** A third of a step is 1000 frames,
///   so the frame is 500 or more from every step and slice boundary in the
///   file — further than [`fade_frames`] — and no fade here is ever
///   restarted by a boundary beside it ([`STEP_FRAMES`]).
const ENTER_AT: usize = BLOCK_START + 3 * STEP_FRAMES + STEP_FRAMES / 3;
/// The frame the parameter moves on where the change is **held back** to the
/// next step boundary: a pattern change either way, and pattern→Off.
///
/// Mid-step, a step earlier than [`ENTER_AT`], so that the boundary it waits
/// for is [`LEAVE_BOUNDARY`] and not the block's own end.
const LEAVE_AT: usize = BLOCK_START + 2 * STEP_FRAMES + STEP_FRAMES / 3;
/// The step boundary a change made at [`LEAVE_AT`] takes effect on: three
/// quarters of the way through the block, where — as at [`ENTER_AT`] — every
/// action is reading somewhere other than live.
const LEAVE_BOUNDARY: usize = BLOCK_START + 3 * STEP_FRAMES;

/// **Pattern change**: the `pattern` parameter moving between two patterns
/// that share nothing, mid-step and on a step boundary, in both directions.
///
/// Either way the change lands on [`LEAVE_BOUNDARY`] — a pattern change is
/// held back to the next step boundary of the pattern that is playing, and
/// the one made *on* that boundary takes effect there and then — so that is
/// the frame the jump is asserted at.
#[test]
fn a_pattern_change_lands_without_a_click() {
    let data = data_pair();

    for (name, from, to) in [("1 to 2", Some(1), Some(2)), ("2 to 1", Some(2), Some(1))] {
        for (when, at) in [("mid-step", LEAVE_AT), ("on a boundary", LEAVE_BOUNDARY)] {
            let label = format!("a pattern change {name} {when}");
            let run = run(&data, |frame| ctx_at(frame, 1, if frame < at { from } else { to }));

            assert_no_click(&label, &run, 1);
            assert_jumped_at(&label, &run, LEAVE_BOUNDARY);
        }
    }
}

/// **Off→pattern** — and back, which is the other direction of the same
/// parameter move.
///
/// Off joins a pattern at the frame the parameter arrives, wherever in the
/// bar that is, so the jump this asserts is one the grid had no say in
/// ([`Selector`] §Where it does not wait). Going the other way the bypass
/// does wait for the step boundary, so the jump is at [`LEAVE_BOUNDARY`].
///
/// [`Selector`]: stuttter::engine::Engine
#[test]
fn off_into_a_pattern_and_back_lands_without_a_click() {
    for (name, action) in every_action() {
        let data = data_with(action);

        let label = format!("Off into {name}");
        let entering = run(&data, |frame| ctx_at(frame, 1, (frame >= ENTER_AT).then_some(1)));
        assert_no_click(&label, &entering, 1);
        assert_jumped_at(&label, &entering, ENTER_AT);

        let label = format!("{name} into Off");
        let leaving = run(&data, |frame| ctx_at(frame, 1, (frame < LEAVE_AT).then_some(1)));
        assert_no_click(&label, &leaving, 1);
        assert_jumped_at(&label, &leaving, LEAVE_BOUNDARY);
    }
}

/// The frame the stop/start test presses stop on — [`ENTER_AT`], where every
/// action is reading somewhere behind live and nothing else is happening.
const STOP: usize = ENTER_AT;
/// How long it leaves the transport stopped: a whole step, which is long
/// enough for the fade back to live to finish inside it.
const STOPPED_FRAMES: usize = STEP_FRAMES;
/// The frame it presses play again on.
const RESUME: usize = STOP + STOPPED_FRAMES;

/// **Stop/start**: the host stops the transport mid-block and starts it
/// again from where it stopped, which is what pressing stop and then play
/// does.
///
/// Two jumps to hide, in opposite directions: on the stop the read comes
/// back to live from wherever the action had got to, and on the restart it
/// leaves live for the middle of the step the song is still sitting on.
#[test]
fn stopping_and_starting_the_transport_is_smooth() {
    for (name, action) in every_action() {
        let data = data_with(action);
        let run = run(&data, |frame| match frame {
            frame if frame < STOP => ctx_at(frame, 1, Some(1)),
            frame if frame < RESUME => ctx_at(STOP, 1, Some(1)).with_playing(false),
            frame => ctx_at(frame - STOPPED_FRAMES, 1, Some(1)),
        });

        // Both edges are jumps for every action in the list: at [`STOP`] the
        // read is behind live and comes back to it, and at [`RESUME`] it
        // leaves live for the middle of the step the song is still on.
        let label = format!("stop and start under {name}");
        assert_no_click(&label, &run, 2);
        assert_jumped_at(&label, &run, STOP);
        assert_jumped_at(&label, &run, RESUME);

        // Stopped is bypassed, which is the other half of the rule and the
        // reason the stop frame is a jump at all.
        for frame in RESUME - fade_frames()..RESUME {
            assert_eq!(run.left[frame], input(frame).0, "frame {frame} was not bypassed");
        }
    }
}

/// The frame the jump tests move the song position on — [`ENTER_AT`] again,
/// mid-step and mid-slice, so that the fade seen there is the transport's and
/// not a wrap's.
const JUMP_AT: usize = ENTER_AT;
/// The beat the song has got to by then.
const JUMP_BEAT: f64 = JUMP_AT as f64 / BEAT_FRAMES as f64;

/// The transport of a run the host moves the song position in: beat 0
/// onwards up to [`JUMP_AT`], and `to` onwards from there — a loop jump when
/// `to` is behind where the song had got to, a relocation when it is ahead.
fn jumped_to(to: f64) -> impl Fn(usize) -> BlockCtx {
    move |frame| {
        let beat = match frame < JUMP_AT {
            true => frame as f64 / BEAT_FRAMES as f64,
            false => to + (frame - JUMP_AT) as f64 / BEAT_FRAMES as f64,
        };

        BlockCtx::new(beat, TEMPO, SAMPLE_RATE, 1).with_pattern(Some(1))
    }
}

/// **A simulated loop jump**, both ways: the song position moving backwards
/// (a loop whose end it ran into) and forwards (a click in the arranger),
/// landing somewhere the walk was not reading.
#[test]
fn a_loop_jump_or_relocation_is_smooth() {
    // Back by a cycle and two steps, and forward by two steps. Both move the
    // read for every action in the list — the backward one by crossing the
    // longest slice's boundary (a jump *inside* a slice leaves a Repeat's `d`
    // alone, which is correct and smooth but proves nothing), the forward one
    // by landing past the end of the block, in live audio. And both land
    // mid-slice, so the fade they start is not restarted beside it.
    let back = JUMP_BEAT - (SPAN_BEATS + 2.0 * STEP_BEATS);
    let forward = JUMP_BEAT + 2.0 * STEP_BEATS;

    for (name, action) in every_action() {
        let data = data_with(action);

        for (direction, to) in [("back", back), ("forward", forward)] {
            let label = format!("a jump {direction} under {name}");
            let run = run(&data, jumped_to(to));

            assert_no_click(&label, &run, 1);
            assert_jumped_at(&label, &run, JUMP_AT);
        }
    }
}

/// The other half of the loop-jump rule, and the reason the engine measures
/// jumps in the read position rather than in the song position: a loop whose
/// length is a whole number of pattern cycles leaves the read exactly where
/// it was, and is read **straight through** — no jump, no fade, and still no
/// click.
///
/// Worth asserting here rather than only in `engine.rs`, because fading it
/// would be audible in this very measurement: two heads over the same
/// material sum to +3 dB in the middle of an equal-power ramp, which is a
/// step this threshold would catch.
#[test]
fn a_loop_jump_the_grid_lines_up_with_plays_straight_through() {
    for (name, action) in every_action() {
        let data = data_with(action);
        let run = run(&data, jumped_to(JUMP_BEAT - SPAN_BEATS));

        assert_no_click(&format!("a cycle-aligned jump under {name}"), &run, 0);

        // Not "the read stood still": inside a ramp it is moving at the
        // action's own speed, and what the aligned jump must not do is move
        // it by more than that step ([`MAX_READ_STEP`]).
        let step = (run.position[JUMP_AT] - run.position[JUMP_AT - 1]).abs();
        assert!(step <= MAX_READ_STEP, "{name}: the aligned jump moved the read {step} frames");
        assert!(!run.faded[JUMP_AT], "{name}: a jump the grid lines up with was crossfaded");
    }
}

/// The host's own block boundaries are a seam too — the walk carries the
/// read position, the level ramp and the fade across them — so the same
/// runs in host-sized blocks must come out as smooth as they do one frame
/// at a time.
#[test]
fn the_block_size_does_not_introduce_a_click() {
    for (name, action) in every_action() {
        let data = data_with(action);

        for block in [HOST_BLOCK, 64, 1] {
            let (left, right) = run_in_blocks(&data, Some(1), block);
            assert_smooth(&format!("{name} in blocks of {block}"), &left, &right);
        }
    }
}

/// The threshold is not a number chosen to make the tests pass: it is the
/// signal's own measured slope times a small factor, it clears the worst the
/// engine's own smoothing can legitimately do ([`derived_bound`]), and it is
/// a small fraction of full scale — so a step it allows is inaudible and a
/// step it catches is a click.
#[test]
fn the_threshold_is_derived_from_the_signals_own_slope() {
    // The runs use the Smoothing parameter's default, which is what the
    // requirement asks of this test and what the bound below is computed at.
    assert_eq!(ctx_at(0, 1, Some(1)).fade_frames() as usize, fade_frames());
    assert_eq!(fade_frames(), 144, "the default fade is not 3 ms of 48 kHz");

    // The signal really is smooth: a slope of well under a hundredth of full
    // scale per frame, and nowhere near the two frequencies' sum, which is
    // the bound a pair of sines could in principle reach.
    let bound: f64 = FREQUENCIES.iter().map(|hz| 0.5 * TAU * hz / f64::from(SAMPLE_RATE)).sum();
    assert!(*MAX_SLOPE < bound, "the measured slope {:.5} exceeds its own bound", *MAX_SLOPE);
    assert!(*MAX_SLOPE < 0.02, "the signal is not smooth: slope {:.5}", *MAX_SLOPE);

    // Wide enough for the smoothing the engine is entitled to…
    assert!(
        *THRESHOLD >= derived_bound(),
        "the threshold {:.5} is under the {:.5} the engine's own smoothing may step by",
        *THRESHOLD,
        derived_bound(),
    );
    // …and narrow enough to be a click test: a tenth of full scale, where
    // the cuts the teeth test measures are whole multiples of it.
    assert!(*THRESHOLD < 0.12, "the threshold {:.5} is too wide to catch a click", *THRESHOLD);
}

/// The teeth: with Smoothing at zero every boundary in this file is a cut —
/// a one-frame fade, which renders the outgoing head alone and then drops it
/// — and the cuts step past the threshold by a wide margin.
///
/// Without this the whole file could pass with the crossfade deleted. The
/// runs are otherwise identical to the ones above, so what they measure is
/// the fade and nothing else.
///
/// # Why a count rather than every case
///
/// How big a cut *is* depends on where in the signal it lands. Two heads a
/// whole number of the input's periods apart read the same value, so a cut
/// between them is silent however little smoothing there was — and with a
/// periodic input some of the jumps in this file land there, whatever the two
/// frequencies are: a Repeat's wrap moves the head by a slice, a Reverse's
/// exit by twice the block, and the ramps by a fraction of a frame that the
/// interpolator fills in. That is a fact about sine waves, not about the
/// engine, and it is the reason this test asks for a **large majority** of
/// the cuts to be caught and for the worst of them to be enormous, rather
/// than for every single one to click.
///
/// As measured when this test was written: 29 of the 33 cuts step past the
/// threshold, the loudest of them by 1.12 — more than ten times it, and more
/// than half of full scale — while the four that escape are all under 0.036,
/// which is below what the *smoothed* runs of the same boundaries reach
/// (0.038, a double-speed fade). There is nothing between those two groups:
/// no choice of [`SLOPE_FACTOR`] between 2.2 and 9 would catch a different
/// number of them, which is what makes the factor's exact value uncritical.
#[test]
fn the_threshold_catches_the_cuts_the_fades_hide() {
    /// How many of the cuts have to trip the threshold, as a fraction: two
    /// in three, which no accident of phase comes near.
    const CAUGHT_IN: (usize, usize) = (2, 3);

    let cut = |ctx: BlockCtx| ctx.with_smooth_ms(0.0);
    let (mut cases, mut loudest) = (0, 0.0_f64);
    let mut escaped: Vec<String> = Vec::new();

    for (name, action) in every_action() {
        let data = data_with(action);
        let jump = jumped_to(JUMP_BEAT - (SPAN_BEATS + 2.0 * STEP_BEATS));

        for (boundary, run) in [
            ("its own edges", run(&data, |frame| cut(ctx_at(frame, 1, Some(1))))),
            (
                "Off into it",
                run(&data, |frame| cut(ctx_at(frame, 1, (frame >= ENTER_AT).then_some(1)))),
            ),
            ("a loop jump", run(&data, |frame| cut(jump(frame)))),
        ] {
            let (worst, at) = worst_step(&run.left);

            cases += 1;
            loudest = loudest.max(worst);
            if worst <= *THRESHOLD {
                escaped.push(format!("{name}, {boundary}: {worst:.5} at frame {at}"));
            }
        }
    }

    let caught = cases - escaped.len();
    assert!(
        caught * CAUGHT_IN.1 >= cases * CAUGHT_IN.0,
        "only {caught} of {cases} cuts stepped past the {:.5} threshold — a threshold that \
         lets cuts through is not a click test. The ones it let through:\n  {}",
        *THRESHOLD,
        escaped.join("\n  "),
    );
    // The margin, as a number a future change to the signal or the threshold
    // has to keep: the loudest cut is an order of magnitude over the line,
    // not a hair over it.
    assert!(
        loudest > 8.0 * *THRESHOLD,
        "the loudest cut stepped only {loudest:.5}, against a threshold of {:.5} — the margin \
         between a click and the smoothing that hides it has gone",
        *THRESHOLD,
    );
}
