//! Wheel scrolling spread across a few frames instead of applied in one jump.
//!
//! A notched wheel delivers one large delta per detent, which reads as the
//! content teleporting rather than moving. Easing each detent over a short
//! motion keeps the reading position traceable, and folding a fresh detent
//! into the motion already running is what lets a fast series of them build
//! speed instead of restarting from a standstill. A trackpad already reports
//! continuous pixel deltas, so only line deltas are worth animating.

use crate::{Pixels, Point, px};
use scheduler::Instant;
use std::time::Duration;

/// A motion shorter than this would be over within a frame or two, so it is
/// applied at once; animating it would only delay it.
const DURATION_EPSILON: f32 = 0.01;

/// A position within this many pixels of the destination counts as arrived:
/// the difference is below what the display can show, and chasing it would
/// keep the frame pump awake indefinitely.
const ARRIVAL_EPSILON: f32 = 0.01;

/// One wheel detent being carried to its destination.
///
/// Positions are distances from the start of the scrollable content, growing
/// as the content scrolls away, so a container that stores a negative offset
/// negates it on the way in and out.
#[derive(Debug)]
pub(crate) struct SmoothWheelMotion {
    start_time: Instant,
    duration: Duration,
    start_position: f32,
    destination: f32,
    last_position: f32,
    curve: SmoothWheelCurve,
}

/// Where one frame of an animation puts the content.
pub(crate) struct SmoothWheelStep {
    pub(crate) position: f32,
    /// How fast the content is moving, for a detent arriving during this
    /// motion to inherit.
    pub(crate) velocity: f32,
    /// The motion has nothing left to travel and can be dropped.
    pub(crate) finished: bool,
}

/// What a fresh detent asks of the container.
pub(crate) enum SmoothWheel {
    /// Animate over the coming frames.
    Motion(SmoothWheelMotion),
    /// Too short to animate; move there in one step.
    Jump(f32),
    /// Already there, with nothing to do.
    Settled,
}

impl SmoothWheelMotion {
    /// Aim a detent that asks for `destination` from where the content stands
    /// now, inheriting `velocity` from whatever motion it interrupted.
    pub(crate) fn start(
        now: Instant,
        position: f32,
        destination: f32,
        velocity: f32,
        scroll_max: f32,
    ) -> SmoothWheel {
        let destination = destination.clamp(0., scroll_max);
        if (destination - position).abs() < ARRIVAL_EPSILON {
            return SmoothWheel::Settled;
        }

        // Speed carried out of a motion that was already pressed against an
        // end is speed the content never used, and letting it shorten the new
        // motion would make a reversal snap.
        let velocity =
            if (position <= 0. && velocity < 0.) || (position >= scroll_max && velocity > 0.) {
                0.
            } else {
                velocity
            };

        let distance = destination - position;
        let mut duration = travel_duration(distance).as_secs_f32();
        // Content already moving this way covers part of the distance on its
        // own, so the motion is cut short by what that speed accounts for.
        // Without this a stream of detents would restart a full-length motion
        // each time and never build up speed.
        if velocity.abs() >= f32::EPSILON {
            let velocity_bound = distance / velocity * 2.5;
            if velocity_bound >= 0. {
                duration = duration.min(velocity_bound);
            }
        }
        if duration < DURATION_EPSILON {
            return SmoothWheel::Jump(destination);
        }

        let duration = Duration::from_secs_f32(duration);
        SmoothWheel::Motion(Self {
            start_time: now,
            duration,
            start_position: position,
            destination,
            last_position: position,
            curve: SmoothWheelCurve::for_motion(velocity, distance, duration),
        })
    }

    /// How long this motion runs for.
    #[cfg(test)]
    pub(crate) fn duration(&self) -> Duration {
        self.duration
    }

    /// Where the motion is aiming, so a detent arriving mid-flight adds to the
    /// distance still to travel rather than to where the content happens to
    /// have reached.
    pub(crate) fn destination(&self) -> f32 {
        self.destination
    }

    /// Advance to `now`. `position` is where the content actually stands,
    /// which the motion follows: content inserted or removed ahead of the
    /// viewport moves the whole motion with it, so the distance left to travel
    /// stays the one the wheel asked for.
    pub(crate) fn advance(
        &mut self,
        now: Instant,
        position: f32,
        scroll_max: f32,
    ) -> SmoothWheelStep {
        let layout_shift = position - self.last_position;
        self.start_position += layout_shift;
        self.destination = (self.destination + layout_shift).clamp(0., scroll_max);

        let progress =
            now.duration_since(self.start_time).as_secs_f32() / self.duration.as_secs_f32();
        let elapsed = progress >= 1.;
        let (eased, derivative) = self.curve.sample(progress);
        let distance = self.destination - self.start_position;
        let position = if elapsed {
            self.destination
        } else {
            (self.start_position + distance * eased).clamp(0., scroll_max)
        };

        let mut velocity = if elapsed {
            0.
        } else {
            distance * derivative / self.duration.as_secs_f32()
        };
        if (position <= 0. && velocity < 0.) || (position >= scroll_max && velocity > 0.) {
            velocity = 0.;
        }

        self.last_position = position;
        SmoothWheelStep {
            position,
            velocity,
            finished: elapsed || (position - self.destination).abs() <= ARRIVAL_EPSILON,
        }
    }
}

/// The wheel animation of one scrolling container, kept across frames.
///
/// Each axis animates on its own, because a container that scrolls both ways
/// can be moved along either. The travel limit is recorded here as well: a
/// wheel event arrives between frames and has no viewport of its own to
/// measure how far the content can still go.
///
/// Offsets are the container's own negative-going ones, so this negates them
/// into travelled distances and back again.
#[derive(Default, Debug)]
pub(crate) struct SmoothWheelState {
    x: Option<SmoothWheelMotion>,
    y: Option<SmoothWheelMotion>,
    scroll_max: Point<Pixels>,
}

impl SmoothWheelState {
    /// Whether a motion is still in flight, which is what says another frame
    /// is owed.
    pub(crate) fn is_animating(&self) -> bool {
        self.x.is_some() || self.y.is_some()
    }

    /// Drop the animation, for a container being moved by something other than
    /// the wheel: a programmatic scroll is a jump to a position, and easing
    /// towards a destination that has been overruled would drag it back.
    pub(crate) fn cancel(&mut self) {
        self.x = None;
        self.y = None;
    }

    /// Record how far the content can travel, as the frame that just laid it
    /// out measured it.
    pub(crate) fn set_scroll_max(&mut self, scroll_max: Point<Pixels>) {
        self.scroll_max = scroll_max;
    }

    /// Aim at where `delta` asks the content to end up, answering with the
    /// offset the container holds until the next frame advances it.
    pub(crate) fn steer(
        &mut self,
        now: Instant,
        offset: Point<Pixels>,
        delta: Point<Pixels>,
    ) -> Point<Pixels> {
        let scroll_max = self.scroll_max;

        Point {
            x: px(-steer_axis(
                &mut self.x,
                now,
                -offset.x.0,
                delta.x.0,
                scroll_max.x.0,
            )),
            y: px(-steer_axis(
                &mut self.y,
                now,
                -offset.y.0,
                delta.y.0,
                scroll_max.y.0,
            )),
        }
    }

    /// Move one frame along, answering the offset this frame should draw at.
    pub(crate) fn advance(
        &mut self,
        now: Instant,
        offset: Point<Pixels>,
        scroll_max: Point<Pixels>,
    ) -> Point<Pixels> {
        Point {
            x: px(-advance_axis(&mut self.x, now, -offset.x.0, scroll_max.x.0).0),
            y: px(-advance_axis(&mut self.y, now, -offset.y.0, scroll_max.y.0).0),
        }
    }
}

/// Advance one axis, dropping a motion that has arrived. Answers where the
/// axis stands and how fast it is moving.
fn advance_axis(
    motion: &mut Option<SmoothWheelMotion>,
    now: Instant,
    position: f32,
    scroll_max: f32,
) -> (f32, f32) {
    let Some(running) = motion.as_mut() else {
        return (position, 0.);
    };

    let step = running.advance(now, position, scroll_max);
    if step.finished {
        *motion = None;
    }
    (step.position, step.velocity)
}

/// Fold one wheel delta into one axis, answering where that axis stands once
/// the detent has been taken into account.
fn steer_axis(
    motion: &mut Option<SmoothWheelMotion>,
    now: Instant,
    position: f32,
    delta: f32,
    scroll_max: f32,
) -> f32 {
    let (position, velocity) = advance_axis(motion, now, position, scroll_max);
    if delta == 0. {
        return position;
    }

    // A detent lands past where the motion in flight is headed, not past where
    // the content has reached, so a fast series of them accumulates.
    let destination = motion
        .as_ref()
        .map_or(position, SmoothWheelMotion::destination)
        - delta;

    match SmoothWheelMotion::start(now, position, destination, velocity, scroll_max) {
        SmoothWheel::Motion(started) => {
            *motion = Some(started);
            position
        }
        SmoothWheel::Jump(destination) => {
            *motion = None;
            destination
        }
        SmoothWheel::Settled => {
            *motion = None;
            position
        }
    }
}

/// How long one detent takes to land: a short hop is over quickly, and a long
/// one is given more frames so the content stays readable while it moves. Both
/// ends are capped because a motion outside them stops reading as one gesture.
fn travel_duration(distance: f32) -> Duration {
    const RAMP_START_PX: f32 = 120.;
    const RAMP_END_PX: f32 = 480.;
    const MIN_FRAMES: f32 = 6.;
    const MAX_FRAMES: f32 = 12.;
    const FRAMES_PER_SECOND: f32 = 60.;

    let slope = (MIN_FRAMES - MAX_FRAMES) / (RAMP_END_PX - RAMP_START_PX);
    let offset = MAX_FRAMES - RAMP_START_PX * slope;
    let frames = (offset + distance.abs() * slope).clamp(MIN_FRAMES, MAX_FRAMES);
    Duration::from_secs_f32(frames / FRAMES_PER_SECOND)
}

/// The easing applied over a motion, as the first control point of a cubic
/// bezier whose second point is fixed at the decelerating `(0.58, 1)`. The
/// first point's slope comes from the speed the motion started with, which is
/// what makes an interrupted motion continue rather than restart.
#[derive(Clone, Copy, Debug)]
struct SmoothWheelCurve {
    x1: f32,
    y1: f32,
}

impl SmoothWheelCurve {
    fn for_motion(velocity: f32, distance: f32, duration: Duration) -> Self {
        let slope = if distance.abs() < f32::EPSILON {
            0.
        } else {
            (velocity * duration.as_secs_f32() / distance).clamp(-1000., 1000.)
        };
        Self {
            x1: 0.42,
            y1: 0.42 * slope,
        }
    }

    /// The eased fraction of the distance covered at `progress`, and how fast
    /// it is being covered there. The bezier is parameterized by its own
    /// control variable rather than by time, so the parameter matching this
    /// progress is bisected out first.
    fn sample(self, progress: f32) -> (f32, f32) {
        let progress = progress.clamp(0., 1.);
        let mut low = 0.;
        let mut high = 1.;
        for _ in 0..16 {
            let parameter = (low + high) * 0.5;
            if cubic_value(parameter, self.x1, 0.58) < progress {
                low = parameter;
            } else {
                high = parameter;
            }
        }

        let parameter = (low + high) * 0.5;
        let value = cubic_value(parameter, self.y1, 1.);
        let x_derivative = cubic_derivative(parameter, self.x1, 0.58);
        let derivative = if x_derivative.abs() < f32::EPSILON {
            0.
        } else {
            cubic_derivative(parameter, self.y1, 1.) / x_derivative
        };
        (value, derivative)
    }
}

fn cubic_value(parameter: f32, first: f32, second: f32) -> f32 {
    let inverse = 1. - parameter;
    3. * inverse * inverse * parameter * first
        + 3. * inverse * parameter * parameter * second
        + parameter * parameter * parameter
}

fn cubic_derivative(parameter: f32, first: f32, second: f32) -> f32 {
    let inverse = 1. - parameter;
    3. * inverse * inverse * first
        + 6. * inverse * parameter * (second - first)
        + 3. * parameter * parameter * (1. - second)
}

#[cfg(test)]
mod tests {
    use crate::smooth_scroll::{SmoothWheelCurve, travel_duration};
    use std::time::Duration;

    #[test]
    fn the_curve_starts_out_at_the_speed_it_inherited() {
        assert_eq!(travel_duration(120.).as_millis(), 200);
        assert_eq!(travel_duration(300.).as_millis(), 150);
        assert_eq!(travel_duration(480.).as_millis(), 100);

        let velocity = 120.;
        let distance = 240.;
        let duration = Duration::from_millis(160);
        let curve = SmoothWheelCurve::for_motion(velocity, distance, duration);
        let (_, derivative) = curve.sample(0.0001);
        let sampled_velocity = distance * derivative / duration.as_secs_f32();
        assert!((sampled_velocity - velocity).abs() < 2.);

        // Nothing to inherit leaves the curve at its own resting ease-out.
        let idle = SmoothWheelCurve::for_motion(0., distance, duration);
        assert_eq!(idle.x1, 0.42);
        assert_eq!(idle.y1, 0.);
    }
}
