//! **The feature walkthrough's automated half**: everything
//! `docs/feature-walkthrough.md` relies on that can be checked without
//! Bitwig.
//!
//! The document is the manual protocol — fifteen sections of Do and Expect
//! against a real host, a real display and a real drum loop, none of which
//! exist in a test run. This file holds that protocol **as data**
//! (chopdrop's `tests/feature_walkthrough.rs` is the model) and asserts the
//! three kinds of claim a step can rest on:
//!
//! - **Its coverage.** [`PROTOCOL`] is the document's section list with the
//!   development plan's tasks each one covers, and
//!   [`the_protocol_covers_every_verify_manual_task_in_the_plan`] holds it
//!   against `IMPLEMENTATION_PLAN.md` itself — so a `Verify: manual` task
//!   added to the plan, or a section dropped from the document, is a failure
//!   here rather than a hole in a protocol nobody reruns.
//! - **Its quotations.** Every string the document puts in backticks as the
//!   text the window draws is in [`quotations`], against the constant or the
//!   formatter it comes from. A label reworded in `editor/` without the
//!   document following is then a red test, which is the whole reason the
//!   protocol is allowed to quote exact text at all.
//! - **Its expectations.** One test per section for the behaviour the step
//!   asks the user to confirm: the parameter readouts, the remote-controls
//!   page, each action's sound over the ramp trick, the pattern-change
//!   boundary, the window's bands, the gestures through the writer API, the
//!   key routing table and the three keep-on-top transitions.
//!
//! # The two local-only files
//!
//! `IMPLEMENTATION_PLAN.md` (the local development plan) and
//! `docs/feature-walkthrough.md` are not in the public repo, and this test
//! file is — so the two tests that read them **skip** when the file is
//! absent instead of failing a public checkout, and say so on stdout.
//! Everything else here is unconditional. Where both files are present,
//! nothing is skipped: a run there that prints a skip line is itself a
//! finding.
//!
//! # What it is not
//!
//! Not a second copy of the suite. The offset maths is `actions.rs`'s and
//! `engine.rs`'s business, the click acceptance test is `tests/click.rs`,
//! the parameter table is `tests/wiring.rs`'s, and the identity of the
//! remote-controls page's three parameters is `lib.rs`'s. What is only
//! here is the *protocol's* side of each: that the words and figures the
//! document tells someone to look for are the ones this build produces.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use egui::{Event, Key, Modifiers, Rect, pos2, vec2};
use nice_plug::prelude::*;

use stuttter::actions::SpeedPreset;
use stuttter::editor::grid::{self, BlockKind, GridHit, StepGrid, TimeRowDrag};
use stuttter::editor::history::{self as undo_buttons, Step};
use stuttter::editor::keys::{self, Pin, Route};
use stuttter::editor::layout::EditorLayout;
use stuttter::editor::block;
use stuttter::editor::panel;
use stuttter::editor::theme;
use stuttter::editor::top_bar::{self, LengthBox, Look, Selection};
use stuttter::editor::volume::{self, VolumeDraw};
use stuttter::editor::wave::{self, Shade, Subject, WaveView};
use stuttter::editor::{self, DEFAULT_HEIGHT, DEFAULT_WIDTH, STEPS_SHOWN, WINDOW_TITLE};
use stuttter::engine::{BlockCtx, Engine};
use stuttter::history::MAX_LOOKBACK_SECONDS;
use stuttter::model::{StutterData, StutterModel};
use stuttter::pattern::{
    Action, Block, Filter, Lane, NoteValue, PATTERN_COUNT, Pattern, Rate, Source,
};
use stuttter::scope::Scope;
use stuttter::starters::Starter;
use stuttter::state::{Prefs, PrefsCell, Snapshot};
use stuttter::{Stuttter, StuttterParams, PATTERN_MAX, PATTERN_OFF, SMOOTH_MIN_MS};

// ---- the protocol, as data --------------------------------------------

/// One section of `docs/feature-walkthrough.md`.
struct Walk {
    /// The section's id, which is also the prefix of every step in it:
    /// `W6.3` is the third step of `W6`.
    id: &'static str,
    /// The heading's words, after the id and the dash.
    title: &'static str,
    /// The plan tasks whose `Verify: manual` half this section is. Empty for
    /// the two sections that cover a manual check without a task of its own.
    tasks: &'static [&'static str],
}

/// Every section of the document, in its order — the table of contents, and
/// the plan coverage, as one list.
///
/// The document's own §Plan coverage table is this list read the other way
/// round, which is what
/// [`the_protocol_covers_every_verify_manual_task_in_the_plan`] and
/// [`the_document_has_the_sections_and_quotations_this_file_asserts`] hold
/// each other to.
const PROTOCOL: [Walk; 15] = [
    Walk { id: "W1", title: "Install and load the device", tasks: &["6.2"] },
    Walk { id: "W2", title: "The device panel and its remote controls", tasks: &["1.4"] },
    Walk { id: "W3", title: "Each action on a drum loop", tasks: &["4.11"] },
    Walk { id: "W4", title: "Pattern switching and automation", tasks: &["4.11"] },
    Walk { id: "W5", title: "The window: its bands and the family look", tasks: &["5.14"] },
    Walk {
        id: "W6",
        title: "The pattern strip, copy and clear",
        tasks: &["5.2", "5.11"],
    },
    Walk { id: "W7", title: "Rate and length", tasks: &["5.3"] },
    Walk { id: "W8", title: "Mix, Smoothing, Undo and Redo", tasks: &["5.4"] },
    Walk {
        id: "W9",
        title: "The Time row and the block panel",
        tasks: &["5.6", "5.7", "5.8"],
    },
    Walk { id: "W10", title: "The Volume row", tasks: &["5.9"] },
    Walk { id: "W11", title: "The waveform view", tasks: &["5.10"] },
    Walk { id: "W12", title: "The keyboard", tasks: &["1.6", "5.12"] },
    Walk { id: "W13", title: "Keep on top", tasks: &["5.13"] },
    Walk { id: "W14", title: "Save, close and reopen", tasks: &[] },
    Walk {
        id: "W15",
        title: "Running the protocol and recording the result",
        tasks: &["7.3"],
    },
];

/// One string the document quotes as the text the window draws, and where in
/// this build it comes from.
struct Quote {
    /// The section the quotation is read in, so a failure says which step of
    /// the protocol went stale.
    section: &'static str,
    /// The words the document puts in backticks. Short enough to sit on one
    /// line of the document, because the check against the file is a plain
    /// substring search and a wrapped quotation would not be found.
    shown: &'static str,
    /// The text this build produces, which has to contain them.
    drawn: String,
}

/// Every quotation in the document, against the constant, the formatter or
/// the parameter it is read off.
///
/// A `Vec` and not a `const` because half of them are formatted — a hover
/// text that names two patterns, a block label that is two speeds, a
/// parameter's display string — and those are the ones most worth pinning:
/// a constant can be read by eye, a format string cannot.
fn quotations() -> Vec<Quote> {
    let params = StuttterParams::default();
    let pattern = &params.pattern;
    let mix = &params.mix;
    let smooth = &params.smooth;

    // The repeat W9.19 selects: a 1/4 loop over twelve 1/16 steps.
    let mut chain = Pattern::new();
    assert_eq!(chain.add_block(0, 12, Action::repeat(NoteValue::Quarter)), Some(0));
    let chain_description = panel::describe(&chain, 0);
    let (low, high) = panel::source_span(&chain, 0).expect("the repeat reads somewhere");
    let span = chain.span_beats();
    let wash = wave::SourceWash {
        block: 0,
        from: low / span,
        to: high / span,
        colour: BlockKind::Repeat.colour(),
        level: wave::WashLevel::Selected,
        stretch: true,
    };

    vec![
        // W2 — the three parameters as the device panel reads them out.
        Quote::new("W2", "Off", pattern.normalized_value_to_string(0.0, true)),
        Quote::new(
            "W2",
            "100 %",
            mix.normalized_value_to_string(mix.default_normalized_value(), true),
        ),
        Quote::new("W2", "0 %", mix.normalized_value_to_string(0.0, true)),
        Quote::new(
            "W2",
            "3.0 ms",
            smooth.normalized_value_to_string(smooth.default_normalized_value(), true),
        ),
        Quote::new("W2", "0.5 ms", smooth.normalized_value_to_string(0.0, true)),
        Quote::new("W2", "20.0 ms", smooth.normalized_value_to_string(1.0, true)),
        Quote::new("W2", "Main", String::from(MAIN_PAGE)),
        // W5 — the window itself.
        Quote::new("W5", "stuttter", String::from(WINDOW_TITLE)),
        Quote::new("W5", "900 × 540", format!("{DEFAULT_WIDTH} × {DEFAULT_HEIGHT}")),
        // W6 — the pattern strip and its menu.
        Quote::new(
            "W6",
            "Click to edit and play this pattern",
            String::from(top_bar::PATTERN_HOVER),
        ),
        Quote::new("W6", "Follow the playing pattern", String::from(top_bar::FOLLOW_HOVER)),
        Quote::new(
            "W6",
            "A note on C6 plays it for as long as the note is held.",
            top_bar::pattern_hover(0, Some(96), false),
        ),
        Quote::new(
            "W6",
            "A note on C6 starts it, and it keeps playing until the next note.",
            top_bar::pattern_hover(0, Some(96), true),
        ),
        Quote::new("W6", "Latch", String::from(top_bar::LATCH_LABEL)),
        Quote::new("W6", "Notes off", top_bar::notes_label(None)),
        Quote::new("W6", "Notes C6", top_bar::notes_label(Some(96))),
        Quote::new("W6", "C6–D#7", top_bar::notes_choice(Some(96))),
        Quote::new("W6", "Pattern 3", top_bar::pattern_menu_title(2)),
        Quote::new("W6", "Copy to…", String::from(top_bar::COPY_LABEL)),
        Quote::new("W6", "Clear", String::from(top_bar::CLEAR_LABEL)),
        Quote::new("W6", "Fill with…", String::from(top_bar::FILL_LABEL)),
        Quote::new(
            "W6",
            "Repeats that get faster across the whole pattern",
            String::from(Starter::StutterBuild.description()),
        ),
        Quote::new("W6", "Stutter build", String::from(Starter::StutterBuild.label())),
        Quote::new("W6", "Tape stop ending", String::from(Starter::TapeStopEnding.label())),
        Quote::new("W6", "Reverse snares", String::from(Starter::ReverseSnares.label())),
        Quote::new("W6", "Half-time", String::from(Starter::HalfTime.label())),
        Quote::new("W6", "Gate", String::from(Starter::Gate.label())),
        Quote::new("W6", "Filter sweep", String::from(Starter::FilterSweep.label())),
        Quote::new(
            "W6",
            "Replace pattern 9 with a copy of pattern 3.",
            top_bar::copy_item_hover(2, 8),
        ),
        Quote::new(
            "W6",
            "This is the pattern you are copying.",
            String::from(top_bar::COPY_SELF_HOVER),
        ),
        // W7 — rate and length.
        Quote::new(
            "W7",
            "How long one step of the edited pattern lasts.",
            String::from(top_bar::RATE_HOVER),
        ),
        Quote::new("W7", "1/8, 1/8T, 1/16, 1/16T, 1/32", top_bar::RATE_LABELS.join(", ")),
        Quote::new("W7", "1/16", String::from(top_bar::rate_label(Rate::DEFAULT))),
        Quote::new(
            "W7",
            "How many steps the edited pattern has, 1 to 64.",
            String::from(top_bar::LENGTH_HOVER),
        ),
        // W8 — the two rails and the two history buttons.
        Quote::new("W8", "Mix", String::from(top_bar::MIX_LABEL)),
        Quote::new("W8", "Smooth", String::from(top_bar::SMOOTH_LABEL)),
        Quote::new(
            "W8",
            "How much of the chopped signal you hear",
            String::from(top_bar::MIX_HOVER),
        ),
        Quote::new(
            "W8",
            "the length of the short crossfade",
            String::from(top_bar::SMOOTH_HOVER),
        ),
        Quote::new("W8", "Undo", String::from(Step::Undo.label())),
        Quote::new("W8", "Redo", String::from(Step::Redo.label())),
        Quote::new("W8", "nothing to undo", String::from(Step::Undo.empty_hover())),
        Quote::new("W8", "nothing to redo", String::from(Step::Redo.empty_hover())),
        // W9 — the Time row's labels, hover texts and menu.
        Quote::new("W9", "No block selected", String::from(panel::NOTHING_SELECTED)),
        Quote::new("W9", "Click an empty step to add a block.", String::from(panel::EMPTY_HINT)),
        Quote::new(
            "W9",
            "Right-click a block, or press Delete, to remove it.",
            String::from(panel::EMPTY_HINT),
        ),
        Quote::new("W9", "Repeats 1/4 of audio 3 times", chain_description),
        Quote::new("W9", "reads here", wave::wash_caption(&wash)),
        Quote::new("W9", "T", String::from(panel::TRIPLET_LABEL)),
        Quote::new("W9", "Remove", String::from(panel::REMOVE_LABEL)),
        Quote::new("W9", "Rpt 1/16", label_of(Action::repeat_at(Rate::Sixteenth))),
        Quote::new("W9", "Rpt 1/32", label_of(Action::repeat_at(Rate::ThirtySecond))),
        Quote::new("W9", "Rpt 1/4", label_of(Action::repeat(NoteValue::Quarter))),
        Quote::new("W9", "Rev", label_of(Action::from_preset(SpeedPreset::Reverse))),
        Quote::new("W9", "½×", label_of(Action::from_preset(SpeedPreset::HalfSpeed))),
        Quote::new("W9", "2×", label_of(Action::from_preset(SpeedPreset::DoubleSpeed))),
        Quote::new("W9", "Stop", label_of(Action::from_preset(SpeedPreset::TapeStop))),
        Quote::new("W9", "Start", label_of(Action::from_preset(SpeedPreset::TapeStart))),
        Quote::new("W9", "1→0.1", label_of(Action::ramp(1.0, 0.1))),
        Quote::new(
            "W9",
            "Rpt 1/16 1→2",
            label_of(Action { from: 1.0, to: 2.0, ..Action::repeat(NoteValue::Sixteenth) }),
        ),
        Quote::new(
            "W9",
            "…Stop",
            label_of(Action::from_preset(SpeedPreset::TapeStop).with_source(Source::Continue)),
        ),
        Quote::new(
            "W9",
            "Repeat, Reverse, Half speed, Double speed, Tape stop, Tape start",
            block::KIND_LABELS.join(", "),
        ),
        Quote::new("W9", "Loop", String::from(block::LOOP_LABEL)),
        Quote::new("W9", "Off", String::from(block::LOOP_OFF_LABEL)),
        Quote::new(
            "W9",
            "1/1, 1/2, 1/4, 1/8, 1/16, 1/32, 1/64",
            panel::BASE_LABELS.join(", "),
        ),
        Quote::new("W9", "Source", String::from(block::SOURCE_LABEL)),
        Quote::new("W9", "Block start", String::from(block::SOURCE_START_LABEL)),
        Quote::new("W9", "Fixed spot", String::from(block::SOURCE_SPOT_LABEL)),
        Quote::new("W9", "Continue", String::from(block::SOURCE_CONTINUE_LABEL)),
        Quote::new("W9", "Speed", String::from(block::RAMP_FROM_LABEL)),
        Quote::new("W9", "Duplicate", String::from(block::DUPLICATE_LABEL)),
        // W10 — the Volume row.
        Quote::new(
            "W10",
            "Step 1: 100 %.",
            volume::hover_text(Lane::Volume, Filter::Off, 0, Pattern::DEFAULT_VOLUME),
        ),
        Quote::new(
            "W10",
            "Drag across the row to draw levels",
            String::from(volume::DRAW_HINT),
        ),
        // W10b — the filter.
        Quote::new("W10", "Filter: Off", volume::filter_text(Filter::Off)),
        Quote::new("W10", "Filter: Low-pass", volume::filter_text(Filter::LowPass)),
        Quote::new("W10", "This pattern's filter", String::from(volume::FILTER_HOVER)),
        Quote::new("W10", "Cutoff", String::from(volume::lane_label(Lane::Cutoff))),
        Quote::new("W10", "Res", String::from(volume::lane_label(Lane::Resonance))),
        Quote::new(
            "W10",
            "Step 1: cutoff 20.0 kHz.",
            volume::hover_text(Lane::Cutoff, Filter::Off, 0, 1.0),
        ),
        Quote::new(
            "W10",
            "The filter is off for this pattern: drawing here turns on Low-pass.",
            String::from(volume::FILTER_OFF_HINT),
        ),
        Quote::new(
            "W10",
            "Double-click a bar to put it back to 100 %.",
            String::from(volume::DRAW_HINT),
        ),
        // W11 — the waveform view.
        Quote::new(
            "W11",
            "What came in, drawn dim, and what went out, drawn bright",
            String::from(wave::LIVE_HINT),
        ),
        Quote::new(
            "W11",
            "What went out is in the colour of the block that played it",
            String::from(wave::LIVE_HINT),
        ),
        Quote::new("W11", "Pattern 1 is playing", wave::hover_text(Some(0), 2)),
        Quote::new(
            "W11",
            "Click pattern 1 to watch what is playing",
            wave::hover_text(Some(0), 2),
        ),
        Quote::new(
            "W11",
            "Pattern is Off, so nothing is being drawn",
            wave::hover_text(None, 2),
        ),
        // W13 — the keep-on-top switch.
        Quote::new("W13", "On top", String::from(top_bar::KEEP_LABEL)),
        Quote::new(
            "W13",
            "Keep this window above Bitwig's.",
            String::from(top_bar::KEEP_HOVER),
        ),
        Quote::new(
            "W13",
            "close the window and open it once more",
            String::from(top_bar::KEEP_HOVER),
        ),
    ]
}

impl Quote {
    /// One quotation, read in `section`.
    fn new(section: &'static str, shown: &'static str, drawn: String) -> Quote {
        Quote { section, shown, drawn }
    }
}

/// The name of the remote-controls page the document tells the user to look
/// for. Not a constant of the crate — the page's name is written inline in
/// `ClapPlugin::remote_controls` — so
/// [`w2_the_remote_controls_page_is_the_one_the_protocol_opens`] reads it
/// back off the declaration and this is only what the document says.
const MAIN_PAGE: &str = "Main";

/// A block's label as the Time row writes it.
fn label_of(action: Action) -> String {
    grid::block_label(action).to_string()
}

/// A row rectangle the width of the window's grid and one cell tall — the
/// band the Time row and the Volume row are drawn into
/// (`editor::DEFAULT_WIDTH` is this plus the panel's padding).
fn row_rect() -> Rect {
    Rect::from_min_size(pos2(0.0, 0.0), vec2(editor::GRID_WIDTH, theme::CELL_HEIGHT))
}

// ---- the two local-only files -----------------------------------------

/// A path inside the repository, which is the manifest directory's parent.
fn repo(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join(relative)
}

/// The contents of a repository file, or `None` where it is one of the two
/// the public repo does not carry (this module's header).
fn local_file(relative: &str) -> Option<String> {
    match fs::read_to_string(repo(relative)) {
        Ok(text) => Some(text),
        Err(_) => {
            println!("skipped: {relative} is not in this checkout");
            None
        }
    }
}

/// **The coverage** (the protocol covers every `Verify: manual` item in the
/// plan): the plan's own list of them, against [`PROTOCOL`].
///
/// Both directions. A task the plan marks `Verify: manual` with no section
/// is an item nobody will ever check; a section claiming a task the plan
/// does not mark is a protocol that has drifted off the plan it is the other
/// half of. The plan is read rather than copied for exactly that reason —
/// a copied list agrees with itself.
#[test]
fn the_protocol_covers_every_verify_manual_task_in_the_plan() {
    let Some(plan) = local_file("IMPLEMENTATION_PLAN.md") else {
        return;
    };

    // One task per `- [ ] <number> …` bullet, over the plan with all of its
    // whitespace folded to single spaces: a task's `Verify: manual` sits at
    // the end of its bullet, several lines down, and the plan wraps at 79
    // columns — so the two words are as often as not on either side of a
    // line break. Folded, each bullet runs to the next `- [`, which is where
    // the next task's own clauses begin.
    let folded = plan.split_whitespace().collect::<Vec<&str>>().join(" ");
    let manual: BTreeSet<&str> = folded
        .split("- [")
        .skip(1)
        .filter(|bullet| bullet.contains("Verify: manual"))
        // `x] 7.2 Dead-surface audit: …` — the id is the second word.
        .filter_map(|bullet| bullet.split_whitespace().nth(1))
        .filter(|id| {
            id.split_once('.').is_some_and(|(phase, step)| {
                !phase.is_empty()
                    && phase.chars().all(|c| c.is_ascii_digit())
                    && !step.is_empty()
                    && step.chars().all(|c| c.is_ascii_digit())
            })
        })
        .collect();

    assert!(manual.len() > 10, "the plan's `Verify: manual` tasks did not parse: {manual:?}");

    let covered: BTreeSet<&str> =
        PROTOCOL.iter().flat_map(|walk| walk.tasks.iter().copied()).collect();
    assert_eq!(
        manual, covered,
        "the plan's `Verify: manual` tasks and the protocol's sections have parted company"
    );

    // And every section is reachable from a task or deliberately not one of
    // them: the two registry-only sections are named here so that a third
    // cannot appear by accident.
    let taskless: Vec<&str> =
        PROTOCOL.iter().filter(|walk| walk.tasks.is_empty()).map(|walk| walk.id).collect();
    assert_eq!(taskless, vec!["W14"], "a section covers nothing in the plan");
}

/// **The document itself**: every section of [`PROTOCOL`] is a heading in
/// it, every task is a row of its coverage table, every quotation of
/// [`quotations`] is in its text, and it has no section this file does not
/// know about.
///
/// The last of those is what keeps the file from growing a step whose
/// expectations nothing holds: a sixteenth section added to the document
/// fails here until it is added to [`PROTOCOL`] with the plan task it
/// covers.
#[test]
fn the_document_has_the_sections_and_quotations_this_file_asserts() {
    let Some(doc) = local_file("docs/feature-walkthrough.md") else {
        return;
    };

    for walk in &PROTOCOL {
        let heading = format!("## {} — {}", walk.id, walk.title);
        assert!(doc.contains(&heading), "the document has no section `{heading}`");

        for task in walk.tasks {
            let row = format!("| {task} |");
            assert!(
                doc.lines().any(|line| line.starts_with(&row) && line.contains(walk.id)),
                "the coverage table does not put task {task} in {}",
                walk.id
            );
        }
    }

    // Every step is `- **W6.3**`, numbered from one without a gap, and the
    // results sheet at the end lists exactly those — the one page a person
    // fills in while running this, so a step the sheet has no box for is a
    // step that quietly stops being reported.
    let mut steps: Vec<(&str, usize)> = Vec::new();
    for line in doc.lines() {
        if let Some(rest) = line.trim_start().strip_prefix("- **W")
            && let Some(label) = rest.split("**").next()
            && let Some((section, step)) = label.split_once('.')
            && let Ok(step) = step.parse::<usize>()
        {
            steps.push((section, step));
        }
    }

    for walk in &PROTOCOL {
        let numbers: Vec<usize> =
            steps.iter().filter(|(id, _)| *id == &walk.id[1..]).map(|(_, step)| *step).collect();
        assert!(!numbers.is_empty(), "{} has no steps", walk.id);
        assert_eq!(
            numbers,
            (1..=numbers.len()).collect::<Vec<usize>>(),
            "{}'s steps are misnumbered",
            walk.id
        );

        let row = doc
            .lines()
            .find(|line| line.starts_with(walk.id) && line[walk.id.len()..].starts_with(' '))
            .unwrap_or_else(|| panic!("the results sheet has no row for {}", walk.id));
        let listed: Vec<&str> = row.split_whitespace().skip(1).collect();
        assert_eq!(
            listed,
            numbers.iter().map(|step| format!(".{step}")).collect::<Vec<String>>(),
            "the results sheet's {} row is not its steps",
            walk.id
        );
    }

    let known: BTreeSet<&str> = PROTOCOL.iter().map(|walk| walk.id).collect();
    for line in doc.lines() {
        if let Some(heading) = line.strip_prefix("## ")
            && let Some(id) = heading.split(' ').next()
            && let Some(number) = id.strip_prefix('W')
            && !number.is_empty()
            && number.chars().all(|c| c.is_ascii_digit())
        {
            assert!(known.contains(id), "the document has a section `{id}` this file does not");
        }
    }

    for quote in quotations() {
        assert!(
            doc.contains(quote.shown),
            "{}: the document does not quote `{}`",
            quote.section,
            quote.shown
        );
    }
}

/// **The quotations** are text this build really draws: every string the
/// document quotes, against the label, the hover text or the readout it was
/// taken from.
///
/// Unconditional, and the reason the protocol may quote exact words at all:
/// without it, a rewording in `editor/` would leave a step of the protocol
/// asking for text that no longer exists, and the only thing that would
/// notice is the person running it.
#[test]
fn every_quotation_in_the_protocol_is_text_this_build_really_draws() {
    for quote in quotations() {
        assert!(
            quote.drawn.contains(quote.shown),
            "{}: the protocol quotes `{}`, but this build draws `{}`",
            quote.section,
            quote.shown,
            quote.drawn
        );
    }
}

// ---- W1: the device the protocol installs -----------------------------

/// **W1.2 and W1.3**: the device Bitwig finds after the install — an audio
/// effect under the audio FX, stereo in and out, with a note input (the
/// notes that play the patterns) and no note output.
#[test]
fn w1_the_device_bitwig_finds_is_a_stereo_audio_effect_with_a_note_input() {
    assert_eq!(Stuttter::NAME, WINDOW_TITLE, "the browser name and the window title differ");
    assert!(
        Stuttter::CLAP_FEATURES.contains(&ClapFeature::AudioEffect),
        "the device would not be listed with the audio FX"
    );
    assert!(Stuttter::CLAP_FEATURES.contains(&ClapFeature::Stereo));
    assert_eq!(Stuttter::MIDI_INPUT, MidiConfig::Basic, "W1.3 expects a note input");
    assert_eq!(Stuttter::MIDI_OUTPUT, MidiConfig::None);

    let layout = Stuttter::AUDIO_IO_LAYOUTS[0];
    assert_eq!(layout.main_input_channels, NonZeroU32::new(2));
    assert_eq!(layout.main_output_channels, NonZeroU32::new(2));
}

/// **W1.1 and W1.2**: the two commands the step runs, and the two paths it
/// copies between, are the ones `docs/install.md` gives — the public half of
/// the protocol, so this one is not allowed to skip.
#[test]
fn w1_the_install_steps_name_the_files_this_repo_really_has() {
    let install = fs::read_to_string(repo("docs/install.md")).expect("docs/install.md");
    let script = fs::read_to_string(repo("scripts/build-in-docker.sh"))
        .expect("W1.1 runs scripts/build-in-docker.sh");

    assert!(repo("docker/Dockerfile").is_file(), "W1.1 builds the image from docker/Dockerfile");
    assert!(
        script.lines().any(|line| {
            line.contains("docker build") && line.split_whitespace().last() == Some("docker")
        }),
        "the script builds its image from somewhere other than the `docker` directory"
    );
    assert!(
        script.contains("bundle stuttter --release"),
        "the script bundles something other than the release plugin"
    );

    for named in ["target/bundled/stuttter.clap", "~/.clap", "./scripts/build-in-docker.sh"] {
        assert!(install.contains(named), "docs/install.md no longer names `{named}`");
    }
}

// ---- W2: the device panel and the remote-controls page ----------------

/// **W2.2 to W2.5**: what the device panel reads out, and the one piece of
/// text a host round-trips — typing `off` back into Pattern selects the
/// position whose only display text is `Off`.
///
/// The readouts themselves are in [`quotations`], which is what holds them
/// against the document; this is the behaviour behind them.
#[test]
fn w2_the_parameters_read_out_and_read_back_the_way_the_protocol_types_them() {
    let params = StuttterParams::default();
    let pattern = &params.pattern;

    // W2.3: seventeen positions, Off at the bottom and 16 at the top, each
    // landing on a whole pattern rather than between two.
    assert_eq!(pattern.step_count(), Some(PATTERN_MAX as usize), "a knob, not 17 positions");
    for position in PATTERN_OFF..=PATTERN_MAX {
        let normalized = pattern.preview_normalized(position);
        assert_eq!(pattern.preview_plain(normalized), position, "position {position}");
    }

    // W2.4: `off`, `Off` and `OFF` all select it, and nothing else does.
    let off = pattern.preview_normalized(PATTERN_OFF);
    for typed in ["off", "Off", "OFF"] {
        assert_eq!(pattern.string_to_normalized_value(typed), Some(off), "typed `{typed}`");
    }
    assert_eq!(pattern.string_to_normalized_value("nonsense"), None);

    // W2.5: both ends of both rails are exactly the documented figures.
    assert_eq!(params.mix.preview_plain(0.0), 0.0);
    assert_eq!(params.mix.preview_plain(1.0), 1.0);
    assert_eq!(params.smooth.preview_plain(0.0), SMOOTH_MIN_MS);
}

/// A [`RemoteControlsContext`] that records the declaration instead of
/// building CLAP pages out of it: nice-plug's own lives inside the wrapper
/// and needs a host, and the protocol's step is about what the page *says*.
///
/// Names only. That the three slots point at this instance's own parameters
/// rather than at copies is `lib.rs`'s
/// `the_remote_controls_are_one_main_page_of_the_three_parameters`; what is
/// asked here is what W2.6 can see.
#[derive(Default)]
struct RecordedPages {
    /// Each page with the name of the section it was declared in.
    pages: Vec<(String, RecordedPage)>,
}

/// The section being filled. It owns its pages until `add_section` moves
/// them out, which is how nice-plug's own section works too — the associated
/// type has no lifetime to borrow the recorder with.
#[derive(Default)]
struct RecordedSection {
    pages: Vec<RecordedPage>,
}

/// The page being filled: its name, and one slot per knob — the parameter's
/// name, or `None` for a spacer.
#[derive(Default)]
struct RecordedPage {
    name: String,
    slots: Vec<Option<String>>,
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
        let mut page = RecordedPage { name: name.into(), slots: Vec::new() };
        f(&mut page);
        self.pages.push(page);
    }
}

impl RemoteControlsPage for RecordedPage {
    fn add_param(&mut self, param: &impl Param) {
        self.slots.push(Some(param.name().to_string()));
    }

    fn add_spacer(&mut self) {
        self.slots.push(None);
    }
}

/// **W2.1 and W2.6**: one page, `Main`, in a section called `stuttter`,
/// holding Pattern, Mix and Smoothing in that order and leaving the page's
/// other five knobs empty.
#[test]
fn w2_the_remote_controls_page_is_the_one_the_protocol_opens() {
    let mut recorded = RecordedPages::default();
    Stuttter::default().remote_controls(&mut recorded);

    assert_eq!(recorded.pages.len(), 1, "W2.6 expects one page");
    let (section, page) = &recorded.pages[0];
    assert_eq!(section, Stuttter::NAME);
    assert_eq!(page.name, MAIN_PAGE, "the document quotes the page's name");
    assert_eq!(
        page.slots,
        &[
            Some(String::from("Pattern")),
            Some(String::from("Mix")),
            Some(String::from("Smoothing")),
        ]
    );
    assert!(page.slots.len() <= 8, "a CLAP page holds eight knobs; the rest stay empty");
}

// ---- the engine fixture W3 and W4 listen to --------------------------

/// 32 768 Hz — the fixture rate the rest of the suite uses, and for its
/// reason: at [`TEMPO`] one beat is exactly [`BEAT`] frames, so every read
/// position lands on a whole frame and the ramp reads back exactly.
const RATE: f32 = 32_768.0;
/// 120 BPM, which is the tempo the document's test project is built at.
const TEMPO: f64 = 120.0;
/// Frames in one beat at [`RATE`] and [`TEMPO`].
const BEAT: usize = 16_384;
/// Frames in one 1/16 step.
const STEP: usize = BEAT / 4;
/// Steps in the fixture pattern: eight, so a cycle is two beats.
const STEPS: u16 = 8;
/// Frames in one cycle of it.
const CYCLE: usize = STEPS as usize * STEP;
/// The block length the runs hand the engine; 512 divides neither a step nor
/// a beat, so every assertion is also one about reading across a block seam.
const BLOCK: usize = 512;
/// Frames to leave unasserted either side of a read-position jump:
/// [`SMOOTH_MIN_MS`] is 16 frames at [`RATE`], and inside a fade the output
/// is two positions mixed by design.
const FADED: usize = 64;
/// Frames to leave unasserted at the start of a block whose offset opens at
/// **less than two frames** — which, over the ramp, is a window the output
/// cannot be read as a position at all.
///
/// The interpolator is four-point Hermite, so a read needs the sample one
/// *past* the one it lands between; within two frames of the newest recorded
/// frame that sample has not been recorded yet and reads as silence, and the
/// cubic through it overshoots by a fraction of the sample's own magnitude.
/// Over real audio that is a brief, small distortion at `d < 2`; over the
/// ramp trick, where a sample is its own index, it is 5 % of eighty thousand
/// and swamps the figure being measured. It is a known finding, not fixed
/// here.
///
/// Only the tape stop opens there — `d = p²/(2B)` takes 181 frames to reach
/// two at this fixture's block length, where every other action's offset is
/// either a whole number of frames (exact, no interpolation at all) or past
/// two frames within four.
const LEADING_EDGE: usize = 256;

/// Where the speed blocks of W3 sit: steps 5 to 8 of the fixture, so there
/// is a whole beat of recorded past in front of them.
const AT: u16 = 4;
/// How many steps those blocks cover: four, one beat.
const SPAN: u16 = 4;
/// The frame of a cycle such a block starts on.
const BLOCK_AT: usize = AT as usize * STEP;
/// Its length in frames.
const BLOCK_FRAMES: usize = SPAN as usize * STEP;

/// A pattern of [`STEPS`] 1/16 steps with `action` over `start..start + len`.
fn pattern_with(action: Action, start: u16, len: u16) -> Pattern {
    let mut pattern = Pattern::new();
    pattern.set_rate(Rate::Sixteenth);
    pattern.set_length(STEPS);
    assert_eq!(pattern.add_block(start, len, action), Some(0));

    pattern
}

/// Pattern 1 holding that block, the other fifteen empty.
fn data_with(action: Action, start: u16, len: u16) -> StutterData {
    StutterData::from_patterns([pattern_with(action, start, len)])
}

/// Runs the first `frames` frames of the ramp input `x[n] = n` through a
/// fresh engine with `pattern` selected and `mix` on the rail, in [`BLOCK`]
/// sized blocks, and returns the left channel.
///
/// The ramp trick: the output of a frame *is* the position it was read from,
/// so each action's sound is checked as the position it read rather than by
/// listening to it.
fn ramp_through(data: &StutterData, pattern: Option<u8>, mix: f32, frames: usize) -> Vec<f32> {
    let mut engine = Engine::new(RATE, Arc::new(Scope::new()));
    let mut out = Vec::with_capacity(frames);

    let mut start = 0;
    while start < frames {
        let len = BLOCK.min(frames - start);
        let mut left: Vec<f32> = (start..start + len).map(|n| n as f32).collect();
        let mut right: Vec<f32> = (start..start + len).map(|n| -(n as f32)).collect();
        let ctx = BlockCtx::new(start as f64 / BEAT as f64, TEMPO, RATE, len)
            .with_playing(true)
            .with_pattern(pattern)
            .with_smooth_ms(SMOOTH_MIN_MS)
            .with_mix(mix);

        engine.run_block(&ctx, data, &mut left, &mut right);

        for (frame, (l, r)) in left.iter().zip(right.iter()).enumerate() {
            assert_eq!(*l, -*r, "the two channels parted company at frame {}", start + frame);
        }
        out.extend_from_slice(&left);
        start += len;
    }

    out
}

/// The read speed the output was moving at, frame by frame: the difference
/// between adjacent read positions, which is what the ear hears as pitch.
fn speeds(out: &[f32], range: std::ops::Range<usize>) -> Vec<f64> {
    range.map(|n| f64::from(out[n + 1]) - f64::from(out[n])).collect()
}

// ---- W3: each action on a drum loop -----------------------------------

/// **W3.1**: the classic stutter — "the first sixteenth plays live and the
/// next three are the same sixteenth again", and the pattern repeats every
/// cycle.
#[test]
fn w3_a_repeat_plays_its_first_slice_live_and_then_loops_it() {
    let data = data_with(Action::repeat_at(Rate::Sixteenth), 0, STEPS);
    let out = ramp_through(&data, Some(1), 1.0, 3 * CYCLE);
    let base = 2 * CYCLE;

    for frame in FADED..STEP {
        let n = base + frame;
        assert_eq!(out[n], n as f32, "the first slice of the cycle is not live at frame {n}");
    }
    for slice in 1..STEPS as usize {
        for frame in FADED..STEP {
            let n = base + slice * STEP + frame;
            assert_eq!(
                out[n],
                out[base + frame],
                "slice {slice} is not the first slice again at frame {n}"
            );
        }
    }
}

/// **W3.2**: the other material — a Repeat looping the slice *before* the
/// block is already offset at the block's very first sample, where the
/// block's own first slice would have been live.
#[test]
fn w3_a_repeat_from_before_the_block_is_offset_from_its_first_sample() {
    // A fixed spot on the step before the block's.
    let spot = Source::Spot { step: AT - 1, passes: 0 };
    let action = Action::repeat(NoteValue::Sixteenth).with_source(spot);
    let data = data_with(action, AT, SPAN);
    let out = ramp_through(&data, Some(1), 1.0, 3 * CYCLE);
    let start = 2 * CYCLE + BLOCK_AT;

    for frame in FADED..STEP {
        let n = start + frame;
        assert_eq!(
            out[n],
            (n - STEP) as f32,
            "the slice before the block is not what is looping at frame {n}"
        );
    }
}

/// **W3.3**: `Reverse` — "the four steps *before* the block, reversed". The
/// read position walks backwards from the block's start, one frame per
/// frame.
#[test]
fn w3_reverse_runs_backwards_through_the_audio_before_the_block() {
    let data = data_with(Action::from_preset(SpeedPreset::Reverse), AT, SPAN);
    let out = ramp_through(&data, Some(1), 1.0, 3 * CYCLE);
    let start = 2 * CYCLE + BLOCK_AT;

    for frame in FADED..BLOCK_FRAMES - FADED {
        assert_eq!(
            out[start + frame],
            (start - frame) as f32,
            "the reverse is not at pitch at frame {frame} of the block"
        );
    }
}

/// **W3.4 and W3.5**: `Half speed` ends half the block's length behind the
/// song, and `Double speed` arrives back on the live input as the block ends
/// — which is what makes the step after it join seamlessly.
#[test]
fn w3_half_speed_ends_behind_and_double_speed_ends_on_live() {
    let half = ramp_through(
        &data_with(Action::from_preset(SpeedPreset::HalfSpeed), AT, SPAN),
        Some(1),
        1.0,
        3 * CYCLE,
    );
    let double = ramp_through(
        &data_with(Action::from_preset(SpeedPreset::DoubleSpeed), AT, SPAN),
        Some(1),
        1.0,
        3 * CYCLE,
    );
    let start = 2 * CYCLE + BLOCK_AT;
    let end = start + BLOCK_FRAMES;

    let asserted = FADED..BLOCK_FRAMES - FADED;
    for speed in speeds(&half, start + asserted.start..start + asserted.end) {
        assert!((speed - 0.5).abs() < 0.01, "half speed read at {speed}");
    }
    for speed in speeds(&double, start + asserted.start..start + asserted.end) {
        assert!((speed - 2.0).abs() < 0.01, "double speed read at {speed}");
    }

    // Where each one has got to by the last frame of the block: half speed
    // half the block behind live, double speed on it.
    let last = end - 1;
    assert!(
        (f64::from(half[last]) - (last - BLOCK_FRAMES / 2) as f64).abs() < 2.0,
        "half speed ended at {} rather than half a block behind {last}",
        half[last]
    );
    assert!(
        (f64::from(double[last]) - last as f64).abs() < 2.0,
        "double speed ended at {} rather than on live {last}",
        double[last]
    );
}

/// **W3.6 and W3.7**: the tape shapes — a `Tape stop` slows to a standstill
/// and holds there rather than falling silent, and a `Tape start` is its
/// mirror, arriving at live speed as the block ends.
#[test]
fn w3_tape_stop_winds_down_to_frozen_and_tape_start_winds_up_to_speed() {
    let stop = ramp_through(
        &data_with(Action::from_preset(SpeedPreset::TapeStop), AT, SPAN),
        Some(1),
        1.0,
        3 * CYCLE,
    );
    let start_up = ramp_through(
        &data_with(Action::from_preset(SpeedPreset::TapeStart), AT, SPAN),
        Some(1),
        1.0,
        3 * CYCLE,
    );
    let at = 2 * CYCLE + BLOCK_AT;
    let last = at + BLOCK_FRAMES - 1;

    // The first and the last tenth of each block, as read speeds — the
    // opening measured from [`LEADING_EDGE`], which is where a tape stop's
    // output becomes a position again.
    let tenth = BLOCK_FRAMES / 10;
    let opening = |out: &[f32]| speeds(out, at + LEADING_EDGE..at + tenth);
    let closing = |out: &[f32]| speeds(out, last - tenth..last - 1);

    for speed in opening(&stop) {
        assert!((speed - 1.0).abs() < 0.2, "a tape stop opened at {speed} rather than at speed");
    }
    for speed in closing(&stop) {
        assert!(speed.abs() < 0.2, "a tape stop closed at {speed} rather than frozen");
    }
    for speed in opening(&start_up) {
        assert!(speed.abs() < 0.2, "a tape start opened at {speed} rather than frozen");
    }
    for speed in closing(&start_up) {
        assert!((speed - 1.0).abs() < 0.2, "a tape start closed at {speed} rather than at speed");
    }

    // Frozen is a held sample, not silence: the position stands still and
    // the output is whatever was under it.
    assert!(stop[last] > 0.0, "a frozen tape stop went silent");
}

/// **W1.4, W3.11 and W4.3**: the three bypasses the protocol listens for —
/// an empty pattern, `Pattern` at `Off` and `Mix` at `0 %` — are the input
/// back, sample for sample, over the ramp.
#[test]
fn w3_the_bypasses_the_protocol_checks_are_the_input_sample_for_sample() {
    let frames = 2 * CYCLE;
    let input: Vec<f32> = (0..frames).map(|n| n as f32).collect();
    let empty = StutterData::new();
    let busy = data_with(Action::repeat_at(Rate::Sixteenth), 0, STEPS);

    // W1.4: a device fresh out of the browser, every pattern empty.
    assert_eq!(ramp_through(&empty, Some(1), 1.0, frames), input, "an empty pattern is not dry");
    // W4.3: Off.
    assert_eq!(ramp_through(&busy, None, 1.0, frames), input, "Pattern Off is not dry");
    // W3.11: the rail at the bottom, with a pattern that would otherwise
    // chop every slice of the cycle.
    assert_eq!(ramp_through(&busy, Some(1), 0.0, frames), input, "Mix at 0 % is not dry");
}

/// **W3.12**: how far back the protocol is told it can reach, and the figure
/// the step sets the tempo from.
#[test]
fn w3_the_lookback_is_the_sixty_four_seconds_the_protocol_relies_on() {
    assert_eq!(MAX_LOOKBACK_SECONDS, 64.0);

    // The step's own arithmetic: a 64-step 1/8 pattern spans 32 beats, and
    // reversing all of it reads 64 beats back — which at 30 BPM is 128 s,
    // past the buffer, so its far end must read silence rather than glitch.
    let mut longest = Pattern::new();
    longest.set_rate(Rate::Eighth);
    longest.set_length(Pattern::MAX_LENGTH);
    assert_eq!(longest.span_beats(), 32.0);
    let reach_seconds = 2.0 * longest.span_beats() * 60.0 / 30.0;
    assert!(
        reach_seconds > MAX_LOOKBACK_SECONDS,
        "W3.12 expects the longest reverse at 30 BPM to run out of recording"
    );
}

// ---- W4: pattern switching -------------------------------------------

/// The two patterns W4 switches between: pattern 1 a Repeat over the whole
/// cycle, pattern 2 a Reverse over the whole cycle.
fn switching_data() -> StutterData {
    StutterData::from_patterns([
        pattern_with(Action::repeat_at(Rate::Sixteenth), 0, STEPS),
        pattern_with(Action::from_preset(SpeedPreset::Reverse), 0, STEPS),
    ])
}

/// **W4.1 to W4.3**: a change of `Pattern` waits for the end of the step
/// that is playing — including a change to `Off` — and a change *away* from
/// `Off` does not wait at all.
///
/// Read off [`Engine::selection`], which is the pattern the audio is being
/// made from rather than the one the parameter says: the whole of the
/// protocol's "it lands on the next step boundary" is the gap between those
/// two.
#[test]
fn w4_a_pattern_change_lands_on_the_step_boundary_and_off_joins_at_once() {
    let data = switching_data();
    let mut engine = Engine::new(RATE, Arc::new(Scope::new()));

    let mut run = |at: usize, frames: usize, pattern: Option<u8>| {
        let mut left = vec![0.0; frames];
        let mut right = vec![0.0; frames];
        let ctx = BlockCtx::new(at as f64 / BEAT as f64, TEMPO, RATE, frames)
            .with_playing(true)
            .with_pattern(pattern)
            .with_smooth_ms(SMOOTH_MIN_MS);

        engine.run_block(&ctx, &data, &mut left, &mut right);
        engine.selection()
    };

    // Pattern 1 from the top of a step.
    assert_eq!(run(0, STEP / 2, Some(1)), Some(0), "pattern 1 did not start");
    // W4.1: the parameter moves to 2 half way through a step — the block
    // ends before the boundary, so the walk is still playing pattern 1.
    assert_eq!(run(STEP / 2, STEP / 4, Some(2)), Some(0), "the change did not wait for the step");
    // The next block crosses the boundary, and there it lands.
    assert_eq!(run(STEP / 2 + STEP / 4, STEP, Some(2)), Some(1), "the change missed the boundary");
    // W4.3: to Off, which waits the same way.
    let at = STEP / 2 + STEP / 4 + STEP;
    assert_eq!(run(at, STEP / 4, None), Some(1), "Off did not wait for the step");
    assert_eq!(run(at + STEP / 4, STEP, None), None, "Off missed the boundary");
    // W4.2: away from Off, which does not wait — the selection is the
    // parameter's inside the same mid-step block.
    let at = at + STEP / 4 + STEP;
    assert_eq!(run(at, STEP / 4, Some(1)), Some(0), "leaving Off waited for a boundary");
}

/// **W4.5**: the transport stopping plays live and the history keeps
/// recording, so the first block after play already has the audio from just
/// before it.
#[test]
fn w4_a_stopped_transport_plays_live_and_goes_on_recording() {
    let data = switching_data();
    let mut engine = Engine::new(RATE, Arc::new(Scope::new()));

    // A stopped block of the ramp: live out, and recorded.
    let frames = STEP;
    let mut left: Vec<f32> = (0..frames).map(|n| n as f32).collect();
    let mut right: Vec<f32> = (0..frames).map(|n| -(n as f32)).collect();
    let stopped = BlockCtx::stopped(RATE, frames).with_pattern(Some(1));
    engine.run_block(&stopped, &data, &mut left, &mut right);

    assert_eq!(left, (0..frames).map(|n| n as f32).collect::<Vec<f32>>(), "stopped is not live");
    assert_eq!(engine.selection(), None, "a stopped transport plays a pattern");
    assert_eq!(engine.history().recorded(), frames as u64, "a stopped block was not recorded");
}

// ---- W5: the window --------------------------------------------------

/// **W5.1 to W5.3**: the window the protocol opens — the size it quotes, a
/// minimum it can be resized down to, the bands in the order it reads them,
/// and the sixteen cells it counts across the Time row.
#[test]
fn w5_the_window_opens_at_its_size_with_the_bands_in_place() {
    assert_eq!((DEFAULT_WIDTH, DEFAULT_HEIGHT), (900.0, 540.0), "the quoted window size moved");
    const { assert!(editor::MIN_WIDTH < DEFAULT_WIDTH && editor::MIN_HEIGHT < DEFAULT_HEIGHT) };

    let sizes = [(DEFAULT_WIDTH, DEFAULT_HEIGHT), (editor::MIN_WIDTH, editor::MIN_HEIGHT)];
    for (width, height) in sizes {
        let layout =
            EditorLayout::new(Rect::from_min_size(pos2(0.0, 0.0), vec2(width, height)));
        let column = [
            ("Time row", layout.time_row()),
            ("Volume row", layout.volume_row()),
            ("waveform", layout.waveform()),
        ];
        for (name, band) in column.into_iter().chain([("panel", layout.panel())]) {
            assert!(band.is_positive(), "the {name} band has no room at {width} × {height}");
            assert!(layout.full().contains_rect(band), "the {name} band is outside the window");
            assert!(band.min.y >= layout.separator().max.y, "the {name} band is under the bar");
        }
        for pair in column.windows(2) {
            assert!(pair[0].1.max.y <= pair[1].1.min.y, "{} overlaps {}", pair[0].0, pair[1].0);
        }
        // W5.3: one column, so a step, its level and its sound line up, and
        // the panel beside it.
        for (_, band) in column {
            assert_eq!(band.min.x, layout.time_row().min.x, "the bands do not line up");
            assert_eq!(band.width(), layout.time_row().width(), "the bands are not one width");
        }
        assert!(layout.panel().min.x > layout.time_row().max.x, "the panel is beside them");
        let grid = StepGrid::new(layout.time_row(), STEPS_SHOWN as u16);
        assert!(grid.cell_width() >= theme::MIN_CELL_WIDTH, "sixteen cells at {width}");
    }

    // W5.2: two rows of controls in the bar.
    assert_eq!(theme::BAR_ROWS, 2.0);
}

/// **W7.5**: the longer patterns the protocol drags the length box through —
/// 32 steps at half the width, 64 at a quarter, every one of them still on
/// screen and still clickable, and nothing anywhere to scroll.
#[test]
fn w5_a_longer_pattern_is_fitted_into_the_same_width_rather_than_scrolled() {
    let row = row_rect();

    let full = StepGrid::new(row, STEPS_SHOWN as u16).pitch();
    for steps in [16u16, 32, 64] {
        let grid = StepGrid::new(row, steps);
        assert_eq!(grid.row().width(), row.width(), "the row grew for {steps} steps");
        // The *pitch* is what halves and quarters exactly; a cell is that
        // less the constant gap, which is why the document says "half as
        // wide" of cells that are 14 points rather than 15.
        assert!(
            (grid.pitch() - full * 16.0 / f32::from(steps)).abs() < 0.01,
            "{steps} steps are not fitted into the width of sixteen"
        );
        assert!(grid.cell_width() >= theme::MIN_CELL_WIDTH, "{steps} steps left no cell");
        for step in 0..steps {
            let cell = grid.cell_rect(step);
            assert!(cell.is_positive(), "step {step} of {steps} has no cell");
            assert_eq!(
                grid.step_at(cell.center()),
                Some(step),
                "step {step} of {steps} cannot be clicked"
            );
        }
    }
}

// ---- W6: the pattern strip -------------------------------------------

/// **W6.1 to W6.7**: the strip's two marks and the Follow toggle — the
/// pattern being edited is the filled-in button, the pattern playing is the
/// lit one, a click moves the first and turns Follow off, and a window opens
/// on pattern 1 with Follow off whatever the last one was showing.
#[test]
fn w6_the_strip_lights_the_playing_pattern_and_follow_tracks_it() {
    assert_eq!(top_bar::PATTERN_LABELS.len(), PATTERN_COUNT);
    assert_eq!(top_bar::PATTERN_LABELS[0], "1", "W6.1 counts the buttons from one");
    assert_eq!(top_bar::PATTERN_LABELS[PATTERN_COUNT - 1], "16");

    // W6.7: every window opens on what plays, with Follow off.
    let mut selection = Selection::new();
    assert!(!selection.follow());
    assert_eq!(selection.track(top_bar::playing_index(Some(4))), 3);
    assert_eq!(selection.track(top_bar::playing_index(Some(9))), 3, "it stays put");

    // W6.3: a click edits and plays — the value it sets the parameter to.
    selection.play(3);
    assert_eq!(top_bar::pattern_value(3), 4, "button 4 sets Pattern to 4");
    let playing = top_bar::playing_index(Some(4));
    assert_eq!(selection.look(3, playing), Look { edited: true, playing: true });

    // W6.4: a shift-click edits without playing, and stays put.
    selection.select(6);
    assert_eq!(selection.look(6, playing), Look { edited: true, playing: false });
    assert_eq!(selection.look(3, playing), Look { edited: false, playing: true });
    assert_eq!(selection.track(playing), 6, "a shift-click was undone by following");

    // W6.5: Off lights nothing.
    assert_eq!(top_bar::playing_index(None), None);
    assert!(!selection.look(0, None).playing, "a button is lit with Pattern Off");

    // W6.6: with Follow on, the window follows the parameter.
    selection.set_follow(true);
    assert_eq!(selection.track(top_bar::playing_index(Some(1))), 0);
    assert_eq!(selection.track(top_bar::playing_index(Some(3))), 2);
}

/// **W6.8 to W6.11**: the pattern button's own menu — the copy block's
/// sixteen targets, the copy itself, the clear, and each of them one undo
/// step that puts the old pattern straight back.
#[test]
fn w6_copy_to_and_clear_are_whole_patterns_and_one_undo_step_each() {
    // W6.9: four across, sixteen targets, each of the sixteen exactly once.
    assert_eq!(top_bar::COPY_COLUMNS, 4);
    let targets: Vec<usize> = (0..top_bar::COPY_ROWS)
        .flat_map(|row| {
            (0..top_bar::COPY_COLUMNS).filter_map(move |col| top_bar::copy_target(row, col))
        })
        .collect();
    assert_eq!(targets, (0..PATTERN_COUNT).collect::<Vec<usize>>());
    assert_eq!(top_bar::copy_target(0, top_bar::COPY_COLUMNS), None, "a fifth column has a target");

    let model = StutterModel::default();
    assert!(model.set_rate(2, Rate::ThirtySecond));
    assert_eq!(model.set_length(2, 12), Some(12));
    assert_eq!(model.add_block(2, 1, 3, Action::from_preset(SpeedPreset::Reverse)), Some(0));
    assert!(model.set_volume(2, 0, 0.25));
    model.commit_edits();
    let before = model.data();

    // W6.10: the whole pattern, and one undo step.
    assert!(top_bar::copy_to(&model, 2, 8));
    let copied = model.data();
    assert_eq!(copied.pattern(8), copied.pattern(2), "the copy is not the pattern entire");
    assert_ne!(copied.pattern(8), before.pattern(8), "nothing was copied");
    assert!(model.undo(), "the copy was not one undo step");
    assert_eq!(model.data().pattern(8), before.pattern(8), "undo did not put pattern 9 back");

    // W6.11: Clear empties the row and the levels and keeps the grid.
    assert!(top_bar::clear(&model, 2));
    let cleared = model.data().pattern(2).cloned().expect("pattern 3");
    assert!(cleared.blocks().is_empty(), "Clear left a block on the row");
    assert!(
        cleared.volumes().iter().all(|&level| level == Pattern::DEFAULT_VOLUME),
        "Clear left a level down"
    );
    assert_eq!(cleared.rate(), Rate::ThirtySecond, "Clear changed the rate");
    assert_eq!(cleared.length(), 12, "Clear changed the length");
    assert!(model.undo(), "Clear was not one undo step");
    assert_eq!(model.data().pattern(2), before.pattern(2), "undo did not put the pattern back");
}

// ---- W7: rate and length ---------------------------------------------

/// **W7.2, W7.3, W7.7 and W7.8**: the rate list and the length box — the
/// five rates with the pattern's own ticked, a typed figure outside 1 to 64
/// pulled back to the nearest end, and a shortened pattern losing what falls
/// outside it in one undo step.
#[test]
fn w7_the_rate_and_length_the_protocol_sets_land_on_the_pattern_it_is_editing() {
    assert_eq!(top_bar::RATE_LABELS.len(), Rate::ALL.len());
    for (rate, label) in Rate::ALL.iter().zip(top_bar::RATE_LABELS) {
        assert_eq!(top_bar::rate_label(*rate), label, "the rate list is out of order");
    }

    // W7.3: the shading follows the **beat** whatever the rate is — four
    // steps to a beat at 1/16, three at 1/8T, eight at 1/32 — and the
    // pattern's first beat is the unshaded one.
    assert_eq!(grid::steps_per_beat(Rate::Sixteenth), 4);
    assert_eq!(grid::steps_per_beat(Rate::EighthTriplet), 3);
    assert_eq!(grid::steps_per_beat(Rate::ThirtySecond), 8);
    for rate in Rate::ALL {
        let per_beat = grid::steps_per_beat(rate);
        assert!(!grid::beat_shaded(0, rate), "the first step of a {rate:?} pattern is shaded");
        assert!(
            !grid::beat_shaded(per_beat - 1, rate),
            "the first beat of a {rate:?} pattern is shaded before it ends"
        );
        assert!(
            grid::beat_shaded(per_beat, rate),
            "the second beat of a {rate:?} pattern is not shaded"
        );
        assert!(!grid::beat_shaded(2 * per_beat, rate), "the third beat is shaded too");
    }

    let model = StutterModel::default();
    let showing = model.data().pattern(0).cloned().expect("pattern 1");
    assert_eq!(showing.rate(), Rate::DEFAULT, "a fresh pattern is not at the ticked rate");
    assert_eq!(showing.length(), Pattern::DEFAULT_LENGTH);

    // W7.3: picking a rate is one gesture, and picking the one already shown
    // writes nothing.
    assert!(!top_bar::choose_rate(&model, 0, showing.rate(), showing.rate()), "a no-op wrote");
    assert!(top_bar::choose_rate(&model, 0, showing.rate(), Rate::EighthTriplet));
    assert_eq!(model.data().pattern(0).map(Pattern::rate), Some(Rate::EighthTriplet));

    // W7.7: `100` typed into the length box.
    let mut length = LengthBox::new();
    assert_eq!(length.show(Pattern::DEFAULT_LENGTH), Pattern::DEFAULT_LENGTH);
    *length.value_mut() = 100;
    assert_eq!(length.changed(&model, 0, Pattern::DEFAULT_LENGTH), Some(Pattern::MAX_LENGTH));
    assert_eq!(model.data().pattern(0).map(Pattern::length), Some(Pattern::MAX_LENGTH));
    assert!(length.settle(&model));

    // W7.8: a block past the new end goes, one straddling it is cut, and one
    // undo step brings the pattern and the length back together.
    assert_eq!(model.set_length(0, 16), Some(16));
    assert_eq!(model.add_block(0, 12, 4, Action::repeat_at(Rate::Sixteenth)), Some(0));
    assert_eq!(model.add_block(0, 6, 4, Action::from_preset(SpeedPreset::TapeStop)), Some(0));
    model.commit_edits();
    let before = model.data();

    assert_eq!(model.set_length(0, 8), Some(8));
    model.commit_edits();
    let shortened = model.data().pattern(0).cloned().expect("pattern 1");
    assert_eq!(shortened.blocks().len(), 1, "the block past the end is still there");
    assert_eq!(
        (shortened.blocks()[0].start(), shortened.blocks()[0].len()),
        (6, 2),
        "the straddling block was not cut short"
    );
    assert!(model.undo());
    assert_eq!(model.data().pattern(0), before.pattern(0), "undo split the length from the row");
}

// ---- W8: the rails and the history buttons ---------------------------

/// **W8.3**: a click or a drag anywhere along a rail puts the value under the
/// pointer, and past either end gives exactly that end — which is what makes
/// `0 %` and `100 %` easy to hit.
#[test]
fn w8_a_rail_takes_the_value_under_the_pointer_and_clamps_past_its_ends() {
    let rail = Rect::from_min_size(pos2(100.0, 0.0), vec2(80.0, theme::CONTROL_HEIGHT));

    assert_eq!(top_bar::rail_position(rail, rail.min.x), 0.0);
    assert_eq!(top_bar::rail_position(rail, rail.max.x), 1.0);
    assert_eq!(top_bar::rail_position(rail, rail.center().x), 0.5);
    assert_eq!(top_bar::rail_position(rail, rail.min.x - 500.0), 0.0, "past the left end");
    assert_eq!(top_bar::rail_position(rail, rail.max.x + 500.0), 1.0, "past the right end");
    assert_eq!(top_bar::rail_position(rail, f32::NAN), 0.0, "a NaN pointer");
}

/// **W8.8 to W8.11**: the two history buttons — greyed out with nothing to
/// do and saying so, one step per gesture however many frames it took, and
/// a gesture that ended where it started recording no step at all.
#[test]
fn w8_undo_and_redo_step_whole_gestures_and_grey_out_when_there_is_nothing() {
    let model = StutterModel::default();
    let mut length = LengthBox::new();

    // The window's first frame: the editor commits whenever no gesture is
    // open, and the first commit is what the first gesture is measured
    // against (`undo.rs`: the baseline is `None` until then).
    model.commit_edits();

    // W8.8: a fresh instance.
    for step in Step::BOTH {
        assert!(!step.available(&model), "{} has something to do on a fresh model", step.label());
    }

    // W8.9: a click onto the row, then a drag of it over several frames.
    let prefs = PrefsCell::default();
    let pattern = model.data().pattern(0).cloned().expect("pattern 1");
    let grid = StepGrid::for_pattern(row_rect(), &pattern);
    assert_eq!(grid::add_block_at(&model, &prefs, 0, pattern.rate(), 0), Some(0));
    assert!(Step::Undo.available(&model), "the click recorded no step");

    let blocks = model.data().pattern(0).expect("pattern 1").blocks().to_vec();
    let mut drag = TimeRowDrag::default();
    assert!(drag.press(&grid, &blocks, grid.cell_rect(0).center()), "the press missed the block");
    for step in 1..=2 {
        let blocks = model.data().pattern(0).expect("pattern 1").blocks().to_vec();
        drag.drag_to(&model, &grid, 0, &blocks, grid.cell_rect(step).center());
    }
    assert!(drag.settle(&model), "the drag did not settle");
    assert_eq!(
        model.data().pattern(0).expect("pattern 1").blocks()[0].start(),
        2,
        "the drag did not move the block"
    );

    // One step for the whole drag: undoing once is back at step 0, not at
    // step 1.
    assert!(undo_buttons::take(Step::Undo, &model, &mut length));
    assert_eq!(model.data().pattern(0).expect("pattern 1").blocks()[0].start(), 0);
    assert!(undo_buttons::take(Step::Undo, &model, &mut length));
    assert!(model.data().pattern(0).expect("pattern 1").blocks().is_empty());
    assert!(!Step::Undo.available(&model), "there is still something to undo");

    // W8.9 again, forwards.
    assert!(Step::Redo.available(&model));
    assert!(undo_buttons::take(Step::Redo, &model, &mut length));
    assert!(undo_buttons::take(Step::Redo, &model, &mut length));
    assert_eq!(model.data().pattern(0).expect("pattern 1").blocks()[0].start(), 2);

    // W8.10: an edit after an undo clears the future.
    assert!(undo_buttons::take(Step::Undo, &model, &mut length));
    assert!(model.set_rate(0, Rate::ThirtySecond));
    model.commit_edits();
    assert!(!Step::Redo.available(&model), "the redone future survived a new edit");

    // W8.11: a gesture that ended where it started.
    let steps = model.edit_count();
    let blocks = model.data().pattern(0).expect("pattern 1").blocks().to_vec();
    let mut idle = TimeRowDrag::default();
    assert!(idle.press(&grid, &blocks, grid.cell_rect(0).center()));
    idle.drag_to(&model, &grid, 0, &blocks, grid.cell_rect(0).center());
    assert!(idle.settle(&model));
    assert_eq!(model.edit_count(), steps, "a drag that moved nothing recorded a step");
}

// ---- W9: the Time row ------------------------------------------------

/// **W9.1 to W9.5**: a click on an empty step adds a one-step block of the
/// last-used action, a click on a covered step does nothing, and an
/// untouched memory means "the classic stutter at this pattern's rate".
#[test]
fn w9_a_click_adds_the_last_used_block_and_never_overlaps() {
    let model = StutterModel::default();
    let prefs = PrefsCell::default();

    // W9.1 and W9.2: the first click of a fresh instance.
    assert_eq!(grid::ADDED_LEN, 1, "W9.2 expects a one-step block");
    assert_eq!(
        grid::action_to_add(prefs.get(), Rate::DEFAULT),
        Action::repeat_at(Rate::DEFAULT),
        "a fresh instance does not add the classic stutter"
    );
    assert_eq!(grid::add_block_at(&model, &prefs, 0, Rate::DEFAULT, 0), Some(0));
    let block = model.data().pattern(0).expect("pattern 1").blocks()[0];
    assert_eq!((block.start(), block.len()), (0, grid::ADDED_LEN));

    // W9.4: the same click on a 1/32 pattern, memory untouched.
    assert!(model.set_rate(1, Rate::ThirtySecond));
    assert_eq!(grid::add_block_at(&model, &prefs, 1, Rate::ThirtySecond, 0), Some(0));
    assert_eq!(
        label_of(model.data().pattern(1).expect("pattern 2").blocks()[0].action()),
        "Rpt 1/32",
        "an untouched memory is not the stutter at this pattern's rate"
    );

    // W9.5: a step a block already covers.
    assert_eq!(grid::add_block_at(&model, &prefs, 0, Rate::DEFAULT, 0), None);
    assert_eq!(model.data().pattern(0).expect("pattern 1").blocks().len(), 1);

    // W9.3 and W9.20: what the menu chose is what the next click adds, on
    // this pattern and on any other.
    let showing = model.data().pattern(0).expect("pattern 1").blocks()[0].action();
    assert!(block::choose_kind(&model, &prefs, 0, 0, showing, Rate::DEFAULT, BlockKind::Reverse));
    let reverse = Action::from_preset(SpeedPreset::Reverse);
    assert_eq!(prefs.get().last_action, reverse, "the menu did not set the memory");
    for (pattern, step) in [(0usize, 4u16), (5, 0)] {
        let rate = model.data().pattern(pattern).expect("a pattern").rate();
        let index = grid::add_block_at(&model, &prefs, pattern, rate, step).expect("a block");
        let added = model.data().pattern(pattern).expect("a pattern").blocks()[index].action();
        assert_eq!(added, reverse, "the click on pattern {} did not add a Rev", pattern + 1);
    }
}

/// **W9.6 to W9.9**: the two drag gestures and what stops them — a move
/// keeps the step it was picked up by under the pointer, a stretch keeps the
/// other end, a block never shrinks past one step, and neither gesture
/// crosses a neighbour or leaves the pattern.
#[test]
fn w9_dragging_a_block_moves_or_stretches_it_and_stops_against_what_is_there() {
    let model = StutterModel::default();
    assert_eq!(model.set_length(0, 16), Some(16));
    assert_eq!(model.add_block(0, 4, 4, Action::repeat_at(Rate::Sixteenth)), Some(0));
    assert_eq!(model.add_block(0, 10, 2, Action::from_preset(SpeedPreset::TapeStop)), Some(1));
    model.commit_edits();

    let pattern = model.data().pattern(0).cloned().expect("pattern 1");
    let grid = StepGrid::for_pattern(row_rect(), &pattern);
    let blocks = pattern.blocks().to_vec();

    // W9.6: what the pointer is over, which is what the hover text and the
    // cursor are read from.
    assert_eq!(grid.hit(grid.cell_rect(0).center(), &blocks), Some(GridHit::Step { step: 0 }));
    assert_eq!(
        grid.hit(grid.cell_rect(5).center(), &blocks),
        Some(GridHit::Body { index: 0, step: 5 }),
        "a body hit names the step of the row the pointer is over"
    );
    assert_eq!(
        grid.hit(grid.edge_rect(4, 4, grid::BlockEdge::Start).center(), &blocks),
        Some(GridHit::Edge { index: 0, edge: grid::BlockEdge::Start })
    );

    // W9.7: a move, grabbed by the block's second step.
    let mut drag = TimeRowDrag::default();
    assert!(drag.press(&grid, &blocks, grid.cell_rect(5).center()));
    assert_eq!(drag.drag_to(&model, &grid, 0, &blocks, grid.cell_rect(2).center()), Some((1, 4)));

    // W9.9: pulled hard into the neighbour at step 11, and then off the end.
    let blocks = model.data().pattern(0).expect("pattern 1").blocks().to_vec();
    assert_eq!(drag.drag_to(&model, &grid, 0, &blocks, grid.cell_rect(15).center()), Some((6, 4)));
    let blocks = model.data().pattern(0).expect("pattern 1").blocks().to_vec();
    assert_eq!(
        drag.drag_to(&model, &grid, 0, &blocks, grid.cell_rect(0).center()),
        Some((0, 4)),
        "the move did not stop at the first step"
    );
    assert!(drag.settle(&model));

    // W9.8: a stretch, and the one-step floor.
    let blocks = model.data().pattern(0).expect("pattern 1").blocks().to_vec();
    let mut stretch = TimeRowDrag::default();
    let far = grid.edge_rect(blocks[0].start(), blocks[0].len(), grid::BlockEdge::End).center();
    assert!(stretch.press(&grid, &blocks, far));
    let out = stretch.drag_to(&model, &grid, 0, &blocks, grid.cell_rect(8).center());
    assert_eq!(out, Some((0, 9)));
    let blocks = model.data().pattern(0).expect("pattern 1").blocks().to_vec();
    assert_eq!(
        stretch.drag_to(&model, &grid, 0, &blocks, grid.cell_rect(0).center()),
        Some((0, 1)),
        "a stretch turned the block inside out"
    );
    assert!(stretch.settle(&model));

    // Nothing was overwritten anywhere in that: the neighbour is where it
    // was, and the two blocks do not overlap.
    let after = model.data().pattern(0).cloned().expect("pattern 1");
    assert_eq!((after.blocks()[1].start(), after.blocks()[1].len()), (10, 2));
    assert!(after.blocks()[0].end() <= after.blocks()[1].start(), "the blocks overlap");
}

/// **W9.14 to W9.23**: the right-click menu — the seven kinds in their
/// colours, the settings each kind actually has, the unnaming of a nudged
/// preset, Duplicate's fitting and the Delete key's hovered block.
#[test]
fn w9_the_block_menu_offers_what_the_block_has_and_each_item_is_one_step() {
    // W9.14 and W9.15: six presets, six labels, each kind its own colour.
    assert_eq!(block::KIND_LABELS.len(), block::PRESET_KINDS.len());
    assert_eq!(block::KIND_HOVERS.len(), block::PRESET_KINDS.len());
    let colours: BTreeSet<[u8; 4]> =
        BlockKind::ALL.iter().map(|kind| kind.colour().to_array()).collect();
    assert_eq!(colours.len(), BlockKind::ALL.len(), "two kinds of block share a colour");
    for kind in block::PRESET_KINDS {
        let action = block::kind_action(kind, Action::repeat_at(Rate::Sixteenth), Rate::Sixteenth);
        assert_eq!(BlockKind::of(action), kind, "the {kind:?} item does not give a {kind:?}");
    }

    // W9.16: every block has a loop and a source, whatever its speeds.
    let repeat = Action::repeat_at(Rate::Sixteenth);
    let reverse = Action::from_preset(SpeedPreset::Reverse);
    let looped = block::with_loop(reverse, Some(NoteValue::Quarter));
    assert_eq!(looped.looping, Some(NoteValue::Quarter));
    assert_eq!(block::with_source(repeat, Source::Continue).source, Source::Continue);
    assert_eq!(repeat.source, Source::BlockStart, "the default source is the block start");

    // W9.18: nudging a preset's endpoint unnames it, and picking the preset
    // again puts the name back.
    let stop = Action::from_preset(SpeedPreset::TapeStop);
    let nudged = Action::ramp(1.0, 0.1);
    assert_eq!(BlockKind::of(nudged), BlockKind::Ramp, "a nudged tape stop kept its name");
    assert_eq!(block::kind_action(BlockKind::TapeStop, nudged, Rate::Sixteenth), stop);

    // W9.19: three repeats of a beat, then a stop that continues them, picks
    // up exactly where the third repeat came back to.
    let mut chain = Pattern::new();
    chain.set_blocks([
        Block::new(0, 12, Action::repeat(NoteValue::Quarter)),
        Block::new(12, 4, stop.with_source(Source::Continue)),
    ]);
    let repeats = chain.block_path(0).expect("the repeat");
    let tail = chain.block_path(1).expect("the stop");
    assert_eq!(tail.offset(0.0), repeats.end_offset(), "the stop starts on the repeated beat");
    assert_eq!(tail.offset(0.0), 3.0, "three beats back: the start of the looped beat");

    let model = StutterModel::default();
    assert_eq!(model.set_length(0, 8), Some(8));
    assert_eq!(model.add_block(0, 0, 2, repeat), Some(0));
    assert_eq!(model.add_block(0, 6, 2, repeat), Some(1));
    model.commit_edits();

    // W9.21: Duplicate fits the copy into the gap, and does nothing with no
    // room at all.
    assert_eq!(block::duplicate(&model, 0, 0), Some(1), "there was room for a copy");
    let after = model.data().pattern(0).cloned().expect("pattern 1");
    assert_eq!((after.blocks()[1].start(), after.blocks()[1].len()), (2, 2));
    assert_eq!(after.blocks()[1].action(), repeat, "the copy is not the same block");
    assert_eq!(block::duplicate(&model, 0, 2), None, "a copy was made with no room for one");

    // W9.23: the Delete key takes the block the pointer is over, and nothing
    // over an empty step.
    let pattern = model.data().pattern(0).cloned().expect("pattern 1");
    let grid = StepGrid::for_pattern(row_rect(), &pattern);
    let blocks = pattern.blocks().to_vec();
    let over_block = grid.hit(grid.cell_rect(0).center(), &blocks);
    let over_empty = grid.hit(grid.cell_rect(4).center(), &blocks);
    assert_eq!(over_empty, Some(GridHit::Step { step: 4 }));
    assert!(!block::delete_hovered(&model, 0, over_empty), "Delete over an empty step deleted");
    assert!(!block::delete_hovered(&model, 0, None), "Delete off the row deleted");
    assert!(block::delete_hovered(&model, 0, over_block), "Delete over a block did nothing");
    assert!(model.undo(), "Delete was not one undo step");
    assert_eq!(model.data().pattern(0), Some(&pattern), "undo did not bring the block back");
}

// ---- W10: the Volume row ---------------------------------------------

/// **W10.1 to W10.8**: the row's gestures — a sweep that fills the bars it
/// skipped, a click that takes one bar to the height it was clicked at, a
/// double-click back to 100 %, whole-percent levels, and one undo step per
/// gesture.
#[test]
fn w10_a_sweep_draws_every_bar_it_crosses_and_a_double_click_resets_one() {
    let model = StutterModel::default();
    assert_eq!(model.set_length(0, 8), Some(8));
    model.commit_edits();

    let pattern = model.data().pattern(0).cloned().expect("pattern 1");
    let grid = StepGrid::for_pattern(row_rect(), &pattern);
    assert!(
        pattern.volumes().iter().all(|&level| level == Pattern::DEFAULT_VOLUME),
        "W10.1 expects every bar full on a fresh pattern"
    );

    // W10.3 and W10.5: the pointer's height is the level, clamped onto the
    // row at both ends.
    assert_eq!(volume::level_at(&grid, grid.row().max.y), 0.0, "the bottom edge is not silence");
    assert_eq!(volume::level_at(&grid, grid.row().min.y), 1.0, "the top edge is not full");
    assert_eq!(volume::level_at(&grid, grid.row().max.y + 500.0), 0.0, "below the row");
    assert_eq!(volume::level_at(&grid, grid.row().min.y - 500.0), 1.0, "above the row");

    // W10.8: levels land on whole percent, which is what the hover text says.
    assert_eq!(volume::quantise(0.5004), 0.5);
    assert_eq!(volume::percent(0.5), 50);

    // W10.4: a sweep from bar 1 to bar 6 in one frame fills the four it
    // jumped over.
    let mut draw = VolumeDraw::default();
    let press = pos2(grid.cell_rect(0).center().x, grid.row().min.y);
    // The press lands on a full bar at full level, so it writes nothing and
    // says so — what it does is give the sweep its starting point.
    let pressed = draw.draw_to(&model, &grid, 0, &pattern, Lane::Volume, press);
    assert!(!pressed, "the press changed a full bar");
    assert_eq!(draw.at(), Some((0, 1.0)), "the press did not take hold of bar 1");
    let pattern = model.data().pattern(0).cloned().expect("pattern 1");
    let to = pos2(grid.cell_rect(5).center().x, grid.row().max.y);
    assert!(draw.draw_to(&model, &grid, 0, &pattern, Lane::Volume, to));
    assert!(draw.settle(&model), "the sweep did not settle");

    let drawn = model.data().pattern(0).cloned().expect("pattern 1");
    let levels: Vec<f32> = drawn.volumes()[..6].to_vec();
    assert_eq!(levels[0], 1.0, "the bar the sweep started on");
    assert_eq!(levels[5], 0.0, "the bar the sweep ended on");
    for pair in levels.windows(2) {
        assert!(pair[1] <= pair[0], "the ramp across the sweep is not monotone: {levels:?}");
    }
    assert!(levels.iter().all(|level| *level <= 1.0), "a level escaped the row");

    // One step for the whole sweep.
    assert!(model.undo(), "the sweep was not one undo step");
    assert!(
        model.data().pattern(0).expect("pattern 1").volumes().iter().all(|&l| l == 1.0),
        "undo left a bar down"
    );

    // W10.6 and W10.7: a click on one bar, then a double-click back.
    assert!(model.set_volume(0, 3, 0.4));
    model.commit_edits();
    let pattern = model.data().pattern(0).cloned().expect("pattern 1");
    assert!(volume::reset(&model, 0, &pattern, Lane::Volume, 3), "the double-click wrote nothing");
    assert_eq!(model.data().pattern(0).expect("pattern 1").volume(3), Pattern::DEFAULT_VOLUME);
    let pattern = model.data().pattern(0).cloned().expect("pattern 1");
    assert!(!volume::reset(&model, 0, &pattern, Lane::Volume, 3), "a full bar was written");
}

// ---- W11: the waveform view ------------------------------------------

/// **W11.1 to W11.8**: the band's brightness and its line — the previous
/// pass drawn dim ahead of the playhead, the whole view dimmed when it is
/// not a picture of the pattern on screen, no line with nothing playing or
/// nothing written, and a trace that never rescales itself.
#[test]
fn w11_the_waveform_dims_the_old_pass_and_the_whole_view_when_it_is_elsewhere() {
    // W11.3: the generation rule.
    assert_eq!(Shade::of(0), Shade::Fresh, "this cycle's column is not drawn fresh");
    assert_eq!(Shade::of(1), Shade::Stale, "last cycle's column is not dimmed");
    assert_eq!(Shade::of(9), Shade::Stale);
    const { assert!(theme::WAVE_STALE_FACTOR < 1.0, "a stale column is not dimmer") };

    // W11.5 and W11.6: whose cycle the band is a picture of.
    assert_eq!(Subject::of(Some(2), 2), Subject::OnScreen);
    assert_eq!(Subject::of(Some(0), 2), Subject::Elsewhere, "another pattern's cycle is on screen");
    assert_eq!(Subject::of(None, 2), Subject::Elsewhere, "Pattern Off is drawn as live");
    assert_eq!(Subject::Elsewhere.shade(0), Shade::Stale, "an elsewhere column is drawn bright");
    assert_eq!(Subject::OnScreen.shade(0), Shade::Fresh);

    // W11.6 and W11.7: the line is withheld with nothing playing, and with a
    // scope nothing has written — which is where a host's `reset()` on a
    // transport stop leaves it.
    let scope = Scope::new();
    assert_eq!(wave::playhead(Some(0), &scope), None, "a line was drawn over an empty scope");
    scope.write(0.25, (0.5, 0.5), (0.5, 0.5));
    assert_eq!(wave::playhead(Some(0), &scope), Some(0.25), "the line is not where the song is");
    assert_eq!(wave::playhead(None, &scope), None, "Pattern Off drew a line");
    scope.clear();
    assert_eq!(wave::playhead(Some(0), &scope), None, "a cleared scope still drew a line");

    // W11.8: past full scale is flat against the edge rather than rescaled.
    let view = WaveView::new(Rect::from_min_size(pos2(0.0, 0.0), vec2(512.0, 90.0)));
    assert_eq!(view.sample_y(wave::FULL_SCALE), view.view().min.y, "full scale is not the top");
    assert_eq!(view.sample_y(-wave::FULL_SCALE), view.view().max.y);
    assert_eq!(view.sample_y(4.0), view.view().min.y, "a hot sample was not clamped");
    assert_eq!(view.sample_y(-4.0), view.view().max.y);

    // W11.4: the band's columns span the band and nothing else.
    assert_eq!(view.column_x(0.0), view.view().min.x);
    assert!((view.phase_x(1.0) - view.view().max.x).abs() < 0.01);
}

// ---- W12: the keyboard -----------------------------------------------

/// **W12.1 to W12.7**: where each key the protocol presses goes. The
/// window's whole key decision table, which is the one thing in this window
/// a user cannot see.
#[test]
fn w12_the_keys_the_protocol_presses_go_where_it_says_they_do() {
    let plain = Modifiers::default();
    let ctrl = Modifiers { command: true, ctrl: true, ..Modifiers::default() };
    let ctrl_shift = Modifiers { shift: true, ..ctrl };

    // W12.1 to W12.3: on to Bitwig.
    assert_eq!(keys::route(Key::Space, plain, false), Route::Host(0x20));
    assert_eq!(keys::route(Key::F1, plain, false), Route::Host(0xFFBE));
    assert_eq!(keys::route(Key::F12, plain, false), Route::Host(0xFFBE + 11));
    assert_eq!(keys::route(Key::S, ctrl, false), Route::Host(u32::from(b's')));
    assert_eq!(keys::route(Key::Num1, ctrl, false), Route::Host(u32::from(b'1')));

    // W12.4: kept back for this window's own history.
    assert_eq!(keys::route(Key::Z, ctrl, false), Route::History(Step::Undo));
    assert_eq!(keys::route(Key::Z, ctrl_shift, false), Route::History(Step::Redo));
    assert_eq!(keys::route(Key::Y, ctrl, false), Route::History(Step::Redo));

    // W12.7: the window's own, and never Bitwig's.
    for key in [Key::Delete, Key::Escape, Key::Enter, Key::Tab, Key::ArrowLeft, Key::A] {
        assert_eq!(keys::route(key, plain, false), Route::Editor, "{key:?} left the window");
    }

    // W12.5: while a box is being typed into, every key is the box's.
    for (key, modifiers) in [
        (Key::Space, plain),
        (Key::F1, plain),
        (Key::S, ctrl),
        (Key::Z, ctrl),
        (Key::Y, ctrl),
        (Key::Delete, plain),
    ] {
        assert_eq!(
            keys::route(key, modifiers, true),
            Route::Editor,
            "{key:?} was taken out of a box being typed into"
        );
    }
}

/// **W12.6**: a shortcut pressed with a hand still on the grid is ignored and
/// the gesture stands — "let go first".
#[test]
fn w12_a_shortcut_pressed_mid_gesture_is_ignored_and_the_gesture_stands() {
    let model = StutterModel::default();
    let mut length = LengthBox::new();
    // A frame with no gesture open, then the edit (see `w8_…` on the
    // baseline the first commit takes).
    model.commit_edits();
    assert!(model.set_rate(0, Rate::ThirtySecond));
    model.commit_edits();
    assert!(Step::Undo.available(&model));

    let press = [Event::Key {
        key: Key::Z,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers { command: true, ctrl: true, ..Modifiers::default() },
    }];

    assert_eq!(undo_buttons::apply(&press, false, true, &model, &mut length), 0, "it stepped");
    assert_eq!(model.data().pattern(0).map(Pattern::rate), Some(Rate::ThirtySecond));
    // And once the hand lets go, the same key takes the gesture back.
    assert_eq!(undo_buttons::apply(&press, false, false, &model, &mut length), 1);
    assert_eq!(model.data().pattern(0).map(Pattern::rate), Some(Rate::DEFAULT));
    // W12.5 from the other side: the same press inside a box steps nothing.
    assert_eq!(undo_buttons::apply(&press, true, false, &model, &mut length), 0);
}

// ---- W13: keep on top ------------------------------------------------

/// **W13.1 to W13.6**: the switch — on for a new device, the two
/// transitions that ask the window manager for something and the two that
/// ask for nothing, a click that flips it, and a pref that is saved with the
/// project but is not in the window's Undo.
#[test]
fn w13_the_keep_on_top_switch_is_on_by_default_and_asks_for_two_things() {
    // W13.1: a fresh device.
    assert!(Prefs::default().keep_on_top, "a new device does not open pinned");

    // W13.3 and W13.4: the two transitions that do something, and the two
    // that do not — the editor hands the pref over on every frame, so a
    // frame in which nothing changed must ask for nothing.
    assert_eq!(keys::pin_change(true, false), Some(Pin::Remove), "W13.3 does not unpin");
    assert_eq!(keys::pin_change(false, true), Some(Pin::Restore), "W13.4 does not put it back");
    assert_eq!(keys::pin_change(true, true), None, "a still switch asked for something");
    assert_eq!(keys::pin_change(false, false), None);

    // The click itself.
    let prefs = PrefsCell::default();
    assert!(!top_bar::toggle_keep_on_top(&prefs), "the first click did not turn it off");
    assert!(!prefs.get().keep_on_top);
    assert!(top_bar::toggle_keep_on_top(&prefs), "the second click did not turn it back on");

    // W13.6: saved with the project, and not an edit — the switch moving
    // leaves the patterns' history alone.
    let model = StutterModel::default();
    prefs.set_keep_on_top(false);
    assert!(!model.can_undo(), "the switch recorded an undo step");
    let saved = Snapshot::of(&model.data(), prefs.get());
    assert!(!saved.prefs().keep_on_top, "the switch was not saved");
}

// ---- W14: save, close and reopen -------------------------------------

/// **W14.1 to W14.4**: what a reopened project brings back — all sixteen
/// patterns and both window prefs, through the same serde door the host
/// stores — and what it deliberately does not: the window's own state, which
/// opens on pattern 1 with Follow off however the last window was left.
#[test]
fn w14_a_reopened_project_brings_the_patterns_and_the_prefs_and_not_the_window() {
    let model = StutterModel::default();
    for index in 0..PATTERN_COUNT {
        let rate = Rate::ALL[index % Rate::ALL.len()];
        assert!(model.set_rate(index, rate));
        assert_eq!(model.set_length(index, 8 + index as u16), Some(8 + index as u16));
        assert_eq!(model.add_block(index, 1, 2, Action::repeat_at(rate)), Some(0));
        assert!(model.set_volume(index, 0, 0.5));
    }
    let last_action = Action::from_preset(SpeedPreset::Reverse);
    let prefs = Prefs { keep_on_top: false, last_action, notes: Some(96), latch: true };
    let saved = Snapshot::of(&model.data(), prefs);

    // The blob the host writes, and reads back.
    let stored = serde_json::to_string(&saved).expect("the snapshot would not serialise");
    let loaded: Snapshot = serde_json::from_str(&stored).expect("the blob is not a snapshot");

    assert_eq!(loaded.data(), model.data(), "the patterns did not come back");
    assert_eq!(loaded.prefs(), prefs, "the window's prefs did not come back");

    // W14.3: the window state is per window and is in neither of those.
    let fresh = Selection::new();
    assert!(!fresh.follow(), "a reopened window has Follow off");
}

/// **W14.5**: a project from a newer build, or a damaged one, opens with the
/// patterns it could not read left empty rather than refusing to load.
#[test]
fn w14_a_project_from_the_future_or_a_damaged_one_still_opens() {
    let future = r#"{"version":9999,"patterns":[],"prefs":{"keep_on_top":false}}"#;
    let loaded: Snapshot = serde_json::from_str(future).expect("a future project must still parse");
    assert_eq!(loaded.data(), StutterData::new(), "a future project was not read as empty");

    let damaged = r#"{"version":1,"patterns":"not a list"}"#;
    assert!(
        serde_json::from_str::<Snapshot>(damaged).is_err(),
        "a damaged blob parsed, so the shell's own default never happens"
    );
}

// ---- W15: the protocol's own step ------------------------------------

/// **W15.1**: the command the protocol tells the user to run first is the
/// one that runs this file.
///
/// A test of a sentence rather than of behaviour, and it is here because the
/// sentence is load-bearing: if the document named another target, a person
/// following it would believe the quotations had been checked when they had
/// not.
#[test]
fn w15_the_command_the_document_names_is_the_one_that_runs_this_file() {
    let Some(doc) = local_file("docs/feature-walkthrough.md") else {
        return;
    };

    let target = module_path!();
    assert_eq!(target, "feature_walkthrough", "this file's test target was renamed");
    assert!(
        doc.contains(&format!("cargo test --test {target}")),
        "the document does not name `cargo test --test {target}`"
    );
    assert!(
        doc.contains(&format!("stuttter/tests/{target}.rs")),
        "the document does not name this file"
    );
}
