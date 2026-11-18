//! **The wiring audit**: the real plugin type, instantiated and driven the
//! way the CLAP wrapper drives it, with nothing mocked that this crate owns.
//!
//! What makes this different from the shell's own tests inside `lib.rs` is
//! the side of the crate boundary it stands on. Everything here goes through
//! the public surface a host has and no more: [`Stuttter::default`], the
//! [`Plugin`] trait's own methods, and the `Arc<dyn Params>` that
//! [`Plugin::params`] hands back — so the three parameters are read and
//! written through [`ParamPtr`], which is what nice-plug's wrapper reads and
//! writes them through, and the sixteen patterns arrive through
//! [`Params::deserialize_fields`], which is what Bitwig calls with the blob
//! it stored. A test that reached into the struct's fields could pass over a
//! plugin whose public doors were wired to nothing; this one cannot.
//!
//! The only stand-in anywhere is the host itself ([`Host`]): nice-plug ships
//! no test host, and there is no Bitwig and no audio device in a test run.
//! Its two refusals are part of the audit — an effect that reads only the
//! past must never report latency, and one that declares no note output must
//! never send an event.
//!
//! The four claims, one test each:
//!
//! - [`the_three_host_parameters_are_the_documented_ones`] — the parameter
//!   list as the host sees it: three ids in one flat group, with the
//!   ranges, defaults, units and display text they are meant to have.
//! - [`the_persist_field_restores_a_whole_project_with_no_editor_anywhere`]
//!   — a project's worth of patterns and both GUI prefs through
//!   `deserialize_fields` and back out of `serialize_fields`, on an instance
//!   that never opened a window.
//! - [`the_process_path_runs_end_to_end_over_the_restored_patterns`] — the
//!   history, the engine and the model under one run of `process()`: a live
//!   cycle recorded, a project loaded between two blocks, and the pattern it
//!   brought playing out of that recording frame for frame.
//! - [`the_process_path_writes_the_scope_the_waveform_band_draws`] — the
//!   scope half of the same path, which is what the editor draws from.
//!
//! The two audio tests use the ramp trick: the input is `x[n] = n`, so every
//! output sample *is* the position it was read from and the offset maths can
//! be checked frame by frame.

use std::collections::{BTreeMap, VecDeque};
use std::ops::Range;

use nice_plug::context::process::SendEventError;
use nice_plug::params::persist::{deserialize_field, serialize_field};
use nice_plug::prelude::*;

use stuttter::actions::SpeedPreset;
use stuttter::history::MAX_LOOKBACK_SECONDS;
use stuttter::model::StutterModel;
use stuttter::pattern::{Action, MAX_STEPS, PATTERN_COUNT, Pattern, Rate};
use stuttter::scope::{COLUMNS, Column, FIRST_GENERATION};
use stuttter::state::{Prefs, SNAPSHOT_VERSION, Snapshot};
use stuttter::{
    Stuttter, MIX_DEFAULT, PATTERN_DEFAULT, PATTERN_MAX, PATTERN_OFF, SMOOTH_DEFAULT_MS,
    SMOOTH_MAX_MS, SMOOTH_MIN_MS,
};

/// 32 768 Hz — a rate no host offers, chosen because it makes the arithmetic
/// exact rather than nearly so (the same fixture rate the shell's own tests
/// and `engine.rs`'s driver tests use, and for the same reason): at [`TEMPO`]
/// one beat is exactly 16 384 frames, so every read position lands on a whole
/// frame, where the interpolator returns the recorded sample itself and the
/// ramp reads back exactly.
const RATE: f32 = 32_768.0;
/// 120 BPM — see [`RATE`].
const TEMPO: f64 = 120.0;
/// Frames in one beat at [`RATE`] and [`TEMPO`].
const BEAT: usize = 16_384;
/// Frames in one step of the fixture pattern: a 1/16 at [`RATE`] and
/// [`TEMPO`], so a quarter of a beat.
const STEP: usize = BEAT / 4;
/// Steps in the fixture pattern: eight, so one cycle is two beats.
const STEPS: u16 = 8;
/// Frames in one cycle of the fixture pattern.
const CYCLE: usize = STEPS as usize * STEP;
/// The block length the runs hand the plugin. 512 frames divides neither a
/// step nor a beat into one block, so every assertion below is also an
/// assertion that the walk reads straight through a block seam.
const BLOCK: usize = 512;
/// Frames to leave unasserted after a read-position jump: the Smoothing
/// parameter's default 3 ms is 99 frames at [`RATE`], and inside that window
/// the output is two positions mixed by design, so the reads are asserted
/// away from the crossfade windows.
const FADED: usize = 128;

// ---- the host ---------------------------------------------------------

/// A stand-in for the host, so `process()` can be driven with no Bitwig and
/// no audio device: it reports one transport and records what the plugin
/// sends it.
///
/// The note queues are here because `ProcessContext` demands them whatever
/// `MIDI_INPUT` says — and a shell that declares no note output should be
/// *seen* to send nothing, which is what `sent` is for.
struct Host {
    transport: Transport,
    incoming: VecDeque<PluginNoteEvent<Stuttter>>,
    sent: Vec<PluginNoteEvent<Stuttter>>,
}

impl Host {
    /// A host reporting `transport`, with nothing queued for the block.
    fn new(transport: Transport) -> Host {
        Host { transport, incoming: VecDeque::new(), sent: Vec::new() }
    }
}

impl ProcessContext<Stuttter> for Host {
    fn plugin_api(&self) -> PluginApi {
        PluginApi::Clap
    }

    // `BackgroundTask = ()`, so there is nothing to run and nothing worth
    // recording: a task that carries no information cannot be asserted on.
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

    // Three calls the shell has no business making, and the audit is that
    // they never happen: stuttter reads only the past and so reports no
    // latency ever, an effect asking the host to restart it would be a bug,
    // and voice capacity may only be set with `CLAP_POLY_MODULATION_CONFIG`,
    // which this plugin does not set.
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

/// The other half of a host: what `activate()` is handed, with the same
/// three refusals as the process half.
impl ActivateContext<Stuttter> for Host {
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

/// A transport that says nothing: no position, no tempo, not playing — a
/// stopped block.
fn stopped() -> Transport {
    Transport::new(RATE)
}

/// The transport a host reports while playing, with the song position at
/// absolute frame `frame` of the run — which is also the frame the input
/// ramp's value is taken from, so "the beat" and "the recorded past" are the
/// same clock.
fn playing_at(frame: usize) -> Transport {
    let mut transport = Transport::new(RATE);
    transport.playing = true;
    transport.tempo = Some(TEMPO);
    transport.pos_beats = Some(frame as f64 / BEAT as f64);

    transport
}

// ---- driving the plugin the way the wrapper does ----------------------

/// Takes `plugin` through the lifecycle the CLAP wrapper runs before the
/// first block, at `rate`.
///
/// In the wrapper's own order (`wrapper/clap/wrapper.rs`, `activate`): every
/// smoother primed from its parameter's value *first* — a smoother built by
/// `with_smoother` starts at 0.0, so without this the opening blocks would be
/// read at a mix no host ever has — then `activate()`, then the `reset()`
/// nice-plug always calls after it.
///
/// Everything downstream of this is the real plugin: the engine it holds
/// afterwards is the one `process()` drives, with its history buffer sized
/// from the rate given here.
fn activate(plugin: &mut Stuttter, rate: f32) {
    let params = plugin.params();
    for (_, param, _) in params.param_map() {
        // SAFETY: the parameters live behind the `Arc` this plugin holds, so
        // they outlive `params` and this loop. nice-plug marks the setters
        // `unsafe` to keep a plugin from moving parameters behind the host's
        // back, which is exactly what a test with no host has to do; there
        // is no memory-safety contract beyond the pointer being live.
        unsafe { param._internal_update_smoother(rate, true) };
    }

    let config = BufferConfig {
        sample_rate: rate,
        min_buffer_size: Some(1),
        max_buffer_size: BLOCK as u32,
        process_mode: ProcessMode::Realtime,
    };
    let mut host = Host::new(stopped());

    assert!(
        plugin.activate(&Stuttter::AUDIO_IO_LAYOUTS[0], &config, &mut host),
        "the plugin refused the stereo layout at {rate} Hz"
    );
    plugin.reset();
}

/// Selects pattern `value` the way a host does: a normalized value written
/// into the `pattern` parameter, found by the id a saved project refers to
/// rather than by position.
fn select_pattern(plugin: &Stuttter, value: i32) {
    let params = plugin.params();
    let (_, pattern, _) = params
        .param_map()
        .into_iter()
        .find(|(id, _, _)| id == "pattern")
        .expect("the plugin has no `pattern` parameter");

    // SAFETY: as in `activate` — the parameter outlives `params`, and the
    // `unsafe` marks this as the host's business rather than the plugin's.
    unsafe {
        let normalized = pattern.preview_normalized(value as f32);
        pattern._internal_set_normalized_value(normalized);
        assert_eq!(pattern.modulated_plain_value(), value as f32, "pattern {value} did not take");
    }
}

/// Runs one block of `channels` through `process()` against `host`, and
/// returns the status.
///
/// The channels go in holding the input, which is where nice-plug's in-place
/// buffer management leaves it before the call: the wrapper copies the input
/// over the output whenever the host did not hand the same pointers for
/// both, so an effect reads and writes one buffer.
fn render_block(plugin: &mut Stuttter, channels: &mut [Vec<f32>; 2], host: &mut Host) {
    let frames = channels[0].len();
    assert_eq!(channels[1].len(), frames, "both channels are the same length");

    let mut buffer = Buffer::default();
    // SAFETY: `channels` outlives `buffer`, which is dropped at the end of
    // this function, and both slices are `frames` long as `set_slices`
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

    assert!(matches!(status, ProcessStatus::KeepAlive), "{status:?}");
    assert!(host.sent.is_empty(), "an effect with no note output sent an event");
}

/// Runs the absolute frames `frames` of the ramp input through `plugin`'s own
/// `process()`, in [`BLOCK`]-sized blocks, with the transport `transport_at`
/// reports for each block's first frame.
///
/// Returns the two output channels, indexed from `frames.start`. The right
/// channel carries `−n`, so a channel copied or swapped over the other shows
/// up.
fn run(
    plugin: &mut Stuttter,
    frames: Range<usize>,
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
        let mut host = Host::new(transport_at(start));

        render_block(plugin, &mut channels, &mut host);

        left.extend_from_slice(&channels[0]);
        right.extend_from_slice(&channels[1]);
        start += block;
    }

    (left, right)
}

// ---- the fixtures, built through the public writer API ----------------

/// The blob a host stores for `snapshot`, under the key a saved project
/// refers to.
///
/// [`serialize_field`] is the function the `Params` derive itself calls for a
/// `#[persist]` field, so this is the same JSON the plugin would have written
/// — not a hand-rolled encoding that only this test can read.
fn stored(snapshot: &Snapshot) -> BTreeMap<String, String> {
    BTreeMap::from([(
        String::from("state"),
        serialize_field(snapshot).expect("the snapshot would not serialise"),
    )])
}

/// The snapshot inside a host's blob, parsed back out.
fn snapshot_in(blob: &BTreeMap<String, String>) -> Snapshot {
    assert_eq!(
        blob.keys().collect::<Vec<&String>>(),
        vec!["state"],
        "one blob, under the key every saved project refers to"
    );

    deserialize_field(&blob["state"]).expect("the stored blob is not a snapshot")
}

/// The audio fixture as a saved project: pattern 1 is [`STEPS`] steps of
/// 1/16 with one Repeat block over the whole of it — the classic stutter,
/// whose read offset is `d = p − (p mod ℓ)`, so the first slice of every
/// cycle plays live and the seven after it replay it.
///
/// Built through the model's writer API and serialised, which is a headless
/// edit twice over: there is no editor in the writing and no editor in the
/// loading.
fn fixture_project() -> Snapshot {
    let model = StutterModel::default();

    assert!(model.set_rate(0, Rate::Sixteenth));
    assert_eq!(model.set_length(0, STEPS), Some(STEPS));
    assert_eq!(model.add_block(0, 0, STEPS, Action::repeat_at(Rate::Sixteenth)), Some(0));

    Snapshot::of(&model.data(), Prefs::default())
}

/// A whole project's worth of state: every one of the sixteen patterns with a
/// rate, a length, a Volume row and two blocks of its own, and both GUI prefs
/// away from their defaults.
///
/// Nothing here is a default, which is what makes the round trip below a
/// fact rather than a coincidence: the Volume rows are not flat *past their
/// pattern's length as well as inside it*, so a save that dropped the hidden
/// half of a row would fail; one block is a hand-drawn ramp, which is the
/// case a stored preset label must not be invented for; and both prefs are
/// the opposite of what a fresh instance holds.
fn filled_project() -> Snapshot {
    let model = StutterModel::default();

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

    let last_action = Action::from_preset(SpeedPreset::Reverse);
    let prefs = Prefs { keep_on_top: false, last_action, notes: Some(96), latch: true };
    assert_ne!(prefs, Prefs::default(), "the fixture prefs are the defaults");

    Snapshot::of(&model.data(), prefs)
}

// ---- the audit --------------------------------------------------------

/// **The parameters, as the host sees them**: three ids in one flat group,
/// each with the range, default, unit and display text it is meant to have.
///
/// Read through [`ParamPtr`] off the `Arc<dyn Params>` the plugin hands back,
/// which is the only view of them a host has — so this also pins the ids a
/// saved project's automation refers to, and the flags that make all three
/// automatable and visible in a generic UI.
#[test]
fn the_three_host_parameters_are_the_documented_ones() {
    let plugin = Stuttter::default();
    let params = plugin.params();
    let map = params.param_map();

    let ids: Vec<&str> = map.iter().map(|(id, _, _)| id.as_str()).collect();
    assert_eq!(ids, vec!["pattern", "mix", "smooth"]);
    // One flat list, no parameter groups: three controls need no tree.
    assert!(map.iter().all(|(_, _, group)| group.is_empty()));

    // SAFETY: every `ParamPtr` here points into the `Arc<dyn Params>` bound
    // above, which outlives this block — the whole of nice-plug's reading
    // path for a parameter is `unsafe` for that one reason.
    unsafe {
        for (id, param, _) in &map {
            assert_eq!(param.flags(), ParamFlags::default(), "{id} is not plainly automatable");
        }

        // `pattern`: 17 stepped positions — Off, then 1 through 16 — opening
        // at 1, with `Off` spelled out and the rest shown as the number.
        let pattern = map[0].1;
        assert_eq!(pattern.name(), "Pattern");
        assert_eq!(pattern.unit(), "");
        assert_eq!(pattern.step_count(), Some(16), "a knob, not 17 positions");
        assert_eq!(pattern.default_plain_value(), PATTERN_DEFAULT as f32);
        assert_eq!(PATTERN_DEFAULT, 1, "the documented default moved");
        assert_eq!(pattern.preview_plain(0.0), PATTERN_OFF as f32);
        assert_eq!(pattern.preview_plain(1.0), PATTERN_MAX as f32);
        assert_eq!((PATTERN_OFF, PATTERN_MAX), (0, 16), "the documented range moved");
        assert_eq!(pattern.normalized_value_to_string(0.0, true), "Off");
        for position in PATTERN_OFF..=PATTERN_MAX {
            let normalized = pattern.preview_normalized(position as f32);
            assert_eq!(pattern.preview_plain(normalized), position as f32, "position {position}");

            let shown = pattern.normalized_value_to_string(normalized, true);
            let expected =
                if position == PATTERN_OFF { String::from("Off") } else { position.to_string() };
            assert_eq!(shown, expected, "position {position}");
            // And the text a host round-trips lands back where it started.
            assert_eq!(pattern.string_to_normalized_value(&shown), Some(normalized), "{shown}");
        }

        // `mix`: a continuous 0–100 %, fully wet by default.
        let mix = map[1].1;
        assert_eq!(mix.name(), "Mix");
        assert_eq!(mix.unit(), " %");
        assert_eq!(mix.step_count(), None, "a stepped mix would be a switch");
        assert_eq!(mix.default_plain_value(), MIX_DEFAULT);
        assert_eq!(MIX_DEFAULT, 1.0, "the documented default moved");
        assert_eq!(mix.preview_plain(0.0), 0.0);
        assert_eq!(mix.preview_plain(1.0), 1.0);
        assert_eq!(mix.normalized_value_to_string(1.0, true), "100 %");
        assert_eq!(mix.normalized_value_to_string(0.5, true), "50 %");
        assert_eq!(mix.normalized_value_to_string(0.0, true), "0 %");

        // `smooth`: 0.5–20 ms, opening at 3 ms.
        let smooth = map[2].1;
        assert_eq!(smooth.name(), "Smoothing");
        assert_eq!(smooth.unit(), " ms");
        assert_eq!(smooth.step_count(), None);
        assert_eq!(smooth.default_plain_value(), SMOOTH_DEFAULT_MS);
        assert_eq!(smooth.preview_plain(0.0), SMOOTH_MIN_MS);
        assert_eq!(smooth.preview_plain(1.0), SMOOTH_MAX_MS);
        assert_eq!(
            (SMOOTH_MIN_MS, SMOOTH_DEFAULT_MS, SMOOTH_MAX_MS),
            (0.5, 3.0, 20.0),
            "the documented range or default moved"
        );
        assert_eq!(smooth.normalized_value_to_string(0.0, true), "0.5 ms");
        assert_eq!(smooth.normalized_value_to_string(1.0, true), "20.0 ms");
    }
}

/// **The persistence door, headless**: a project's sixteen patterns and both
/// GUI prefs go into a real instance through `deserialize_fields` — what
/// Bitwig calls with the blob it stored — and come back out of
/// `serialize_fields` unchanged.
///
/// No editor is created anywhere in this test, which is what "restores work
/// headless" means: the window is not where the state lives, and a project
/// opened with the device's GUI closed must still load.
#[test]
fn the_persist_field_restores_a_whole_project_with_no_editor_anywhere() {
    let saved = filled_project();

    let plugin = Stuttter::default();
    let params = plugin.params();

    // What a fresh instance holds, so a load that quietly did nothing cannot
    // pass this: the defaults are not the project.
    let before = snapshot_in(&params.serialize_fields());
    assert_ne!(before, saved, "the defaults are the project");

    params.deserialize_fields(&stored(&saved));

    let restored = snapshot_in(&params.serialize_fields());
    assert_eq!(restored.version, SNAPSHOT_VERSION, "the blob is not this build's version");
    for (index, (restored, expected)) in
        restored.data().patterns().iter().zip(saved.data().patterns()).enumerate()
    {
        assert_eq!(restored, expected, "pattern {index} came back changed");
    }
    assert_eq!(restored.data(), saved.data());
    assert_eq!(restored.prefs(), saved.prefs(), "the GUI prefs came back changed");
    assert_eq!(restored, saved);

    // And spelled out on the one pattern of the fixture that holds the case
    // a blob cannot be lazy about: a hand-drawn ramp, which came back with
    // its endpoints and *without* a preset label invented for them.
    let data = restored.data();
    let pattern = data.pattern(3).expect("pattern 4 is missing");
    assert_eq!(pattern.length(), 11, "pattern 4's length was not restored");
    assert!(pattern.length() <= Pattern::MAX_LENGTH);
    assert_eq!(pattern.blocks().len(), 2);
    assert_eq!(pattern.blocks()[1].action(), Action::ramp(0.3, -0.7), "the ramp was renamed");
}

/// **The process path, end to end**: one real instance, driven block by
/// block through `process()`, with the history, the engine and the model all
/// live and visible in what comes back out.
///
/// Two phases over the same instance and the same read head:
///
/// 1. A fresh instance's patterns are empty, and an empty pattern is live —
///    so a whole cycle comes back bit for bit, and the history has recorded
///    every frame of it (the engine **always records**).
/// 2. A project arrives between two blocks through the host's own door, and
///    the stutter it brings plays *out of that recording*: the first slice of
///    each cycle live and the seven after it replaying it, frame for frame,
///    for two more cycles. A plugin that had recorded nothing would answer
///    those reads with silence; one whose model was not the model
///    `process()` reads would keep playing live.
#[test]
fn the_process_path_runs_end_to_end_over_the_restored_patterns() {
    let mut plugin = Stuttter::default();
    assert!(plugin.engine().is_none(), "an unactivated instance holds no audio core");

    activate(&mut plugin, RATE);
    select_pattern(&plugin, 1);

    // Phase one: live, and recorded.
    let (left, right) = run(&mut plugin, 0..CYCLE, playing_at);
    for frame in 0..CYCLE {
        assert_eq!(left[frame], frame as f32, "left, frame {frame} of the live cycle");
        assert_eq!(right[frame], -(frame as f32), "right, frame {frame} of the live cycle");
    }

    let history = plugin.engine().expect("activate built no audio core").history();
    assert_eq!(
        history.capacity(),
        (f64::from(RATE) * MAX_LOOKBACK_SECONDS) as usize,
        "the history buffer is not the host rate's worth of lookback"
    );
    assert_eq!(history.recorded(), CYCLE as u64, "the live cycle was not recorded");

    // Phase two: the project, and the stutter it brings.
    plugin.params().deserialize_fields(&stored(&fixture_project()));

    let (left, right) = run(&mut plugin, CYCLE..3 * CYCLE, playing_at);

    let mut asserted = 0;
    for frame in CYCLE..3 * CYCLE {
        // The slice boundaries are where `d` jumps by one slice and the two
        // heads cross over, which is two positions mixed by design.
        if frame % STEP < FADED {
            continue;
        }

        // The read: back to the first step of the cycle this frame is in.
        let position = frame - frame % CYCLE + frame % STEP;
        let index = frame - CYCLE;
        assert_eq!(left[index], position as f32, "left, frame {frame}");
        assert_eq!(right[index], -(position as f32), "right, frame {frame}");
        asserted += 1;
    }

    // Every frame of every slice outside the fades, so a run that skipped
    // its way to green cannot pass.
    assert_eq!(asserted, 2 * STEPS as usize * (STEP - FADED));

    // And it is the engine's output, not the block that came in: only the
    // first slice of each cycle is live.
    let live = CYCLE + FADED;
    let replayed = CYCLE + STEP + FADED;
    assert_eq!(left[live - CYCLE], live as f32, "the first slice is not live");
    assert_ne!(left[replayed - CYCLE], replayed as f32, "the second slice came back as the input");

    // Every frame of both phases went into the buffer, the loaded project
    // and the pattern change included.
    assert_eq!(plugin.engine().unwrap().history().recorded(), 3 * CYCLE as u64);
}

/// **The scope, through the shell**: the waveform summaries the editor's
/// band draws are written by this same `process()` path, into the instance's
/// own scope, as the block is filled.
///
/// One cycle of the stutter, so the two halves of the view differ: the input
/// is the ramp column by column, and the output is the cycle's first slice
/// replayed over the seven steps after it.
#[test]
fn the_process_path_writes_the_scope_the_waveform_band_draws() {
    /// Frames in one column of this fixture's cycle, which divides exactly.
    const COLUMN: usize = CYCLE / COLUMNS;
    /// Columns in one 1/16 slice of it.
    const SLICE: usize = STEP / COLUMN;

    let mut plugin = Stuttter::default();
    activate(&mut plugin, RATE);
    plugin.params().deserialize_fields(&stored(&fixture_project()));
    select_pattern(&plugin, 1);
    assert_eq!(COLUMN * COLUMNS, CYCLE, "the fixture's cycle stopped dividing");

    run(&mut plugin, 0..CYCLE, playing_at);

    let scope = plugin.engine().expect("activate built no audio core").scope();

    // One cycle and no wrap, so every column was written in the first
    // generation and the whole view is current.
    assert_eq!(scope.generation(), FIRST_GENERATION);
    assert_eq!(scope.column(), Some(COLUMNS - 1));
    assert_eq!(scope.playhead(), (CYCLE - 1) as f32 / CYCLE as f32);

    for column in 0..COLUMNS {
        let last = (column * COLUMN + COLUMN - 1) as f32;

        assert_eq!(scope.written_in(column), FIRST_GENERATION, "column {column}");
        assert_eq!(scope.age(column), 0, "column {column}");
        assert_eq!(scope.input(column), Column { min: -last, max: last }, "input column {column}");
    }

    // And the output half is the engine's, not the input: a column in the
    // middle of the second slice replays the matching column of the first,
    // which is where `d = p − (p mod ℓ)` sent it.
    let replayed = SLICE + SLICE / 2;
    assert_eq!(scope.output(replayed), scope.input(replayed - SLICE), "the replayed slice");
    assert_ne!(scope.output(replayed), scope.input(replayed), "that column is live");
    // The first slice itself is live, so there the two halves agree.
    assert_eq!(scope.output(SLICE / 2), scope.input(SLICE / 2), "the live first slice");
}
