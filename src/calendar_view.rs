// The Calendar screen: connect a Google account, see a day's events, and create, edit or delete
// them with their guests. Every call to Google runs in a thread; the screen stays responsive.

use crate::gcal::{self, Draft, Event};
use crate::theme;
use chrono::{Duration, Local, NaiveDate, NaiveTime, Timelike};
use eframe::egui;
use std::sync::mpsc::{Receiver, Sender, channel};

/// What a thread came back with.
enum Done {
    Connected(Result<String, String>),
    Day(NaiveDate, Result<Vec<Event>, String>),
    /// The Google client file was picked.
    Client(Result<(), String>),
    /// Saved, deleted or answered: the day is fetched again.
    Changed(Result<(), String>),
}

pub struct CalendarPage {
    tx: Sender<Done>,
    rx: Receiver<Done>,
    day: NaiveDate,
    events: Vec<Event>,
    /// The day `events` belong to, and when it was fetched.
    loaded: Option<(NaiveDate, std::time::Instant)>,
    loading: bool,
    connecting: bool,
    /// Saving, deleting or answering.
    busy: bool,
    error: Option<String>,
    quick: String,
    /// The event whose details are shown.
    open: Option<String>,
    form: Option<Form>,
}

impl CalendarPage {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        Self {
            tx,
            rx,
            day: Local::now().date_naive(),
            events: vec![],
            loaded: None,
            loading: false,
            connecting: false,
            busy: false,
            error: None,
            quick: String::new(),
            open: None,
            form: None,
        }
    }

    fn run(&self, ctx: &egui::Context, job: impl FnOnce() -> Done + Send + 'static) {
        let (tx, ctx) = (self.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let done = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job))
                .unwrap_or_else(|_| Done::Changed(Err("Something went wrong.".into())));
            let _ = tx.send(done);
            ctx.request_repaint();
        });
    }

    fn load(&mut self, ctx: &egui::Context) {
        if self.loading {
            return;
        }
        self.loading = true;
        let day = self.day;
        self.run(ctx, move || Done::Day(day, gcal::day(day)));
    }

    fn go_to(&mut self, ctx: &egui::Context, day: NaiveDate) {
        self.day = day;
        self.open = None;
        self.events.clear();
        self.loaded = None;
        self.loading = false;
        self.load(ctx);
    }

    fn poll(&mut self, ctx: &egui::Context) {
        while let Ok(done) = self.rx.try_recv() {
            match done {
                Done::Connected(result) => {
                    self.connecting = false;
                    match result {
                        Ok(_) => {
                            self.error = None;
                            let today = Local::now().date_naive();
                            self.go_to(ctx, today);
                        }
                        Err(error) => self.error = Some(error),
                    }
                }
                Done::Client(result) => self.error = result.err(),
                Done::Day(day, result) => {
                    self.loading = false;
                    if day != self.day {
                        // The user moved on meanwhile.
                        self.load(ctx);
                        continue;
                    }
                    match result {
                        Ok(events) => {
                            self.events = events;
                            self.error = None;
                        }
                        Err(error) => self.error = Some(error),
                    }
                    self.loaded = Some((day, std::time::Instant::now()));
                }
                Done::Changed(result) => {
                    self.busy = false;
                    match result {
                        Ok(()) => {
                            self.form = None;
                            self.error = None;
                            self.loaded = None;
                            self.load(ctx);
                        }
                        Err(error) => match self.form.as_mut() {
                            Some(form) => form.error = Some(error),
                            None => self.error = Some(error),
                        },
                    }
                }
            }
        }
    }

    /// Not connected yet: one big button.
    fn connect_ui(&mut self, ui: &mut egui::Ui) {
        ui.add_space(40.0);
        ui.vertical_centered(|ui| {
            ui.label(egui::RichText::new("📅").size(40.0).color(theme::FAINT));
            ui.add_space(6.0);
            ui.label(egui::RichText::new("Connect your Google Calendar").size(18.0).strong());
            ui.label(
                egui::RichText::new(
                    "See what's on today, add events and invite people, without opening the browser.",
                )
                .color(theme::MUTED),
            );
            ui.add_space(16.0);
            if !gcal::has_client() {
                self.client_ui(ui);
                return;
            }
            if self.connecting {
                ui.horizontal(|ui| {
                    ui.add_space((ui.available_width() - 260.0).max(0.0) / 2.0);
                    ui.spinner();
                    ui.label("Waiting for you in the browser…");
                    if ui.button("Cancel").clicked() {
                        gcal::cancel_connect();
                    }
                });
            } else if big_button(ui, "Connect Google account").clicked() {
                self.connecting = true;
                self.error = None;
                self.run(ui.ctx(), || Done::Connected(gcal::connect()));
            }
            ui.add_space(10.0);
            ui.label(
                egui::RichText::new("A Google page opens to pick the account. You can disconnect any time.")
                    .size(12.0)
                    .color(theme::FAINT),
            );
        });
    }

    /// A build without the Google client built in: take the file Google Cloud gives.
    fn client_ui(&mut self, ui: &mut egui::Ui) {
        ui.label(
            egui::RichText::new("This build has no Google sign-in set up yet.")
                .color(theme::WARNING),
        );
        ui.label(
            egui::RichText::new(
                "In Google Cloud, make an OAuth client of type \"Desktop app\" and pick the JSON file it downloads.",
            )
            .size(12.0)
            .color(theme::FAINT),
        );
        ui.add_space(8.0);
        if ui.button("Choose the client file…").clicked() {
            self.run(ui.ctx(), || {
                let picked = rfd::FileDialog::new()
                    .set_title("Google client file")
                    .add_filter("JSON", &["json"])
                    .pick_file();
                Done::Client(picked.map_or(Ok(()), |path| gcal::set_client_file(&path)))
            });
        }
    }

    /// The account, and the buttons to switch or leave it.
    fn account_bar(&mut self, ui: &mut egui::Ui, email: &str) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("●").color(theme::GREEN));
            ui.label(egui::RichText::new(email).color(theme::MUTED));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.connecting {
                    if ui.small_button("Cancel").clicked() {
                        gcal::cancel_connect();
                    }
                    ui.label(egui::RichText::new("Waiting for the browser…").color(theme::FAINT));
                    return;
                }
                if ui
                    .small_button("Disconnect")
                    .on_hover_text("toolswitch forgets this account")
                    .clicked()
                {
                    gcal::disconnect();
                    self.events.clear();
                    self.loaded = None;
                    self.form = None;
                    self.error = None;
                }
                if ui
                    .small_button("Switch account")
                    .on_hover_text("Connect another Google account instead")
                    .clicked()
                {
                    self.connecting = true;
                    self.error = None;
                    self.form = None;
                    self.run(ui.ctx(), || Done::Connected(gcal::connect()));
                }
            });
        });
    }

    /// ◀ Today ▶, the date, and New event.
    fn day_bar(&mut self, ui: &mut egui::Ui) {
        let today = Local::now().date_naive();
        ui.horizontal(|ui| {
            if ui.button("◀").on_hover_text("Previous day").clicked() {
                let day = self.day.pred_opt().unwrap_or(self.day);
                self.go_to(ui.ctx(), day);
            }
            if ui
                .add_enabled(self.day != today, egui::Button::new("Today"))
                .clicked()
            {
                self.go_to(ui.ctx(), today);
            }
            if ui.button("▶").on_hover_text("Next day").clicked() {
                let day = self.day.succ_opt().unwrap_or(self.day);
                self.go_to(ui.ctx(), day);
            }
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(day_name(self.day, today))
                    .size(16.0)
                    .strong(),
            );
            if self.loading {
                ui.spinner();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new("+ New event").color(egui::Color32::WHITE),
                        )
                        .fill(theme::ACCENT),
                    )
                    .clicked()
                {
                    self.form = Some(Form::new(self.day));
                }
                if ui.small_button("⟳").on_hover_text("Refresh").clicked() {
                    self.load(ui.ctx());
                }
            });
        });
    }

    /// "Lunch with Ana tomorrow 1pm" and Enter.
    fn quick_add(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let edit = ui.add(
                egui::TextEdit::singleline(&mut self.quick)
                    .hint_text("Quick add: Lunch with Ana tomorrow 1pm")
                    .desired_width(ui.available_width() - 60.0)
                    .margin(egui::Margin::symmetric(10, 7)),
            );
            let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let add = ui
                .add_enabled(
                    !self.busy && !self.quick.trim().is_empty(),
                    egui::Button::new("Add"),
                )
                .clicked();
            if (add || enter) && !self.quick.trim().is_empty() && !self.busy {
                let text = std::mem::take(&mut self.quick);
                self.busy = true;
                self.run(ui.ctx(), move || {
                    Done::Changed(gcal::quick_add(text.trim()))
                });
            }
        });
    }

    fn list(&mut self, ui: &mut egui::Ui) {
        if self.events.is_empty() {
            if self.loaded.is_some() {
                ui.add_space(30.0);
                ui.vertical_centered(|ui| {
                    ui.label(egui::RichText::new("Nothing on this day").size(16.0));
                    ui.label(
                        egui::RichText::new("Add an event with the button above.")
                            .color(theme::MUTED),
                    );
                });
            }
            return;
        }
        let events = self.events.clone();
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let now = Local::now();
                for event in &events {
                    self.event_card(ui, event, now);
                    ui.add_space(6.0);
                }
            });
    }

    fn event_card(&mut self, ui: &mut egui::Ui, event: &Event, now: chrono::DateTime<Local>) {
        let open = self.open.as_deref() == Some(event.id.as_str());
        let past = event.end < now && !event.all_day;
        let faded = past || event.declined();
        let text_color = if faded { theme::FAINT } else { theme::TEXT };
        let happening = event.start <= now && now < event.end && !event.all_day;
        let mut frame = theme::card().inner_margin(egui::Margin::symmetric(14, 10));
        if happening {
            frame = frame.stroke(egui::Stroke::new(1.0, theme::ACCENT));
        }
        let response = frame
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    // The time column.
                    ui.vertical(|ui| {
                        ui.set_width(56.0);
                        ui.spacing_mut().item_spacing.y = 0.0;
                        if event.all_day {
                            ui.label(
                                egui::RichText::new("All day")
                                    .size(13.0)
                                    .color(theme::ACCENT),
                            );
                        } else {
                            ui.label(
                                egui::RichText::new(event.start.format("%H:%M").to_string())
                                    .color(text_color),
                            );
                            ui.label(
                                egui::RichText::new(event.end.format("%H:%M").to_string())
                                    .size(12.0)
                                    .color(theme::FAINT),
                            );
                        }
                    });
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        let mut title =
                            egui::RichText::new(&event.title).color(text_color).strong();
                        if event.declined() {
                            title = title.strikethrough();
                        }
                        ui.add(egui::Label::new(title).truncate());
                        let mut details = vec![];
                        if happening {
                            details.push("Now".to_string());
                        }
                        if !event.location.is_empty() {
                            details.push(event.location.clone());
                        }
                        match event.guests.len() {
                            0 => {}
                            1 => details.push("1 guest".into()),
                            n => details.push(format!("{n} guests")),
                        }
                        if event.meet.is_some() {
                            details.push("Google Meet".into());
                        }
                        if event.my_response.as_deref() == Some("needsAction") {
                            details.push("Waiting for your answer".into());
                        }
                        if !details.is_empty() {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(details.join("  ·  "))
                                        .size(12.0)
                                        .color(theme::MUTED),
                                )
                                .truncate(),
                            );
                        }
                    });
                    if let Some(link) = &event.meet
                        && !past
                    {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .button("Join")
                                .on_hover_text("Open the Google Meet call")
                                .clicked()
                            {
                                theme::open_folder(std::path::Path::new(link));
                            }
                        });
                    }
                });
                if open {
                    ui.add_space(8.0);
                    self.details(ui, event);
                }
            })
            .response
            .interact(egui::Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        if response.clicked() {
            self.open = if open { None } else { Some(event.id.clone()) };
        }
    }

    /// Under an open card: notes, guests and what can be done with it.
    fn details(&mut self, ui: &mut egui::Ui, event: &Event) {
        ui.separator();
        if !event.description.is_empty() {
            let notes: String = event.description.chars().take(600).collect();
            ui.label(
                egui::RichText::new(strip_html(&notes))
                    .size(13.0)
                    .color(theme::MUTED),
            );
            ui.add_space(4.0);
        }
        for guest in &event.guests {
            ui.horizontal(|ui| {
                let (mark, color, hint) = match guest.response.as_str() {
                    "accepted" => ("✔", theme::GREEN, "Going"),
                    "declined" => ("✖", theme::FAINT, "Not going"),
                    "tentative" => ("?", theme::WARNING, "Maybe"),
                    _ => ("…", theme::FAINT, "No answer yet"),
                };
                ui.label(egui::RichText::new(mark).color(color))
                    .on_hover_text(hint);
                let mut name = if guest.name.is_empty() {
                    guest.email.clone()
                } else {
                    format!("{} <{}>", guest.name, guest.email)
                };
                if guest.organizer {
                    name.push_str(" · organizer");
                }
                if guest.me {
                    name.push_str(" · you");
                }
                ui.label(egui::RichText::new(name).size(13.0));
            });
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if let Some(response) = &event.my_response {
                ui.label(egui::RichText::new("Going?").color(theme::MUTED));
                for (value, label) in [
                    ("accepted", "Yes"),
                    ("tentative", "Maybe"),
                    ("declined", "No"),
                ] {
                    let chosen = response == value;
                    if ui
                        .add_enabled(!self.busy, egui::Button::new(label).selected(chosen))
                        .clicked()
                        && !chosen
                    {
                        self.busy = true;
                        let (event, value) = (event.clone(), value.to_string());
                        self.run(ui.ctx(), move || {
                            Done::Changed(gcal::respond(&event, &value))
                        });
                    }
                }
                ui.add_space(10.0);
            }
            if !event.read_only && ui.button("✏ Edit").clicked() {
                self.form = Some(Form::edit(event));
            }
            if !event.link.is_empty() && ui.button("Open in Google").clicked() {
                theme::open_folder(std::path::Path::new(&event.link));
            }
            if let Some(link) = &event.meet
                && ui.button("Copy Meet link").clicked()
            {
                ui.ctx().copy_text(link.clone());
            }
        });
    }
}

impl crate::Page for CalendarPage {
    fn opened(&mut self) {
        // From the shortcut or the sidebar: back to today, unless an event is being written.
        if self.form.is_none() {
            let today = Local::now().date_naive();
            if self.day != today {
                self.day = today;
                self.events.clear();
            }
            // Fetched again when it's a minute old; the drawing below does it.
            if self
                .loaded
                .is_some_and(|(_, at)| at.elapsed() > std::time::Duration::from_secs(60))
            {
                self.loaded = None;
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.poll(&ctx);
        theme::page_header_with_switch(
            ui,
            "Calendar",
            "Your Google Calendar: what's on today, and new events with guests. Warns 10 min before each one.",
            "calendar",
        );
        ui.add_space(12.0);

        let Some(email) = gcal::account() else {
            self.events.clear();
            self.loaded = None;
            self.connect_ui(ui);
            if let Some(error) = &self.error {
                ui.add_space(8.0);
                ui.vertical_centered(|ui| {
                    ui.label(egui::RichText::new(error).color(theme::WARNING))
                });
            }
            return;
        };
        if self.loaded.map(|(day, _)| day) != Some(self.day) && !self.loading {
            self.load(&ctx);
        }
        // Minutes go by: "Now" and the faded past events move along.
        ctx.request_repaint_after(std::time::Duration::from_secs(30));

        self.account_bar(ui, &email);
        ui.add_space(8.0);
        if let Some(error) = &self.error {
            ui.label(egui::RichText::new(error).color(theme::WARNING));
            ui.add_space(4.0);
        }

        if let Some(form) = self.form.as_mut() {
            match form.ui(ui, self.busy) {
                FormStep::Stay => {}
                FormStep::Cancel => self.form = None,
                FormStep::Save(draft) => {
                    self.busy = true;
                    let editing = form.editing.clone();
                    self.run(&ctx, move || {
                        Done::Changed(match editing {
                            Some(event) => gcal::update(&event, &draft),
                            None => gcal::create(&draft),
                        })
                    });
                }
                FormStep::Delete(notify) => {
                    if let Some(event) = form.editing.clone() {
                        self.busy = true;
                        self.run(&ctx, move || Done::Changed(gcal::delete(&event, notify)));
                    }
                }
            }
            return;
        }

        self.day_bar(ui);
        ui.add_space(8.0);
        self.quick_add(ui);
        ui.add_space(10.0);
        self.list(ui);
    }
}

// ---------- the event form ----------

enum FormStep {
    Stay,
    Cancel,
    Save(Draft),
    /// Delete it; true = tell the guests.
    Delete(bool),
}

struct Form {
    /// None for a new event.
    editing: Option<Event>,
    title: String,
    date: String,
    all_day: bool,
    start: String,
    end: String,
    location: String,
    description: String,
    guests: Vec<String>,
    guest_input: String,
    meet: bool,
    notify: bool,
    /// Delete was clicked once; the second click deletes.
    confirm_delete: bool,
    focus: bool,
    error: Option<String>,
}

impl Form {
    /// A new event on `day`: at the next full hour if it's today, else at 9, for an hour.
    fn new(day: NaiveDate) -> Form {
        let now = Local::now();
        let start = if day == now.date_naive() && now.hour() < 23 {
            NaiveTime::from_hms_opt(now.hour() + 1, 0, 0).unwrap_or(NaiveTime::MIN)
        } else {
            NaiveTime::from_hms_opt(9, 0, 0).unwrap_or(NaiveTime::MIN)
        };
        let end = start + Duration::hours(1);
        Form {
            editing: None,
            title: String::new(),
            date: day.format("%Y-%m-%d").to_string(),
            all_day: false,
            start: start.format("%H:%M").to_string(),
            end: end.format("%H:%M").to_string(),
            location: String::new(),
            description: String::new(),
            guests: vec![],
            guest_input: String::new(),
            meet: false,
            notify: true,
            confirm_delete: false,
            focus: true,
            error: None,
        }
    }

    fn edit(event: &Event) -> Form {
        Form {
            title: event.title.clone(),
            date: event.start.format("%Y-%m-%d").to_string(),
            all_day: event.all_day,
            start: event.start.format("%H:%M").to_string(),
            end: event.end.format("%H:%M").to_string(),
            location: event.location.clone(),
            description: event.description.clone(),
            guests: event
                .guests
                .iter()
                .filter(|g| !g.me)
                .map(|g| g.email.clone())
                .collect(),
            meet: event.meet.is_some(),
            focus: false,
            editing: Some(event.clone()),
            ..Form::new(event.start.date_naive())
        }
    }

    fn add_guest(&mut self) -> bool {
        let mut added = false;
        for email in self.guest_input.split([',', ';', ' ']) {
            let email = email.trim().trim_matches(['<', '>']);
            if gcal::is_email(email) && !self.guests.iter().any(|g| g.eq_ignore_ascii_case(email)) {
                self.guests.push(email.to_string());
                added = true;
            }
        }
        if added || self.guest_input.trim().is_empty() {
            self.guest_input.clear();
            return true;
        }
        false
    }

    /// What's written, checked; the problem otherwise.
    fn draft(&mut self) -> Result<Draft, String> {
        if !self.guest_input.trim().is_empty() && !self.add_guest() {
            return Err(format!(
                "\"{}\" isn't an email address.",
                self.guest_input.trim()
            ));
        }
        let title = self.title.trim();
        if title.is_empty() {
            return Err("Give it a title.".into());
        }
        let today = Local::now().date_naive();
        let day = gcal::parse_date(&self.date, today)
            .ok_or("The date should look like 2026-10-24 or 24/10.")?;
        let (start, end) = if self.all_day {
            let start = gcal::at(day, NaiveTime::MIN).ok_or("Bad date.")?;
            (start, start)
        } else {
            let start_time =
                gcal::parse_time(&self.start).ok_or("The start time should look like 9:30.")?;
            let end_time =
                gcal::parse_time(&self.end).ok_or("The end time should look like 10:30.")?;
            let start = gcal::at(day, start_time).ok_or("Bad time.")?;
            let mut end = gcal::at(day, end_time).ok_or("Bad time.")?;
            // 23:00 to 01:00 ends the next day.
            if end <= start {
                end += Duration::days(1);
            }
            (start, end)
        };
        Ok(Draft {
            title: title.to_string(),
            all_day: self.all_day,
            start,
            end,
            location: self.location.trim().to_string(),
            description: self.description.trim().to_string(),
            guests: self.guests.clone(),
            meet: self.meet,
            notify: self.notify && !self.guests.is_empty(),
        })
    }

    fn ui(&mut self, ui: &mut egui::Ui, busy: bool) -> FormStep {
        let mut step = FormStep::Stay;
        let today = Local::now().date_naive();
        ui.label(
            egui::RichText::new(if self.editing.is_some() {
                "Edit event"
            } else {
                "New event"
            })
            .size(17.0)
            .strong(),
        );
        ui.add_space(8.0);
        egui::ScrollArea::vertical()
            .auto_shrink([false, true])
            .max_height(ui.available_height() - 48.0)
            .show(ui, |ui| {
                egui::Grid::new("event-form")
                    .num_columns(2)
                    .spacing([14.0, 10.0])
                    .show(ui, |ui| {
                        let wide = (ui.available_width() - 80.0).max(200.0);
                        label(ui, "Title");
                        let title = ui.add(
                            egui::TextEdit::singleline(&mut self.title)
                                .hint_text("Meeting with Ana")
                                .desired_width(wide)
                                .margin(egui::Margin::symmetric(10, 7)),
                        );
                        if std::mem::take(&mut self.focus) {
                            title.request_focus();
                        }
                        ui.end_row();

                        label(ui, "Date");
                        ui.horizontal(|ui| {
                            ui.add(egui::TextEdit::singleline(&mut self.date).desired_width(110.0));
                            match gcal::parse_date(&self.date, today) {
                                Some(day) => {
                                    ui.label(
                                        egui::RichText::new(day_name(day, today))
                                            .color(theme::MUTED),
                                    );
                                }
                                None => {
                                    ui.label(
                                        egui::RichText::new("2026-10-24 or 24/10")
                                            .color(theme::WARNING),
                                    );
                                }
                            }
                            if ui.small_button("Today").clicked() {
                                self.date = today.format("%Y-%m-%d").to_string();
                            }
                            if ui.small_button("Tomorrow").clicked() {
                                self.date = today
                                    .succ_opt()
                                    .unwrap_or(today)
                                    .format("%Y-%m-%d")
                                    .to_string();
                            }
                        });
                        ui.end_row();

                        label(ui, "Time");
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut self.all_day, "All day");
                            if !self.all_day {
                                ui.add_space(8.0);
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.start).desired_width(56.0),
                                );
                                ui.label(egui::RichText::new("to").color(theme::MUTED));
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.end).desired_width(56.0),
                                );
                                for (name, minutes) in [("30 min", 30), ("1 h", 60), ("2 h", 120)] {
                                    if ui.small_button(name).clicked()
                                        && let Some(start) = gcal::parse_time(&self.start)
                                    {
                                        self.end = (start + Duration::minutes(minutes))
                                            .format("%H:%M")
                                            .to_string();
                                    }
                                }
                            }
                        });
                        ui.end_row();

                        label(ui, "Guests");
                        ui.vertical(|ui| {
                            let mut remove = None;
                            ui.horizontal_wrapped(|ui| {
                                for (i, guest) in self.guests.iter().enumerate() {
                                    if chip(ui, guest) {
                                        remove = Some(i);
                                    }
                                }
                            });
                            if let Some(i) = remove {
                                self.guests.remove(i);
                            }
                            let edit = ui.add(
                                egui::TextEdit::singleline(&mut self.guest_input)
                                    .hint_text("Add guests by email, then Enter")
                                    .desired_width(wide)
                                    .margin(egui::Margin::symmetric(10, 7)),
                            );
                            let typed_comma = self.guest_input.ends_with([',', ';']);
                            if (edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                                || typed_comma
                            {
                                if self.add_guest() {
                                    self.error = None;
                                } else {
                                    self.error = Some(format!(
                                        "\"{}\" isn't an email address.",
                                        self.guest_input.trim()
                                    ));
                                }
                                edit.request_focus();
                            }
                            if !self.guests.is_empty() {
                                ui.checkbox(&mut self.notify, "Email the guests about it");
                            }
                        });
                        ui.end_row();

                        label(ui, "Video call");
                        match self.editing.as_ref().and_then(|e| e.meet.clone()) {
                            Some(link) => {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        egui::RichText::new("Has a Google Meet link")
                                            .color(theme::MUTED),
                                    );
                                    if ui.small_button("Copy").clicked() {
                                        ui.ctx().copy_text(link);
                                    }
                                });
                            }
                            None => {
                                ui.checkbox(&mut self.meet, "Add a Google Meet link");
                            }
                        }
                        ui.end_row();

                        label(ui, "Where");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.location)
                                .hint_text("Place or address")
                                .desired_width(wide)
                                .margin(egui::Margin::symmetric(10, 7)),
                        );
                        ui.end_row();

                        label(ui, "Notes");
                        ui.add(
                            egui::TextEdit::multiline(&mut self.description)
                                .hint_text("Agenda, links…")
                                .desired_rows(3)
                                .desired_width(wide)
                                .margin(egui::Margin::symmetric(10, 7)),
                        );
                        ui.end_row();
                    });
            });

        if let Some(error) = &self.error {
            ui.label(egui::RichText::new(error).color(theme::WARNING));
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let save = ui.add_enabled(
                !busy,
                egui::Button::new(egui::RichText::new("Save").color(egui::Color32::WHITE))
                    .fill(theme::ACCENT),
            );
            let ctrl_enter = ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Enter));
            if save.clicked() || (ctrl_enter && !busy) {
                match self.draft() {
                    Ok(draft) => {
                        self.error = None;
                        step = FormStep::Save(draft);
                    }
                    Err(error) => self.error = Some(error),
                }
            }
            if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                step = FormStep::Cancel;
            }
            if busy {
                ui.spinner();
            }
            if self.editing.is_some() {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let text = if self.confirm_delete {
                        "Click again to delete"
                    } else {
                        "🗑 Delete"
                    };
                    if ui
                        .add_enabled(
                            !busy,
                            egui::Button::new(egui::RichText::new(text).color(theme::WARNING)),
                        )
                        .clicked()
                    {
                        if self.confirm_delete {
                            step = FormStep::Delete(!self.guests.is_empty() && self.notify);
                        }
                        self.confirm_delete = !self.confirm_delete;
                    }
                });
            }
        });
        step
    }
}

fn label(ui: &mut egui::Ui, text: &str) {
    ui.label(egui::RichText::new(text).color(theme::MUTED));
}

/// A guest's email with a ✖ to take it out. True when ✖ was clicked.
fn chip(ui: &mut egui::Ui, text: &str) -> bool {
    let mut remove = false;
    egui::Frame::new()
        .fill(theme::CARD_HOVER)
        .stroke(egui::Stroke::new(1.0, theme::BORDER))
        .corner_radius(10.0)
        .inner_margin(egui::Margin::symmetric(8, 3))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.label(egui::RichText::new(text).size(13.0));
                if ui
                    .add(
                        egui::Label::new(egui::RichText::new("✖").size(11.0).color(theme::FAINT))
                            .sense(egui::Sense::click()),
                    )
                    .on_hover_text("Remove")
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                {
                    remove = true;
                }
            });
        });
    remove
}

fn big_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(
        egui::Button::new(
            egui::RichText::new(text)
                .size(15.0)
                .color(egui::Color32::WHITE),
        )
        .fill(theme::ACCENT)
        .min_size(egui::vec2(220.0, 38.0))
        .corner_radius(10.0),
    )
}

/// "Today · Friday 23 October", "Tomorrow · …", or just "Monday 26 October".
fn day_name(day: NaiveDate, today: NaiveDate) -> String {
    let name = day.format("%A %-d %B").to_string();
    if day == today {
        format!("Today · {name}")
    } else if Some(day) == today.succ_opt() {
        format!("Tomorrow · {name}")
    } else if Some(day) == today.pred_opt() {
        format!("Yesterday · {name}")
    } else {
        name
    }
}

/// Google keeps notes as a bit of HTML: <br> becomes a new line, other tags go.
fn strip_html(text: &str) -> String {
    let text = text
        .replace("<br>", "\n")
        .replace("<br/>", "\n")
        .replace("<br />", "\n");
    let mut out = String::new();
    let mut in_tag = false;
    for c in text.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}
