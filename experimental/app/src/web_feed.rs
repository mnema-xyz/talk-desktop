//! The rail's counts of a server whose page is not loaded (never opened in
//! this run, or closed after hiding for a while; see `rail_sleep`).
//!
//! The host reads them as the web app does: `GET /api/read-state` through the
//! server's own session, made into the two numbers by `rail_sleep::counts`.
//! It reads them again when the server's socket says something changed (a
//! message, a read, the channels), at most every few seconds, and once a
//! minute in any case. The socket is the host's own and only listens: right
//! after it opens it says the member is idle (`presence_idle`), so it shows
//! the member as away, exactly as the hidden page it replaces did, and it
//! never sends typing, reads or anything else.
use crate::web_client::Trusted;
use crate::web_ws::{Sockets, WsEvent};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tauri::Manager;
use tokio::sync::{Notify, mpsc};

mod budget;
mod live;
mod slot;
use budget::Gate;
use live::Live;
use slot::report;
#[cfg(test)]
use slot::{Applied, apply, mention_rise, mention_text};

/// The quiet time after a change before the counts are read again.
const SETTLE: Duration = Duration::from_secs(2);
/// The counts are read at least this often.
const EVERY: Duration = Duration::from_secs(60);
/// A dropped socket is opened again after this long; an unreachable server
/// is asked again after twice as long each time, up to `REOPEN_MAX`.
const REOPEN: Duration = Duration::from_secs(30);
const REOPEN_MAX: Duration = Duration::from_secs(300);

/// The events after which the counts may have changed.
pub fn changes_counts(frame: &str) -> bool {
    let kind = serde_json::from_str::<serde_json::Value>(frame)
        .ok()
        .and_then(|v| v.get("type")?.as_str().map(str::to_owned));
    matches!(
        kind.as_deref(),
        Some(
            "message_create"
                | "message_update"
                | "message_delete"
                | "read_state"
                | "channels_changed"
                | "permissions_changed"
                | "roles_changed"
                | "member_roles_changed"
                | "dm_created"
                | "dm_member_added"
                | "dm_member_removed"
                | "dm_prefs_updated"
        )
    )
}

/// What the host's socket says right after it opens: the member is idle here.
pub const IDLE: &str = r#"{"type":"presence_idle","payload":{"idle":true}}"#;

struct Feed {
    stop: Arc<Notify>,
    sockets: Arc<Sockets>,
}

static FEEDS: Mutex<Option<HashMap<String, Feed>>> = Mutex::new(None);

fn feeds<T>(f: impl FnOnce(&mut HashMap<String, Feed>) -> T) -> Option<T> {
    FEEDS
        .lock()
        .ok()
        .map(|mut map| f(map.get_or_insert_with(HashMap::new)))
}

/// Whether the host keeps the counts of `id` now.
pub fn running(id: &str) -> bool {
    feeds(|map| map.contains_key(id)).unwrap_or(false)
}

/// Starts keeping the counts of `id` (a server that is ready and has no page).
pub fn start(app: &tauri::AppHandle, id: &str) {
    let session = app.state::<Trusted>().session(id);
    let Some(session) = session else {
        return;
    };
    let stop = Arc::new(Notify::new());
    let sockets = Arc::new(Sockets::default());
    if !register(id, &stop, &sockets) {
        return;
    }
    let app = app.clone();
    let id = id.to_owned();
    let gate = budget::gate(&id);
    tauri::async_runtime::spawn(
        async move { run(&app, &id, &session, &sockets, &stop, &gate).await },
    );
}

/// Notes a new feed of `id`; `false` when it already has one.
fn register(id: &str, stop: &Arc<Notify>, sockets: &Arc<Sockets>) -> bool {
    feeds(|map| {
        if map.contains_key(id) {
            return false;
        }
        map.insert(
            id.to_owned(),
            Feed {
                stop: Arc::clone(stop),
                sockets: Arc::clone(sockets),
            },
        );
        true
    })
    .unwrap_or(false)
}

/// Stops keeping the counts of `id` (its page took over, or it went away).
pub fn stop(id: &str) {
    if let Some(Some(feed)) = feeds(|map| map.remove(id)) {
        feed.stop.notify_one();
        feed.sockets.close_all();
    }
}

/// How often the feed reads and reconnects.
#[derive(Clone, Copy)]
pub(crate) struct Times {
    pub settle: Duration,
    pub every: Duration,
    pub reopen: Duration,
    pub reopen_max: Duration,
}

const TIMES: Times = Times {
    settle: SETTLE,
    every: EVERY,
    reopen: REOPEN,
    reopen_max: REOPEN_MAX,
};

/// What one read of a server gave.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Read {
    /// The rail's two numbers.
    Counts(u32, u32),
    /// The session is gone (401, 403): nothing more to read until the page
    /// signs in again.
    SignedOut,
    /// No answer, or not one that could be read: try again later.
    Unreachable,
}

/// The wait before the `failures`th retry of an unreachable server: doubling
/// from `times.reopen` up to `times.reopen_max`, with up to a fifth of jitter
/// (from `seed`) so that many servers do not ask at the same moment.
pub(crate) fn backoff(times: Times, failures: u32, seed: u64) -> Duration {
    let base = times
        .reopen
        .saturating_mul(1u32 << failures.saturating_sub(1).min(16))
        .min(times.reopen_max);
    let fifth = base.as_millis() as u64 / 5;
    let jitter = if fifth == 0 { 0 } else { seed % fifth };
    base.saturating_sub(Duration::from_millis(jitter))
}

async fn run(
    app: &tauri::AppHandle,
    id: &str,
    session: &Arc<crate::instances::Session>,
    sockets: &Arc<Sockets>,
    stop: &Arc<Notify>,
    gate: &Gate,
) {
    pump(&session.instance, sockets, stop, gate, TIMES, |read| {
        running(id) && report(app, id, read)
    })
    .await;
    // Signed out (or removed): the feed is over until a page opens again.
    stop_quietly(id, stop);
}

/// Forgets the feed of `id` that `own` stops, without waking its loop (it is
/// ending anyway). A newer feed of the same server (its page failed to open
/// meanwhile) stays.
fn stop_quietly(id: &str, own: &Arc<Notify>) {
    let ended = feeds(|map| {
        map.get(id)
            .is_some_and(|feed| Arc::ptr_eq(&feed.stop, own))
            .then(|| map.remove(id))
            .flatten()
    });
    if let Some(Some(feed)) = ended {
        feed.sockets.close_all();
    }
}

/// The feed's loop, free of the app: reads the counts and hands what it read
/// to `report` (which says whether to go on), again after a change and at
/// least every `times.every`, until `stop` or a signed-out session. An
/// unreachable server is asked again later and later (`backoff`).
pub(crate) async fn pump(
    instance: &Arc<crate::web_proxy::Instance>,
    sockets: &Arc<Sockets>,
    stop: &Notify,
    gate: &Gate,
    times: Times,
    report: impl FnMut(Read) -> bool,
) {
    pump_until_over(instance, sockets, stop, gate, times, report).await;
    gate.give_back(sockets);
}

async fn pump_until_over(
    instance: &Arc<crate::web_proxy::Instance>,
    sockets: &Arc<Sockets>,
    stop: &Notify,
    gate: &Gate,
    times: Times,
    mut report: impl FnMut(Read) -> bool,
) {
    // The pokes only wake the loop and may be dropped when many come at once;
    // whether the socket is open is kept apart (`Live`), so a close is never lost.
    let (poke, mut pokes) = mpsc::channel::<bool>(16);
    let live = Arc::new(Live::default());
    let mut failures: u32 = 0;
    // No place for a socket (`budget`): the counts are read on the timer.
    let mut polling = false;
    loop {
        let read = read_counts(instance).await;
        let (signed_out, unreachable) = (read == Read::SignedOut, read == Read::Unreachable);
        if !report(read) || signed_out {
            return;
        }
        // The socket opens only once the session has answered: a server that
        // is signed out or down is never sent a socket.
        if !live.is_open() && !unreachable {
            polling = !gate.take(sockets);
            if !polling {
                listen(instance, sockets, poke.clone(), &live, gate);
            }
        }
        let rest = if unreachable {
            failures += 1;
            live.forget();
            sockets.close_all();
            gate.give_back(sockets);
            backoff(times, failures, uuid::Uuid::new_v4().as_u64_pair().0)
        } else {
            failures = 0;
            if !next(stop, &mut pokes, times.every).await {
                return;
            }
            // A change settles first; a closed socket opens again after a while.
            if live.is_open() || polling {
                times.settle
            } else {
                times.reopen
            }
        };
        if !pause(stop, rest).await {
            return;
        }
        while pokes.try_recv().is_ok() {}
    }
}

/// Waits for a change, a closed socket or `every`; `false` when the feed is
/// stopped.
async fn next(stop: &Notify, pokes: &mut mpsc::Receiver<bool>, every: Duration) -> bool {
    tokio::select! {
        () = stop.notified() => false,
        _ = pokes.recv() => true,
        () = tokio::time::sleep(every) => true,
    }
}

/// Sleeps for `rest`; `false` when the feed was stopped meanwhile.
async fn pause(stop: &Notify, rest: Duration) -> bool {
    tokio::select! {
        () = stop.notified() => false,
        () = tokio::time::sleep(rest) => true,
    }
}

/// Opens the host's socket for the server: it says the member is idle and
/// pokes the loop when the counts may have changed (`true`) or it closed
/// (`false`, also noted in `live`).
fn listen(
    instance: &Arc<crate::web_proxy::Instance>,
    sockets: &Arc<Sockets>,
    poke: mpsc::Sender<bool>,
    live: &Arc<Live>,
    gate: &Gate,
) {
    let speaker = Arc::clone(sockets);
    let generation = live.begin();
    let (ended, place) = (Arc::clone(live), gate.clone());
    // With the conversation query: direct messages count on the rail too.
    let opened = crate::web_ws::open(sockets, Arc::clone(instance), true, move |event| {
        match event {
            WsEvent::Open { id } => {
                let _ = speaker.send(id, IDLE.to_owned());
            }
            WsEvent::Message { data, .. } => {
                if changes_counts(&data) {
                    let _ = poke.try_send(true);
                }
            }
            WsEvent::Close { .. } => {
                if ended.end(generation) {
                    place.give_back(&speaker);
                }
                let _ = poke.try_send(false);
            }
        }
        true
    });
    if opened.is_err() && live.end(generation) {
        gate.give_back(sockets);
    }
}

/// One answer as JSON: `Err` with what the status means for the feed.
async fn get_json(
    instance: &crate::web_proxy::Instance,
    path: &str,
) -> Result<Option<serde_json::Value>, Read> {
    let resp = crate::web_proxy::forward(instance, "GET", path, &[], Vec::new()).await;
    match resp.status {
        200 => serde_json::from_slice(&resp.body)
            .map(Some)
            .map_err(|_| Read::Unreachable),
        401 | 403 => Err(Read::SignedOut),
        // A server without direct messages has no list of them.
        404 => Ok(None),
        _ => Err(Read::Unreachable),
    }
}

/// The server's read state and conversations as the rail's two numbers, as
/// the page counts them (`rail_sleep::counts`).
async fn read_counts(instance: &crate::web_proxy::Instance) -> Read {
    let states = match get_json(instance, "/api/read-state").await {
        Ok(Some(states)) => states,
        Ok(None) => return Read::Unreachable,
        Err(read) => return read,
    };
    let dms = match get_json(instance, "/api/dms").await {
        Ok(dms) => dms.unwrap_or_else(|| serde_json::json!([])),
        Err(read) => return read,
    };
    let (unread, mentions) = crate::rail_sleep::counts(&states, &dms);
    Read::Counts(unread, mentions)
}

#[cfg(test)]
mod tests;
