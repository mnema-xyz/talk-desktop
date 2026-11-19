//! The small dialog the rail needs for a name or a confirmation.
//!
//! A native menu cannot take text, and the rail is too narrow for a field, so
//! the rail page asks the host for a dialog: a window of its own, above the
//! shell, showing a page of the host (`dialog.html`, served like the rail
//! page) for exactly what the spec says. The answer goes back to the rail
//! page as the result of its `rail_dialog` call. Closing the window cancels.
use crate::web_client::app_url;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use tauri::{LogicalPosition, LogicalSize, Manager, Webview, WebviewUrl, window::WindowBuilder};
use tokio::sync::oneshot;
use url::Url;

pub const LABEL: &str = "rail-dialog";
const PAGE: &str = "dialog.html";
const WIDTH: f64 = 400.0;

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
struct Input {
    label: String,
    value: String,
    max: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    placeholder: Option<String>,
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
struct Choice {
    value: String,
    label: String,
}

/// A list to pick from, with a filter: the quick switcher.
#[derive(Deserialize, Serialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
struct List {
    label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    placeholder: Option<String>,
    choices: Vec<Pick>,
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
struct Pick {
    id: String,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    hint: Option<String>,
}

/// A file of text to read: the list of servers to import.
#[derive(Deserialize, Serialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
struct File {
    label: String,
    accept: String,
    max: usize,
}

/// The most a chosen file may hold: an exported list of servers.
const MAX_FILE: usize = crate::instances::MAX_IMPORT_BYTES;

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
struct Colors {
    label: String,
    value: String,
    choices: Vec<Choice>,
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
struct Button {
    id: String,
    text: String,
    #[serde(default)]
    primary: bool,
    #[serde(default)]
    danger: bool,
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    input: Option<Input>,
    #[serde(skip_serializing_if = "Option::is_none")]
    colors: Option<Colors>,
    #[serde(skip_serializing_if = "Option::is_none")]
    list: Option<List>,
    #[serde(skip_serializing_if = "Option::is_none")]
    file: Option<File>,
    buttons: Vec<Button>,
    cancel: String,
}

/// What the person did.
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct Answer {
    button: String,
    value: Option<String>,
    color: Option<String>,
}

/// The dialog on the screen, if any.
#[derive(Default)]
pub struct Open(Mutex<Option<Running>>);

struct Running {
    spec: Spec,
    reply: Option<oneshot::Sender<Answer>>,
}

fn short(text: &str, most: usize) -> bool {
    text.chars().count() <= most && !text.chars().any(|c| c.is_control() && c != '\n')
}

fn color_ok(value: &str) -> bool {
    value.is_empty() || crate::rail_layout::clean_color(value).is_some_and(|c| c == value)
}

fn pick_id_ok(id: &str) -> bool {
    (1..=24).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn button_id_ok(id: &str) -> bool {
    (1..=24).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'))
}

fn parse(raw: &str) -> Result<Spec, String> {
    const BAD: &str = "The dialog is not valid.";
    if raw.len() > 8 * 1024 {
        return Err(BAD.into());
    }
    let spec: Spec = serde_json::from_str(raw).map_err(|_| BAD)?;
    let fine = short(&spec.title, 80)
        && spec.message.as_deref().is_none_or(|m| short(m, 300))
        && spec.input.as_ref().is_none_or(|i| {
            short(&i.label, 60)
                && (1..=80).contains(&i.max)
                && short(&i.value, i.max)
                && i.placeholder.as_deref().is_none_or(|p| short(p, 40))
        })
        && spec.colors.as_ref().is_none_or(|c| {
            short(&c.label, 60)
                && color_ok(&c.value)
                && c.choices.len() <= 12
                && c.choices
                    .iter()
                    .all(|k| color_ok(&k.value) && short(&k.label, 40))
        })
        && spec.list.as_ref().is_none_or(|l| {
            short(&l.label, 60)
                && l.placeholder.as_deref().is_none_or(|p| short(p, 40))
                && (1..=12).contains(&l.choices.len())
                && l.choices.iter().enumerate().all(|(n, k)| {
                    pick_id_ok(&k.id)
                        && short(&k.text, 100)
                        && !k.text.is_empty()
                        && k.hint.as_deref().is_none_or(|h| short(h, 60))
                        && l.choices[..n].iter().all(|before| before.id != k.id)
                })
        })
        && spec
            .file
            .as_ref()
            .is_none_or(|f| short(&f.label, 60) && f.accept == ".json" && f.max <= MAX_FILE)
        && usize::from(spec.input.is_some())
            + usize::from(spec.list.is_some())
            + usize::from(spec.file.is_some())
            <= 1
        && (1..=3).contains(&spec.buttons.len())
        && spec
            .buttons
            .iter()
            .all(|b| button_id_ok(&b.id) && short(&b.text, 40) && !b.text.is_empty())
        && spec.buttons.iter().any(|b| b.id == spec.cancel);
    if fine { Ok(spec) } else { Err(BAD.into()) }
}

fn cancelled(spec: &Spec) -> Answer {
    Answer {
        button: spec.cancel.clone(),
        value: None,
        color: None,
    }
}

/// An answer, held to what the spec allowed. Anything else is a cancel.
fn accept(spec: &Spec, button: &str, value: Option<String>, color: Option<String>) -> Answer {
    let value_ok = match &value {
        None => true,
        Some(v) => match (&spec.input, &spec.list, &spec.file) {
            (Some(input), _, _) => short(v, input.max),
            (_, Some(list), _) => list.choices.iter().any(|k| k.id == *v),
            (_, _, Some(file)) => v.len() <= file.max.min(MAX_FILE),
            _ => false,
        },
    };
    let color_fine = match (&spec.colors, &color) {
        (Some(colors), Some(c)) => colors.choices.iter().any(|k| k.value == *c),
        (None, None) | (Some(_), None) => true,
        (None, Some(_)) => false,
    };
    if spec.buttons.iter().any(|b| b.id == button) && value_ok && color_fine {
        Answer {
            button: button.to_owned(),
            value,
            color,
        }
    } else {
        cancelled(spec)
    }
}

/// How tall the window is for what it shows.
fn height(spec: &Spec) -> f64 {
    190.0
        + if spec.input.is_some() { 70.0 } else { 0.0 }
        + if spec.colors.is_some() { 100.0 } else { 0.0 }
        + if spec.message.is_some() { 40.0 } else { 0.0 }
        + if spec.list.is_some() { 200.0 } else { 0.0 }
        + if spec.file.is_some() { 90.0 } else { 0.0 }
}

fn page_url() -> Url {
    app_url().join(PAGE).expect("constant path")
}

/// Only the dialog page itself may be shown in the dialog window.
fn dialog_url(url: &Url) -> bool {
    let page = page_url();
    url.scheme() == page.scheme()
        && url.host_str() == page.host_str()
        && url.port() == page.port()
        && url.path() == page.path()
}

/// The rail asks for a dialog and waits for the answer. A second request
/// while one is open brings the open one forward and counts as a cancel.
#[tauri::command]
pub async fn rail_dialog(webview: Webview, spec: String) -> Result<Answer, String> {
    let app = super::web_shell::from_rail(&webview)?;
    let spec = parse(&spec)?;
    let open = app.state::<Open>();
    let (reply, answer) = oneshot::channel();
    {
        let mut slot = open.0.lock().map_err(|_| "busy")?;
        if slot.is_some() {
            drop(slot);
            if let Some(window) = app.get_window(LABEL) {
                let _ = window.set_focus();
            }
            return Ok(cancelled(&spec));
        }
        *slot = Some(Running {
            spec: spec.clone(),
            reply: Some(reply),
        });
    }
    let shown = {
        let app = app.clone();
        let spec = spec.clone();
        let (tx, rx) = oneshot::channel();
        // Windows are made on the main thread.
        let _ = app.clone().run_on_main_thread(move || {
            let _ = tx.send(show(&app, &spec));
        });
        rx.await
            .unwrap_or_else(|_| Err("The dialog could not be opened.".into()))
    };
    if let Err(why) = shown {
        if let Ok(mut slot) = open.0.lock() {
            *slot = None;
        }
        return Err(why);
    }
    Ok(answer.await.unwrap_or_else(|_| cancelled(&spec)))
}

fn show(app: &tauri::AppHandle, spec: &Spec) -> Result<(), String> {
    const FAILED: &str = "The dialog could not be opened.";
    let shell = app.get_window(super::web_shell::SHELL).ok_or(FAILED)?;
    let tall = height(spec);
    let scale = shell.scale_factor().unwrap_or(1.0);
    let centre = |position: Result<tauri::PhysicalPosition<i32>, _>,
                  size: Result<tauri::PhysicalSize<u32>, _>| {
        let (position, size) = (position.ok()?, size.ok()?);
        Some(LogicalPosition::new(
            f64::from(position.x) / scale + (f64::from(size.width) / scale - WIDTH) / 2.0,
            f64::from(position.y) / scale + (f64::from(size.height) / scale - tall) / 2.0,
        ))
    };
    let mut builder = WindowBuilder::new(app, LABEL)
        .title(&spec.title)
        .inner_size(WIDTH, tall)
        .resizable(false)
        .maximizable(false)
        .minimizable(false)
        .always_on_top(true)
        .parent(&shell)
        .map_err(|_| FAILED)?;
    builder = match centre(shell.inner_position(), shell.inner_size()) {
        Some(at) => builder.position(at.x, at.y),
        None => builder.center(),
    };
    let window = builder.build().map_err(|_| FAILED)?;
    let script = format!(
        "Object.defineProperty(window,'__MNEMA_DIALOG__',{{value:Object.freeze({})}});",
        serde_json::to_string(spec).map_err(|_| FAILED)?
    );
    let page =
        super::web_shell::host_page_at(app, LABEL, "", WebviewUrl::CustomProtocol(page_url()))
            .on_navigation(dialog_url)
            .initialization_script(script);
    let added: Result<Webview, _> = window.add_child(
        page,
        LogicalPosition::new(0.0, 0.0),
        LogicalSize::new(WIDTH, tall),
    );
    if added.is_err() {
        let _ = window.destroy();
        return Err(FAILED.into());
    }
    let closing = app.clone();
    window.on_window_event(move |event| {
        if matches!(event, tauri::WindowEvent::Destroyed) {
            finish(&closing, None);
        }
    });
    let _ = window.set_focus();
    Ok(())
}

/// Ends the dialog with an answer, or with a cancel for `None`.
fn finish(app: &tauri::AppHandle, answer: Option<Answer>) {
    let Some(open) = app.try_state::<Open>() else {
        return;
    };
    let running = open.0.lock().ok().and_then(|mut slot| slot.take());
    if let Some(Running {
        spec,
        reply: Some(reply),
    }) = running
    {
        let _ = reply.send(answer.unwrap_or_else(|| cancelled(&spec)));
    }
    if let Some(window) = app.get_window(LABEL) {
        let _ = window.destroy();
    }
}

/// The dialog page is done. Only that page, in that window, may say so.
#[tauri::command]
pub fn dialog_done(
    webview: Webview,
    button: String,
    value: Option<String>,
    color: Option<String>,
) -> Result<(), String> {
    if webview.label() != LABEL || !webview.url().is_ok_and(|url| dialog_url(&url)) {
        return Err("Only the dialog may use this.".into());
    }
    let app = webview.app_handle().clone();
    let spec = app.try_state::<Open>().and_then(|open| {
        open.0
            .lock()
            .ok()
            .and_then(|slot| slot.as_ref().map(|r| r.spec.clone()))
    });
    let answer = spec.map(|spec| accept(&spec, &button, value, color));
    finish(&app, answer);
    Ok(())
}

#[cfg(test)]
mod tests;
