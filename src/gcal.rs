// Google Calendar: the day's events, and creating, editing and deleting them with their guests.
// One Google account at a time. Signing in happens in the browser (OAuth with PKCE and a
// redirect to a port on this computer); the tokens are kept in calendar.json, readable only by
// the user. Requests go through curl, like the model downloads, so no HTTP library is needed.
//
// The app's Google client (made in Google Cloud, type "Desktop app") comes built in through
// TOOLSWITCH_GOOGLE_CLIENT_ID / TOOLSWITCH_GOOGLE_CLIENT_SECRET at build time, or from
// google-client.json in the config folder: the JSON file Google Cloud downloads.

use crate::{config, desktop};
use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, NaiveTime, TimeZone};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Digest;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

const FILE: &str = "calendar.json";
const CLIENT_FILE: &str = "google-client.json";
const SCOPES: &str = "openid email https://www.googleapis.com/auth/calendar.events";
const EVENTS_SCOPE: &str = "https://www.googleapis.com/auth/calendar.events";
const API: &str = "https://www.googleapis.com/calendar/v3/calendars/primary/events";
/// How long the browser sign-in may take.
const SIGN_IN_WAIT: std::time::Duration = std::time::Duration::from_secs(300);
/// The background check of today's events, for the notifications.
const REFRESH_EVERY: i64 = 5 * 60;
/// A notification this long before an event starts.
const NOTIFY_BEFORE: i64 = 10 * 60;

// ---------- the account ----------

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct Account {
    email: String,
    refresh_token: String,
    access_token: String,
    /// Unix time, in seconds.
    expires_at: i64,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct State {
    account: Option<Account>,
    /// The day the "today you have…" notification was shown, as "2026-10-23".
    summary_day: String,
}

fn state() -> MutexGuard<'static, State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE
        .get_or_init(|| Mutex::new(config::read(FILE)))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// The tokens let anyone into the calendar: only the user can read the file.
fn save(state: &State) {
    config::write(FILE, state);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(
            config::dir().join(FILE),
            std::fs::Permissions::from_mode(0o600),
        );
    }
}

/// The connected account's email.
pub fn account() -> Option<String> {
    state().account.as_ref().map(|a| a.email.clone())
}

/// The Google client this build signs in with: (id, secret).
fn client() -> Option<(String, String)> {
    if let (Some(id), Some(secret)) = (
        option_env!("TOOLSWITCH_GOOGLE_CLIENT_ID"),
        option_env!("TOOLSWITCH_GOOGLE_CLIENT_SECRET"),
    ) {
        return Some((id.into(), secret.into()));
    }
    let raw = std::fs::read(config::dir().join(CLIENT_FILE)).ok()?;
    let file: Value = serde_json::from_slice(&raw).ok()?;
    // As downloaded from Google Cloud: {"installed": {"client_id": ..., "client_secret": ...}}
    let fields = file.get("installed").unwrap_or(&file);
    Some((
        fields.get("client_id")?.as_str()?.into(),
        fields.get("client_secret")?.as_str()?.into(),
    ))
}

pub fn has_client() -> bool {
    client().is_some()
}

/// Keeps the client file Google Cloud gave, after checking it's one.
pub fn set_client_file(path: &std::path::Path) -> Result<(), String> {
    let raw = std::fs::read(path).map_err(|e| e.to_string())?;
    let file: Value = serde_json::from_slice(&raw).map_err(|_| "That isn't a JSON file.")?;
    let fields = file.get("installed").unwrap_or(&file);
    if fields.get("client_id").is_none() || fields.get("client_secret").is_none() {
        return Err(
            "That file has no client ID. Download it from a \"Desktop app\" client.".into(),
        );
    }
    std::fs::create_dir_all(config::dir()).map_err(|e| e.to_string())?;
    std::fs::write(config::dir().join(CLIENT_FILE), raw).map_err(|e| e.to_string())
}

static CANCEL: AtomicBool = AtomicBool::new(false);

/// Stops a sign-in waiting for the browser.
pub fn cancel_connect() {
    CANCEL.store(true, Ordering::Relaxed);
}

/// Signs in through the browser and keeps the account. Blocks until done: run it in a thread.
/// Returns the account's email.
pub fn connect() -> Result<String, String> {
    CANCEL.store(false, Ordering::Relaxed);
    let (id, secret) = client().ok_or("This build has no Google sign-in set up.")?;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    let redirect = format!(
        "http://127.0.0.1:{}",
        listener.local_addr().map_err(|e| e.to_string())?.port()
    );
    let verifier = random_text(48);
    let challenge = base64_url(&sha2::Sha256::digest(verifier.as_bytes()));
    let csrf = random_text(16);
    let url = format!(
        "https://accounts.google.com/o/oauth2/v2/auth?{}",
        form(&[
            ("client_id", &id),
            ("redirect_uri", &redirect),
            ("response_type", "code"),
            ("scope", SCOPES),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
            ("access_type", "offline"),
            // Always ask which account, so switching is just signing in again.
            ("prompt", "consent select_account"),
            ("state", &csrf),
        ])
    );
    crate::theme::open_folder(std::path::Path::new(&url));

    let code = wait_for_code(&listener, &csrf)?;
    let reply = post_form(
        "https://oauth2.googleapis.com/token",
        &[
            ("client_id", &id),
            ("client_secret", &secret),
            ("code", &code),
            ("code_verifier", &verifier),
            ("redirect_uri", &redirect),
            ("grant_type", "authorization_code"),
        ],
    )?;
    let tokens = reply.json()?;
    // Google lets people untick the calendar box on the consent screen.
    let granted = tokens["scope"].as_str().unwrap_or_default();
    if !granted.split(' ').any(|s| s == EVENTS_SCOPE) {
        return Err(
            "toolswitch needs to see and edit your events: tick that box when you connect.".into(),
        );
    }
    let access = tokens["access_token"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let user = request(
        "GET",
        "https://openidconnect.googleapis.com/v1/userinfo",
        Some(&access),
        None,
    )?
    .json()?;
    let account = Account {
        email: user["email"].as_str().unwrap_or("Google account").into(),
        refresh_token: tokens["refresh_token"].as_str().unwrap_or_default().into(),
        access_token: access,
        expires_at: Local::now().timestamp() + tokens["expires_in"].as_i64().unwrap_or(3600),
    };
    let email = account.email.clone();
    let mut state = state();
    let before = state.account.replace(account);
    save(&state);
    drop(state);
    *cache() = Cache::default();
    // One account at a time: the one switched away from is let go. Not when it's the same
    // account again, since dropping its old token would drop the new one too.
    if let Some(before) = before
        && !before.email.eq_ignore_ascii_case(&email)
    {
        revoke(before);
    }
    Ok(email)
}

/// Waits for the browser to come back to the local port with the sign-in code.
fn wait_for_code(listener: &std::net::TcpListener, csrf: &str) -> Result<String, String> {
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let deadline = std::time::Instant::now() + SIGN_IN_WAIT;
    loop {
        if CANCEL.load(Ordering::Relaxed) {
            return Err("Cancelled.".into());
        }
        if std::time::Instant::now() > deadline {
            return Err("The sign-in took too long. Try again.".into());
        }
        let mut stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(200));
                continue;
            }
            Err(e) => return Err(e.to_string()),
        };
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
        let mut buffer = [0u8; 4096];
        let read = stream.read(&mut buffer).unwrap_or(0);
        let request = String::from_utf8_lossy(&buffer[..read]);
        // "GET /?state=...&code=...&scope=... HTTP/1.1"
        let path = request.split_whitespace().nth(1).unwrap_or_default();
        let Some((_, query)) = path.split_once('?') else {
            // The browser asking for a favicon, say.
            let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
            continue;
        };
        let params: Vec<(String, String)> = query
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .map(|(k, v)| (k.to_string(), decode(v)))
            .collect();
        let param = |name: &str| params.iter().find(|p| p.0 == name).map(|p| p.1.clone());
        let ok = param("state").as_deref() == Some(csrf) && param("code").is_some();
        let message = if ok {
            "Connected. You can close this tab and go back to toolswitch."
        } else {
            "Not connected. You can close this tab and try again from toolswitch."
        };
        let page = format!(
            "<!doctype html><meta charset=utf-8><title>toolswitch</title>\
             <body style=\"font:18px system-ui;background:#09090b;color:#f4f4f5;\
             display:grid;place-items:center;height:90vh\"><p>{message}</p>"
        );
        let _ = stream.write_all(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{page}",
                page.len()
            )
            .as_bytes(),
        );
        if ok {
            return Ok(param("code").unwrap_or_default());
        }
        if param("state").as_deref() == Some(csrf) {
            return Err(match param("error").as_deref() {
                Some("access_denied") => "Sign-in was cancelled.".into(),
                Some(error) => format!("Google said: {error}"),
                None => "Google didn't sign you in.".into(),
            });
        }
    }
}

/// Forgets the account and tells Google to drop its tokens.
pub fn disconnect() {
    let mut state = state();
    let account = state.account.take();
    save(&state);
    drop(state);
    *cache() = Cache::default();
    if let Some(account) = account {
        revoke(account);
    }
}

/// Tells Google to drop the account's tokens, in the background.
fn revoke(account: Account) {
    std::thread::spawn(move || {
        let _ = post_form(
            "https://oauth2.googleapis.com/revoke",
            &[("token", &account.refresh_token)],
        );
    });
}

/// A token that works now, refreshed when it's about to run out.
fn token() -> Result<String, String> {
    let account = state().account.clone().ok_or(SIGNED_OUT)?;
    if account.expires_at - 60 > Local::now().timestamp() {
        return Ok(account.access_token);
    }
    let (id, secret) = client().ok_or("This build has no Google sign-in set up.")?;
    let reply = post_form(
        "https://oauth2.googleapis.com/token",
        &[
            ("client_id", &id),
            ("client_secret", &secret),
            ("refresh_token", &account.refresh_token),
            ("grant_type", "refresh_token"),
        ],
    )?;
    if reply.status == 400 || reply.status == 401 {
        // Revoked from the Google account, or expired: sign in again.
        let mut state = state();
        state.account = None;
        save(&state);
        return Err(SIGNED_OUT.into());
    }
    let tokens = reply.json()?;
    let access = tokens["access_token"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let mut state = state();
    if let Some(account) = state.account.as_mut() {
        account.access_token = access.clone();
        account.expires_at =
            Local::now().timestamp() + tokens["expires_in"].as_i64().unwrap_or(3600);
    }
    save(&state);
    Ok(access)
}

pub const SIGNED_OUT: &str = "Google signed you out. Connect again.";

// ---------- events ----------

#[derive(Clone, Default)]
pub struct Guest {
    pub email: String,
    pub name: String,
    /// "accepted", "declined", "tentative" or "needsAction".
    pub response: String,
    pub organizer: bool,
    pub me: bool,
}

#[derive(Clone)]
pub struct Event {
    pub id: String,
    pub title: String,
    pub all_day: bool,
    pub start: DateTime<Local>,
    pub end: DateTime<Local>,
    pub location: String,
    pub description: String,
    pub guests: Vec<Guest>,
    /// The Google Meet link.
    pub meet: Option<String>,
    /// The event in Google Calendar on the web.
    pub link: String,
    /// The organizer is someone else and doesn't let guests change it.
    pub read_only: bool,
    /// My answer when I'm a guest.
    pub my_response: Option<String>,
    /// The guests as Google sent them, to keep their answers when the list changes.
    attendees: Vec<Value>,
}

impl Event {
    fn from_json(e: &Value) -> Option<Event> {
        let (start, all_day) = when(&e["start"])?;
        let (end, _) = when(&e["end"]).unwrap_or((start, all_day));
        let attendees = e["attendees"].as_array().cloned().unwrap_or_default();
        let guests: Vec<Guest> = attendees
            .iter()
            .filter(|a| !a["resource"].as_bool().unwrap_or(false))
            .map(|a| Guest {
                email: a["email"].as_str().unwrap_or_default().into(),
                name: a["displayName"].as_str().unwrap_or_default().into(),
                response: a["responseStatus"].as_str().unwrap_or("needsAction").into(),
                organizer: a["organizer"].as_bool().unwrap_or(false),
                me: a["self"].as_bool().unwrap_or(false),
            })
            .collect();
        let organizer_me = e["organizer"]["self"].as_bool().unwrap_or(false);
        let my_response = guests
            .iter()
            .find(|g| g.me && !organizer_me)
            .map(|g| g.response.clone());
        let meet = e["hangoutLink"].as_str().map(String::from).or_else(|| {
            e["conferenceData"]["entryPoints"]
                .as_array()?
                .iter()
                .find(|p| p["entryPointType"] == "video")?["uri"]
                .as_str()
                .map(String::from)
        });
        Some(Event {
            id: e["id"].as_str()?.into(),
            title: e["summary"].as_str().unwrap_or("(No title)").into(),
            all_day,
            start,
            end,
            location: e["location"].as_str().unwrap_or_default().into(),
            description: e["description"].as_str().unwrap_or_default().into(),
            guests,
            meet,
            link: e["htmlLink"].as_str().unwrap_or_default().into(),
            read_only: !organizer_me && !e["guestsCanModify"].as_bool().unwrap_or(false),
            my_response,
            attendees,
        })
    }

    pub fn declined(&self) -> bool {
        self.my_response.as_deref() == Some("declined")
    }
}

/// {"dateTime": "2026-10-23T10:00:00-03:00"} or {"date": "2026-10-23"} for all day.
fn when(v: &Value) -> Option<(DateTime<Local>, bool)> {
    if let Some(text) = v["dateTime"].as_str() {
        let at = DateTime::parse_from_rfc3339(text).ok()?;
        return Some((at.with_timezone(&Local), false));
    }
    let day = NaiveDate::parse_from_str(v["date"].as_str()?, "%Y-%m-%d").ok()?;
    Some((midnight(day)?, true))
}

fn midnight(day: NaiveDate) -> Option<DateTime<Local>> {
    Local
        .from_local_datetime(&day.and_time(NaiveTime::MIN))
        .earliest()
}

/// The events of one day, in order.
pub fn day(day: NaiveDate) -> Result<Vec<Event>, String> {
    let from = midnight(day).ok_or("Bad date")?;
    let to = midnight(day.succ_opt().ok_or("Bad date")?).ok_or("Bad date")?;
    let url = format!(
        "{API}?{}",
        form(&[
            ("singleEvents", "true"),
            ("orderBy", "startTime"),
            ("maxResults", "250"),
            ("timeMin", &from.to_rfc3339()),
            ("timeMax", &to.to_rfc3339()),
        ])
    );
    let list = call("GET", &url, None)?;
    let events: Vec<Event> = list["items"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter(|e| e["status"] != "cancelled")
                .filter_map(Event::from_json)
                .collect()
        })
        .unwrap_or_default();
    if day == Local::now().date_naive() {
        let mut cache = cache();
        cache.today = events.clone();
        cache.day = Some(day);
        cache.fetched = Local::now().timestamp();
    }
    Ok(events)
}

/// What the form fills in.
#[derive(Clone)]
pub struct Draft {
    pub title: String,
    pub all_day: bool,
    pub start: DateTime<Local>,
    pub end: DateTime<Local>,
    pub location: String,
    pub description: String,
    pub guests: Vec<String>,
    pub meet: bool,
    /// Email the guests about it.
    pub notify: bool,
}

impl Draft {
    fn body(&self, before: Option<&Event>) -> Value {
        let (start, end) = if self.all_day {
            let first = self.start.date_naive();
            let last = self.end.date_naive().max(first);
            (
                json!({ "date": first.format("%Y-%m-%d").to_string() }),
                // Google's end date for all day is the day after.
                json!({ "date": last.succ_opt().unwrap_or(last).format("%Y-%m-%d").to_string() }),
            )
        } else {
            (
                json!({ "dateTime": self.start.to_rfc3339() }),
                json!({ "dateTime": self.end.to_rfc3339() }),
            )
        };
        // Guests already there keep their answer.
        let attendees: Vec<Value> = self
            .guests
            .iter()
            .map(|email| {
                before
                    .and_then(|b| {
                        b.attendees.iter().find(|a| {
                            a["email"]
                                .as_str()
                                .is_some_and(|e| e.eq_ignore_ascii_case(email))
                        })
                    })
                    .cloned()
                    .unwrap_or_else(|| json!({ "email": email }))
            })
            .chain(
                // Rooms aren't shown as guests, but stay booked.
                before
                    .into_iter()
                    .flat_map(|b| &b.attendees)
                    .filter(|a| a["resource"].as_bool().unwrap_or(false))
                    .cloned(),
            )
            .collect();
        let mut body = json!({
            "summary": self.title,
            "start": start,
            "end": end,
            "location": self.location,
            "description": self.description,
            "attendees": attendees,
        });
        if self.meet && before.is_none_or(|b| b.meet.is_none()) {
            body["conferenceData"] = json!({
                "createRequest": {
                    "requestId": random_text(12),
                    "conferenceSolutionKey": { "type": "hangoutsMeet" },
                }
            });
        }
        body
    }

    fn query(&self) -> String {
        form(&[
            ("sendUpdates", if self.notify { "all" } else { "none" }),
            ("conferenceDataVersion", "1"),
        ])
    }
}

pub fn create(draft: &Draft) -> Result<(), String> {
    call(
        "POST",
        &format!("{API}?{}", draft.query()),
        Some(&draft.body(None)),
    )?;
    forget_today();
    Ok(())
}

pub fn update(event: &Event, draft: &Draft) -> Result<(), String> {
    let url = format!("{API}/{}?{}", encode(&event.id), draft.query());
    call("PATCH", &url, Some(&draft.body(Some(event))))?;
    forget_today();
    Ok(())
}

pub fn delete(event: &Event, notify: bool) -> Result<(), String> {
    let url = format!(
        "{API}/{}?{}",
        encode(&event.id),
        form(&[("sendUpdates", if notify { "all" } else { "none" })])
    );
    call("DELETE", &url, None)?;
    forget_today();
    Ok(())
}

/// Answers an invitation: "accepted", "declined" or "tentative".
pub fn respond(event: &Event, response: &str) -> Result<(), String> {
    let attendees: Vec<Value> = event
        .attendees
        .iter()
        .map(|a| {
            let mut a = a.clone();
            if a["self"].as_bool().unwrap_or(false) {
                a["responseStatus"] = json!(response);
            }
            a
        })
        .collect();
    let url = format!("{API}/{}?sendUpdates=all", encode(&event.id));
    call("PATCH", &url, Some(&json!({ "attendees": attendees })))?;
    forget_today();
    Ok(())
}

/// "Lunch with Ana tomorrow 1pm": Google reads the date and time from the text.
pub fn quick_add(text: &str) -> Result<(), String> {
    let url = format!("{API}/quickAdd?{}", form(&[("text", text)]));
    call("POST", &url, None)?;
    forget_today();
    Ok(())
}

/// A calendar call with the account's token, and Google's error message if it fails.
fn call(method: &str, url: &str, body: Option<&Value>) -> Result<Value, String> {
    let token = token()?;
    let body = body.map(Value::to_string);
    let reply = request(method, url, Some(&token), body.as_deref())?;
    if reply.status == 401 {
        return Err(SIGNED_OUT.into());
    }
    if reply.body.trim().is_empty() {
        return Ok(Value::Null);
    }
    reply.json()
}

// ---------- in the background: notifications ----------

#[derive(Default)]
struct Cache {
    day: Option<NaiveDate>,
    today: Vec<Event>,
    /// Unix time of the last fetch; 0 = fetch again.
    fetched: i64,
    /// (event id, start) already notified.
    notified: std::collections::HashSet<(String, i64)>,
}

fn cache() -> MutexGuard<'static, Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Something changed: today's list is fetched again on the next turn.
fn forget_today() {
    cache().fetched = 0;
}

static FETCHING: AtomicBool = AtomicBool::new(false);

/// The next event today that hasn't started, for the tray.
pub fn next() -> Option<Event> {
    let now = Local::now();
    cache()
        .today
        .iter()
        .find(|e| !e.all_day && !e.declined() && e.start > now)
        .cloned()
}

/// Called by the background thread: keeps today's events fresh, says what's on for today once
/// a day, and warns a few minutes before each event.
pub fn tick() -> bool {
    if account().is_none() {
        return false;
    }
    let now = Local::now();
    let today = now.date_naive();
    let stale = {
        let cache = cache();
        cache.day != Some(today) || now.timestamp() - cache.fetched > REFRESH_EVERY
    };
    // The network can be slow: the other tools don't wait for it.
    if stale && !FETCHING.swap(true, Ordering::Relaxed) {
        std::thread::spawn(move || {
            let fetched = std::panic::catch_unwind(|| day(today)).unwrap_or(Err(String::new()));
            FETCHING.store(false, Ordering::Relaxed);
            if let Ok(events) = fetched {
                morning_summary(today, &events);
            }
        });
    }

    let mut changed = false;
    let mut cache = cache();
    let due: Vec<Event> = cache
        .today
        .iter()
        .filter(|e| !e.all_day && !e.declined())
        .filter(|e| {
            let left = (e.start - now).num_seconds();
            (0..=NOTIFY_BEFORE).contains(&left)
        })
        .cloned()
        .collect();
    for event in due {
        if cache
            .notified
            .insert((event.id.clone(), event.start.timestamp()))
        {
            let mut body = format!(
                "{} – {}",
                event.start.format("%H:%M"),
                event.end.format("%H:%M")
            );
            if !event.location.is_empty() {
                body.push_str(&format!(" · {}", event.location));
            }
            if event.meet.is_some() {
                body.push_str(" · Google Meet");
            }
            let mins = ((event.start - now).num_seconds() + 59) / 60;
            desktop::notify(&format!("📅 {} in {mins} min", event.title), &body);
            changed = true;
        }
    }
    changed
}

/// "Today: 10:00 Standup, 15:30 Dentist", the first time the app sees the day.
fn morning_summary(today: NaiveDate, events: &[Event]) {
    {
        let mut state = state();
        let day = today.format("%Y-%m-%d").to_string();
        if state.summary_day == day {
            return;
        }
        state.summary_day = day;
        save(&state);
    }
    let events: Vec<&Event> = events.iter().filter(|e| !e.declined()).collect();
    if events.is_empty() {
        return;
    }
    let lines: Vec<String> = events
        .iter()
        .take(6)
        .map(|e| {
            if e.all_day {
                format!("All day · {}", e.title)
            } else {
                format!("{} · {}", e.start.format("%H:%M"), e.title)
            }
        })
        .collect();
    let more = events.len().saturating_sub(6);
    let mut body = lines.join("\n");
    if more > 0 {
        body.push_str(&format!("\n…and {more} more"));
    }
    let title = match events.len() {
        1 => "📅 Today: 1 event".to_string(),
        n => format!("📅 Today: {n} events"),
    };
    desktop::notify(&title, &body);
}

// ---------- HTTP through curl ----------

struct Reply {
    status: u16,
    body: String,
}

impl Reply {
    /// The body as JSON; an error with Google's message when the call failed.
    fn json(&self) -> Result<Value, String> {
        let value: Value = serde_json::from_str(&self.body).unwrap_or(Value::Null);
        if (200..300).contains(&self.status) {
            return Ok(value);
        }
        let message = value["error"]["message"]
            .as_str()
            .or(value["error_description"].as_str())
            .or(value["error"].as_str())
            .map(String::from)
            .unwrap_or_else(|| format!("Google answered with error {}.", self.status));
        Err(message)
    }
}

fn post_form(url: &str, fields: &[(&str, &str)]) -> Result<Reply, String> {
    request("POST", url, None, Some(&form(fields)))
}

/// One HTTP request. Everything goes to curl through its standard input, so the token never
/// shows up in the list of running programs.
fn request(
    method: &str,
    url: &str,
    token: Option<&str>,
    body: Option<&str>,
) -> Result<Reply, String> {
    // Inside quotes, curl's config reads \\ and \" as escapes.
    let quote = |s: &str| {
        format!(
            "\"{}\"",
            s.replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n")
                .replace('\r', "\\r")
        )
    };
    let mut config = format!(
        "url = {}\nrequest = {}\nsilent\nshow-error\nmax-time = 30\nwrite-out = \"\\n%{{http_code}}\"\n",
        quote(url),
        quote(method)
    );
    if let Some(token) = token {
        config.push_str(&format!(
            "header = {}\n",
            quote(&format!("Authorization: Bearer {token}"))
        ));
    }
    if let Some(body) = body {
        let kind = if body.starts_with('{') {
            "application/json"
        } else {
            "application/x-www-form-urlencoded"
        };
        config.push_str(&format!(
            "header = {}\n",
            quote(&format!("Content-Type: {kind}"))
        ));
        config.push_str(&format!("data-raw = {}\n", quote(body)));
    } else if method == "POST" {
        config.push_str("header = \"Content-Length: 0\"\n");
    }
    let mut child = desktop::command("curl")
        .args(["-K", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("curl: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(config.as_bytes())
            .map_err(|e| e.to_string())?;
    }
    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&output.stdout);
    let (body, status) = text.rsplit_once('\n').unwrap_or(("", &text));
    let status: u16 = status.trim().parse().unwrap_or(0);
    if status == 0 {
        return Err("Could not reach Google. Are you online?".into());
    }
    Ok(Reply {
        status,
        body: body.to_string(),
    })
}

// ---------- small helpers ----------

/// "a=1&b=x%20y"
fn form(fields: &[(&str, &str)]) -> String {
    fields
        .iter()
        .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn encode(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = vec![];
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or_default();
                match u8::from_str_radix(hex, 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn base64_url(bytes: &[u8]) -> String {
    const ABC: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &b)| n | (b as u32) << (16 - 8 * i));
        for i in 0..=chunk.len() {
            out.push(ABC[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

/// Random letters and digits, from `bytes` random bytes.
fn random_text(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    getrandom::fill(&mut buffer).expect("the system has no random numbers");
    base64_url(&buffer)
}

/// "9", "9:30", "21.15", "930", "9am", "9:30 pm"
pub fn parse_time(text: &str) -> Option<NaiveTime> {
    let text = text.trim().to_lowercase().replace(' ', "");
    let (body, pm, am) = if let Some(b) = text.strip_suffix("pm") {
        (b.to_string(), true, false)
    } else if let Some(b) = text.strip_suffix("am") {
        (b.to_string(), false, true)
    } else {
        (
            text.trim_end_matches("hs")
                .trim_end_matches('h')
                .to_string(),
            false,
            false,
        )
    };
    let (h, m) = match body.split_once([':', '.']) {
        Some((h, m)) => (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?),
        None if body.len() >= 3 => {
            let (h, m) = body.split_at(body.len() - 2);
            (h.parse().ok()?, m.parse().ok()?)
        }
        None => (body.parse().ok()?, 0),
    };
    let h = match (h, pm, am) {
        (12, false, true) => 0,
        (h, true, _) if h < 12 => h + 12,
        (h, _, _) => h,
    };
    NaiveTime::from_hms_opt(h, m, 0)
}

/// "2026-10-24", "24/10", "24/10/2026", "today", "tomorrow", "hoy", "mañana"
pub fn parse_date(text: &str, today: NaiveDate) -> Option<NaiveDate> {
    let text = text.trim().to_lowercase();
    match text.as_str() {
        "today" | "hoy" => return Some(today),
        "tomorrow" | "mañana" | "manana" => return today.succ_opt(),
        _ => {}
    }
    if let Ok(day) = NaiveDate::parse_from_str(&text, "%Y-%m-%d") {
        return Some(day);
    }
    let parts: Vec<&str> = text.split(['/', '-', '.']).collect();
    let (d, m): (u32, u32) = (parts.first()?.parse().ok()?, parts.get(1)?.parse().ok()?);
    let y: i32 = match parts.get(2) {
        Some(y) if y.len() == 2 => 2000 + y.parse::<i32>().ok()?,
        Some(y) => y.parse().ok()?,
        None => {
            // "24/10" in December means next year's.
            let this_year = NaiveDate::from_ymd_opt(today.year(), m, d)?;
            return Some(if this_year < today - Duration::days(30) {
                NaiveDate::from_ymd_opt(this_year.year() + 1, m, d)?
            } else {
                this_year
            });
        }
    };
    NaiveDate::from_ymd_opt(y, m, d)
}

/// A local date and time; the earliest one when a clock change makes it ambiguous.
pub fn at(day: NaiveDate, time: NaiveTime) -> Option<DateTime<Local>> {
    Local.from_local_datetime(&day.and_time(time)).earliest()
}

/// Looks like an email address.
pub fn is_email(text: &str) -> bool {
    let text = text.trim();
    text.split_once('@').is_some_and(|(user, domain)| {
        !user.is_empty()
            && domain.contains('.')
            && !domain.starts_with('.')
            && !domain.ends_with('.')
    }) && !text.contains(char::is_whitespace)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Timelike;

    #[test]
    fn helpers() {
        assert_eq!(base64_url(b"hello"), "aGVsbG8");
        assert_eq!(base64_url(b"abc"), "YWJj");
        assert_eq!(base64_url(b"ab"), "YWI");
        // RFC 7636's example
        let challenge = base64_url(&sha2::Sha256::digest(
            b"dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
        ));
        assert_eq!(challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
        assert_eq!(encode("a b/é"), "a%20b%2F%C3%A9");
        assert_eq!(decode("4%2F0Ab_x+y%"), "4/0Ab_x y%");
        assert_eq!(form(&[("a", "1"), ("b", "x y")]), "a=1&b=x%20y");
        assert!(is_email("ana@gmail.com"));
        assert!(!is_email("ana@gmail"));
        assert!(!is_email("ana gmail.com"));
    }

    #[test]
    fn times_and_dates() {
        let t = |s| parse_time(s).map(|t| (t.hour(), t.minute()));
        assert_eq!(t("9"), Some((9, 0)));
        assert_eq!(t("9:30"), Some((9, 30)));
        assert_eq!(t("21.15"), Some((21, 15)));
        assert_eq!(t("930"), Some((9, 30)));
        assert_eq!(t("1745"), Some((17, 45)));
        assert_eq!(t("3pm"), Some((15, 0)));
        assert_eq!(t("12am"), Some((0, 0)));
        assert_eq!(t("25:00"), None);
        assert_eq!(t("x"), None);
        let today = NaiveDate::from_ymd_opt(2026, 12, 20).unwrap();
        assert_eq!(
            parse_date("tomorrow", today),
            NaiveDate::from_ymd_opt(2026, 12, 21)
        );
        assert_eq!(
            parse_date("2026-10-24", today),
            NaiveDate::from_ymd_opt(2026, 10, 24)
        );
        assert_eq!(
            parse_date("24/12", today),
            NaiveDate::from_ymd_opt(2026, 12, 24)
        );
        assert_eq!(
            parse_date("5/1", today),
            NaiveDate::from_ymd_opt(2027, 1, 5)
        );
        assert_eq!(
            parse_date("5/1/27", today),
            NaiveDate::from_ymd_opt(2027, 1, 5)
        );
        assert_eq!(parse_date("31/2", today), None);
    }

    #[test]
    fn reads_events() {
        let e = Event::from_json(&json!({
            "id": "abc",
            "summary": "Standup",
            "start": { "dateTime": "2026-10-23T10:00:00-03:00" },
            "end": { "dateTime": "2026-10-23T10:30:00-03:00" },
            "organizer": { "email": "boss@x.com" },
            "attendees": [
                { "email": "boss@x.com", "organizer": true, "responseStatus": "accepted" },
                { "email": "me@x.com", "self": true, "responseStatus": "needsAction" },
                { "email": "room@x.com", "resource": true },
            ],
            "hangoutLink": "https://meet.google.com/abc",
        }))
        .unwrap();
        assert_eq!(e.guests.len(), 2);
        assert!(e.read_only);
        assert_eq!(e.my_response.as_deref(), Some("needsAction"));
        assert_eq!((e.end - e.start).num_minutes(), 30);
        let all_day = Event::from_json(&json!({
            "id": "d", "start": { "date": "2026-10-23" }, "end": { "date": "2026-10-24" },
            "organizer": { "self": true },
        }))
        .unwrap();
        assert!(all_day.all_day && !all_day.read_only);
        assert_eq!(all_day.title, "(No title)");

        // Editing keeps the answers of guests already there, and the room.
        let draft = Draft {
            title: "Standup".into(),
            all_day: false,
            start: e.start,
            end: e.end,
            location: String::new(),
            description: String::new(),
            guests: vec!["Boss@x.com".into(), "new@x.com".into()],
            meet: true,
            notify: true,
        };
        let body = draft.body(Some(&e));
        let attendees = body["attendees"].as_array().unwrap();
        assert_eq!(attendees[0]["responseStatus"], "accepted");
        assert_eq!(attendees[1], json!({ "email": "new@x.com" }));
        assert_eq!(attendees[2]["email"], "room@x.com");
        // It already has a Meet link: no new one.
        assert!(body.get("conferenceData").is_none());
        assert!(draft.body(None).get("conferenceData").is_some());
    }
}

#[cfg(test)]
mod online {
    #[test]
    #[ignore]
    fn talks_to_google() {
        let reply = super::post_form(
            "https://oauth2.googleapis.com/token",
            &[("client_id", "x"), ("code", "y"), ("grant_type", "authorization_code")],
        )
        .unwrap();
        println!("{} {}", reply.status, reply.body);
        assert_eq!(reply.status, 401);
        assert!(reply.json().is_err());
        let reply = super::request("GET", "https://www.googleapis.com/calendar/v3/calendars/primary/events", Some("bad\"token"), None).unwrap();
        println!("{} {:?}", reply.status, reply.json().err());
    }
}
