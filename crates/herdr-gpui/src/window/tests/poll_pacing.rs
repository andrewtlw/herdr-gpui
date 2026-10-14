use super::*;
use crate::window::tick::{HIDDEN_TICK, SHOWN_TICK};
use gpui::{Entity, VisualTestContext, WindowVisibility};
use std::time::Duration;

/// A fixture window with the real poll loop, which the fixture leaves out.
fn polled_window(cx: &mut gpui::TestAppContext) -> (Entity<HerdrWindow>, &mut VisualTestContext) {
    let (view, cx) = cx.add_window_view(fixture_window);
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view._poll = HerdrWindow::spawn_poll(window, cx)
        });
    });
    // Start the loop, so its first timer counts from here.
    cx.run_until_parked();
    // The first ticks activate the surface, which leaves state for another
    // tick; settle that first.
    for _ in 0..8 {
        advance(cx, SHOWN_TICK);
    }
    assert!(!pending(&view, cx), "the fixture settles");
    (view, cx)
}

/// What the connection's event thread does when the daemon sends something.
fn daemon_update(view: &Entity<HerdrWindow>, cx: &mut VisualTestContext) {
    view.read_with(cx, |view, _| {
        view.endpoints[0].connection.inbox.lock().unwrap().dirty = true;
    });
}

fn pending(view: &Entity<HerdrWindow>, cx: &mut VisualTestContext) -> bool {
    view.read_with(cx, |view, _| view.endpoints[0].connection.has_update())
}

fn advance(cx: &mut VisualTestContext, by: Duration) {
    cx.executor().advance_clock(by);
    cx.run_until_parked();
}

/// A hidden window ticks slowly, and quickly again once it is shown.
#[gpui::test]
fn a_hidden_window_ticks_slowly_until_it_is_shown(cx: &mut gpui::TestAppContext) {
    let (view, cx) = polled_window(cx);
    cx.simulate_visibility_change(WindowVisibility::Hidden);
    // The tick already waiting may be a quick one; let it pass.
    advance(cx, SHOWN_TICK);

    daemon_update(&view, cx);
    advance(cx, SHOWN_TICK * 4);
    assert!(
        pending(&view, cx),
        "hidden, the window does not tick every frame"
    );
    advance(cx, HIDDEN_TICK);
    assert!(!pending(&view, cx), "but it still ticks");

    cx.simulate_visibility_change(WindowVisibility::Visible);
    // At most one slow tick is still waiting when the window is shown.
    advance(cx, HIDDEN_TICK);
    daemon_update(&view, cx);
    advance(cx, SHOWN_TICK);
    assert!(!pending(&view, cx), "shown again, it ticks every frame");
}
