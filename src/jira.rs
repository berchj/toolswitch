// Jira: a summary of the issues assigned to you that aren't done yet, from Jira Cloud. You
// connect it once with your site, your email and an API token. On, it checks every 15 minutes
// and tells you when something new is assigned to you. Talks to Jira with curl, like models.rs.

use crate::{config, desktop, theme};
use chrono::{DateTime, Local, NaiveDate};
use eframe::egui;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

const FILE: &str = "jira.json";
const CHECK_EVERY: i64 = 15 * 60;
const TOKEN_PAGE: &str = "https://id.atlassian.com/manage-profile/security/api-tokens";
const JQL: &str =
    "assignee = currentUser() AND statusCategory != Done ORDER BY priority DESC, updated DESC";
/// Enough for anyone's to-do list; more pages than this and the rest is left out.
const MAX_PAGES: usize = 5;

#[derive(Default, Clone, Serialize, Deserialize)]
#[serde(default)]
struct Saved {
    /// "https://yourcompany.atlassian.net"
    site: String,
    email: String,
    token: String,
    /// The issues seen last time, to tell which ones are new.
    known: Vec<String>,
    last_check: i64,
}

impl Saved {
    fn connected(&self) -> bool {
        !self.site.is_empty() && !self.email.is_empty() && !self.token.is_empty()
    }
}

#[derive(Clone, PartialEq)]
enum Stage {
    Todo,
    InProgress,
}

#[derive(Clone)]
struct Issue {
    key: String,
    summary: String,
    status: String,
    stage: Stage,
    priority: String,
    kind: String,
    project: String,
    due: Option<NaiveDate>,
    updated: i64,
}

#[derive(Default)]
struct State {
    saved: Saved,
    /// Who the token belongs to.
    me: String,
    issues: Vec<Issue>,
    error: Option<String>,
    /// This session got the list at least once.
    loaded: bool,
}

/// The settings hold the API token: only this user may read the file.
fn save(saved: &Saved) {
    config::write(FILE, saved);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(
            config::dir().join(FILE),
            std::fs::Permissions::from_mode(0o600),
        );
    }
}

fn state() -> MutexGuard<'static, State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE
        .get_or_init(|| {
            Mutex::new(State {
                saved: config::read(FILE),
                ..Default::default()
            })
        })
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

static BUSY: AtomicBool = AtomicBool::new(false);

/// "yourcompany", "yourcompany.atlassian.net" or the whole address, as "https://…".
fn site_url(input: &str) -> String {
    let host = input
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or_default()
        .to_lowercase();
    if host.is_empty() {
        String::new()
    } else if host.contains('.') {
        format!("https://{host}")
    } else {
        format!("https://{host}.atlassian.net")
    }
}

/// A Jira API call. The email and token go to curl through its standard input, so they don't
/// show up in the list of running programs.
fn call(
    saved: &Saved,
    path: &str,
    body: Option<&serde_json::Value>,
) -> Result<serde_json::Value, String> {
    let quote = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let mut curl = desktop::command("curl");
    curl.args(["-sS", "--max-time", "20", "-K", "-", "-w", "\n%{http_code}"])
        .args(["-H", "Accept: application/json"]);
    if let Some(body) = body {
        curl.args([
            "-X",
            "POST",
            "-H",
            "Content-Type: application/json",
            "--data",
        ])
        .arg(body.to_string());
    }
    curl.arg(format!("{}{path}", saved.site))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = curl.spawn().map_err(|e| format!("curl: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = writeln!(
            stdin,
            "user = \"{}:{}\"",
            quote(&saved.email),
            quote(&saved.token)
        );
    }
    let out = child.wait_with_output().map_err(|e| format!("curl: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    let (body, code) = text.rsplit_once('\n').unwrap_or(("", &text));
    match code.trim() {
        "200" => serde_json::from_str(body).map_err(|e| format!("Jira sent something odd: {e}")),
        "000" | "" => Err("Could not reach Jira. Are you online, and is the site right?".into()),
        "401" | "403" => Err("Jira didn't accept the email or the API token.".into()),
        "404" => Err("That site doesn't look like a Jira site.".into()),
        code => Err(format!("Jira answered with error {code}.")),
    }
}

fn text(value: &serde_json::Value, path: &str) -> String {
    value
        .pointer(path)
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string()
}

fn issue(raw: &serde_json::Value) -> Issue {
    let f = &raw["fields"];
    let updated = DateTime::parse_from_str(&text(f, "/updated"), "%Y-%m-%dT%H:%M:%S%.f%z")
        .map_or(0, |at| at.timestamp());
    Issue {
        key: text(raw, "/key"),
        summary: text(f, "/summary"),
        status: text(f, "/status/name"),
        stage: if text(f, "/status/statusCategory/key") == "indeterminate" {
            Stage::InProgress
        } else {
            Stage::Todo
        },
        priority: text(f, "/priority/name"),
        kind: text(f, "/issuetype/name"),
        project: text(f, "/project/name"),
        due: NaiveDate::parse_from_str(&text(f, "/duedate"), "%Y-%m-%d").ok(),
        updated,
    }
}

/// Who you are and your open issues.
fn fetch(saved: &Saved) -> Result<(String, Vec<Issue>), String> {
    // A search without a valid login answers with nothing instead of an error, so this goes first.
    let me = call(saved, "/rest/api/3/myself", None)?;
    let mut issues = vec![];
    let mut next: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut body = serde_json::json!({
            "jql": JQL,
            "fields": ["summary", "status", "priority", "issuetype", "project", "duedate", "updated"],
            "maxResults": 100,
        });
        if let Some(token) = &next {
            body["nextPageToken"] = token.clone().into();
        }
        let page = call(saved, "/rest/api/3/search/jql", Some(&body))?;
        if let Some(found) = page["issues"].as_array() {
            issues.extend(found.iter().map(issue));
        }
        next = page["nextPageToken"].as_str().map(str::to_string);
        if next.is_none() || page["isLast"].as_bool() == Some(true) {
            break;
        }
    }
    Ok((text(&me, "/displayName"), issues))
}

/// Gets the list in a thread. `connecting`: these are new details, saved only if they work.
/// `tell`: a notification for each newly assigned issue.
fn refresh(saved: Saved, connecting: bool, tell: bool, ctx: Option<egui::Context>) {
    if BUSY.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(move || {
        let result = fetch(&saved);
        let mut new = vec![];
        {
            let mut s = state();
            match result {
                Ok((me, issues)) => {
                    if connecting {
                        s.saved = saved;
                        s.saved.known.clear();
                    }
                    // The first list after connecting is all "new": nothing to tell.
                    if !s.saved.known.is_empty() {
                        new = issues
                            .iter()
                            .filter(|i| !s.saved.known.contains(&i.key))
                            .cloned()
                            .collect();
                    }
                    s.saved.known = issues.iter().map(|i| i.key.clone()).collect();
                    s.saved.last_check = Local::now().timestamp();
                    save(&s.saved);
                    s.me = me;
                    s.issues = issues;
                    s.error = None;
                    s.loaded = true;
                }
                Err(error) => s.error = Some(error),
            }
        }
        if tell {
            match new.as_slice() {
                [] => {}
                [one] => desktop::notify(&format!("☑ Assigned to you: {}", one.key), &one.summary),
                many => desktop::notify(
                    &format!("☑ {} new Jira issues for you", many.len()),
                    &many
                        .iter()
                        .map(|i| format!("{} {}", i.key, i.summary))
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
            }
        }
        BUSY.store(false, Ordering::SeqCst);
        if let Some(ctx) = ctx {
            ctx.request_repaint();
        }
    });
}

pub fn tick() -> bool {
    let saved = state().saved.clone();
    let due = saved.connected() && Local::now().timestamp() - saved.last_check >= CHECK_EVERY;
    if due {
        refresh(saved, false, true, None);
    }
    due
}

fn open(url: &str) {
    // The same program that shows a folder opens a web address in the browser.
    theme::open_folder(std::path::Path::new(url));
}

#[derive(Default)]
pub struct JiraPage {
    looked: bool,
    /// The connect form is up: not connected yet, or changing the account.
    editing: bool,
    /// Connect was clicked and Jira hasn't answered yet.
    connecting: bool,
    site: String,
    email: String,
    token: String,
}

impl JiraPage {
    fn connect_form(&mut self, ui: &mut egui::Ui, busy: bool, error: Option<&str>) {
        let ctx = ui.ctx().clone();
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(egui::RichText::new("Connect to Jira").size(18.0).strong());
            ui.label(
                egui::RichText::new(
                    "Use your Jira Cloud site, the email you sign in with and an API token. They stay on this computer.",
                )
                .size(13.0)
                .color(theme::MUTED),
            );
            ui.add_space(8.0);
            let field = |ui: &mut egui::Ui, name: &str, value: &mut String, hint: &str, password: bool| {
                ui.label(egui::RichText::new(name).size(13.0).color(theme::MUTED));
                ui.add(
                    egui::TextEdit::singleline(value)
                        .hint_text(hint)
                        .password(password)
                        .desired_width(f32::INFINITY)
                        .margin(egui::Margin::symmetric(10, 8)),
                );
            };
            field(ui, "Site", &mut self.site, "yourcompany.atlassian.net", false);
            field(ui, "Email", &mut self.email, "you@company.com", false);
            field(ui, "API token", &mut self.token, "Paste it here", true);
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let ready = !busy
                    && !site_url(&self.site).is_empty()
                    && self.email.contains('@')
                    && !self.token.trim().is_empty();
                if ui
                    .add_enabled(
                        ready,
                        egui::Button::new(egui::RichText::new("Connect").color(egui::Color32::WHITE))
                            .fill(theme::ACCENT),
                    )
                    .clicked()
                {
                    let saved = Saved {
                        site: site_url(&self.site),
                        email: self.email.trim().into(),
                        token: self.token.trim().into(),
                        ..Default::default()
                    };
                    refresh(saved, true, false, Some(ctx.clone()));
                    self.connecting = true;
                }
                if ui.button("🔗 Create an API token").clicked() {
                    open(TOKEN_PAGE);
                }
                if state().saved.connected() && ui.button("Cancel").clicked() {
                    self.editing = false;
                }
                if busy {
                    ui.add(egui::Spinner::new().color(theme::ACCENT));
                }
            });
            if let Some(error) = error {
                ui.label(egui::RichText::new(format!("✖ {error}")).color(theme::WARNING));
            }
        });
    }

    fn summary(&mut self, ui: &mut egui::Ui, busy: bool) {
        let ctx = ui.ctx().clone();
        let (saved, me, issues, error) = {
            let s = state();
            (
                s.saved.clone(),
                s.me.clone(),
                s.issues.clone(),
                s.error.clone(),
            )
        };
        let today = Local::now().date_naive();
        let in_progress = issues
            .iter()
            .filter(|i| i.stage == Stage::InProgress)
            .count();
        let overdue = issues
            .iter()
            .filter(|i| i.due.is_some_and(|d| d < today))
            .count();
        let this_week = issues
            .iter()
            .filter(|i| {
                i.due
                    .is_some_and(|d| d >= today && (d - today).num_days() < 7)
            })
            .count();

        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let title = match issues.len() {
                    _ if busy && issues.is_empty() => "Looking at Jira…".to_string(),
                    0 => "✔ Nothing assigned to you".to_string(),
                    1 => "1 open issue".to_string(),
                    n => format!("{n} open issues"),
                };
                ui.label(egui::RichText::new(title).size(18.0).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_enabled(!busy, egui::Button::new("Account"))
                        .clicked()
                    {
                        self.site = saved.site.trim_start_matches("https://").into();
                        self.email = saved.email.clone();
                        self.token.clear();
                        self.editing = true;
                    }
                    if ui
                        .add_enabled(!busy, egui::Button::new("⟳ Refresh"))
                        .clicked()
                    {
                        refresh(saved.clone(), false, false, Some(ctx.clone()));
                    }
                    if busy {
                        ui.add(egui::Spinner::new().color(theme::ACCENT));
                    }
                });
            });
            let mut facts = vec![
                format!("{in_progress} in progress"),
                format!("{} to do", issues.len() - in_progress),
            ];
            if overdue > 0 {
                facts.push(format!("⚠ {overdue} overdue"));
            }
            if this_week > 0 {
                facts.push(format!("{this_week} due this week"));
            }
            ui.label(
                egui::RichText::new(facts.join("  ·  ")).color(if overdue > 0 {
                    theme::WARNING
                } else {
                    theme::MUTED
                }),
            );
            let who = if me.is_empty() {
                saved.email.clone()
            } else {
                me
            };
            let mut line = format!("{who} · {}", saved.site.trim_start_matches("https://"));
            if saved.last_check > 0 {
                line += &format!(" · checked {}", crate::reminders::when(saved.last_check));
            }
            ui.label(egui::RichText::new(line).size(12.0).color(theme::FAINT));
            if let Some(error) = &error {
                ui.label(egui::RichText::new(format!("✖ {error}")).color(theme::WARNING));
            }
        });

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for (stage, name) in [(Stage::InProgress, "IN PROGRESS"), (Stage::Todo, "TO DO")] {
                    let group: Vec<&Issue> = issues.iter().filter(|i| i.stage == stage).collect();
                    if group.is_empty() {
                        continue;
                    }
                    ui.add_space(14.0);
                    ui.label(egui::RichText::new(name).size(11.0).color(theme::FAINT));
                    for issue in group {
                        if row(ui, issue, today).clicked() {
                            open(&format!("{}/browse/{}", saved.site, issue.key));
                        }
                    }
                }
            });
    }
}

/// One issue: key and summary on the left, due date, priority and status on the right.
fn row(ui: &mut egui::Ui, issue: &Issue, today: NaiveDate) -> egui::Response {
    let response = theme::card()
        .inner_margin(egui::Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.set_max_width(ui.available_width() - 230.0);
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(&issue.key)
                                .strong()
                                .color(theme::ACCENT),
                        );
                        ui.label(egui::RichText::new(&issue.summary));
                    });
                    let mut about = vec![issue.project.clone(), issue.kind.clone()];
                    if issue.updated > 0 {
                        about.push(format!("updated {}", crate::reminders::when(issue.updated)));
                    }
                    ui.label(
                        egui::RichText::new(about.join(" · "))
                            .size(12.0)
                            .color(theme::FAINT),
                    );
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new(&issue.status)
                            .size(12.0)
                            .color(theme::MUTED),
                    );
                    if !issue.priority.is_empty() {
                        ui.label(
                            egui::RichText::new(&issue.priority)
                                .size(12.0)
                                .color(theme::FAINT),
                        );
                    }
                    if let Some(due) = issue.due {
                        let (text, color) = if due < today {
                            (format!("⚠ due {}", due.format("%d %b")), theme::WARNING)
                        } else {
                            (format!("📅 {}", due.format("%d %b")), theme::MUTED)
                        };
                        ui.label(egui::RichText::new(text).size(12.0).color(color));
                    }
                });
            });
        })
        .response;
    let response = response.interact(egui::Sense::click());
    if response.hovered() {
        ui.painter().rect_stroke(
            response.rect,
            theme::RADIUS,
            egui::Stroke::new(1.0, theme::BORDER_HOVER),
            egui::StrokeKind::Inside,
        );
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

impl crate::Page for JiraPage {
    fn opened(&mut self) {
        self.looked = false;
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let (connected, loaded, error) = {
            let s = state();
            (s.saved.connected(), s.loaded, s.error.clone())
        };
        // Each time the screen opens it gets the list again, unless it's from a moment ago.
        if !self.looked {
            self.looked = true;
            let saved = state().saved.clone();
            let fresh = loaded && Local::now().timestamp() - saved.last_check < 60;
            if connected && !fresh {
                refresh(saved, false, false, Some(ctx.clone()));
            }
        }
        let busy = BUSY.load(Ordering::SeqCst);
        if busy {
            ctx.request_repaint_after(std::time::Duration::from_millis(300));
        }
        theme::page_header_with_switch(
            ui,
            "Jira",
            "The issues assigned to you that aren't done yet. On, it checks every 15 minutes and tells you when you get a new one.",
            "jira",
        );
        ui.add_space(16.0);
        if !connected || self.editing {
            self.connect_form(ui, busy, error.as_deref());
            // Connected with the new details: back to the list.
            if self.connecting && !busy {
                self.connecting = false;
                if error.is_none() {
                    self.editing = false;
                    self.token.clear();
                }
            }
        } else {
            self.summary(ui, busy);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn site_urls() {
        assert_eq!(site_url("acme"), "https://acme.atlassian.net");
        assert_eq!(
            site_url(" acme.atlassian.net "),
            "https://acme.atlassian.net"
        );
        assert_eq!(
            site_url("https://Acme.atlassian.net/jira/your-work"),
            "https://acme.atlassian.net"
        );
        assert_eq!(site_url("jira.acme.com"), "https://jira.acme.com");
        assert_eq!(site_url(""), "");
    }

    #[test]
    fn reads_an_issue() {
        let raw = serde_json::json!({
            "key": "CT-12",
            "fields": {
                "summary": "Fix login",
                "status": {"name": "En curso", "statusCategory": {"key": "indeterminate"}},
                "priority": {"name": "High"},
                "issuetype": {"name": "Bug"},
                "project": {"name": "Portal"},
                "duedate": "2026-10-30",
                "updated": "2026-10-20T12:34:56.000-0300"
            }
        });
        let i = issue(&raw);
        assert_eq!(i.key, "CT-12");
        assert!(i.stage == Stage::InProgress);
        assert_eq!(i.due, NaiveDate::from_ymd_opt(2026, 10, 30));
        assert!(i.updated > 0);
        let empty = issue(&serde_json::json!({"key": "CT-1", "fields": {}}));
        assert!(empty.stage == Stage::Todo && empty.due.is_none());
    }
}
