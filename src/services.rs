// The tools that work by themselves (reminders, calendar notifications, breaks, the shut down timer, watching the PC,
// tidying downloads, backups, updates, Jira) share one background thread that wakes up every few
// seconds and gives each turned-on tool a turn. Nothing else runs while they have nothing to do.

use crate::config;
use eframe::egui;
use std::time::Duration;

const TICK: Duration = Duration::from_secs(15);

/// `refresh_tray` rebuilds the tray menu, which shows countdowns (next break, shut down).
pub fn start(ctx: egui::Context, refresh_tray: impl Fn() + Send + 'static) {
    std::thread::spawn(move || {
        let mut last = std::time::SystemTime::now();
        let mut turns: u64 = 0;
        loop {
            turns += 1;
            let now = std::time::SystemTime::now();
            // A big jump means the computer was asleep: the tools that count time start over.
            let woke_up = now.duration_since(last).is_ok_and(|gap| gap > TICK * 4);
            last = now;
            // One tool failing (a library giving up on odd data) doesn't stop the others.
            let mut changed = false;
            let mut turn = |tick: &dyn Fn() -> bool| {
                changed |= std::panic::catch_unwind(std::panic::AssertUnwindSafe(tick))
                    .unwrap_or(false);
            };
            if config::enabled("reminders") {
                turn(&crate::reminders::tick);
            }
            if config::enabled("calendar") {
                turn(&crate::gcal::tick);
            }
            if config::enabled("breaks") {
                turn(&|| crate::breaks::tick(woke_up));
            }
            turn(&crate::power::tick);
            if config::enabled("monitor") {
                turn(&crate::monitor::tick);
            }
            if config::enabled("organize") {
                turn(&crate::organize::tick);
            }
            if config::enabled("backup") {
                turn(&crate::backup::tick);
            }
            if config::enabled("updates") {
                turn(&crate::updates::tick);
            }
            if config::enabled("jira") {
                turn(&crate::jira::tick);
            }
            if changed {
                ctx.request_repaint();
            }
            // The countdowns in the tray move once a minute.
            if changed || turns.is_multiple_of(4) {
                refresh_tray();
            }
            std::thread::sleep(TICK);
        }
    });
}
