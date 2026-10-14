//! How closely the user can be watching a window, and the schedule background
//! monitors keep because of it. A monitor that shells out every few seconds
//! costs battery whether or not anyone looks, so it works at full rate only
//! while its window is the active one, less often while the window is merely
//! shown, and rarely while nothing of it is on screen.

use gpui::Window;
use std::{
    sync::{Condvar, Mutex, PoisonError},
    time::{Duration, Instant},
};

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Pace {
    /// The window has keyboard focus.
    #[default]
    Active,
    /// Another window has focus, but this one is at least partly on screen.
    Shown,
    /// Covered, minimized, on another Space, or on a sleeping or locked
    /// display: frames drawn for it would not be seen.
    Hidden,
}

impl Pace {
    pub(crate) fn of(window: &Window) -> Self {
        if window.is_window_active() {
            Self::Active
        } else if window.is_visible() {
            Self::Shown
        } else {
            Self::Hidden
        }
    }
}

/// How long a monitor waits between runs at each pace.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Intervals {
    pub active: Duration,
    pub shown: Duration,
    pub hidden: Duration,
}

impl Intervals {
    pub(crate) const fn at(&self, pace: Pace) -> Duration {
        match pace {
            Pace::Active => self.active,
            Pace::Shown => self.shown,
            Pace::Hidden => self.hidden,
        }
    }
}

/// A monitor worker's schedule: the UI thread sets its pace and stops it, and
/// the worker waits on it between runs. A faster pace wakes the wait, so a
/// window brought back is refreshed at once rather than at the end of a long
/// hidden interval.
pub(crate) struct Schedule {
    state: Mutex<State>,
    changed: Condvar,
}

struct State {
    stopped: bool,
    pace: Pace,
}

impl Schedule {
    pub(crate) fn new(pace: Pace) -> Self {
        Self {
            state: Mutex::new(State {
                stopped: false,
                pace,
            }),
            changed: Condvar::new(),
        }
    }

    pub(crate) fn stop(&self) {
        self.lock().stopped = true;
        self.changed.notify_all();
    }

    /// Cheap when unchanged, so the UI thread may call it on every poll.
    pub(crate) fn set_pace(&self, pace: Pace) {
        let mut state = self.lock();
        if state.pace != pace {
            state.pace = pace;
            self.changed.notify_all();
        }
    }

    /// Waits until `interval` at the current pace has passed since `since`,
    /// re-reading the pace whenever it changes; false once stopped.
    pub(crate) fn wait(&self, since: Instant, interval: impl Fn(Pace) -> Duration) -> bool {
        let mut state = self.lock();
        loop {
            if state.stopped {
                return false;
            }
            let due = since + interval(state.pace);
            let now = Instant::now();
            if now >= due {
                return true;
            }
            state = self
                .changed
                .wait_timeout(state, due - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
