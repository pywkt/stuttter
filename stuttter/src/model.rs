//! The sixteen patterns, and the one way they reach the audio thread.
//!
//! [`StutterData`] is all sixteen of them as one **immutable** value, and
//! [`StutterModel`] is the only thing that changes it: every writer — a GUI
//! gesture, a pattern copy, an undo, the host loading a project — goes
//! through the model's writer mutex, which builds a replacement
//! `StutterData` and publishes it through a `basedrop::SharedCell`. The
//! audio thread does one `SharedCell::get` at the top of its block and reads
//! that for the whole block. It never takes the mutex, never allocates and
//! never frees.
//!
//! This is bitstep's `model.rs` scheme with its one simplification: a
//! [`Pattern`] holds no atomics, so there is no such thing here as a "value
//! edit" that stores into the published data in place. Every write is a
//! shape edit — clone, mutate, publish — which is cheap enough at this size
//! (sixteen patterns of at most sixty-four blocks) and is what makes a
//! published snapshot safe to hand to undo and to the state save as it
//! stands.
//!
//! # Why a published snapshot is never edited
//!
//! Because a reader holds it. `SharedCell::get` hands the audio thread a
//! `Shared<StutterData>` which it keeps until the end of the block, so the
//! data it is reading must not change under it mid-block — a block that
//! started on a four-step Repeat must finish on one. Publishing a
//! replacement gives the next block the edit and leaves this one alone, and
//! the retired copy is freed by [`StutterModel::collect`] once no reader
//! holds it: on the GUI thread per frame, and by every writer, so an
//! instance with no editor open still reclaims.
//!
//! # What is here
//!
//! The model and its publishing: the sixteen patterns, the writer mutex over
//! them, the collector behind that same mutex, and the whole writer surface
//! — [`StutterModel::set_rate`], [`StutterModel::set_length`],
//! [`StutterModel::set_volume`], the six block writers
//! ([`StutterModel::add_block`], [`StutterModel::resize_block`],
//! [`StutterModel::move_block`], [`StutterModel::set_block_action`],
//! [`StutterModel::duplicate_block`], [`StutterModel::delete_block`], plus
//! [`StutterModel::set_block_span`] for a left-edge drag),
//! [`StutterModel::copy_pattern`] and [`StutterModel::clear_pattern`]. Every
//! one of them clamps, because [`Pattern`] does: the model's own job is the
//! pattern index, the lock and the publish.
//!
//! And undo / redo, which is those same two doors used from here:
//! [`StutterModel::commit_edits`] hands [`StutterModel::data`] to the
//! [`EditHistory`] as one step, [`StutterModel::undo`] and
//! [`StutterModel::redo`] walk the stacks and [`StutterModel::install`] puts
//! the step they return back. The stacks live behind their own mutex, taken
//! *before* the writer mutex and never the other way round.
//!
//! Elsewhere: the serde snapshot that goes through the same two doors
//! (`state.rs`), whose load calls [`StutterModel::clear_history`] — what the
//! host loaded is a new starting point, not an edit to step back out of; and
//! the playback position the GUI draws the playhead from, which is
//! deliberately *not* in `StutterData` — a shape edit republishes the data,
//! and the playhead must not be republished with it.

use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Mutex, MutexGuard};

use basedrop::{Collector, Handle, Shared, SharedCell};

use crate::pattern::{Action, Filter, Lane, PATTERN_COUNT, Pattern, Rate};
use crate::starters::Starter;
use crate::undo::EditHistory;

/// All sixteen patterns as one value — what the audio thread reads and what
/// a writer replaces.
///
/// Immutable once published: the setters are [`StutterModel`]'s, and they
/// work on a clone. Cloning one is what a write costs and what
/// [`StutterModel::copy_pattern`] is built from.
#[derive(Clone, Debug, PartialEq)]
pub struct StutterData {
    patterns: [Pattern; PATTERN_COUNT],
}

impl StutterData {
    /// Sixteen fresh patterns — a new plugin instance, and what a corrupt
    /// snapshot loads as.
    pub fn new() -> StutterData {
        StutterData { patterns: std::array::from_fn(|_| Pattern::new()) }
    }

    /// Sixteen patterns from an iterator of any length — the shape a loaded
    /// snapshot arrives in. Extra patterns are dropped and missing ones are
    /// [`Pattern::new`], so a snapshot that stored twelve, or two hundred,
    /// still loads as exactly [`PATTERN_COUNT`] of them.
    pub fn from_patterns(patterns: impl IntoIterator<Item = Pattern>) -> StutterData {
        let mut data = StutterData::new();
        for (slot, pattern) in data.patterns.iter_mut().zip(patterns) {
            *slot = pattern;
        }

        data
    }

    /// One pattern, or `None` for an index that is not a pattern.
    ///
    /// `None` rather than a panic because the `pattern` host parameter is
    /// what indexes this on the audio thread, and a host may hand over any
    /// number at all.
    pub fn pattern(&self, index: usize) -> Option<&Pattern> {
        self.patterns.get(index)
    }

    /// All sixteen, in `pattern` parameter order.
    pub fn patterns(&self) -> &[Pattern; PATTERN_COUNT] {
        &self.patterns
    }
}

impl Default for StutterData {
    fn default() -> StutterData {
        StutterData::new()
    }
}

/// The one owner of the concurrency scheme: the audio thread reads the
/// published [`StutterData`] lock-free through [`StutterModel::shared`],
/// every writer goes through here.
///
/// A writer clones the published data, edits the clone, and swaps it in
/// under the writer mutex. The mutex is what keeps two writers from each
/// building a replacement from the same starting point and one of them
/// losing — the GUI and a host state load are both writers, and they can
/// land in the same instant.
pub struct StutterModel {
    /// Dropped by hand in [`StutterModel`]'s `Drop` so the published
    /// `StutterData` reaches the collector's queue *before* the collector is
    /// torn down.
    cell: ManuallyDrop<SharedCell<StutterData>>,
    /// Allocates every replacement `StutterData`. Also dropped by hand, for
    /// the same reason: `Collector::try_cleanup` refuses while a handle is
    /// live.
    handle: ManuallyDrop<Handle>,
    /// Serialises all writers. Holds no data — what it protects is the
    /// published `StutterData` and the `cell` pointer.
    writer: Mutex<()>,
    /// `Collector::collect` needs `&mut`, and the collector has to be moved
    /// out in `Drop` to be cleaned up, hence the `Option`.
    collector: Mutex<Option<Collector>>,
    /// Bumped by every write (see [`StutterModel::edit_lock`]), so undo can
    /// tell whether anything changed since its last commit without
    /// snapshotting the patterns to find out.
    edits: AtomicU64,
    /// Undo and redo, as whole-model snapshots. Its own mutex because the
    /// audio thread never touches it and the GUI commits to it between
    /// gestures; locked **before** `writer` wherever both are needed
    /// ([`StutterModel::step_history`]), never after.
    history: Mutex<EditHistory<StutterData>>,
}

impl StutterModel {
    /// Publishes `data` as the starting set of patterns.
    pub fn new(data: StutterData) -> StutterModel {
        let collector = Collector::new();
        let handle = collector.handle();
        let cell = SharedCell::new(Shared::new(&handle, data));

        StutterModel {
            cell: ManuallyDrop::new(cell),
            handle: ManuallyDrop::new(handle),
            writer: Mutex::new(()),
            collector: Mutex::new(Some(collector)),
            edits: AtomicU64::new(0),
            history: Mutex::new(EditHistory::new()),
        }
    }

    /// The audio thread's single entry point: one `get()` at the top of the
    /// block, then that reference for the whole block.
    ///
    /// Does not allocate, lock or block — it is a pair of atomic
    /// increments.
    pub fn shared(&self) -> Shared<StutterData> {
        self.cell.get()
    }

    /// A copy of the published patterns, for the state save and for undo to
    /// push on its stack.
    ///
    /// Allocates, so writer threads only. Takes the writer lock, so what
    /// comes back is never half of one edit and half of the next.
    pub fn data(&self) -> StutterData {
        let _writer = self.writer_lock();

        (*self.cell.get()).clone()
    }

    /// Publishes a whole replacement set of patterns: the shared half of a
    /// host state load and of an undo step.
    ///
    /// Headless by construction — it is the same publish every GUI gesture
    /// makes, with no editor in it anywhere (restores go through the model's
    /// writer API and must work headless).
    pub fn install(&self, data: StutterData) {
        let _writer = self.edit_lock();
        self.publish(data);
    }

    /// Frees the `StutterData`s that writes replaced, once no reader holds
    /// them any more. Call it every GUI frame; every write calls it too, so
    /// an instance with no editor open still reclaims.
    ///
    /// Never call this from the audio thread — it drops memory.
    pub fn collect(&self) {
        if let Some(collector) = self.collector_lock().as_mut() {
            collector.collect();
        }
    }

    /// How many writes this model has taken, in any order they happened —
    /// the cheap "has anything changed?" [`StutterModel::commit_edits`] asks
    /// between gestures. A write that turns out to change nothing still
    /// counts.
    pub fn edit_count(&self) -> u64 {
        self.edits.load(Relaxed)
    }

    /// How many `StutterData`s basedrop still has alive: the published one
    /// plus every retired one a reader has not let go of yet. A steady state
    /// is `1`, so this is the only way to see that [`StutterModel::collect`]
    /// really reclaims.
    ///
    /// Takes the collector mutex, so never call it from the audio thread.
    pub fn live_allocs(&self) -> usize {
        self.collector_lock().as_ref().map_or(0, Collector::alloc_count)
    }

    // ---- the writer surface ----

    /// Sets one pattern's step length. `false`, and nothing published, for
    /// an index that is not a pattern.
    pub fn set_rate(&self, pattern: usize, rate: Rate) -> bool {
        self.edit(pattern, |pattern| {
            pattern.set_rate(rate);
            Some(())
        })
        .is_some()
    }

    /// Sets one pattern's length in steps, clamped to
    /// `1..=Pattern::MAX_LENGTH`, returning the length it actually got — a
    /// length drag reads that back. Blocks left outside are dropped or
    /// truncated by [`Pattern::set_length`].
    pub fn set_length(&self, pattern: usize, length: u16) -> Option<u16> {
        self.edit(pattern, |pattern| {
            pattern.set_length(length);
            Some(pattern.length())
        })
    }

    /// Sets one step's level on the Volume row, clamped to `0.0..=1.0`
    /// (NaN included — see [`crate::pattern::clamp_volume`]). A step outside
    /// the row is ignored, which still publishes.
    pub fn set_volume(&self, pattern: usize, step: u16, volume: f32) -> bool {
        self.edit(pattern, |pattern| {
            pattern.set_volume(step, volume);
            Some(())
        })
        .is_some()
    }

    /// Replaces a whole Volume row, truncated or padded to the row's length
    /// and clamped entry by entry ([`Pattern::set_volumes`]) — a draw
    /// gesture across the row, and the Volume half of a snapshot load.
    pub fn set_volumes(&self, pattern: usize, volumes: impl IntoIterator<Item = f32>) -> bool {
        self.edit(pattern, |pattern| {
            pattern.set_volumes(volumes);
            Some(())
        })
        .is_some()
    }

    /// Replaces a whole row of levels on `lane` ([`Pattern::set_lane`]) — a
    /// draw gesture across the Volume, Cutoff or Res row.
    pub fn set_lane(
        &self,
        pattern: usize,
        lane: Lane,
        levels: impl IntoIterator<Item = f32>,
    ) -> bool {
        self.edit(pattern, |pattern| {
            pattern.set_lane(lane, levels);
            Some(())
        })
        .is_some()
    }

    /// Sets one step's level on `lane` ([`Pattern::set_level`]) — a
    /// double-click putting a bar back.
    pub fn set_level(&self, pattern: usize, lane: Lane, step: u16, level: f32) -> bool {
        self.edit(pattern, |pattern| {
            pattern.set_level(lane, step, level);
            Some(())
        })
        .is_some()
    }

    /// The filter picked in the window ([`Pattern::choose_filter`]). Like
    /// the other writers it does not commit; the caller makes the pick one
    /// undo step.
    pub fn choose_filter(&self, pattern: usize, filter: Filter) -> bool {
        self.edit(pattern, |pattern| {
            pattern.choose_filter(filter);
            Some(())
        })
        .is_some()
    }

    /// Adds a block of up to `len` steps at `start`, returning its index on
    /// the Time row. `None`, and nothing published, where the gesture has
    /// nowhere to go: the step is covered already, the row is full, or the
    /// index is not a pattern ([`Pattern::add_block`]).
    pub fn add_block(
        &self,
        pattern: usize,
        start: u16,
        len: u16,
        action: Action,
    ) -> Option<usize> {
        self.edit(pattern, |pattern| pattern.add_block(start, len, action))
    }

    /// Resizes a block from its right edge, returning the length it got
    /// ([`Pattern::resize_block`]).
    pub fn resize_block(&self, pattern: usize, index: usize, len: u16) -> Option<u16> {
        self.edit(pattern, |pattern| pattern.resize_block(index, len))
    }

    /// Moves a block, keeping its length, returning the step it landed on —
    /// clamped against its neighbours and the ends of the pattern
    /// ([`Pattern::move_block`]).
    pub fn move_block(&self, pattern: usize, index: usize, start: u16) -> Option<u16> {
        self.edit(pattern, |pattern| pattern.move_block(index, start))
    }

    /// Sets both of a block's edges at once — the left-edge drag, where the
    /// start moves and the length follows — returning the span it got
    /// ([`Pattern::set_block_span`]).
    pub fn set_block_span(
        &self,
        pattern: usize,
        index: usize,
        start: u16,
        len: u16,
    ) -> Option<(u16, u16)> {
        self.edit(pattern, |pattern| pattern.set_block_span(index, start, len))
    }

    /// Gives a block a new action, sanitised ([`Action::sanitised`]).
    /// `false`, and nothing published, for a block or pattern that does not
    /// exist.
    pub fn set_block_action(&self, pattern: usize, index: usize, action: Action) -> bool {
        self.edit(pattern, |pattern| pattern.set_block_action(index, action).then_some(()))
            .is_some()
    }

    /// Copies a block into the space immediately after it, returning the
    /// copy's index, or `None` where there is no room
    /// ([`Pattern::duplicate_block`]).
    pub fn duplicate_block(&self, pattern: usize, index: usize) -> Option<usize> {
        self.edit(pattern, |pattern| pattern.duplicate_block(index))
    }

    /// Removes a block. `false`, and nothing published, for a block or
    /// pattern that does not exist.
    pub fn delete_block(&self, pattern: usize, index: usize) -> bool {
        self.edit(pattern, |pattern| pattern.delete_block(index).then_some(())).is_some()
    }

    /// Copies pattern `from` over pattern `to`, whole: rate, length, blocks
    /// and the Volume row. `false`, and nothing published, if either index
    /// is not a pattern or they are the same one.
    pub fn copy_pattern(&self, from: usize, to: usize) -> bool {
        if from == to || from >= PATTERN_COUNT || to >= PATTERN_COUNT {
            return false;
        }

        self.edit_data(|data| {
            data.patterns[to] = data.patterns[from].clone();
            Some(())
        })
        .is_some()
    }

    /// Fills one pattern with a starter: its rate and length kept, what was
    /// drawn on it replaced by `starter`'s ([`Starter::fill`]). `false`, and
    /// nothing published, for an index that is not a pattern.
    pub fn fill_pattern(&self, pattern: usize, starter: Starter) -> bool {
        self.edit(pattern, |pattern| {
            *pattern = starter.fill(pattern);
            Some(())
        })
        .is_some()
    }

    /// Empties one pattern: no blocks, the Volume row back to unity, the
    /// rate and length left as they are ([`Pattern::clear`]).
    pub fn clear_pattern(&self, pattern: usize) -> bool {
        self.edit(pattern, |pattern| {
            pattern.clear();
            Some(())
        })
        .is_some()
    }

    // ---- undo and redo ----
    //
    // Every write bumps `edits`; the editor calls `commit_edits` whenever the
    // user is between gestures, and whatever changed since the last commit
    // becomes one undo step (see `crate::undo`). So a drag is one step,
    // committed on release, however many writes it made. The host parameters
    // — Pattern, Mix, Smoothing — are not in here at all: the host owns
    // those, and its own undo covers them.

    /// Records the edits made since the last call as one undo step. Cheap
    /// when nothing changed (one atomic load); a snapshot otherwise. GUI
    /// thread, between gestures.
    pub fn commit_edits(&self) {
        let mut history = self.history_lock();
        self.commit_into(&mut history);
    }

    /// Steps back one undo step, publishing the patterns as they were.
    /// `false`, and nothing published, if there was nothing to undo.
    ///
    /// Commits first, so an edit whose gesture has not been committed yet is
    /// the step this undoes.
    pub fn undo(&self) -> bool {
        self.step_history(EditHistory::undo)
    }

    /// Steps forward again after an undo. `false` if there was nothing to
    /// redo — which is also what a new edit since the undo leaves behind.
    pub fn redo(&self) -> bool {
        self.step_history(EditHistory::redo)
    }

    /// Whether [`StutterModel::undo`] has anything to do, as of the last
    /// commit — what greys the editor's Undo button out.
    pub fn can_undo(&self) -> bool {
        self.history_lock().can_undo()
    }

    /// Whether [`StutterModel::redo`] has anything to do.
    pub fn can_redo(&self) -> bool {
        self.history_lock().can_redo()
    }

    /// Forgets both stacks: the host loaded a project, and stepping back into
    /// the patterns it replaced would be a surprise (`state.rs`).
    /// The next commit takes a fresh baseline from whatever is published now.
    pub fn clear_history(&self) {
        self.history_lock().clear();
    }

    // ---- internals ----

    fn history_lock(&self) -> MutexGuard<'_, EditHistory<StutterData>> {
        self.history.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Takes the baseline snapshot, if the edit count moved since the last
    /// one. Caller holds the history lock; [`StutterModel::data`] takes the
    /// writer lock inside it, which is the one allowed order.
    fn commit_into(&self, history: &mut EditHistory<StutterData>) {
        history.commit(self.edits.load(Relaxed), || self.data());
    }

    /// The shared half of [`StutterModel::undo`] and [`StutterModel::redo`]:
    /// commit, take one step, publish what it returned, and tell the history
    /// the edit count that restore left behind so the restore is not itself
    /// a step to undo.
    fn step_history(
        &self,
        step: impl FnOnce(&mut EditHistory<StutterData>) -> Option<StutterData>,
    ) -> bool {
        let mut history = self.history_lock();
        self.commit_into(&mut history);
        let Some(target) = step(&mut history) else {
            return false;
        };
        self.install(target);
        history.rebase(self.edits.load(Relaxed));

        true
    }

    /// The writer mutex, poisoning ignored: a panicking writer must not
    /// wedge every later edit (which in a plugin means a dead GUI), and what
    /// it guards is a `StutterData` that is only ever published whole, so
    /// there is no half-finished edit for the next writer to find.
    fn writer_lock(&self) -> MutexGuard<'_, ()> {
        self.writer.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The writer lock for a write: counts the edit for the undo history.
    fn edit_lock(&self) -> MutexGuard<'_, ()> {
        let guard = self.writer_lock();
        self.edits.fetch_add(1, Relaxed);

        guard
    }

    fn collector_lock(&self) -> MutexGuard<'_, Option<Collector>> {
        self.collector.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Runs an edit on one pattern: `f` gets a copy of the published one.
    /// Returning `Some` publishes the replacement; returning `None` — or an
    /// index that is not a pattern — abandons the edit, leaving the old data
    /// published and nothing allocated for the collector.
    fn edit<R>(&self, pattern: usize, f: impl FnOnce(&mut Pattern) -> Option<R>) -> Option<R> {
        self.edit_data(|data| f(data.patterns.get_mut(pattern)?))
    }

    /// [`StutterModel::edit`] across all sixteen — what copy-pattern needs.
    fn edit_data<R>(&self, f: impl FnOnce(&mut StutterData) -> Option<R>) -> Option<R> {
        let _writer = self.edit_lock();
        let mut next = (*self.cell.get()).clone();
        let result = f(&mut next)?;
        self.publish(next);

        Some(result)
    }

    /// Publishes a replacement `StutterData` and reclaims whatever that
    /// retired, so an instance whose editor is closed does not pile up old
    /// copies. Caller holds the writer lock.
    fn publish(&self, data: StutterData) {
        self.cell.set(Shared::new(&self.handle, data));
        self.collect();
    }
}

impl Default for StutterModel {
    /// A model of sixteen fresh patterns — a new plugin instance.
    fn default() -> StutterModel {
        StutterModel::new(StutterData::new())
    }
}

impl Drop for StutterModel {
    /// `Collector` has no `Drop` of its own — dropping one leaks every
    /// allocation it ever handed out, which in a host means a whole
    /// `StutterData` per plugin instance. So tear down in order: release the
    /// published data and the handle, drain the queue, then free the
    /// collector itself.
    ///
    /// If someone still holds a `Shared<StutterData>` from
    /// [`StutterModel::shared`], `try_cleanup` refuses and we leak instead
    /// of freeing memory that is still being read — the safe direction.
    fn drop(&mut self) {
        // SAFETY: `drop` runs once, and neither field is touched afterwards.
        unsafe {
            ManuallyDrop::drop(&mut self.cell);
            ManuallyDrop::drop(&mut self.handle);
        }
        if let Some(mut collector) = self.collector_lock().take() {
            collector.collect();
            let _ = collector.try_cleanup();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Arc;
    use std::thread;

    use crate::actions::SpeedPreset;
    use crate::pattern::{MAX_BLOCKS, MAX_STEPS, NoteValue, Source};
    use crate::undo::UNDO_DEPTH;

    /// What the audio thread would read: the published pattern at `index`.
    fn published(model: &StutterModel, index: usize) -> Pattern {
        model.shared().pattern(index).expect("the pattern is published").clone()
    }

    /// A model is handed to the audio thread and to the GUI at once, which
    /// is a compile-time claim about it, so it is tested at compile time.
    #[test]
    fn a_model_is_shared_between_threads() {
        fn assert_send_sync<T: Send + Sync>() {}

        assert_send_sync::<StutterModel>();
        assert_send_sync::<StutterData>();
        assert_send_sync::<Shared<StutterData>>();
    }

    /// A fresh instance is sixteen default patterns, published.
    #[test]
    fn a_fresh_model_publishes_sixteen_default_patterns() {
        let model = StutterModel::default();
        let data = model.shared();

        assert_eq!(data.patterns().len(), PATTERN_COUNT);
        for index in 0..PATTERN_COUNT {
            assert_eq!(data.pattern(index), Some(&Pattern::new()), "pattern {index}");
        }
        assert_eq!(data.pattern(PATTERN_COUNT), None);
        assert_eq!(model.edit_count(), 0);
    }

    /// `from_patterns` takes what it is given and fills the rest in, so a
    /// snapshot of the wrong length cannot make a model of the wrong size
    #[test]
    fn data_is_always_sixteen_patterns_however_many_arrive() {
        let mut odd = Pattern::new();
        odd.set_rate(Rate::ThirtySecond);

        let short = StutterData::from_patterns([odd.clone()]);
        assert_eq!(short.pattern(0), Some(&odd));
        assert_eq!(short.pattern(1), Some(&Pattern::new()));
        assert_eq!(short.patterns().len(), PATTERN_COUNT);

        let long = StutterData::from_patterns((0..100).map(|_| odd.clone()));
        assert_eq!(long.patterns().len(), PATTERN_COUNT);
        assert!(long.patterns().iter().all(|pattern| *pattern == odd));
    }

    /// The point of the whole file: a write is visible through
    /// `SharedCell::get` afterwards — and only through a *new* snapshot. The
    /// one a reader already holds is the one it keeps, which is what lets
    /// the audio thread read a block without a lock.
    #[test]
    fn a_write_publishes_a_new_snapshot_and_leaves_the_old_one_alone() {
        let model = StutterModel::default();
        let before = model.shared();

        assert!(model.set_rate(0, Rate::EighthTriplet));

        assert_eq!(before.pattern(0).map(Pattern::rate), Some(Rate::DEFAULT));
        assert_eq!(model.shared().pattern(0).map(Pattern::rate), Some(Rate::EighthTriplet));
        assert_eq!(model.edit_count(), 1);
    }

    /// Every writer the model has, each one read back through the published
    /// snapshot.
    #[test]
    fn the_whole_writer_surface_reaches_the_published_snapshot() {
        let model = StutterModel::default();

        assert!(model.set_rate(3, Rate::Eighth));
        assert_eq!(model.set_length(3, 8), Some(8));
        assert!(model.set_volume(3, 2, 0.25));

        let eighth = NoteValue::Eighth;
        let repeat = Action::repeat(eighth).with_source(Source::Spot { step: 3, passes: 0 });
        assert_eq!(model.add_block(3, 0, 2, Action::default()), Some(0));
        assert_eq!(model.add_block(3, 4, 2, repeat), Some(1));
        assert_eq!(model.resize_block(3, 0, 3), Some(3));
        assert_eq!(model.move_block(3, 0, 1), Some(1));
        assert!(model.set_block_action(3, 0, Action::from_preset(SpeedPreset::TapeStop)));
        assert_eq!(model.set_block_span(3, 1, 5, 2), Some((5, 2)));
        assert_eq!(model.duplicate_block(3, 1), Some(2));
        assert!(model.delete_block(3, 2));

        let pattern = published(&model, 3);
        assert_eq!(pattern.rate(), Rate::Eighth);
        assert_eq!(pattern.length(), 8);
        assert_eq!(pattern.volume(2), 0.25);
        assert_eq!(pattern.blocks().len(), 2);
        assert_eq!((pattern.blocks()[0].start(), pattern.blocks()[0].len()), (1, 3));
        assert_eq!(pattern.blocks()[0].action(), Action::from_preset(SpeedPreset::TapeStop));
        assert_eq!((pattern.blocks()[1].start(), pattern.blocks()[1].len()), (5, 2));
        assert_eq!(pattern.blocks()[1].action(), repeat);

        // Only pattern 3 moved: a write names the pattern it is about.
        for index in (0..PATTERN_COUNT).filter(|&index| index != 3) {
            assert_eq!(published(&model, index), Pattern::new(), "pattern {index}");
        }
    }

    /// Out-of-range arguments come back clamped, through the model's own
    /// setters rather than only through [`Pattern`]'s — the GUI writes here.
    #[test]
    fn every_writer_clamps_what_it_is_given() {
        let model = StutterModel::default();

        assert_eq!(model.set_length(0, 0), Some(1));
        assert_eq!(model.set_length(0, 9_999), Some(Pattern::MAX_LENGTH));

        assert!(model.set_volume(0, 0, 4.0));
        assert!(model.set_volume(0, 1, -1.0));
        assert!(model.set_volume(0, 2, f32::NAN));
        assert_eq!(published(&model, 0).volume(0), 1.0);
        assert_eq!(published(&model, 0).volume(1), 0.0);
        assert_eq!(published(&model, 0).volume(2), 0.0);

        // Truncated to the row, padded with unity, every entry clamped.
        assert!(model.set_volumes(0, (0..MAX_STEPS + 8).map(|step| step as f32 / 8.0)));
        let pattern = published(&model, 0);
        assert_eq!(pattern.volume(0), 0.0);
        assert_eq!(pattern.volume(4), 0.5);
        assert_eq!(pattern.volume(MAX_STEPS as u16 - 1), 1.0);

        // A block is cut to the pattern, and a speed to ±2.
        assert_eq!(model.add_block(0, 60, 99, Action::ramp(-9.0, 9.0)), Some(0));
        let block = published(&model, 0).blocks()[0];
        assert_eq!((block.start(), block.len()), (60, 4));
        assert_eq!(block.action(), Action { from: -2.0, to: 2.0, ..Action::ramp(1.0, 1.0) });

        // A step past the end of the pattern lands on its last step.
        assert_eq!(model.set_length(1, 4), Some(4));
        assert_eq!(model.add_block(1, 50, 1, Action::default()), Some(0));
        assert_eq!(published(&model, 1).blocks()[0].start(), 3);
    }

    /// A write that has nowhere to go publishes nothing: the snapshot a
    /// reader holds is still the current one afterwards.
    #[test]
    fn a_refused_write_publishes_nothing() {
        let model = StutterModel::default();
        assert_eq!(model.add_block(0, 0, 4, Action::default()), Some(0));
        // A pattern with no room left in it, for the duplicate below.
        assert_eq!(model.set_length(1, 4), Some(4));
        assert_eq!(model.add_block(1, 0, 4, Action::default()), Some(0));

        let unchanged = (*model.shared()).clone();

        // No such pattern.
        assert!(!model.set_rate(PATTERN_COUNT, Rate::Eighth));
        assert_eq!(model.set_length(PATTERN_COUNT, 4), None);
        assert!(!model.set_volume(PATTERN_COUNT, 0, 0.5));
        assert!(!model.set_volumes(PATTERN_COUNT, [0.5]));
        assert_eq!(model.add_block(PATTERN_COUNT, 0, 1, Action::default()), None);
        assert!(!model.clear_pattern(PATTERN_COUNT));

        // No such block, and a step that is covered already.
        assert_eq!(model.add_block(0, 2, 1, Action::default()), None);
        assert_eq!(model.resize_block(0, 7, 2), None);
        assert_eq!(model.move_block(0, 7, 2), None);
        assert_eq!(model.set_block_span(0, 7, 2, 2), None);
        assert!(!model.set_block_action(0, 7, Action::default()));
        assert!(!model.delete_block(0, 7));

        // A block filling its pattern has no room after it.
        assert_eq!(model.duplicate_block(1, 0), None);

        assert_eq!(*model.shared(), unchanged);
    }

    /// The row cannot be grown past [`MAX_BLOCKS`], however hard a writer
    /// pushes — the bound hostile state and a held-down key share.
    #[test]
    fn the_block_row_stops_at_its_limit() {
        let model = StutterModel::default();
        assert_eq!(model.set_length(0, Pattern::MAX_LENGTH), Some(Pattern::MAX_LENGTH));

        for step in 0..Pattern::MAX_LENGTH {
            assert!(model.add_block(0, step, 1, Action::default()).is_some(), "step {step}");
        }
        assert_eq!(published(&model, 0).blocks().len(), MAX_BLOCKS);
        assert_eq!(model.duplicate_block(0, 0), None);
    }

    /// Copy takes the whole pattern over, and refuses the two cases that
    /// are not a copy.
    #[test]
    fn copy_pattern_replaces_the_target_and_refuses_a_non_copy() {
        let model = StutterModel::default();

        assert!(model.set_rate(0, Rate::ThirtySecond));
        assert_eq!(model.set_length(0, 12), Some(12));
        assert!(model.set_volume(0, 1, 0.5));
        assert_eq!(model.add_block(0, 3, 2, Action::from_preset(SpeedPreset::Reverse)), Some(0));

        // The target starts out different, so a copy that did nothing would
        // show.
        assert_eq!(model.set_length(9, 4), Some(4));
        assert!(model.copy_pattern(0, 9));
        assert_eq!(published(&model, 9), published(&model, 0));

        let before = model.data();
        assert!(!model.copy_pattern(0, 0));
        assert!(!model.copy_pattern(0, PATTERN_COUNT));
        assert!(!model.copy_pattern(PATTERN_COUNT, 0));
        assert_eq!(model.data(), before);
    }

    /// Clear takes what was drawn off the grid and leaves the grid.
    #[test]
    fn clear_pattern_keeps_the_rate_and_the_length() {
        let model = StutterModel::default();

        assert!(model.set_rate(2, Rate::SixteenthTriplet));
        assert_eq!(model.set_length(2, 24), Some(24));
        assert!(model.set_volume(2, 0, 0.0));
        assert_eq!(model.add_block(2, 0, 4, Action::default()), Some(0));

        assert!(model.clear_pattern(2));

        let pattern = published(&model, 2);
        assert_eq!(pattern.rate(), Rate::SixteenthTriplet);
        assert_eq!(pattern.length(), 24);
        assert!(pattern.blocks().is_empty());
        assert!(pattern.volumes().iter().all(|&volume| volume == Pattern::DEFAULT_VOLUME));
    }

    /// `install` is the state-load and undo door: it replaces everything,
    /// with no editor anywhere in it.
    #[test]
    fn install_publishes_a_whole_replacement_headless() {
        let model = StutterModel::default();
        assert!(model.set_rate(0, Rate::Eighth));

        let mut loaded = Pattern::new();
        loaded.set_rate(Rate::ThirtySecond);
        loaded.set_length(7);
        let restored = StutterData::from_patterns((0..PATTERN_COUNT).map(|_| loaded.clone()));

        let before = model.shared();
        model.install(restored.clone());

        assert_eq!(*model.shared(), restored);
        assert_eq!(before.pattern(0).map(Pattern::rate), Some(Rate::Eighth));
    }

    /// `data()` is the save side of that door, and it is a copy: editing the
    /// model afterwards does not reach into what was saved.
    #[test]
    fn data_hands_out_a_copy_of_the_published_patterns() {
        let model = StutterModel::default();
        assert_eq!(model.add_block(5, 2, 2, Action::default()), Some(0));

        let saved = model.data();
        assert!(model.clear_pattern(5));

        assert_eq!(saved.pattern(5).map(|pattern| pattern.blocks().len()), Some(1));
        assert_eq!(published(&model, 5).blocks().len(), 0);
        assert_eq!(model.data(), *model.shared());
    }

    /// Writers reclaim what they retire even with no editor open, and a
    /// reader's snapshot is never freed under it: the alloc count is one
    /// while nobody holds an old one, and one more for each that is still
    /// held.
    #[test]
    fn retired_snapshots_are_reclaimed_but_never_while_read() {
        let model = StutterModel::default();
        assert_eq!(model.live_allocs(), 1);

        let held = model.shared();
        for step in 0..8 {
            assert!(model.set_volume(0, step, 0.5));
        }

        // The eight writes each published and collected; what is left is the
        // current snapshot plus the one `held` keeps alive.
        assert_eq!(model.live_allocs(), 2);

        drop(held);
        model.collect();
        assert_eq!(model.live_allocs(), 1);
    }

    /// Two writers and a reader at once: no edit is lost (each thread's
    /// writes all land), the reader only ever sees whole patterns, and
    /// nothing leaks once they stop.
    #[test]
    fn concurrent_writers_serialise_and_a_reader_sees_whole_patterns() {
        const WRITES: u16 = 200;

        let model = Arc::new(StutterModel::default());
        assert_eq!(model.set_length(0, 4), Some(4));
        assert_eq!(model.add_block(0, 0, 4, Action::default()), Some(0));

        let writers = [1usize, 2].map(|pattern| {
            let model = Arc::clone(&model);
            thread::spawn(move || {
                for write in 0..WRITES {
                    model.set_volume(pattern, write % MAX_STEPS as u16, 0.5);
                    model.set_rate(pattern, Rate::ALL[write as usize % Rate::ALL.len()]);
                }
            })
        });

        // Meanwhile the "audio thread": one `get` per block, read whole.
        for _ in 0..2_000 {
            let data = model.shared();
            let pattern = data.pattern(0).expect("pattern 0");
            assert_eq!(pattern.length(), 4);
            assert_eq!(pattern.blocks().len(), 1);
            assert_eq!(pattern.blocks()[0].len(), 4);
        }

        for writer in writers {
            writer.join().expect("a writer panicked");
        }

        // 2 setup writes + two threads of 2 writes each, none lost.
        assert_eq!(model.edit_count(), 2 + 2 * 2 * u64::from(WRITES));
        model.collect();
        assert_eq!(model.live_allocs(), 1);
        for pattern in [1, 2] {
            assert_eq!(published(&model, pattern).volume(0), 0.5, "pattern {pattern}");
        }
    }

    // ---- undo and redo ----

    /// A model with a committed starting point, the way the editor leaves it
    /// after its first frame: sixteen default patterns and an empty history.
    fn committed_model() -> StutterModel {
        let model = StutterModel::default();
        model.commit_edits();
        assert!(!model.can_undo() && !model.can_redo(), "nothing to step through yet");

        model
    }

    /// One commit per edit, then back and forward through them one at a time.
    #[test]
    fn undo_and_redo_step_through_committed_edits() {
        let model = committed_model();
        assert_eq!(model.set_length(0, 8), Some(8));
        model.commit_edits();
        assert_eq!(model.add_block(0, 2, 4, Action::default()), Some(0));
        model.commit_edits();
        assert!(model.can_undo() && !model.can_redo());

        assert!(model.undo());
        assert!(published(&model, 0).blocks().is_empty(), "the block is gone again");
        assert_eq!(published(&model, 0).length(), 8, "but not the length before it");
        assert!(model.undo());
        assert_eq!(published(&model, 0).length(), Pattern::new().length());
        assert!(!model.undo(), "nothing before the starting point");
        assert!(!model.can_undo());

        assert!(model.redo());
        assert_eq!(published(&model, 0).length(), 8);
        assert!(model.redo());
        assert_eq!(published(&model, 0).blocks().len(), 1);
        assert!(!model.redo(), "nothing left to redo");
    }

    /// The gesture rule: a drag writes per pixel and undoes as one step.
    #[test]
    fn a_whole_gesture_is_one_undo_step() {
        let model = committed_model();

        // A stroke across the Volume row: eight writes, no commit between.
        for step in 0..8 {
            assert!(model.set_volume(0, step, 0.25));
        }
        model.commit_edits();
        assert_eq!(model.edit_count(), 8, "every write counted");

        assert!(model.undo());
        for step in 0..8 {
            assert_eq!(published(&model, 0).volume(step), 1.0, "step {step} came back");
        }
        assert!(!model.undo(), "the eight writes were one step");
    }

    /// An uncommitted gesture is committed by the undo itself, so Ctrl+Z
    /// right after a drag undoes that drag.
    #[test]
    fn an_uncommitted_edit_is_committed_by_undo() {
        let model = committed_model();
        assert!(model.set_rate(0, Rate::Eighth));

        assert!(model.undo());
        assert_eq!(published(&model, 0).rate(), Pattern::new().rate());
        assert!(model.redo());
        assert_eq!(published(&model, 0).rate(), Rate::Eighth);
    }

    /// Undo covers every writer, including the two that touch a whole
    /// pattern at once.
    #[test]
    fn undo_covers_pattern_copies_and_clears() {
        let model = committed_model();
        assert_eq!(model.set_length(0, 32), Some(32));
        assert_eq!(model.add_block(0, 0, 8, Action::default()), Some(0));
        model.commit_edits();
        assert!(model.copy_pattern(0, 3));
        model.commit_edits();
        assert!(model.clear_pattern(0));
        model.commit_edits();

        assert!(model.undo());
        assert_eq!(published(&model, 0).blocks().len(), 1, "the clear is undone");
        assert!(model.undo());
        assert_eq!(published(&model, 3), Pattern::new(), "the copy is undone");
        assert_eq!(published(&model, 0).blocks().len(), 1, "and only the copy");
    }

    /// A new edit after an undo is a new branch: the redo stack goes.
    #[test]
    fn an_edit_after_undo_clears_the_redo() {
        let model = committed_model();
        assert_eq!(model.set_length(0, 8), Some(8));
        model.commit_edits();
        assert!(model.undo());
        assert!(model.can_redo());

        assert_eq!(model.set_length(0, 12), Some(12));
        model.commit_edits();
        assert!(!model.can_redo(), "the branch that is gone cannot be redone");
        assert!(!model.redo());
        assert_eq!(published(&model, 0).length(), 12);
        assert!(model.undo(), "and the new edit is still one step back");
        assert_eq!(published(&model, 0).length(), Pattern::new().length());
    }

    /// A write that lands the value already there bumps the edit count, and
    /// still records no step — nothing changed to step back out of.
    #[test]
    fn a_write_of_the_same_value_is_not_an_undo_step() {
        let model = committed_model();
        assert!(model.set_volume(0, 0, 1.0));
        model.commit_edits();

        assert_eq!(model.edit_count(), 1);
        assert!(!model.can_undo());
        assert!(!model.undo());
    }

    /// Past the depth cap the oldest step is dropped, so a long session's
    /// history stays bounded.
    #[test]
    fn the_depth_cap_drops_the_oldest_step() {
        let model = committed_model();
        let extra = 10u16;
        for edit in 1..=(UNDO_DEPTH as u16 + extra) {
            assert!(model.set_volume(0, 0, f32::from(edit) / 128.0));
            model.commit_edits();
        }

        let mut undone = 0;
        while model.undo() {
            undone += 1;
        }
        assert_eq!(undone, UNDO_DEPTH, "exactly the cap, and no more");
        assert_eq!(
            published(&model, 0).volume(0),
            f32::from(extra) / 128.0,
            "the ten oldest steps were dropped, so the oldest state left is the tenth",
        );
    }

    /// What a host project load does: the patterns it replaced are not an
    /// edit to step back into.
    #[test]
    fn clearing_the_history_forgets_both_stacks() {
        let model = committed_model();
        assert_eq!(model.set_length(0, 8), Some(8));
        model.commit_edits();
        assert_eq!(model.set_length(0, 24), Some(24));
        model.commit_edits();
        assert!(model.undo());
        assert!(model.can_undo() && model.can_redo());

        model.clear_history();
        assert!(!model.can_undo() && !model.can_redo());
        assert!(!model.undo() && !model.redo());
        assert_eq!(published(&model, 0).length(), 8, "and nothing moved");

        // The next commit only takes a fresh baseline, from here on.
        assert_eq!(model.set_length(0, 16), Some(16));
        model.commit_edits();
        assert!(model.undo());
        assert_eq!(published(&model, 0).length(), 8);
    }

    /// Stepping back and forth republishes, and every copy that retires is
    /// reclaimed: an undo is a write like any other.
    #[test]
    fn undo_does_not_leak_the_data_it_replaces() {
        let model = committed_model();
        for step in 0..16 {
            assert!(model.set_volume(0, step, 0.5));
            model.commit_edits();
        }
        while model.undo() {}
        while model.redo() {}

        model.collect();
        assert_eq!(model.live_allocs(), 1);
        assert_eq!(published(&model, 0).volume(15), 0.5, "forward again at the newest");
    }

    /// The history is behind its own mutex and the model is `Sync`, so undo
    /// is callable from the GUI thread while the audio thread reads — the
    /// lock order (history, then writer) is the same everywhere.
    #[test]
    fn undo_runs_next_to_a_reader() {
        let model = Arc::new(StutterModel::default());
        model.commit_edits();

        let reader = {
            let model = Arc::clone(&model);
            thread::spawn(move || {
                for _ in 0..2_000 {
                    let data = model.shared();
                    assert!(data.pattern(0).expect("pattern 0").length() >= 1);
                }
            })
        };

        for length in 1..=64u16 {
            assert!(model.set_length(0, length).is_some());
            model.commit_edits();
        }
        for _ in 0..64 {
            model.undo();
        }
        reader.join().expect("the reader panicked");
        assert_eq!(published(&model, 0).length(), Pattern::new().length());
    }
}
