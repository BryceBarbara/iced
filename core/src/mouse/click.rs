//! Track mouse clicks.
use crate::mouse::Button;
use crate::time::Instant;
use crate::{Point, Transformation};

use std::ops::Mul;
use std::sync::{PoisonError, RwLock};

/// The clock [`Click::new`] reads to timestamp a click.
static CLOCK: RwLock<fn() -> Instant> = RwLock::new(Instant::now);

/// Sets the clock [`Click::new`] reads to timestamp a click.
///
/// The time between two clicks decides whether they form a [`Kind::Double`]
/// or [`Kind::Triple`] click. The default clock is [`Instant::now`], and
/// nothing changes unless this is called.
///
/// A test harness or an automation driver that replays input on its own
/// timeline can install a virtual clock here, so the kind of a scripted
/// click does not depend on how fast the host happens to deliver it.
///
/// The clock is process-wide.
pub fn set_clock(now: fn() -> Instant) {
    *CLOCK.write().unwrap_or_else(PoisonError::into_inner) = now;
}

/// The current time, as read by the clock set with [`set_clock`].
fn now() -> Instant {
    let clock = *CLOCK.read().unwrap_or_else(PoisonError::into_inner);

    clock()
}

/// A mouse click.
#[derive(Debug, Clone, Copy)]
pub struct Click {
    kind: Kind,
    button: Button,
    position: Point,
    time: Instant,
}

/// The kind of mouse click.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A single click
    Single,

    /// A double click
    Double,

    /// A triple click
    Triple,
}

impl Kind {
    fn next(self) -> Kind {
        match self {
            Kind::Single => Kind::Double,
            Kind::Double => Kind::Triple,
            Kind::Triple => Kind::Double,
        }
    }
}

impl Click {
    /// Creates a new [`Click`] with the given position and previous last
    /// [`Click`].
    pub fn new(
        position: Point,
        button: Button,
        previous: Option<Click>,
    ) -> Click {
        let time = now();

        let kind = if let Some(previous) = previous {
            if previous.is_consecutive(position, time)
                && button == previous.button
            {
                previous.kind.next()
            } else {
                Kind::Single
            }
        } else {
            Kind::Single
        };

        Click {
            kind,
            button,
            position,
            time,
        }
    }

    /// Returns the [`Kind`] of [`Click`].
    pub fn kind(&self) -> Kind {
        self.kind
    }

    /// Returns the position of the [`Click`].
    pub fn position(&self) -> Point {
        self.position
    }

    fn is_consecutive(&self, new_position: Point, time: Instant) -> bool {
        let duration = if time > self.time {
            Some(time - self.time)
        } else {
            None
        };

        self.position.distance(new_position) < 6.0
            && duration
                .map(|duration| duration.as_millis() <= 300)
                .unwrap_or(false)
    }
}

impl Mul<Transformation> for Click {
    type Output = Click;

    fn mul(self, transformation: Transformation) -> Click {
        Click {
            kind: self.kind,
            button: self.button,
            position: self.position * transformation,
            time: self.time,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::Duration;

    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicU64, Ordering};

    static EPOCH: OnceLock<Instant> = OnceLock::new();
    static ELAPSED_MS: AtomicU64 = AtomicU64::new(0);

    fn virtual_now() -> Instant {
        *EPOCH.get_or_init(Instant::now)
            + Duration::from_millis(ELAPSED_MS.load(Ordering::SeqCst))
    }

    #[test]
    fn a_custom_clock_decides_double_or_single() {
        set_clock(virtual_now);

        let position = Point::new(10.0, 10.0);
        let click = |previous, ms| {
            ELAPSED_MS.store(ms, Ordering::SeqCst);
            Click::new(position, Button::Left, previous)
        };

        // Microseconds apart on the wall clock, but a second apart on
        // the custom one: two single clicks.
        let first = click(None, 0);
        let late = click(Some(first), 1_000);

        // Within the window on the custom clock: a double click.
        let double = click(Some(late), 1_100);

        set_clock(Instant::now);

        assert_eq!(first.kind(), Kind::Single);
        assert_eq!(late.kind(), Kind::Single);
        assert_eq!(double.kind(), Kind::Double);
    }
}
