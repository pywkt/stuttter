//! The recorded past: a stereo ring buffer the plugin reads itself out of.
//!
//! Everything stuttter does reduces to recording the input and choosing how
//! far behind "now" to read it. This is the recording half. It records
//! **every** input frame, always — transport stopped or playing, pattern Off
//! or not — so that a block which starts on the next step already has its
//! material behind it, and so that nothing on the audio path ever has to
//! decide whether to start recording.
//!
//! Positions are **absolute frame indices**, counted from the first frame
//! ever recorded: frame 0 is that first frame and [`History::recorded`] is
//! one *past* the newest — so the newest, `recorded - 1`, is the frame
//! `d = 0` (live) reads, and `recorded` itself is already the future and
//! reads silence. Those are the positions the engine's read formula produces
//! (`now − d × 60 / tempo × sample_rate`), and counting absolutely rather
//! than backwards from the write head is what lets a fractional read ask for
//! the four frames around a position without having to know where the ring's
//! seam is. A position outside what the buffer holds — past the newest
//! frame, further back than its capacity, or before the first frame ever
//! recorded — reads **silence**, not a wrapped neighbour and not the edge
//! frame: a read that has run off the end of the recorded past is
//! the one case where this plugin has nothing to play.
//!
//! Reads come in two kinds. [`History::frame`] answers a whole position
//! exactly; [`History::read`] answers a fractional one with **4-point cubic
//! Hermite (Catmull-Rom)** interpolation, chopdrop's `voices.rs` polynomial
//! ported over, because every action but "live" reads at a position the
//! tempo conversion lands between frames.
//!
//! The purity rule holds here: no nice-plug types, nothing of the framework
//! — the buffer is handed a sample rate in `initialize()` and per-frame
//! samples in `process()`, and is unit-tested without either. Every method
//! the audio thread calls ([`History::record`], [`History::frame`],
//! [`History::read`]) is arithmetic, branches and indexing over memory that
//! was allocated once, by [`History::new`], off the audio thread.

use std::f32::consts::FRAC_PI_2;

/// How far back the buffer can be read, in seconds.
///
/// Generous on purpose: a block can anchor up to a bar before itself,
/// continue from the block before it, and run backwards at double speed, so
/// offsets of several bars are ordinary — 64 s is 16 bars of 4/4 at 60 BPM.
/// A read that does reach the far end fades out (`engine.rs`) rather than
/// clicking. The cost is ~25 MB at 48 kHz and ~98 MB at 192 kHz, allocated
/// once in `activate()`.
pub const MAX_LOOKBACK_SECONDS: f64 = 64.0;

/// The most frames one channel of the buffer will ever be sized to:
/// [`MAX_LOOKBACK_SECONDS`] at 768 kHz, four times the highest rate any host
/// offers.
///
/// The sample rate arrives from the host, so it is clamped like every other
/// host number: a nonsense rate sizes the buffer to something harmless
/// instead of asking the allocator for terabytes.
const MAX_CAPACITY_FRAMES: usize = 768_000 * 64;

/// A stereo ring buffer of the recent input, read by absolute frame index.
///
/// The two channels are kept as two slices of one fixed length: the frame at
/// absolute position `p` lives at index `p % capacity` in each of them, which
/// is why there is no write pointer to keep in step — the count of frames
/// recorded so far *is* the write position.
pub struct History {
    /// The left channel, one slot per frame of capacity.
    left: Box<[f32]>,
    /// The right channel, the same length as [`History::left`].
    right: Box<[f32]>,
    /// How many frames have been recorded since construction (or the last
    /// [`History::reset`]) — the absolute position the next frame is written
    /// at, and one past the newest readable position.
    recorded: u64,
}

impl History {
    /// A buffer sized to hold [`MAX_LOOKBACK_SECONDS`] at `sample_rate`,
    /// with nothing recorded in it yet.
    ///
    /// This is the one allocating call, made from the plugin's
    /// `initialize()`. The rate is clamped NaN-safely (`max` before `min`, so
    /// a NaN falls to the low end rather than through both tests) into one
    /// frame … [`MAX_CAPACITY_FRAMES`]: a host that reports 0, a negative
    /// rate or nothing at all gets a buffer that reads silence, not a panic.
    pub fn new(sample_rate: f32) -> Self {
        let frames = (f64::from(sample_rate) * MAX_LOOKBACK_SECONDS)
            .ceil()
            .max(1.0)
            .min(MAX_CAPACITY_FRAMES as f64);

        Self::with_capacity(frames as usize)
    }

    /// A buffer of exactly `frames` frames per channel, clamped into one …
    /// [`MAX_CAPACITY_FRAMES`].
    ///
    /// What [`History::new`] is written over, and what a test that wants to
    /// watch the ring wrap in a handful of frames asks for directly.
    pub fn with_capacity(frames: usize) -> Self {
        let frames = frames.clamp(1, MAX_CAPACITY_FRAMES);

        Self {
            left: vec![0.0; frames].into_boxed_slice(),
            right: vec![0.0; frames].into_boxed_slice(),
            recorded: 0,
        }
    }

    /// How many frames of one channel the buffer holds.
    pub fn capacity(&self) -> usize {
        self.left.len()
    }

    /// How many frames have been recorded since construction — the absolute
    /// position the next [`History::record`] writes at, and one past the
    /// newest position [`History::frame`] will answer.
    pub fn recorded(&self) -> u64 {
        self.recorded
    }

    /// The oldest absolute position still in the buffer.
    ///
    /// Zero until the ring has filled once, then the newest position minus
    /// the capacity: a read below this one has fallen out the back and is
    /// silence.
    pub fn oldest(&self) -> u64 {
        self.recorded.saturating_sub(self.capacity() as u64)
    }

    /// Record one input frame, overwriting the oldest one when the ring is
    /// full.
    ///
    /// Called for every frame of every block, from the audio thread: two
    /// stores and an increment, no branch on how much is recorded. The count
    /// saturates instead of wrapping, which at 192 kHz is a few million years
    /// away and is here only so that nothing on this path can panic in a
    /// debug build.
    pub fn record(&mut self, left: f32, right: f32) {
        let index = (self.recorded % self.capacity() as u64) as usize;

        self.left[index] = left;
        self.right[index] = right;
        self.recorded = self.recorded.saturating_add(1);
    }

    /// Whether the buffer holds the frame at absolute `position`.
    ///
    /// False covers all three ways a read can be outside the recorded past,
    /// and they are different questions, not one: `position` is negative or
    /// below [`History::oldest`] (before the first frame ever recorded, or
    /// further back than the capacity — both of those are silent rather than
    /// wrapped), or it is at or past [`History::recorded`], which would be
    /// reading the future the plugin promises never to read.
    ///
    /// Signed, because an interpolated read asks for the frame *before* the
    /// one it is centred on, which at the very start of a project is position
    /// −1.
    pub fn holds(&self, position: i64) -> bool {
        position >= 0 && (position as u64) >= self.oldest() && (position as u64) < self.recorded
    }

    /// The frame at absolute position `position`, or silence if the buffer
    /// does not [`hold`](History::holds) it.
    pub fn frame(&self, position: i64) -> (f32, f32) {
        if !self.holds(position) {
            return (0.0, 0.0);
        }

        let index = (position as u64 % self.capacity() as u64) as usize;
        (self.left[index], self.right[index])
    }

    /// The frame at **fractional** absolute position `position`, cubic-
    /// Hermite interpolated, or silence if the buffer does not
    /// [`hold`](History::holds) the frame the position falls in.
    ///
    /// This is the read the engine actually uses: an offset of `d` beats is
    /// `d × 60 / tempo × sample_rate` frames, which lands between frames for
    /// every tempo and every speed but the exact identities, and reading it
    /// rounded would alias the pitch shift that reading at a changing rate
    /// *is*.
    ///
    /// Next to the newest recorded frame, where the fourth tap does not exist
    /// yet, the read is linear between the two frames either side.
    ///
    /// The four frames around the position come from [`History::frame`], so
    /// the window's edges interpolate against **silence** rather than against
    /// a wrapped neighbour — chopdrop's `interpolate` clamps to the edge frame
    /// there, which is right for a sample file with a last frame and wrong
    /// here, where running off the recorded past means there is nothing to
    /// play. Inside the window the seam of the ring is invisible: positions
    /// are absolute, so the four taps are the four frames that were recorded
    /// in that order whether or not the modulo wrapped between them.
    ///
    /// A position that is not a number reads silence, like one outside the
    /// window: nothing on this path may panic on a number that came, through
    /// the tempo, from the host.
    pub fn read(&self, position: f64) -> (f32, f32) {
        if position.is_nan() {
            return (0.0, 0.0);
        }

        let base = position.floor();
        // The float-to-int cast saturates rather than wrapping or going
        // undefined, so an absurd position lands on `i64::MIN`/`i64::MAX` and
        // `holds` turns it into silence below.
        let index = base as i64;
        if !self.holds(index) {
            return (0.0, 0.0);
        }

        let t = (position - base) as f32;
        // Saturating too, so that nothing here can overflow in a debug build
        // even if `recorded` had somehow reached its own saturation point.
        let at = |offset: i64| self.frame(index.saturating_add(offset));
        let ((l0, r0), (l1, r1)) = (at(-1), at(0));
        let (l2, r2) = at(1);

        // Within two frames of the newest one the fourth tap has not been
        // recorded yet. A cubic fitted through that silence overshoots by a
        // fraction of the sample, so the newest edge is read linearly instead
        // — exact on a ramp, and only ever for an offset under two frames.
        if index.saturating_add(2) >= self.recorded.min(i64::MAX as u64) as i64 {
            return (l1 + (l2 - l1) * t, r1 + (r2 - r1) * t);
        }

        let (l3, r3) = at(2);
        (hermite(l0, l1, l2, l3, t), hermite(r0, r1, r2, r3, t))
    }

    /// Forget everything recorded: the buffer is as it was built, so every
    /// read is silence again until frames arrive.
    ///
    /// What the plugin's `reset()` wants — the host has told us the audio
    /// before this point is not continuous with what comes next, and a block
    /// that stitched across the seam would play material from somewhere else
    /// in the song. Does not allocate, and does not change the capacity.
    pub fn reset(&mut self) {
        self.left.fill(0.0);
        self.right.fill(0.0);
        self.recorded = 0;
    }
}

/// One channel interpolated between `y1` and `y2` at fraction `t`, from the
/// four frames `y0 … y3` around it: **cubic Hermite (Catmull-Rom)**, copied
/// from chopdrop's `voices.rs`.
///
/// The polynomial is written as `((a·t + b)·t + c)·t + y1`, which is not
/// just Horner's rule being tidy: the constant term *is* `y1`, so `t = 0`
/// returns the recorded frame bit for bit, with no rounding to accumulate.
/// That is what makes the exact identities of the offset maths — double
/// speed ending on live, a whole-frame repeat wrap — come out exact rather
/// than nearly so. A straight line through the interpolator is still that
/// line, too: on evenly spaced samples of one, `a` and `b` both come out
/// zero and all that is left is `c·t + y1` — which is what lets the tests'
/// ramp trick read an action's offset straight off its output.
fn hermite(y0: f32, y1: f32, y2: f32, y3: f32, t: f32) -> f32 {
    let a = 0.5 * (-y0 + 3.0 * y1 - 3.0 * y2 + y3);
    let b = 0.5 * (2.0 * y0 - 5.0 * y1 + 4.0 * y2 - y3);
    let c = 0.5 * (-y0 + y2);
    ((a * t + b) * t + c) * t + y1
}

/// How many output frames a fade of `milliseconds` lasts at `sample_rate`:
/// 144 frames for the default 3 ms at 48 kHz.
///
/// Rounded **up**, and never zero: a fade of no frames is a cut, which is the
/// click the fade exists to avoid. Both arguments are guarded the way every
/// host number is — `max` before `min`, so a NaN falls to the low end rather
/// than through both tests — and the result is capped at a second's worth of
/// frames at the highest rate the clamp admits, because a crossfade longer
/// than that is not a crossfade and the cast must land somewhere deliberate.
pub fn fade_frames_for(milliseconds: f32, sample_rate: f64) -> u32 {
    // The lint wants `clamp`; the order here *is* the NaN guard, see above.
    #[allow(clippy::manual_clamp)]
    let rate = sample_rate.max(1.0).min(MAX_SAMPLE_RATE);
    #[allow(clippy::manual_clamp)]
    let milliseconds = f64::from(milliseconds).max(0.0).min(1000.0);

    ((milliseconds / 1000.0) * rate).ceil().clamp(1.0, MAX_FADE_FRAMES as f64) as u32
}

/// The highest sample rate a host number is believed at: 768 kHz, four times
/// the highest rate any host offers, and the rate this module's own capacity
/// cap is sized at.
///
/// Shared rather than redefined per module, so "a believable sample rate"
/// means one thing across the crate: [`fade_frames_for`] clamps to it, and
/// `engine.rs` reads a transport reporting more than it as unusable.
pub const MAX_SAMPLE_RATE: f64 = 768_000.0;

/// The longest fade [`fade_frames_for`] will return: one second at
/// [`MAX_SAMPLE_RATE`].
const MAX_FADE_FRAMES: u32 = 768_000;

/// One read head: where in the recorded past it is, and how fast it is
/// moving through it.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Head {
    /// Its fractional absolute position — what [`History::read`] takes.
    position: f64,
    /// Frames of recorded past per output frame: the step it took last.
    ///
    /// Not a speed the head is told. The engine drives the live head by
    /// position (it has the offset `d` and the tempo, and that product is the
    /// position), so the distance between two consecutive driven positions is
    /// the only place a speed is written down — and it is the speed the head
    /// has to keep going at once it is the one fading out.
    rate: f64,
}

impl Head {
    /// The same head one output frame later, at the speed it already had.
    fn advanced(self) -> Self {
        Self { position: self.position + self.rate, rate: self.rate }
    }
}

/// A running crossfade: the head being faded out, and how far along the ramp
/// the fade is.
#[derive(Clone, Copy, Debug)]
struct Fade {
    /// The outgoing head — the only one in this module that moves itself.
    out: Head,
    /// Output frames of this fade already rendered, `0 ..= frames`.
    ///
    /// Counted in frames rather than carried as a gain, for the reason
    /// chopdrop's `FadeState` counts them: "the fade is over" is then an
    /// integer fact, not the far end of a float accumulation, which is what
    /// lets a test assert a fade's length as a number of samples.
    rendered: u32,
    /// This fade's length in output frames; never zero.
    ///
    /// The fade carries its own length so that [`Reader::set_fade_frames`] —
    /// the Smoothing parameter, which a host can automate — cannot leave
    /// `rendered` stranded past the end of a shortened ramp. A Smoothing
    /// change lands on the next fade that starts from nothing.
    frames: u32,
}

/// Two read heads over a [`History`], crossfading on every jump.
///
/// The offset the engine reads at jumps at most block starts and ends, every
/// repeat wrap, every pattern change and every transport jump. Read straight
/// through, each of those is a step in the output, which is a click. The
/// policy is two heads: on a jump the old head keeps going at its own speed
/// and fades out while the new one fades in — **equal-power**, over the
/// Smoothing parameter's milliseconds ([`fade_frames_for`]) — and then the
/// old head stops.
///
/// The engine drives one head: it hands [`Reader::read`] the position it
/// computed for this output frame, having called [`Reader::jump`] first if
/// that position is a jump rather than a step. The other head is the one
/// fading out, and it is the only one that moves itself — "its own speed" is
/// the step the driven head took last, held for the rest of the fade.
///
/// Equal-power is `cos` and `sin` of `π/2 × rendered / frames`: the two gains
/// square to one at every point of the ramp, so uncorrelated material keeps
/// its loudness across the fade, and both ends of the ramp are flat (the
/// derivative of `sin` at 0 is what the first frame of a fade-in steps by, and
/// a cosine ramp starts there at zero slope). The ends are exact: a fade that
/// has rendered no frames is the outgoing head bit for bit, and a finished
/// fade is gone rather than left multiplying by something near one.
///
/// # A jump during a fade
///
/// There is **never a third head**: a jump arriving while a fade still runs
/// restarts the fade *from the current mix*, which means one of the two heads
/// in flight is dropped. The one dropped is the **quieter** of the two, and
/// the survivor keeps the gain it already had:
///
/// - **Early in the ramp** (`rendered ≤ frames / 2`), the head fading in is
///   the quieter one, so the new position replaces *it*: the ramp and the head
///   it is fading out carry on untouched, and the new position arrives at the
///   gain the head it replaced was at.
/// - **Late in the ramp, and when no fade is running at all**, the outgoing
///   head is the quieter one (or there is none). It is dropped, the head that
///   was fading in becomes the one fading out, and `rendered` is mirrored
///   about the middle of the ramp — `frames − rendered` — which is exactly the
///   point where `cos` hands the survivor the gain its `sin` had. For an idle
///   reader (`rendered = frames`, no fade) that mirror is `0`: a whole fade
///   from the start, which is the ordinary case.
///
/// Either way the envelope is continuous — the two gains still square to one
/// across the restart — and the only step left in the output is the dropped
/// head's own gain times the difference between what it was reading and what
/// took its place. Dropping the quieter head is what bounds that step: its
/// gain is at most `√2/2`, and that only for a jump landing exactly halfway
/// through a fade. Two heads cannot do better than that bound, and the
/// engine uses two heads.
///
/// Nothing here allocates, locks or branches on anything but its own state:
/// every method is called per output frame from the audio thread, and the
/// two heads are two fixed slots, not a pool.
pub struct Reader {
    /// The head the engine drives, as of the last [`Reader::read`]; `None`
    /// before the first one and after a [`Reader::reset`], when there is no
    /// position to continue from and so nothing a jump could fade out.
    live: Option<Head>,
    /// The running fade, if any. One slot: this is the "never a third head"
    /// invariant, held by the type rather than by a check.
    fade: Option<Fade>,
    /// The length the next fade started from nothing will take, in output
    /// frames; never zero. [`Reader::set_fade_frames`] writes it.
    length: u32,
    /// Whether [`Reader::jump`] has been called since the last read — i.e.
    /// whether the next position is a jump rather than a step.
    jumping: bool,
}

impl Reader {
    /// A reader with no heads running yet, whose fades will take
    /// `fade_frames` output frames (clamped to at least one — a fade of no
    /// frames is the cut it exists to avoid).
    pub fn new(fade_frames: u32) -> Self {
        Self { live: None, fade: None, length: fade_frames.max(1), jumping: false }
    }

    /// Set the length of fades **started from now on**, in output frames.
    ///
    /// A running fade keeps the length it started with: Smoothing is a host
    /// parameter and may be automated mid-fade, and re-scaling a ramp that is
    /// halfway through would move the gains it is holding — a step, in the one
    /// place whose whole job is not to have one.
    pub fn set_fade_frames(&mut self, frames: u32) {
        self.length = frames.max(1);
    }

    /// The length the next fade will take, in output frames.
    pub fn fade_frames(&self) -> u32 {
        self.length
    }

    /// Tell the reader that the **next** [`Reader::read`] position is a jump,
    /// not a step: start a fade, or restart the running one.
    ///
    /// The fade starts here rather than on that read, so that a reader asked
    /// how many heads it is running answers for the jump as soon as the jump
    /// is known — and so that a jump declared at the very end of a block,
    /// with no frame left to render, is still faded by the next block.
    ///
    /// The engine knows where the jumps are — block edges, repeat wraps,
    /// pattern changes, transport discontinuities — so they are declared
    /// rather than guessed from the size of a position change, which at a
    /// reverse's speed is indistinguishable from an ordinary step.
    pub fn jump(&mut self) {
        self.jumping = true;
        self.start_fade();
    }

    /// Whether a fade is running.
    pub fn is_fading(&self) -> bool {
        self.fade.is_some()
    }

    /// How many heads the next [`Reader::read`] will mix: one, or two while a
    /// fade runs. Never three, however the jumps arrive — see [`Reader`]
    /// §A jump during a fade.
    pub fn active_heads(&self) -> usize {
        1 + usize::from(self.fade.is_some())
    }

    /// Read one output frame at absolute fractional `position`, mixing in the
    /// outgoing head if a fade is running.
    ///
    /// With no fade this is [`History::read`] and nothing more, bit for bit,
    /// which is what keeps the engine's exact identities readable straight
    /// off the output (the tests' ramp trick).
    pub fn read(&mut self, history: &History, position: f64) -> (f32, f32) {
        // Set by `jump`, which has already started the fade: all this frame
        // needs it for is knowing that the position it was handed is a jump
        // and not a step.
        let jumped = core::mem::take(&mut self.jumping);

        let rate = match self.live {
            // A step the driven head took: the distance it covered is its
            // speed, which is what it will keep going at if a later jump
            // leaves it fading out. A non-finite step teaches nothing — the
            // position arrives through the host's tempo — so the head keeps
            // the speed it had.
            Some(live) if !jumped => {
                let step = position - live.position;
                if step.is_finite() { step } else { live.rate }
            }
            // A jump is not a step: the new head's speed is unknown until the
            // next frame measures it, and until then it holds the old one.
            Some(live) => live.rate,
            // The first frame after construction or a reset: nothing has
            // moved yet.
            None => 0.0,
        };
        self.live = Some(Head { position, rate });

        let (left, right) = history.read(position);

        let Some(fade) = self.fade.as_mut() else {
            return (left, right);
        };

        // The outgoing head is the only reader here that can outrun "now",
        // and it is held there. A head that was moving faster than the
        // playhead — the end of a double-speed ramp reads live — walks into
        // the future the frame after it is cut loose, and the future reads
        // silence: a cut, where holding the head on the newest frame keeps
        // it playing the live audio it was already on. The far end needs no
        // such guard: a head that falls out the back of the buffer has
        // genuinely nothing left to play, and `History` answers that with
        // silence.
        let newest = history.recorded().saturating_sub(1) as f64;
        let (out_left, out_right) = history.read(fade.out.position.min(newest));

        let progress = fade.rendered as f32 / fade.frames as f32;
        let (fading_in, fading_out) = (FRAC_PI_2 * progress).sin_cos();

        fade.rendered += 1;
        fade.out = fade.out.advanced();
        if fade.rendered >= fade.frames {
            self.fade = None;
        }

        (
            fading_out * out_left + fading_in * left,
            fading_out * out_right + fading_in * right,
        )
    }

    /// Forget both heads: the next read is a plain [`History::read`] again,
    /// with no fade and no speed carried over.
    ///
    /// What the plugin's `reset()` wants, alongside [`History::reset`] — the
    /// host has said the audio either side of this point is not continuous,
    /// so a head still coasting across the seam would play material from
    /// somewhere else in the song.
    pub fn reset(&mut self) {
        self.live = None;
        self.fade = None;
        self.jumping = false;
    }

    /// Start the fade this frame's jump needs, or restart the running one
    /// from the current mix — [`Reader`] §A jump during a fade has the why of
    /// every line of this.
    fn start_fade(&mut self) {
        // Nothing has been read yet, so there is no head to fade out: the
        // jump is simply where this reader starts.
        let Some(live) = self.live else {
            return;
        };

        // The old head, one frame on at its own speed — where it would have
        // been had the jump not happened. If that is not a position at all
        // (the driven one reached here through the host's tempo), there is
        // nothing to fade from: no fade starts, and one already running is
        // left to finish rather than cut, which also keeps every head this
        // module ever reads at a finite position — a non-finite one would come
        // back from the `min` in `read` as live audio rather than as the
        // silence it deserves.
        let continued = live.advanced();
        if !continued.position.is_finite() {
            return;
        }

        match self.fade.as_mut() {
            // Early in the ramp the head fading in is the quieter one, so the
            // new position replaces it and the ramp carries on untouched.
            Some(fade) if 2 * fade.rendered <= fade.frames => {}
            // Late in the ramp the outgoing head is the quieter one: drop it,
            // promote the head that was fading in, and mirror the ramp so
            // that promotion hands it the gain it already had.
            Some(fade) => {
                fade.rendered = fade.frames - fade.rendered;
                fade.out = continued;
            }
            // Nothing running: a whole fade, at the length Smoothing is at.
            None => {
                self.fade = Some(Fade { out: continued, rendered: 0, frames: self.length });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::f32::consts::SQRT_2;

    use super::*;

    /// Record `count` frames whose values say where they came from: the left
    /// channel is the absolute position as a float, the right channel is that
    /// negated, so a test can tell a position from a channel swap.
    fn record_ramp(history: &mut History, count: u64) {
        for position in history.recorded()..history.recorded() + count {
            history.record(position as f32, -(position as f32));
        }
    }

    /// The capacity is [`MAX_LOOKBACK_SECONDS`] at the rate the host
    /// reported — the intended sizing, and the one that decides whether the
    /// longest reverse has material behind it.
    #[test]
    fn the_capacity_is_sixty_four_seconds_at_the_sample_rate() {
        assert_eq!(History::new(48_000.0).capacity(), 48_000 * 64);
        assert_eq!(History::new(44_100.0).capacity(), 44_100 * 64);
        assert_eq!(History::new(192_000.0).capacity(), 192_000 * 64);
    }

    /// Which is ~25 MB a channel pair at 48 kHz and ~98 MB at 192 kHz. Asserted as bytes
    /// because that is the number that has to stay true if anyone ever
    /// reaches for a wider sample type.
    #[test]
    fn the_buffer_costs_the_megabytes_the_spec_budgets() {
        let bytes = |rate: f32| History::new(rate).capacity() * 2 * size_of::<f32>();

        assert_eq!(bytes(48_000.0), 24_576_000);
        assert_eq!(bytes(192_000.0), 98_304_000);
    }

    /// A rate the host had no business reporting sizes a small harmless
    /// buffer instead of panicking or asking for terabytes: NaN falls to the
    /// low end of the clamp (`max` before `min`), and an absurd rate stops at
    /// [`MAX_CAPACITY_FRAMES`].
    #[test]
    fn a_nonsense_sample_rate_still_sizes_to_something_harmless() {
        for rate in [0.0, -48_000.0, f32::NAN, f32::NEG_INFINITY] {
            assert_eq!(History::new(rate).capacity(), 1, "rate {rate} sized badly");
        }

        for rate in [1.0e9, f32::INFINITY] {
            assert_eq!(History::new(rate).capacity(), MAX_CAPACITY_FRAMES);
        }

        assert_eq!(History::with_capacity(0).capacity(), 1);
        assert_eq!(History::with_capacity(usize::MAX).capacity(), MAX_CAPACITY_FRAMES);
    }

    /// Nothing recorded yet: every position reads silence, backwards and
    /// forwards. A block that starts the instant the plugin is loaded has no
    /// past to play, and must play nothing rather than whatever the
    /// allocation happened to contain.
    #[test]
    fn an_empty_buffer_reads_silence_everywhere() {
        let history = History::with_capacity(8);

        assert_eq!(history.recorded(), 0);
        assert_eq!(history.oldest(), 0);

        for position in [-100, -1, 0, 1, 7, 8, 1_000] {
            assert_eq!(history.frame(position), (0.0, 0.0), "position {position}");
        }
    }

    /// Before the first frame ever recorded is silence, and so is the frame
    /// after the newest one: the buffer answers exactly the window it holds,
    /// and the plugin never reads the future (which is why it reports no
    /// latency).
    #[test]
    fn reads_outside_the_recorded_window_are_silent() {
        let mut history = History::with_capacity(8);
        record_ramp(&mut history, 3);

        assert_eq!(history.frame(-2), (0.0, 0.0));
        assert_eq!(history.frame(-1), (0.0, 0.0));
        assert_eq!(history.frame(0), (0.0, -0.0));
        assert_eq!(history.frame(2), (2.0, -2.0));
        assert_eq!(history.frame(3), (0.0, 0.0), "position 3 is not recorded yet");
        assert_eq!(history.frame(9), (0.0, 0.0));
    }

    /// Every recorded frame reads back as the pair it was given, in the
    /// channel it was given in — the whole buffer is of no use if left and
    /// right can swap on the way through the ring.
    #[test]
    fn the_frames_read_back_per_channel_as_recorded() {
        let mut history = History::with_capacity(4);
        history.record(0.25, -0.5);
        history.record(0.75, 1.0);

        assert_eq!(history.frame(0), (0.25, -0.5));
        assert_eq!(history.frame(1), (0.75, 1.0));
    }

    /// Past the capacity the ring wraps: the newest frames are readable, the
    /// ones they overwrote are silence, and the seam is not visible in
    /// between. Written with a four-frame buffer so the positions can be
    /// listed by hand — the arithmetic is the same one 1.5 million frames
    /// run.
    #[test]
    fn reads_wrap_and_the_overwritten_frames_fall_out_the_back() {
        let mut history = History::with_capacity(4);
        record_ramp(&mut history, 6);

        assert_eq!(history.recorded(), 6);
        assert_eq!(history.oldest(), 2);

        for position in [-1, 0, 1] {
            assert_eq!(history.frame(position), (0.0, 0.0), "position {position} should be gone");
        }

        for position in 2..6 {
            let expected = (position as f32, -(position as f32));
            assert_eq!(history.frame(position), expected, "position {position}");
        }

        assert_eq!(history.frame(6), (0.0, 0.0), "position 6 is not recorded yet");
    }

    /// The window slides one frame per recorded frame, and keeps sliding for
    /// as many laps as the ring is asked for: every lap, the oldest readable
    /// position is the newest minus the capacity.
    #[test]
    fn the_readable_window_is_always_the_last_capacity_frames() {
        let mut history = History::with_capacity(5);

        for lap in 1..=7 {
            record_ramp(&mut history, 5);

            let newest = lap * 5 - 1;
            assert_eq!(history.recorded(), newest + 1);
            assert_eq!(history.oldest(), (newest + 1).saturating_sub(5));

            assert_eq!(history.frame(newest as i64), (newest as f32, -(newest as f32)));

            let oldest = history.oldest() as i64;
            assert_eq!(history.frame(oldest), (oldest as f32, -(oldest as f32)));
            assert_eq!(history.frame(oldest - 1), (0.0, 0.0), "lap {lap} kept one too many");
        }
    }

    /// At a whole position the interpolator is the plain read, bit for bit —
    /// everywhere, including the two positions where half its window is
    /// outside the buffer. The polynomial's constant term is `y1`, which is
    /// what keeps the engine's exact identities (double speed ending on
    /// live, a repeat wrapping on a whole frame) exact.
    #[test]
    fn a_fractional_read_at_a_whole_position_is_the_recorded_frame() {
        let mut history = History::with_capacity(8);
        record_ramp(&mut history, 12);

        for position in -3..16 {
            let whole = history.frame(position);
            assert_eq!(history.read(position as f64), whole, "position {position}");
        }
    }

    /// A linear ramp survives the interpolator exactly: Catmull-Rom
    /// reproduces a straight line, so reading `x[n] = n` at position `p`
    /// returns `p` itself, fractional part and all. This is the property the
    /// whole test strategy leans on (the ramp trick) — the output of an
    /// action *is* its read position only if the fractional read is this
    /// transparent.
    #[test]
    fn fractional_reads_reproduce_a_linear_ramp_exactly() {
        let mut history = History::with_capacity(64);
        record_ramp(&mut history, 40);

        // Whole positions with the full four-frame window inside the buffer:
        // one frame of headroom below, two above.
        for base in 1..=37 {
            for fraction in [0.0, 0.125, 0.25, 0.5, 0.75, 0.875] {
                let position = f64::from(base) + fraction;
                let expected = (position as f32, -(position as f32));
                assert_eq!(history.read(position), expected, "position {position}");
            }
        }
    }

    /// Any straight line, not just the identity: a ramp of slope 1/8 through
    /// an offset reads back as that same line at fractional positions. The
    /// values are dyadic so the assertion can be exact; what is being checked
    /// is that the cubic and quadratic terms vanish, not that `f32` rounds
    /// kindly.
    #[test]
    fn fractional_reads_reproduce_a_sloped_ramp_exactly() {
        let line = |position: f32| -0.5 + 0.125 * position;
        let mut history = History::with_capacity(32);
        for position in 0..20 {
            history.record(line(position as f32), -line(position as f32));
        }

        for base in 1..=17 {
            for fraction in [0.125, 0.25, 0.5, 0.75] {
                let position = f64::from(base) + fraction;
                let expected = line(position as f32);
                assert_eq!(history.read(position), (expected, -expected), "at {position}");
            }
        }
    }

    /// Inside the window the ring's seam is invisible: a read whose four
    /// frames straddle the wrap point is still the ramp. Absolute positions
    /// are what make that free — the four taps are the four frames recorded
    /// in that order whether or not the modulo wrapped between them.
    #[test]
    fn a_fractional_read_across_the_ring_seam_is_still_the_ramp() {
        let mut history = History::with_capacity(8);
        record_ramp(&mut history, 12);

        // The seam is at position 8, which lives in slot 0 on top of the
        // frame that was position 0.
        assert_eq!(history.oldest(), 4);
        for position in [5.5, 6.25, 7.5, 7.875, 8.5, 9.75] {
            let expected = (position as f32, -(position as f32));
            assert_eq!(history.read(position), expected, "position {position}");
        }
    }

    /// Outside the recorded window a fractional read is silence, the same
    /// three ways a whole one is: past the newest frame, further back than
    /// the capacity, and before the first frame ever recorded. Not a wrapped
    /// neighbour, and not the edge frame chopdrop's sample player clamps to —
    /// here, having run off the recorded past means there is nothing to play.
    #[test]
    fn fractional_reads_outside_the_recorded_window_are_silent() {
        let empty = History::with_capacity(8);
        for position in [-8.5, -0.5, 0.25, 1.5, 100.5] {
            assert_eq!(empty.read(position), (0.0, 0.0), "empty at {position}");
        }

        let mut history = History::with_capacity(8);
        record_ramp(&mut history, 12);

        for position in [-0.5, 0.5, 2.25, 3.5, 3.999] {
            assert_eq!(history.read(position), (0.0, 0.0), "fallen out the back: {position}");
        }
        for position in [12.0, 12.5, 13.25, 1.0e9] {
            assert_eq!(history.read(position), (0.0, 0.0), "the future: {position}");
        }
    }

    /// A position that is not a number, or is infinite, reads silence instead
    /// of panicking or reading frame 0: it reaches here through the host's
    /// tempo, and nothing on the audio path may trust a host number.
    #[test]
    fn a_nonsense_read_position_is_silence_not_a_panic() {
        let mut history = History::with_capacity(8);
        record_ramp(&mut history, 6);

        for position in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(history.read(position), (0.0, 0.0), "position {position}");
        }
    }

    /// At the window's edges the interpolation runs against silence, so the
    /// curve leaves the recorded past instead of holding its last value: half
    /// a frame past the newest frame of a steady signal is half its level.
    /// Hand-computed from the polynomial (the taps at position 4.5 are
    /// `1, 1, 0, 0`), because the point is the shape of the edge, and a
    /// fade-out there is why a read that walks off the end does not click.
    #[test]
    fn the_window_edges_interpolate_towards_silence() {
        let mut history = History::with_capacity(8);
        for _ in 0..5 {
            history.record(1.0, -1.0);
        }

        // Interior: a steady signal passes through untouched.
        assert_eq!(history.read(2.5), (1.0, -1.0));

        // Leaving the newest frame, towards the silence beyond it.
        assert_eq!(history.read(4.5), (0.5, -0.5));
        assert_eq!(history.read(5.0), (0.0, 0.0));

        // And arriving at the oldest frame, from the silence before it:
        // Catmull-Rom overshoots a step by 1/16 here, as it does at any step.
        assert_eq!(history.read(0.5), (1.0625, -1.0625));
        assert_eq!(history.read(-0.25), (0.0, 0.0));
    }

    /// A reset puts the buffer back to how it was built: positions start from
    /// zero again and every read is silence until frames arrive, so no block
    /// can stitch across a discontinuity the host warned us about.
    #[test]
    fn a_reset_forgets_everything_and_keeps_the_capacity() {
        let mut history = History::with_capacity(4);
        record_ramp(&mut history, 10);
        history.reset();

        assert_eq!(history.capacity(), 4);
        assert_eq!(history.recorded(), 0);
        assert_eq!(history.frame(0), (0.0, 0.0));
        assert_eq!(history.frame(9), (0.0, 0.0));

        history.record(1.0, 2.0);
        assert_eq!(history.frame(0), (1.0, 2.0));
    }

    /// Record `count` frames of one steady value, in the left channel and
    /// negated in the right — a parked head then reads a constant, which is
    /// what lets the crossfade's gains be read straight off the output.
    fn record_value(history: &mut History, count: u64, value: f32) {
        for _ in 0..count {
            history.record(value, -value);
        }
    }

    /// Four 256-frame regions of distinct steady values, so that a head's
    /// *position* decides which value appears in the output: a head parked in
    /// the middle of a region contributes that region's value and nothing
    /// else, and an output that contains a region's value is an output some
    /// head is sitting in it.
    fn record_regions(history: &mut History) {
        for value in REGION_VALUES {
            record_value(history, 256, value);
        }
    }

    /// The values of the four regions [`record_regions`] lays down, and
    /// [`REGIONS`] the position at the middle of each.
    const REGION_VALUES: [f32; 4] = [1.0, 0.5, 0.25, 0.75];
    /// The middle of each of [`record_regions`]'s four regions.
    const REGIONS: [f64; 4] = [128.0, 384.0, 640.0, 896.0];

    /// The equal-power gain pair a fade holds at `progress`: `(outgoing,
    /// incoming)`, written out here rather than taken from the reader so
    /// that the assertions below are against the intended ramp.
    fn gains(progress: f32) -> (f32, f32) {
        let (fading_in, fading_out) = (FRAC_PI_2 * progress).sin_cos();
        (fading_out, fading_in)
    }

    /// With nothing jumping, the reader *is* [`History::read`] — bit for
    /// bit, both channels, one head. The engine's exact identities are
    /// asserted through this path (the ramp trick), so anything the reader
    /// added to a sample outside a fade would be an error in every one of
    /// them.
    #[test]
    fn with_nothing_jumping_the_reader_is_the_plain_fractional_read() {
        let mut history = History::with_capacity(1024);
        record_ramp(&mut history, 600);
        let mut reader = Reader::new(144);

        for step in 0..500 {
            let position = 50.0 + f64::from(step) * 1.125;
            assert_eq!(reader.read(&history, position), history.read(position));
            assert_eq!(reader.active_heads(), 1);
            assert!(!reader.is_fading());
        }
    }

    /// A jump fades the old head out over exactly the configured number of
    /// frames, and the old head keeps going at its own speed while it fades:
    /// the ramp trick makes the output say where each head is, so the mix is
    /// checkable sample by sample. The ends are exact — the first frame of the
    /// fade is the old head bit for bit, and the frame after the last is the
    /// new one — which is what keeps a jump from stepping the output at either
    /// end of the ramp.
    #[test]
    fn a_jump_fades_the_old_head_out_while_it_keeps_its_own_speed() {
        let mut history = History::with_capacity(2048);
        record_ramp(&mut history, 1024);
        let mut reader = Reader::new(16);

        // Walk the driven head up to 500 at one frame per frame, which is the
        // speed it will carry into the fade.
        for position in 496..=500 {
            reader.read(&history, f64::from(position));
        }
        assert_eq!(reader.active_heads(), 1);

        reader.jump();
        for step in 0..16_u16 {
            // The count is of the heads the *next* read mixes, so it is asked
            // before the read rather than after.
            assert_eq!(reader.active_heads(), 2, "step {step}");

            let position = 300.0 + f64::from(step);
            let (left, right) = reader.read(&history, position);

            // The old head is where it would have been had the jump not
            // happened: still moving one frame per frame.
            let old = 501.0 + f32::from(step);
            let (fading_out, fading_in) = gains(f32::from(step) / 16.0);
            let expected = fading_out * old + fading_in * position as f32;

            assert!((left - expected).abs() < 1.0e-3, "step {step}: {left} != {expected}");
            assert_eq!(right, -left, "step {step} lost a channel");
        }

        // The ends of the ramp, exactly: nothing but the old head on the
        // first frame of the fade, nothing but the new one after the last.
        assert_eq!(reader.read(&history, 316.0), (316.0, -316.0));
        assert_eq!(reader.active_heads(), 1);
        assert!(!reader.is_fading());
    }

    /// The very first frame of a fade is the outgoing head bit for bit: the
    /// fade starts at the mix it is replacing, which is the whole point of
    /// seeding it with where the old head *would have been* this frame rather
    /// than where it was last frame.
    #[test]
    fn the_first_frame_of_a_fade_is_the_old_head_exactly() {
        let mut history = History::with_capacity(2048);
        record_ramp(&mut history, 1024);
        let mut reader = Reader::new(144);

        for position in 200..=204 {
            reader.read(&history, f64::from(position));
        }

        reader.jump();
        assert_eq!(reader.read(&history, 700.0), (205.0, -205.0));
    }

    /// The fade is **equal-power**, not linear: at the middle of the ramp two
    /// heads reading the same steady value sum to `√2`, the +3 dB a
    /// constant-power crossfade has on correlated material. A linear
    /// crossfade would read 1.0 there, and would dip by 3 dB on uncorrelated
    /// material instead — which is the audible hole equal power avoids.
    #[test]
    fn the_crossfade_is_equal_power() {
        let mut history = History::with_capacity(2048);
        record_value(&mut history, 1024, 1.0);
        let mut reader = Reader::new(100);

        reader.read(&history, 128.0);
        reader.read(&history, 128.0);
        reader.jump();

        for step in 0..=50_u16 {
            let (left, right) = reader.read(&history, 640.0);
            let (fading_out, fading_in) = gains(f32::from(step) / 100.0);

            assert!((left - (fading_out + fading_in)).abs() < 1.0e-6, "step {step}: {left}");
            assert_eq!(right, -left);

            if step == 0 {
                assert_eq!(left, 1.0, "the ramp starts on the old head exactly");
            }
            if step == 50 {
                assert!((left - SQRT_2).abs() < 1.0e-6, "the middle is not equal-power: {left}");
            }
        }
    }

    /// A jump into a smooth signal leaves no step in the output: with a sine
    /// as the input and the default-ish Smoothing, no adjacent-sample delta
    /// exceeds a small multiple of the sine's own maximum slope. The same
    /// jump with a one-frame fade — the shortest the clamp allows, i.e.
    /// effectively a cut — steps by nearly the full peak-to-peak, which is
    /// what gives this test teeth: the threshold is not one a hard switch
    /// would also pass.
    #[test]
    fn a_jump_across_a_smooth_signal_leaves_no_step() {
        const PERIOD: f64 = 480.0;
        const FADE: u32 = 480;
        // Nearly half a period, so the two heads are as far apart in value as
        // this signal can put them: the worst jump there is.
        const JUMP: f64 = 237.0;
        // The frame to jump on, chosen so that the cut the teeth-check runs
        // lands with one head on a peak and the other near the opposite one.
        const AT: usize = 316;

        let mut history = History::with_capacity(8192);
        for frame in 0..4000 {
            let phase = f64::from(frame) / PERIOD * std::f64::consts::TAU;
            history.record(phase.sin() as f32, -(phase.sin() as f32));
        }

        // The input's own largest step, which the threshold is derived from.
        let slope = (1..4000)
            .map(|frame| (history.frame(frame).0 - history.frame(frame - 1).0).abs())
            .fold(0.0_f32, f32::max);

        let run = |fade_frames: u32| {
            let mut reader = Reader::new(fade_frames);
            let mut output = Vec::new();
            let mut position = 1000.0;

            for frame in 0..1000 {
                if frame == AT {
                    reader.jump();
                    position -= JUMP;
                }
                output.push(reader.read(&history, position).0);
                position += 1.0;
            }

            output
                .windows(2)
                .map(|pair| (pair[1] - pair[0]).abs())
                .fold(0.0_f32, f32::max)
        };

        let faded = run(FADE);
        assert!(faded <= 3.0 * slope, "the crossfade stepped by {faded} (slope {slope})");

        let cut = run(1);
        assert!(cut > 1.5, "a one-frame fade should step by nearly the peak-to-peak: {cut}");
    }

    /// A jump arriving while a fade is still running never opens a third
    /// head: the **quieter** of the two heads in flight is dropped, and the
    /// survivor keeps the gain it already had. Asserted on the four distinct
    /// regions, where a value in the output is a head sitting in that region:
    /// the dropped region's value is gone from the mix the same frame.
    ///
    /// This is the half of the policy that cannot be had for free — the early
    /// case here replaces the head fading *in*, so the ramp carries on with
    /// the same outgoing head at the same gain.
    #[test]
    fn a_jump_early_in_a_fade_replaces_the_head_fading_in() {
        let mut history = History::with_capacity(2048);
        record_regions(&mut history);
        let mut reader = Reader::new(100);

        // Parked in region 0, then a jump into region 1, taken halfway.
        reader.read(&history, REGIONS[0]);
        reader.read(&history, REGIONS[0]);
        reader.jump();
        for _ in 0..50 {
            reader.read(&history, REGIONS[1]);
            assert!(reader.active_heads() <= 2);
        }

        // Halfway is the tie, and the tie goes to the cheaper path: the head
        // fading in is replaced by region 2, and region 1 is gone.
        reader.jump();
        let (left, _) = reader.read(&history, REGIONS[2]);
        let (fading_out, fading_in) = gains(0.5);
        let expected = fading_out * REGION_VALUES[0] + fading_in * REGION_VALUES[2];

        assert!((left - expected).abs() < 1.0e-6, "{left} != {expected}");
        assert_eq!(reader.active_heads(), 2);

        // The ramp carries on from where it was rather than starting again,
        // so it still ends on its original last frame — and ends on the new
        // head alone, exactly.
        for _ in 0..49 {
            reader.read(&history, REGIONS[2]);
        }
        assert_eq!(reader.active_heads(), 1);
        assert_eq!(reader.read(&history, REGIONS[2]).0, REGION_VALUES[2]);
    }

    /// Late in the ramp it is the outgoing head that is quieter, so that one
    /// is dropped: the head that was fading in becomes the one fading out,
    /// **at the gain it already had**, and the new position arrives at the
    /// gain the dropped head was at. The step left in the output is therefore
    /// exactly the dropped head's gain times the difference it was carrying —
    /// the bound that makes "never a third head" affordable.
    #[test]
    fn a_jump_late_in_a_fade_drops_the_head_fading_out() {
        let mut history = History::with_capacity(2048);
        record_regions(&mut history);
        let mut reader = Reader::new(100);

        // A twin, driven identically but never jumping again, says what this
        // frame would have been without the jump — which is what the step the
        // jump leaves has to be measured against.
        let mut twin = Reader::new(100);

        for reader in [&mut reader, &mut twin] {
            reader.read(&history, REGIONS[0]);
            reader.read(&history, REGIONS[0]);
            reader.jump();
            for _ in 0..75 {
                reader.read(&history, REGIONS[1]);
            }
        }

        // Three quarters along: region 0 is the quiet one now.
        reader.jump();
        let (after, _) = reader.read(&history, REGIONS[2]);
        let (before, _) = twin.read(&history, REGIONS[1]);

        // Mirrored about the middle of the ramp, the survivor (region 1)
        // holds the gain it had, and region 2 enters at region 0's gain.
        let (dropped_gain, survivor_gain) = gains(0.75);
        let expected = survivor_gain * REGION_VALUES[1] + dropped_gain * REGION_VALUES[2];
        assert!((after - expected).abs() < 1.0e-6, "{after} != {expected}");
        assert_eq!(reader.active_heads(), 2);

        // Which is the bound, written as the step it leaves: the dropped
        // head's own gain times what replaced it.
        let step = dropped_gain * (REGION_VALUES[0] - REGION_VALUES[2]);
        assert!((before - after - step).abs() < 1.0e-6, "{before} → {after} is not {step}");
        assert!(dropped_gain <= SQRT_2 / 2.0, "the quieter head was not the one dropped");
    }

    /// However fast the jumps arrive, there are never more than two heads and
    /// the envelope never collapses: with one steady value recorded
    /// everywhere, every head reads 1.0, so the output *is* the sum of the two
    /// gains — and it stays inside `[1, √2]` through jump after jump, which is
    /// the two gains still squaring to one across every restart.
    #[test]
    fn jumps_arriving_mid_fade_keep_two_heads_and_a_continuous_envelope() {
        let mut history = History::with_capacity(2048);
        record_value(&mut history, 1024, 1.0);
        let mut reader = Reader::new(100);
        let mut position = 200.0;

        for frame in 0..300 {
            // Every seventh frame, a jump back — often enough that most of
            // them land in the middle of the fade the last one started.
            if frame % 7 == 6 {
                reader.jump();
                position -= 4.0;
            }

            let (left, right) = reader.read(&history, position);
            position += 1.0;

            assert!(reader.active_heads() <= 2, "frame {frame} opened a third head");
            assert!(left >= 1.0 - 1.0e-6, "frame {frame} dipped to {left}");
            assert!(left <= SQRT_2 + 1.0e-6, "frame {frame} rose to {left}");
            assert_eq!(right, -left);
        }
    }

    /// The outgoing head is held at "now" instead of walking into the future:
    /// a head cut loose at the end of a double-speed ramp is moving two frames
    /// per frame and already reading live, so the next frame is past the newest
    /// one recorded. Silence there would be the cut the fade exists to avoid,
    /// so the head holds on the newest frame — the ramp trick shows it reading
    /// exactly that position.
    #[test]
    fn the_outgoing_head_is_held_at_now_rather_than_reading_the_future() {
        let mut history = History::with_capacity(2048);
        record_ramp(&mut history, 1024);
        let newest = 1023.0;
        let mut reader = Reader::new(16);

        // Two frames of recorded past per output frame, ending on live.
        for position in [1017.0, 1019.0, 1021.0, newest] {
            reader.read(&history, position);
        }

        reader.jump();
        // Where it would have been is 1025, which is the future: held at the
        // newest frame, and the first frame of the fade is that head alone.
        assert_eq!(reader.read(&history, 500.0), (newest as f32, -(newest as f32)));

        // And it stays held there rather than fading into silence.
        let (fading_out, fading_in) = gains(1.0 / 16.0);
        let (left, _) = reader.read(&history, 501.0);
        let expected = fading_out * newest as f32 + fading_in * 501.0;
        assert!((left - expected).abs() < 1.0e-3, "{left} != {expected}");
    }

    /// A jump before anything has been read is just where the reader starts:
    /// there is no head to fade out, so no fade starts and the frame is the
    /// plain read. The same after a reset, which is what the plugin's
    /// `reset()` calls — a head still coasting across a seam the host warned
    /// about would play material from elsewhere in the song.
    #[test]
    fn a_jump_with_no_head_to_fade_is_just_where_the_reader_starts() {
        let mut history = History::with_capacity(2048);
        record_ramp(&mut history, 1024);
        let mut reader = Reader::new(144);

        reader.jump();
        assert_eq!(reader.read(&history, 400.0), (400.0, -400.0));
        assert_eq!(reader.active_heads(), 1);

        // Mid-fade, a reset drops both heads.
        for position in 401..=405 {
            reader.read(&history, f64::from(position));
        }
        reader.jump();
        reader.read(&history, 700.0);
        assert!(reader.is_fading());

        reader.reset();
        assert_eq!(reader.active_heads(), 1);
        assert_eq!(reader.read(&history, 800.0), (800.0, -800.0));
    }

    /// A position that is not a number reads silence and poisons nothing:
    /// the positions arrive through the host's tempo, and a NaN that got
    /// into a head's speed would make every faded sample after it NaN. No
    /// fade starts from a head that is nowhere, and the reads either side of
    /// the nonsense are exact.
    #[test]
    fn a_nonsense_position_is_silence_and_poisons_no_head() {
        let mut history = History::with_capacity(2048);
        record_ramp(&mut history, 1024);

        for nonsense in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut reader = Reader::new(144);
            for position in 496..=500 {
                reader.read(&history, f64::from(position));
            }

            assert_eq!(reader.read(&history, nonsense), (0.0, 0.0), "{nonsense}");

            // A jump out of nowhere has nothing to fade from.
            reader.jump();
            assert_eq!(reader.read(&history, 300.0), (300.0, -300.0), "after {nonsense}");
            assert_eq!(reader.active_heads(), 1);

            // And the reader is walking again, fading as it should.
            reader.read(&history, 301.0);
            reader.jump();
            let (left, _) = reader.read(&history, 600.0);
            assert_eq!(left, 302.0, "the old head did not continue after {nonsense}");
        }
    }

    /// Smoothing is a host parameter, so it can be automated in the middle of
    /// a fade. A running fade keeps the length it started with — re-scaling a
    /// ramp halfway through would move the gains it is holding, a step in the
    /// one place whose job is not to have one — and the next fade that starts
    /// from nothing takes the new length.
    #[test]
    fn a_smoothing_change_lands_on_the_next_fade() {
        let mut history = History::with_capacity(2048);
        record_ramp(&mut history, 1024);
        let mut reader = Reader::new(100);

        reader.read(&history, 200.0);
        reader.read(&history, 201.0);
        reader.jump();
        reader.read(&history, 500.0);

        reader.set_fade_frames(10);
        assert_eq!(reader.fade_frames(), 10);

        // 99 frames rendered of the 100 the fade started with.
        for step in 1..99 {
            reader.read(&history, 500.0 + f64::from(step));
            assert!(reader.is_fading(), "the running fade was cut short at {step}");
        }
        reader.read(&history, 599.0);
        assert!(!reader.is_fading());

        // The next fade is the new length: ten frames, then one head again.
        reader.jump();
        for step in 0..10 {
            assert_eq!(reader.active_heads(), 2, "step {step}");
            reader.read(&history, 300.0);
        }
        assert_eq!(reader.active_heads(), 1);
        assert_eq!(reader.read(&history, 300.0), (300.0, -300.0));
    }

    /// Milliseconds to frames: rounded up, never zero, and guarded at both
    /// ends like every other host number. 3 ms at 48 kHz — the Smoothing
    /// default — is 144 frames.
    #[test]
    fn fade_frames_round_up_and_are_never_zero() {
        assert_eq!(fade_frames_for(3.0, 48_000.0), 144);
        assert_eq!(fade_frames_for(0.5, 48_000.0), 24);
        assert_eq!(fade_frames_for(20.0, 48_000.0), 960);
        assert_eq!(fade_frames_for(3.0, 44_100.0), 133, "132.3 frames rounds up");
        assert_eq!(fade_frames_for(20.0, 192_000.0), 3_840);

        for (milliseconds, rate) in [
            (0.0, 48_000.0),
            (f32::NAN, 48_000.0),
            (f32::NEG_INFINITY, 48_000.0),
            (3.0, 0.0),
            (3.0, -48_000.0),
            (3.0, f64::NAN),
        ] {
            assert_eq!(fade_frames_for(milliseconds, rate), 1, "{milliseconds} ms at {rate}");
        }

        assert_eq!(fade_frames_for(f32::INFINITY, f64::INFINITY), MAX_FADE_FRAMES);
    }

    /// Next to the newest frame, where the fourth tap has not been recorded
    /// yet, the read is linear — exact on a ramp, where a cubic through the
    /// missing frame's silence would overshoot.
    #[test]
    fn the_newest_frames_read_linearly_and_exactly() {
        let mut history = History::with_capacity(64);
        for frame in 0..10 {
            history.record(frame as f32, -(frame as f32));
        }

        for position in [7.25, 7.5, 8.0, 8.5, 8.75, 9.0] {
            assert_eq!(history.read(position), (position as f32, -(position as f32)), "{position}");
        }
        // Further back the cubic is in charge, and exact on a ramp too.
        assert_eq!(history.read(4.5), (4.5, -4.5));
    }
}
