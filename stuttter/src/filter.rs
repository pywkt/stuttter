//! The filter on the effect signal: a pattern's [`Filter`] shape with the
//! cutoff and resonance of the step that is playing (its Cutoff and Res
//! rows, [`crate::pattern::Lane`]).
//!
//! # Like the Volume row
//!
//! It acts where the Volume row acts — on what the engine plays, only while a
//! pattern plays — and it moves the way a level moves: every change is a
//! straight ramp over the Smoothing time ([`Ramp`]). That covers a cutoff or
//! resonance changing at a step boundary, a pattern with a filter starting or
//! stopping (the filter is faded in and out against the unfiltered signal),
//! and one shape giving way to another: the filter is a state-variable one,
//! which makes its low-, band- and high-pass outputs at once from the same
//! state, so a change of shape is a crossfade between two of them rather than
//! a filter thrown away and another started cold.
//!
//! With the filter off — [`Filter::Off`], or nothing playing — and its fade
//! finished, the signal comes back **bit for bit** ([`FilterStage::process`]),
//! which is what keeps every pattern without a filter, and every project from
//! before there was one, exactly as it was.
//!
//! # Always warm
//!
//! The filter runs on every frame, on or off, so that its state follows the
//! signal: switched on, it starts from where a filter that had been running
//! all along would be, and the fade in has no thump in it.
//!
//! Pure and allocation-free, like the other core modules: plain numbers in,
//! plain numbers out, safe on the audio thread.

use crate::pattern::{Filter, cutoff_hz, resonance_q};

/// The highest cutoff the filter is tuned to, as a fraction of the sample
/// rate: the top of the Cutoff row (20 kHz) is past half of a 44.1 kHz rate,
/// where the filter's tuning blows up, so it is held a little short of it.
const MAX_CUTOFF_RATIO: f32 = 0.45;

/// What the filter is asked for on one frame: the pattern's shape and the
/// playing step's two levels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FilterTarget {
    /// The shape; [`Filter::Off`] fades the filter out.
    pub filter: Filter,
    /// The Cutoff row level, `0.0..=1.0` ([`cutoff_hz`]).
    pub cutoff: f32,
    /// The Res row level, `0.0..=1.0` ([`resonance_q`]).
    pub resonance: f32,
}

impl FilterTarget {
    /// No filter: what live audio, Off and a stopped transport ask for.
    pub const BYPASS: FilterTarget =
        FilterTarget { filter: Filter::Off, cutoff: 1.0, resonance: 0.0 };
}

/// A value that moves to each new target in a straight line over a given
/// number of frames, as the Volume row's level does.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Ramp {
    value: f32,
    target: f32,
    step: f32,
    remaining: u32,
}

impl Ramp {
    /// A ramp sitting on `value`.
    const fn at(value: f32) -> Ramp {
        Ramp { value, target: value, step: 0.0, remaining: 0 }
    }

    /// Straight onto `value`, with nothing left to ramp.
    fn jump(&mut self, value: f32) {
        *self = Ramp::at(value);
    }

    /// This frame's value, heading for `target` over `frames` frames from
    /// wherever it is now. A ramp already heading for `target` carries on.
    fn next(&mut self, target: f32, frames: u32) -> f32 {
        if target != self.target {
            self.target = target;
            self.remaining = frames.max(1);
            self.step = (target - self.value) / self.remaining as f32;
        }
        if self.remaining > 0 {
            self.remaining -= 1;
            self.value = match self.remaining {
                // The last frame lands on the target exactly, whatever the
                // sum of the steps came to.
                0 => self.target,
                _ => self.value + self.step,
            };
        }

        self.value
    }
}

/// One channel's state: the two integrators of the state-variable filter.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Svf {
    ic1: f32,
    ic2: f32,
}

/// The tuning of the state-variable filter for one cutoff and one Q
/// (Andrew Simper's trapezoidal form, which stays stable while it is swept).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Coefficients {
    k: f32,
    a1: f32,
    a2: f32,
    a3: f32,
}

impl Coefficients {
    fn new(cutoff: f32, resonance: f32, sample_rate: f32) -> Coefficients {
        let hz = cutoff_hz(cutoff).min(sample_rate * MAX_CUTOFF_RATIO);
        let g = (std::f32::consts::PI * hz / sample_rate).tan();
        let k = 1.0 / resonance_q(resonance);
        let a1 = 1.0 / (1.0 + g * (g + k));
        let a2 = g * a1;

        Coefficients { k, a1, a2, a3: g * a2 }
    }
}

impl Svf {
    /// One sample in; its low-, band- and high-pass outputs out. The
    /// band-pass is scaled to unity at its peak, so turning the resonance up
    /// narrows it rather than making it louder.
    fn tick(&mut self, input: f32, c: &Coefficients) -> (f32, f32, f32) {
        let v3 = input - self.ic2;
        let v1 = c.a1 * self.ic1 + c.a2 * v3;
        let v2 = self.ic2 + c.a2 * self.ic1 + c.a3 * v3;
        self.ic1 = flush(2.0 * v1 - self.ic1);
        self.ic2 = flush(2.0 * v2 - self.ic2);

        (v2, c.k * v1, input - c.k * v1 - v2)
    }
}

/// Zero for a subnormal, so that a filter ringing down into silence does not
/// leave its state crawling through the slow range of floats.
fn flush(value: f32) -> f32 {
    match value.abs() < f32::MIN_POSITIVE {
        true => 0.0,
        false => value,
    }
}

/// How much of each output one shape is: low, band, high.
fn mix_of(filter: Filter) -> (f32, f32, f32) {
    match filter {
        Filter::LowPass | Filter::Off => (1.0, 0.0, 0.0),
        Filter::BandPass => (0.0, 1.0, 0.0),
        Filter::HighPass => (0.0, 0.0, 1.0),
    }
}

/// The filter stage the engine runs its effect signal through: a stereo
/// state-variable filter with every setting ramped (the module's §Like the
/// Volume row).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FilterStage {
    sample_rate: f32,
    left: Svf,
    right: Svf,
    /// How much of the filtered signal is heard: 0 off, 1 on.
    amount: Ramp,
    cutoff: Ramp,
    resonance: Ramp,
    low: Ramp,
    band: Ramp,
    high: Ramp,
    /// The tuning last worked out, and the two levels it was for: a cutoff
    /// that is not moving costs no `tan` per frame.
    tuned: (f32, f32, Coefficients),
}

impl FilterStage {
    /// A filter at `sample_rate`, off.
    pub fn new(sample_rate: f32) -> FilterStage {
        let sample_rate = match sample_rate.is_finite() && sample_rate > 0.0 {
            true => sample_rate,
            false => 48_000.0,
        };
        let open = FilterTarget::BYPASS;

        FilterStage {
            sample_rate,
            left: Svf::default(),
            right: Svf::default(),
            amount: Ramp::at(0.0),
            cutoff: Ramp::at(open.cutoff),
            resonance: Ramp::at(open.resonance),
            low: Ramp::at(1.0),
            band: Ramp::at(0.0),
            high: Ramp::at(0.0),
            tuned: (
                open.cutoff,
                open.resonance,
                Coefficients::new(open.cutoff, open.resonance, sample_rate),
            ),
        }
    }

    /// Off, with no state: what the plugin's `reset()` wants.
    pub fn reset(&mut self) {
        *self = FilterStage::new(self.sample_rate);
    }

    /// One stereo frame through the filter `target` asks for, every change
    /// ramped over `frames` frames (the Smoothing time).
    ///
    /// Off and finished fading, the frame comes back exactly as it went in.
    pub fn process(
        &mut self,
        (left, right): (f32, f32),
        target: FilterTarget,
        frames: u32,
    ) -> (f32, f32) {
        let on = target.filter != Filter::Off;
        if on {
            let (low, band, high) = mix_of(target.filter);
            // Fully off, there is nothing to ramp *from*: the fade in starts
            // on the settings it is fading in to.
            if self.amount.value == 0.0 {
                self.cutoff.jump(target.cutoff);
                self.resonance.jump(target.resonance);
                self.low.jump(low);
                self.band.jump(band);
                self.high.jump(high);
            }
            self.cutoff.next(target.cutoff, frames);
            self.resonance.next(target.resonance, frames);
            self.low.next(low, frames);
            self.band.next(band, frames);
            self.high.next(high, frames);
        } else {
            // Fading out keeps the settings it was fading from.
            for ramp in [&mut self.cutoff, &mut self.resonance] {
                let target = ramp.target;
                ramp.next(target, frames);
            }
        }
        let amount = self.amount.next(if on { 1.0 } else { 0.0 }, frames);

        let (cutoff, resonance) = (self.cutoff.value, self.resonance.value);
        if (cutoff, resonance) != (self.tuned.0, self.tuned.1) {
            let tuning = Coefficients::new(cutoff, resonance, self.sample_rate);
            self.tuned = (cutoff, resonance, tuning);
        }
        let tuning = self.tuned.2;
        let (ll, lb, lh) = self.left.tick(left, &tuning);
        let (rl, rb, rh) = self.right.tick(right, &tuning);

        if amount == 0.0 {
            return (left, right);
        }
        let (wl, wb, wh) = (self.low.value, self.band.value, self.high.value);
        let filtered_left = wl * ll + wb * lb + wh * lh;
        let filtered_right = wl * rl + wb * rb + wh * rh;

        (left + amount * (filtered_left - left), right + amount * (filtered_right - right))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f32 = 48_000.0;
    /// The Smoothing default, 3 ms, in frames.
    const FADE: u32 = 144;

    fn target(filter: Filter, cutoff: f32, resonance: f32) -> FilterTarget {
        FilterTarget { filter, cutoff, resonance }
    }

    /// The RMS of a sine at `hz` through `stage` with `target`, measured
    /// after the filter has settled.
    fn rms_through(stage: &mut FilterStage, hz: f32, target: FilterTarget) -> f32 {
        let mut sum = 0.0;
        let mut count = 0;
        for n in 0..24_000 {
            let x = (std::f32::consts::TAU * hz * n as f32 / RATE).sin();
            let (y, _) = stage.process((x, x), target, FADE);
            if n >= 12_000 {
                sum += y * y;
                count += 1;
            }
        }

        (sum / count as f32).sqrt()
    }

    /// Off, the signal comes back bit for bit, however long it runs and
    /// whatever the levels say.
    #[test]
    fn off_is_an_exact_bypass() {
        let mut stage = FilterStage::new(RATE);
        for n in 0..10_000 {
            let x = ((n * 7919) % 2001) as f32 / 1000.0 - 1.0;
            let off = target(Filter::Off, 0.1, 0.9);
            assert_eq!(stage.process((x, -x), off, FADE), (x, -x), "frame {n}");
        }
        assert_eq!(stage.amount.value, 0.0);
    }

    /// A low-pass at 200 Hz keeps a 50 Hz tone and cuts a 5 kHz one; a
    /// high-pass the other way round; a band-pass at 1 kHz keeps 1 kHz.
    #[test]
    fn each_shape_keeps_what_it_should() {
        let low = 0.4; // about 316 Hz
        let pass = |filter, hz, cutoff| {
            rms_through(&mut FilterStage::new(RATE), hz, target(filter, cutoff, 0.0))
        };
        let full = std::f32::consts::FRAC_1_SQRT_2;

        assert!(pass(Filter::LowPass, 50.0, low) > 0.9 * full);
        assert!(pass(Filter::LowPass, 5_000.0, low) < 0.02);
        assert!(pass(Filter::HighPass, 50.0, 0.6) < 0.02);
        assert!(pass(Filter::HighPass, 5_000.0, 0.6) > 0.9 * full);
        let centre = (1_000.0f32 / 20.0).ln() / 1_000.0f32.ln();
        assert!((pass(Filter::BandPass, 1_000.0, centre) - full).abs() < 0.05 * full);
        assert!(pass(Filter::BandPass, 50.0, centre) < 0.2 * full);
    }

    /// Switching on fades the filter in over the Smoothing time rather than
    /// cutting in: the first frame is still almost the input, the last frame
    /// of the fade is fully filtered.
    #[test]
    fn the_filter_fades_in_and_out() {
        let mut stage = FilterStage::new(RATE);
        let on = target(Filter::LowPass, 0.0, 0.0);

        stage.process((1.0, 1.0), on, FADE);
        assert!((stage.amount.value - 1.0 / FADE as f32).abs() < 1e-6);
        for _ in 1..FADE {
            stage.process((1.0, 1.0), on, FADE);
        }
        assert_eq!(stage.amount.value, 1.0);

        stage.process((1.0, 1.0), FilterTarget::BYPASS, FADE);
        assert!(stage.amount.value < 1.0 && stage.amount.value > 0.9);
        for _ in 1..FADE {
            stage.process((1.0, 1.0), FilterTarget::BYPASS, FADE);
        }
        assert_eq!(stage.amount.value, 0.0);
        assert_eq!(stage.process((0.25, 0.5), FilterTarget::BYPASS, FADE), (0.25, 0.5));
    }

    /// A cutoff that jumps a long way between two steps is ramped, so the
    /// output moves by no more than a smooth signal would.
    #[test]
    fn a_cutoff_change_is_ramped_not_stepped() {
        let mut stage = FilterStage::new(RATE);
        let mut last = 0.0;
        let mut biggest: f32 = 0.0;
        for n in 0..20_000 {
            let cutoff = if (n / 2_000) % 2 == 0 { 0.1 } else { 0.95 };
            let x = (std::f32::consts::TAU * 220.0 * n as f32 / RATE).sin() * 0.5;
            let (y, _) = stage.process((x, x), target(Filter::LowPass, cutoff, 0.5), FADE);
            if n > 0 {
                biggest = biggest.max((y - last).abs());
            }
            last = y;
        }
        assert!(biggest < 0.1, "a step of {biggest} in one frame");
    }

    /// Full resonance at the top of the row, at the lowest and highest rates
    /// a host might run, on a full-scale square wave: nothing blows up, and
    /// silence afterwards rings down to exact zeros, not subnormals.
    #[test]
    fn extremes_stay_finite_and_ring_down_to_zero() {
        for rate in [8_000.0, 22_050.0, 44_100.0, 192_000.0, 0.0, f32::NAN] {
            let mut stage = FilterStage::new(rate);
            for filter in [Filter::LowPass, Filter::BandPass, Filter::HighPass] {
                for cutoff in [0.0, 0.5, 1.0] {
                    for n in 0..4_000 {
                        let x = if (n / 37) % 2 == 0 { 1.0 } else { -1.0 };
                        let (l, r) = stage.process((x, x), target(filter, cutoff, 1.0), FADE);
                        assert!(l.is_finite() && r.is_finite(), "{rate} {filter:?} {cutoff}");
                        assert!(l.abs() < 50.0, "{rate} {filter:?} {cutoff}: {l}");
                    }
                }
            }
            for _ in 0..200_000 {
                stage.process((0.0, 0.0), target(Filter::LowPass, 0.5, 1.0), FADE);
            }
            assert_eq!((stage.left, stage.right), (Svf::default(), Svf::default()), "{rate}");
        }
    }

    /// A reset forgets the state and the fade.
    #[test]
    fn a_reset_is_off_with_no_state() {
        let mut stage = FilterStage::new(RATE);
        rms_through(&mut stage, 300.0, target(Filter::HighPass, 0.5, 0.5));
        stage.reset();
        assert_eq!(stage, FilterStage::new(RATE));
    }
}
