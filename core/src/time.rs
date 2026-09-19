//! Keep track of time, both in native and web platforms!

pub use web_time::Duration;
pub use web_time::Instant;
pub use web_time::SystemTime;

use std::sync::{PoisonError, RwLock};

/// The clock [`now`] reads.
static CLOCK: RwLock<fn() -> Instant> = RwLock::new(Instant::now);

/// Sets the clock that decides what user input means.
///
/// Some widgets read the time to interpret input: the time between two
/// presses decides whether a [`Click`] is a double or a triple click, and
/// the time since a scrollable last scrolled decides whether the next mouse
/// wheel event keeps scrolling it or reaches a scrollable nested inside it.
/// Those reads all go through [`now`], and this sets what [`now`] returns.
///
/// The default clock is [`Instant::now`], and nothing changes unless this is
/// called. A test harness or an automation driver that replays input on its
/// own timeline can install a virtual clock here, so the meaning of a
/// scripted gesture does not depend on how fast the host happens to deliver
/// it.
///
/// Time that only paces rendering or animation (redraw requests, the caret
/// blink, window frames) does not read this clock.
///
/// The clock is process-wide.
///
/// [`Click`]: crate::mouse::Click
pub fn set_clock(now: fn() -> Instant) {
    *CLOCK.write().unwrap_or_else(PoisonError::into_inner) = now;
}

/// The current time, as read by the clock set with [`set_clock`].
///
/// Read this, instead of [`Instant::now`], wherever the time decides what
/// user input means.
pub fn now() -> Instant {
    let clock = *CLOCK.read().unwrap_or_else(PoisonError::into_inner);

    clock()
}

/// Creates a [`Duration`] representing the given amount of milliseconds.
pub fn milliseconds(milliseconds: u64) -> Duration {
    Duration::from_millis(milliseconds)
}

/// Creates a [`Duration`] representing the given amount of seconds.
pub fn seconds(seconds: u64) -> Duration {
    Duration::from_secs(seconds)
}

/// Creates a [`Duration`] representing the given amount of minutes.
pub fn minutes(minutes: u64) -> Duration {
    seconds(minutes * 60)
}

/// Creates a [`Duration`] representing the given amount of hours.
pub fn hours(hours: u64) -> Duration {
    minutes(hours * 60)
}

/// Creates a [`Duration`] representing the given amount of days.
pub fn days(days: u64) -> Duration {
    hours(days * 24)
}
