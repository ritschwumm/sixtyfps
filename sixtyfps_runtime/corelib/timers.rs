/* LICENSE BEGIN
    This file is part of the SixtyFPS Project -- https://sixtyfps.io
    Copyright (c) 2020 Olivier Goffart <olivier.goffart@sixtyfps.io>
    Copyright (c) 2020 Simon Hausmann <simon.hausmann@sixtyfps.io>

    SPDX-License-Identifier: GPL-3.0-only
    This file is also available under commercial licensing terms.
    Please contact info@sixtyfps.io for more information.
LICENSE END */
/*!
    Support for timers.

    Timers are just a bunch of callbacks sorted by expiry date.
*/

#![warn(missing_docs)]
use std::cell::{Cell, RefCell};

type TimerCallback = Box<dyn Fn()>;

/// The TimerMode specifies what should happen after the timer fired.
#[derive(Copy, Clone)]
pub enum TimerMode {
    /// A SingleShot timer is fired only once.
    SingleShot,
    /// A Repeated timer is fired repeatedly until it is stopped.
    Repeated,
}

/// Timer is a handle to the timer system that allows triggering a callback to be called
/// after a specified period of time.
#[derive(Default)]
pub struct Timer {
    id: Cell<Option<usize>>,
}

impl Timer {
    /// Starts the timer with the given mode and duration, in order for the callback to called when the
    /// timer fires. If the timer has been started previously and not fired yet, then it will be restarted.
    ///
    /// Arguments:
    /// * `mode`: The timer mode to apply, i.e. whether to repeatedly fire the timer or just once.
    /// * `duration`: The duration from now until when the fire should fire.
    /// * `callback`: The function to call when the time has been reached or exceeded.
    pub fn start(&self, mode: TimerMode, duration: std::time::Duration, callback: TimerCallback) {
        CURRENT_TIMERS.with(|timers| {
            let mut timers = timers.borrow_mut();
            let id = timers.start_or_restart_timer(self.id.get(), mode, duration, callback);
            self.id.set(Some(id));
        })
    }

    /// Stops the previously started timer. Does nothing if the timer has never been started. A stopped
    /// timer cannot be restarted with restart() -- instead you need to call start().
    pub fn stop(&self) {
        if let Some(id) = self.id.take() {
            CURRENT_TIMERS.with(|timers| {
                timers.borrow_mut().remove_timer(id);
            });
        }
    }

    /// Restarts the timer, if it was previously started.
    pub fn restart(&self) {
        if let Some(id) = self.id.get() {
            CURRENT_TIMERS.with(|timers| {
                timers.borrow_mut().deactivate_timer(id);
                timers.borrow_mut().activate_timer(id);
            });
        }
    }

    /// Returns true if the timer is running; false otherwise.
    pub fn running(&self) -> bool {
        self.id
            .get()
            .map(|timer_id| CURRENT_TIMERS.with(|timers| timers.borrow().timers[timer_id].running))
            .unwrap_or(false)
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        if let Some(id) = self.id.get() {
            CURRENT_TIMERS.with(|timers| {
                timers.borrow_mut().remove_timer(id);
            })
        }
    }
}

struct TimerData {
    duration: std::time::Duration,
    mode: TimerMode,
    running: bool,
    callback: Option<TimerCallback>,
}

#[derive(Clone, Copy)]
struct ActiveTimer {
    id: usize,
    timeout: instant::Instant,
}

/// TimerList provides the interface to the event loop for activating times and
/// determining the nearest timeout.
pub struct TimerList {
    timers: vec_arena::Arena<TimerData>,
    active_timers: Vec<ActiveTimer>,
}

impl Default for TimerList {
    fn default() -> Self {
        Self { timers: Default::default(), active_timers: Vec::new() }
    }
}

impl TimerList {
    /// Returns the timeout of the timer that should fire the soonest, or None if there
    /// is no timer active.
    pub fn next_timeout() -> Option<instant::Instant> {
        CURRENT_TIMERS.with(|timers| {
            timers
                .borrow()
                .active_timers
                .first()
                .map(|first_active_timer| first_active_timer.timeout)
        })
    }

    /// Activates any expired timers by calling their callback function. Returns true if any timers were
    /// activated; false otherwise.
    pub fn maybe_activate_timers() -> bool {
        let now = instant::Instant::now();
        // Shortcut: Is there any timer worth activating?
        if TimerList::next_timeout().map(|timeout| now < timeout).unwrap_or(false) {
            return false;
        }

        CURRENT_TIMERS.with(|timers| {
            let mut any_activated = false;

            // The active timer list is cleared here and not-yet-fired ones are inserted below, in order to allow
            // timer callbacks to register their own timers.
            let timers_to_process = std::mem::take(&mut timers.borrow_mut().active_timers);
            for active_timer in timers_to_process.into_iter() {
                if active_timer.timeout <= now {
                    any_activated = true;

                    let callback = timers.borrow_mut().timers[active_timer.id].callback.take();
                    callback.as_ref().map(|cb| cb());

                    let mut timers = timers.borrow_mut();
                    timers.timers[active_timer.id].callback = callback;

                    if matches!(timers.timers[active_timer.id].mode, TimerMode::Repeated) {
                        timers.activate_timer(active_timer.id);
                    }
                } else {
                    timers.borrow_mut().register_active_timer(active_timer);
                }
            }

            any_activated
        })
    }

    fn start_or_restart_timer(
        &mut self,
        id: Option<usize>,
        mode: TimerMode,
        duration: std::time::Duration,
        callback: TimerCallback,
    ) -> usize {
        let timer_data = TimerData { duration, mode, running: false, callback: Some(callback) };
        let inactive_timer_id = if let Some(id) = id {
            self.deactivate_timer(id);
            self.timers[id] = timer_data;
            id
        } else {
            self.timers.insert(timer_data)
        };
        self.activate_timer(inactive_timer_id);
        inactive_timer_id
    }

    fn deactivate_timer(&mut self, id: usize) {
        let mut i = 0;
        while i < self.active_timers.len() {
            if self.active_timers[i].id == id {
                self.active_timers.remove(i);
                self.timers[id].running = false;
                break;
            } else {
                i += 1;
            }
        }
    }

    fn activate_timer(&mut self, timer_id: usize) {
        self.register_active_timer(ActiveTimer {
            id: timer_id,
            timeout: instant::Instant::now() + self.timers[timer_id].duration,
        });
    }

    fn register_active_timer(&mut self, new_active_timer: ActiveTimer) {
        let insertion_index = lower_bound(&self.active_timers, |existing_timer| {
            existing_timer.timeout < new_active_timer.timeout
        });

        self.active_timers.insert(insertion_index, new_active_timer);
        self.timers[new_active_timer.id].running = true;
    }

    fn remove_timer(&mut self, timer_id: usize) {
        self.deactivate_timer(timer_id);
        self.timers.remove(timer_id);
    }
}

thread_local!(static CURRENT_TIMERS : RefCell<TimerList> = RefCell::default());

fn lower_bound<T>(vec: &Vec<T>, mut less_than: impl FnMut(&T) -> bool) -> usize {
    let mut left = 0;
    let mut right = vec.len();

    while left != right {
        let mid = left + (right - left) / 2;
        let value = &vec[mid];
        if less_than(value) {
            left = mid + 1;
        } else {
            right = mid;
        }
    }

    left
}
