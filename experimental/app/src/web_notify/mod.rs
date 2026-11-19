//! OS notifications and the taskbar/dock badge for the pages of the servers.
//!
//! The web app asks for a toast with plain text and the ids of the message it
//! is about; the host shows it through the platform's own API (Windows toast,
//! macOS UserNotifications) and, when the user clicks the toast, brings that
//! server to the front and tells its page which message to jump to. The
//! platform code is driven only from here: no page can call it directly. Only
//! the page of a server may use these commands, never a gaming surface, the
//! rail or the selector, and a toast always belongs to the server that asked
//! for it.
use serde::Serialize;
use std::time::Instant;
use tauri::{Manager, Webview};

#[cfg(target_os = "macos")]
mod mac;
#[cfg(target_os = "macos")]
use mac as platform;
#[cfg(windows)]
mod win;
#[cfg(windows)]
use win as platform;
#[cfg(not(any(windows, target_os = "macos")))]
mod other;
#[cfg(not(any(windows, target_os = "macos")))]
use other as platform;
mod badge;
#[cfg(feature = "notify-selftest")]
mod selftest;
mod throttle;

pub(crate) use badge::apply_badge;
pub use throttle::Notifier;
use throttle::{Offer, Toast};

const MAX_TITLE: usize = 100;
const MAX_BODY: usize = 240;
const MAX_SERVER_NAME: usize = 60;
const MAX_ID: usize = 64;

/// What the page learns when a toast was used.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct Click {
    channel_id: String,
    message_id: String,
}

/// What the operating system lets this app do, in the words of the web's
/// Notification API (`default` = not asked yet).
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Permission {
    Granted,
    Denied,
    #[cfg(any(target_os = "macos", test))]
    Default,
    #[cfg(any(not(windows), test))]
    Unsupported,
}

/// Invisible format characters (Unicode `Cf`) that reorder or hide text:
/// zero-width spaces and marks, bidi overrides and isolates, the word joiner,
/// the byte order mark and the tag characters (which spell ASCII text that no
/// one sees). The zero-width joiner stays (emoji sequences need it) and so does the
/// zero-width non-joiner (Persian words need it).
fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{ad}' | '\u{61c}' | '\u{180e}' | '\u{200b}' | '\u{200e}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{206f}'
            | '\u{feff}' | '\u{e0000}'..='\u{e007f}'
    )
}

/// Plain text only: control characters become spaces, invisible format
/// characters are dropped, the length is bounded in characters, and anything
/// cut off is marked.
pub fn clean(text: &str, max: usize) -> String {
    let flat: String = text
        .chars()
        .filter(|c| !is_invisible(*c))
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let flat = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let mut cut: String = flat.chars().take(max.saturating_sub(1)).collect();
    cut.push('\u{2026}');
    cut
}

/// Ids are UUIDs; anything else is refused instead of passed on.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// A plain toast from the host itself, for a hint the person should see once.
/// It has no page and no click target, and a user who turned toasts off sees nothing.
pub fn host_toast(app: &tauri::AppHandle, title: &str, body: &str) {
    platform::host_toast(app, &clean(title, MAX_TITLE), &clean(body, MAX_BODY));
}

/// A quiet toast from the host for a server without a loaded page (new
/// mentions there): no sound, no click target, never a permission prompt;
/// shown only where the system already lets this app notify.
pub fn quiet_toast(app: &tauri::AppHandle, title: &str, body: &str) {
    platform::quiet_toast(app, &clean(title, MAX_TITLE), &clean(body, MAX_BODY));
}

/// The server whose page made the call.
fn server_page(webview: &Webview) -> Result<std::sync::Arc<crate::instances::Session>, String> {
    super::web_client::instance_page(webview)
}

#[tauri::command]
pub fn desktop_notify_open(
    webview: Webview,
    on_event: tauri::ipc::Channel<Click>,
) -> Result<(), String> {
    let session = server_page(&webview)?;
    let trusted = webview.app_handle().state::<super::web_client::Trusted>();
    let mut registry = trusted.registry.lock().map_err(|_| "busy")?;
    registry
        .slot_mut(&session.id)
        .ok_or("No instance is selected.")?
        .clicks = Some(on_event);
    Ok(())
}

/// What the OS currently allows, without asking.
#[tauri::command]
pub async fn desktop_notify_permission(webview: Webview) -> Result<Permission, String> {
    server_page(&webview)?;
    Ok(platform::permission().await)
}

/// Asks the OS for permission if it has not been asked yet (macOS shows its
/// own prompt then) and reports the outcome.
#[tauri::command]
pub async fn desktop_notify_request_permission(webview: Webview) -> Result<Permission, String> {
    server_page(&webview)?;
    Ok(platform::request_permission().await)
}

#[tauri::command]
pub async fn desktop_notify(
    webview: Webview,
    title: String,
    body: String,
    channel_id: String,
    message_id: String,
) -> Result<(), String> {
    let session = server_page(&webview)?;
    if !valid_id(&channel_id) || !valid_id(&message_id) {
        return Err("Invalid notification target.".into());
    }
    deliver(
        webview.app_handle(),
        &session.id,
        &title,
        &body,
        channel_id,
        message_id,
    )
    .await
    .map(|_| ())
}

/// Whether the person turned the notifications of this server off.
fn muted(app: &tauri::AppHandle, server: &str) -> bool {
    app.state::<super::web_client::Trusted>()
        .registry
        .lock()
        .is_ok_and(|registry| registry.is_muted(server, super::web_proxy::now()))
}

/// The title with the name of the server behind it, once there is more than
/// one: a toast says which server it is from.
fn tagged(title: &str, name: &str, several: bool) -> String {
    let title = clean(title, MAX_TITLE);
    if several {
        format!("{title} \u{b7} {}", clean(name, MAX_SERVER_NAME))
    } else {
        title
    }
}

/// Shows one toast: bounded plain text, rate limited, and, where the OS has
/// not been asked yet, preceded by asking. A toast that comes too soon after
/// the last one of its server is held and shown once the gap is over (a burst
/// shows its newest toast, not none). A toast the user has turned off in the
/// OS is dropped without an error, like the browser does.
async fn deliver(
    app: &tauri::AppHandle,
    server: &str,
    title: &str,
    body: &str,
    channel_id: String,
    message_id: String,
) -> Result<bool, String> {
    // Notifications of a muted server are off.
    if muted(app, server) {
        return Ok(false);
    }
    let toast = Toast {
        title: title.to_owned(),
        body: body.to_owned(),
        click: Click {
            channel_id,
            message_id,
        },
    };
    match app.state::<Notifier>().offer(server, Instant::now(), toast) {
        Offer::Show(toast) => show(app, server, toast).await,
        Offer::Held { wait, flush } => {
            if flush {
                let (app, server) = (app.clone(), server.to_owned());
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(wait).await;
                    let held = app.state::<Notifier>().take_held(&server, Instant::now());
                    if let Some(toast) = held {
                        let _ = show(&app, &server, toast).await;
                    }
                });
            }
            Ok(false)
        }
    }
}

/// Hands a toast that passed the rate limit to the platform, tagged with the
/// name of its server. When it does not reach the screen the server's gap is
/// given back.
async fn show(app: &tauri::AppHandle, server: &str, toast: Toast) -> Result<bool, String> {
    let shown = async {
        let (session, title) = {
            let trusted = app.state::<super::web_client::Trusted>();
            let registry = trusted.registry.lock().map_err(|_| "busy")?;
            let slot = registry.slot(server).ok_or("No instance is selected.")?;
            // A toast that was held when the server was muted is not shown.
            if registry.is_muted(server, super::web_proxy::now()) {
                return Ok(false);
            }
            (
                slot.generation.clone(),
                tagged(
                    &toast.title,
                    &registry.display_name(slot),
                    registry.len() > 1,
                ),
            )
        };
        platform::show(
            app,
            &title,
            &clean(&toast.body, MAX_BODY),
            toast.click,
            session,
        )
        .await
    }
    .await;
    if !matches!(shown, Ok(true)) {
        app.state::<Notifier>().release(server);
    }
    shown
}
/// The user used a toast: bring the server it came from to the front and tell
/// its page which message the toast was about.
///
/// A toast of a server that is no longer in the list, or of an earlier run of
/// its page, is dropped: its ids mean nothing to (and must not reach) any
/// other page.
pub(crate) fn activated(app: &tauri::AppHandle, click: Click, session: &str) {
    let trusted = app.state::<super::web_client::Trusted>();
    // One lookup for both, so a page that restarted meanwhile does not get
    // the click of its predecessor.
    let Some((id, channel)) = trusted.registry.lock().ok().and_then(|r| {
        r.by_generation(session)
            .map(|s| (s.id.clone(), s.clicks.clone()))
    }) else {
        return;
    };
    #[cfg(feature = "notify-selftest")]
    selftest::clicked(&click);
    super::web_shell::bring_forward(app, &id);
    if let Some(channel) = channel {
        let _ = channel.send(click);
    }
}

/// Prepares the platform side (macOS installs the click handler here, early
/// enough for a toast that launched the app).
pub fn setup(app: &tauri::AppHandle) {
    platform::init(app);
    #[cfg(feature = "notify-selftest")]
    selftest::start(app);
}

/// What the page of a server has waiting, both numbers. The host sums the
/// mentions over its servers for the icon and shows both on the rail.
#[tauri::command]
pub fn desktop_report_unread(webview: Webview, unread: u32, mentions: u32) -> Result<(), String> {
    let session = server_page(&webview)?;
    let app = webview.app_handle();
    let trusted = app.state::<super::web_client::Trusted>();
    trusted
        .registry
        .lock()
        .map_err(|_| "busy")?
        .report(&session.id, unread, mentions);
    super::web_shell::counts_changed(app);
    Ok(())
}

/// The plain badge of a bundle from before `desktop_report_unread`: its
/// mentions, for its server only.
#[tauri::command]
pub fn desktop_set_badge(webview: Webview, count: u32) -> Result<(), String> {
    let session = server_page(&webview)?;
    let app = webview.app_handle();
    let trusted = app.state::<super::web_client::Trusted>();
    trusted
        .registry
        .lock()
        .map_err(|_| "busy")?
        .report_badge(&session.id, count);
    super::web_shell::counts_changed(app);
    Ok(())
}

/// A server left the list or its page went away: nothing may be delivered to
/// it anymore, and its toasts (those of page run `generation`) leave the
/// screen and the notification centre. Those of other servers stay where the
/// platform can tell them apart.
pub fn closed(app: &tauri::AppHandle, server: &str, generation: &str) {
    app.state::<Notifier>().forget(server);
    let others_remain = app
        .state::<super::web_client::Trusted>()
        .registry
        .lock()
        .map_or(true, |r| r.len() > 0);
    platform::clear(app, generation, others_remain);
}

#[cfg(test)]
mod tests;
