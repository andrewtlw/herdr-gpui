//! The window's poll loop. Worker results, daemon state, and timed UI work
//! reach the window through [`HerdrWindow::tick`], which a timer runs while
//! the window exists and every display frame runs when an update is waiting,
//! so a new surface is shown on the refresh it arrives for.
//!
//! The timer slows down while nothing of the window is on screen: frames are
//! not drawn then, and nothing a tick still does needs frame latency.

use super::HerdrWindow;
use crate::{pace::Pace, state::LiveState};
use gpui::{Context, Task, WeakEntity, Window};
use std::time::{Duration, Instant};

/// While any of the window is on screen, the timer is as quick as a frame,
/// so background results and timed UI work keep their latency.
pub(super) const SHOWN_TICK: Duration = Duration::from_millis(16);
/// Nothing is drawn while the window is hidden. What a tick still does then,
/// such as ringing for a finished agent or saving layout, can wait this long.
pub(super) const HIDDEN_TICK: Duration = Duration::from_millis(250);

impl HerdrWindow {
    /// The timer half of the poll loop.
    pub(super) fn spawn_poll(window: &mut Window, cx: &mut Context<Self>) -> Task<()> {
        let timer = cx.background_executor().clone();
        cx.spawn_in(window, async move |this, cx| {
            let mut interval = SHOWN_TICK;
            loop {
                timer.timer(interval).await;
                let next = this.update_in(cx, |this, window, cx| {
                    this.tick(window, cx);
                    if window.is_visible() {
                        SHOWN_TICK
                    } else {
                        HIDDEN_TICK
                    }
                });
                match next {
                    Ok(next) => interval = next,
                    Err(_) => break,
                }
            }
        })
    }

    /// Runs every display frame while the window draws, so a new surface is
    /// shown on the refresh it arrives for instead of on the next timer tick.
    pub(super) fn poll_on_frame(this: WeakEntity<Self>, window: &mut Window) {
        window.on_next_frame(move |window, cx| {
            let alive = this.update(cx, |view, cx| {
                if view
                    .endpoints
                    .iter()
                    .any(|endpoint| endpoint.connection.has_update())
                    || view.group_terminals_updated()
                {
                    view.tick(window, cx);
                }
            });
            if alive.is_ok() {
                Self::poll_on_frame(this, window);
            }
        });
    }

    pub(super) fn tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pace = Pace::of(window);
        self.updater.set_channel(self.config.updates.channel);
        if self.updater.poll() {
            match self.updater.commit_restart() {
                Ok(true) => {
                    cx.quit();
                    return;
                }
                Ok(false) => {}
                Err(error) => eprintln!("App update restart failed: {error}"),
            }
            cx.notify();
        }
        if self.avatars.as_mut().is_some_and(|avatars| avatars.poll()) {
            cx.notify();
        }
        if let Some(chrome) = self.sidebar_preferences.as_mut().and_then(|p| p.loaded()) {
            self.apply_stored_chrome(chrome);
            cx.notify();
        }
        let old_pane = self
            .live
            .snapshot
            .as_ref()
            .and_then(|s| s.focused_pane_id.clone());
        let focused_tab = |live: &LiveState| {
            live.snapshot
                .as_ref()
                .map(|s| (s.focused_workspace_id.clone(), s.focused_tab_id.clone()))
        };
        let old_tab = focused_tab(&self.live);
        self.poll_endpoints(cx);
        if self
            .icon_font_notice
            .observe(self.config.icon_font_missing, self.live.surface.as_deref())
        {
            cx.notify();
        }
        self.post_system_notifications(window, cx);
        self.ring_bell(window);
        self.poll_integrations(cx);
        self.poll_links(window, cx);
        if self.settings.task.is_none() {
            let mut reload = false;
            for endpoint in &self.endpoints {
                reload |= endpoint.connection.take_settings_reload();
            }
            if reload {
                self.load_shared_settings(cx);
            }
        }
        // Switching Herdr tabs, from a shortcut, an agent, or the sidebar,
        // moves the group holding the window's connection to the new tab, or
        // brings the terminal back from behind a page. Arriving in another
        // workspace does the same for that workspace's group, so its tab is
        // the one just navigated to.
        if let (Some((old_workspace, old_tab)), Some((workspace, Some(tab)))) =
            (old_tab, focused_tab(&self.live))
            && (old_workspace != workspace || old_tab.as_ref() != Some(&tab))
        {
            self.terminal_focus_moved(&tab, old_workspace != workspace, cx);
        }
        // After the focus check: a swap replaces `live` with another
        // connection's, which is not a move of this one's focus.
        if self.poll_group_terminals() {
            self.redraw_terminal(cx);
        }
        self.reconcile_group_terminals(cx);
        // After polling: a connection that just got its first snapshot, or a
        // reconnect, is told the theme without waiting for it to change.
        self.sync_host_theme();
        self.save_group_layouts(cx);
        self.poll_browser(window, cx);
        self.poll_remote_browser(window, cx);
        self.offer_browser_skill(window, cx);
        self.poll_sessions(cx);
        self.flush_scrollbar(cx);
        self.flush_split(cx);
        self.poll_find(window, cx);
        self.follow_terminal_note(window, cx);
        self.follow_selection(cx);
        self.poll_copy_mode(window, cx);
        #[cfg(target_os = "macos")]
        crate::app_badge::sync(window.window_handle().window_id(), &self.endpoints, cx);
        self.observe_agent_work(window, cx);
        self.cancel_stale_image();
        self.poll_file_transfer(cx);
        self.update_workspace_dialog(window, cx);
        self.poll_teleport(window, cx);
        self.poll_carry(window, cx);
        self.poll_fan_out(window, cx);
        self.poll_dispatch(window, cx);
        self.poll_device_setup(window, cx);
        self.poll_worktree_script(window, cx);
        self.poll_editor_open(cx);
        self.poll_worktree_source(cx);
        self.poll_hover_menu(Instant::now(), window, cx);
        if self.tick_flash(Instant::now()) {
            cx.notify();
        }
        self.poll_tab_rename(window, cx);
        self.poll_pane_rename(window, cx);
        self.poll_pane_processes(cx);
        if old_pane
            != self
                .live
                .snapshot
                .as_ref()
                .and_then(|s| s.focused_pane_id.clone())
        {
            self.discard_composition(cx);
        }
        self.poll_github(window, cx);
        if self.update_workspace_pr() {
            cx.notify();
        }
        self.update_merged_cleanup(window, cx);
        if self.update_git() {
            cx.notify();
        }
        if self.update_usage(cx) {
            cx.notify();
        }
        self.poll_devices_overview(cx);
        if self.update_system_load(pace) {
            cx.notify();
        }
        self.update_checkpoints(cx);
        self.update_port_forwards(cx);
        if self.update_listening_ports(pace) {
            cx.notify();
        }
        if self.live.missing_installation && !self.install_warning_shown {
            self.install_warning_shown = true;
            self.show_install_modal(window, cx);
        }
        self.announce_version_mismatch(window, cx);
        self.resize();
        self.refresh_palette(window, cx);
        self.report_focus();
        self.sync_window_title(window);
    }
}
