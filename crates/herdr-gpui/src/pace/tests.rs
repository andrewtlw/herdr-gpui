#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;
use gpui::{Empty, Render, TestAppContext, WindowVisibility};
use std::{
    sync::{Arc, mpsc},
    thread,
};

/// Long enough that a wait ending inside a test can only have been woken.
const FOREVER: Duration = Duration::from_secs(3600);
/// Bounds every wait on another thread, so a missed wakeup fails, not hangs.
const BOUND: Duration = Duration::from_secs(10);

const INTERVALS: Intervals = Intervals {
    active: Duration::ZERO,
    shown: FOREVER,
    hidden: FOREVER,
};

fn waiter(schedule: &Arc<Schedule>) -> mpsc::Receiver<bool> {
    let (sender, result) = mpsc::channel();
    let schedule = schedule.clone();
    thread::spawn(move || {
        let _ = sender.send(schedule.wait(Instant::now(), |pace| INTERVALS.at(pace)));
    });
    result
}

#[test]
fn each_pace_has_its_own_interval() {
    let intervals = Intervals {
        active: Duration::from_secs(1),
        shown: Duration::from_secs(2),
        hidden: Duration::from_secs(3),
    };
    assert_eq!(intervals.at(Pace::Active), Duration::from_secs(1));
    assert_eq!(intervals.at(Pace::Shown), Duration::from_secs(2));
    assert_eq!(intervals.at(Pace::Hidden), Duration::from_secs(3));
}

#[test]
fn a_due_run_starts_at_once() {
    let schedule = Schedule::new(Pace::Hidden);
    let long_ago = Instant::now() - Duration::from_secs(2);
    assert!(schedule.wait(long_ago, |_| Duration::from_secs(1)));
}

#[test]
fn a_stopped_worker_wakes_at_once_and_stays_stopped() {
    let schedule = Arc::new(Schedule::new(Pace::Hidden));
    let result = waiter(&schedule);
    schedule.stop();
    assert!(!result.recv_timeout(BOUND).unwrap());
    assert!(!schedule.wait(Instant::now(), |_| Duration::ZERO));
}

#[test]
fn a_faster_pace_ends_a_long_hidden_wait() {
    let schedule = Arc::new(Schedule::new(Pace::Hidden));
    let result = waiter(&schedule);
    schedule.set_pace(Pace::Active);
    assert!(
        result.recv_timeout(BOUND).unwrap(),
        "the window came back, so the monitor runs now"
    );
}

#[test]
fn an_unchanged_or_slower_pace_keeps_waiting() {
    let schedule = Arc::new(Schedule::new(Pace::Shown));
    let result = waiter(&schedule);
    schedule.set_pace(Pace::Shown);
    schedule.set_pace(Pace::Hidden);
    assert!(
        result.recv_timeout(Duration::from_millis(200)).is_err(),
        "nothing became due"
    );
    schedule.stop();
    assert!(!result.recv_timeout(BOUND).unwrap());
}

struct Blank;

impl Render for Blank {
    fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl gpui::IntoElement {
        Empty
    }
}

#[gpui::test]
fn a_window_is_paced_by_focus_then_visibility(cx: &mut TestAppContext) {
    let (_, cx) = cx.add_window_view(|_, _| Blank);
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    assert_eq!(cx.update(|window, _| Pace::of(window)), Pace::Active);
    cx.deactivate_window();
    assert_eq!(cx.update(|window, _| Pace::of(window)), Pace::Shown);
    cx.simulate_visibility_change(WindowVisibility::Hidden);
    assert_eq!(cx.update(|window, _| Pace::of(window)), Pace::Hidden);
    cx.simulate_visibility_change(WindowVisibility::Visible);
    assert_eq!(cx.update(|window, _| Pace::of(window)), Pace::Shown);
}
