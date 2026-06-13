//! Real AVRCP via raw D-Bus `org.bluez.MediaPlayer1` (bluetooth-design.md §6,
//! AUD-011). bluer has no media support, so this owns a `dbus-tokio`
//! `SyncConnection`: it finds the player, watches `PropertiesChanged` to publish
//! now-playing `MediaInfo`, routes `TransportCommand`s to the player methods, and
//! reconnects on the staleness threshold. Real D-Bus glue (hardware-validated);
//! only `media_info_from` is unit-tested.
use crate::artwork::fetch_artwork;
use crate::bluetooth::avrcp::{
    sanitize_duration_ms, sanitize_text, DbusHealth, PlaybackStatus, TrackMetadata,
    TransportCommand,
};
use crate::state::{AppStateHandle, MediaInfo};
use crate::sys::supervisor::wait_for_shutdown;
use dbus::arg::{prop_cast, PropMap, RefArg};
use dbus::message::MatchRule;
use dbus::nonblock::stdintf::org_freedesktop_dbus::{ObjectManager, Properties};
use dbus::nonblock::{Proxy, SyncConnection};
use dbus_tokio::connection;
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex};

// ─── Type alias to tame PropMap complexity ───────────────────────────────────

/// A D-Bus object dict: path → interface name → property map.
type ManagedObjects = HashMap<dbus::Path<'static>, HashMap<String, PropMap>>;

// ─── Constants ───────────────────────────────────────────────────────────────

const DBUS_TIMEOUT: Duration = Duration::from_secs(5);
const RECONNECT_BACKOFF: Duration = Duration::from_secs(2);
const PLAYER_POLL: Duration = Duration::from_secs(3);
const MEDIAPLAYER1_IFACE: &str = "org.bluez.MediaPlayer1";
const MEDIATRANSPORT1_IFACE: &str = "org.bluez.MediaTransport1";
const BLUEZ_DEST: &str = "org.bluez";

// ─── Public converter (unit-tested) ──────────────────────────────────────────

/// Build the web `MediaInfo` DTO from the AVRCP model + a current position.
pub fn media_info_from(
    status: PlaybackStatus,
    track: &TrackMetadata,
    position_ms: Option<u32>,
) -> MediaInfo {
    MediaInfo {
        status: status.as_str().to_string(),
        title: track.title.clone(),
        artist: track.artist.clone(),
        album: track.album.clone(),
        duration_ms: track.duration_ms,
        position_ms,
        artwork_url: None,
    }
}

// ─── D-Bus helpers ───────────────────────────────────────────────────────────

/// Extract the `…/dev_XX_XX_XX_XX_XX_XX` prefix from a BlueZ object path.
/// Works for both player paths (`…/dev_.../playerN`) and transport paths
/// (`…/dev_.../sepM/fdK`). Returns `None` if no `dev_` segment is found.
///
/// Example: `/org/bluez/hci0/dev_44_4A_DB_B4_E7_0D/player0`
///        → `/org/bluez/hci0/dev_44_4A_DB_B4_E7_0D`
pub(crate) fn device_prefix(path: &str) -> Option<&str> {
    // Walk segments; return the slice up to and including the dev_… segment.
    let mut end = 0;
    let mut found = false;
    for segment in path.split('/') {
        if segment.starts_with("dev_") {
            end += segment.len();
            found = true;
            break;
        }
        // +1 for the '/' separator (skip leading empty segment from the initial '/').
        end += segment.len() + 1;
    }
    if found {
        // `end` points one past the last char of the dev_ segment.
        Some(&path[..end])
    } else {
        None
    }
}

/// A MediaPlayer1 candidate distilled from BlueZ's managed objects.
struct PlayerCandidate {
    path: dbus::Path<'static>,
    /// The candidate's device has a `MediaTransport1` in state `active`.
    device_active: bool,
    /// The player's `Status` is `playing` or `paused`.
    playing: bool,
}

/// Pick which player to bind, **deterministically** and with **stickiness**.
///
/// Tiers, best first: (1) a player whose device has an active transport,
/// (2) a player that is playing/paused, (3) any player. The best non-empty tier
/// is the candidate pool.
///
/// Two properties matter for stability — BlueZ returns objects in HashMap order,
/// which is randomized per call:
/// - **Deterministic:** candidates are sorted by path, so the same set always
///   yields the same winner (without this, two equally-ranked devices flap).
/// - **Sticky:** if `current` is still in the chosen pool, keep it. When two
///   devices both stream (both Tier 1), we stay on the one we already follow
///   instead of switching every poll — yet still move off it the moment it drops
///   out of the pool (e.g. its transport goes inactive). (AUD-041)
fn select_player(
    mut candidates: Vec<PlayerCandidate>,
    current: Option<&dbus::Path<'static>>,
) -> Option<dbus::Path<'static>> {
    candidates.sort_by(|a, b| a.path.to_string().cmp(&b.path.to_string()));

    let pool: Vec<&PlayerCandidate> = {
        let tier1: Vec<&PlayerCandidate> = candidates.iter().filter(|c| c.device_active).collect();
        if !tier1.is_empty() {
            tier1
        } else {
            let tier2: Vec<&PlayerCandidate> = candidates.iter().filter(|c| c.playing).collect();
            if !tier2.is_empty() {
                tier2
            } else {
                candidates.iter().collect()
            }
        }
    };

    // Stickiness: keep the currently-bound player if it still qualifies.
    if let Some(cur) = current {
        if pool.iter().any(|c| &c.path == cur) {
            return Some(cur.clone());
        }
    }

    pool.first().map(|c| c.path.clone())
}

/// Find the best MediaPlayer1 to follow. `current` is the currently-bound player
/// (if any), used for stickiness so two simultaneously-active devices don't cause
/// the manager to flap between them every poll. See [`select_player`].
async fn find_active_player(
    conn: &Arc<SyncConnection>,
    current: Option<&dbus::Path<'static>>,
) -> Option<dbus::Path<'static>> {
    let proxy = Proxy::new(BLUEZ_DEST, "/", DBUS_TIMEOUT, conn.clone());
    let objects: ManagedObjects = match proxy.get_managed_objects().await {
        Ok(o) => o,
        Err(e) => {
            eprintln!("soundsync: avrcp: GetManagedObjects error: {e}");
            return None;
        }
    };

    // Collect device prefixes that have an active MediaTransport1.
    let mut active_prefixes: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (path, ifaces) in &objects {
        if let Some(transport_props) = ifaces.get(MEDIATRANSPORT1_IFACE) {
            if let Some(state) = prop_cast::<String>(transport_props, "State") {
                if state == "active" {
                    if let Some(prefix) = device_prefix(path) {
                        active_prefixes.insert(prefix.to_string());
                    }
                }
            }
        }
    }

    // Distill the player candidates, then choose deterministically.
    let candidates: Vec<PlayerCandidate> = objects
        .iter()
        .filter(|(_, ifaces)| ifaces.contains_key(MEDIAPLAYER1_IFACE))
        .map(|(path, ifaces)| {
            let device_active =
                device_prefix(path).is_some_and(|prefix| active_prefixes.contains(prefix));
            let playing = ifaces
                .get(MEDIAPLAYER1_IFACE)
                .and_then(|props| prop_cast::<String>(props, "Status"))
                .is_some_and(|status| status == "playing" || status == "paused");
            PlayerCandidate {
                path: path.clone(),
                device_active,
                playing,
            }
        })
        .collect();

    select_player(candidates, current)
}

/// Extract `TrackMetadata` from a `Track` property dict (`a{sv}`).
fn track_from_propmap(map: &PropMap) -> TrackMetadata {
    let title = prop_cast::<String>(map, "Title")
        .cloned()
        .and_then(sanitize_text);
    let artist = prop_cast::<String>(map, "Artist")
        .cloned()
        .and_then(sanitize_text);
    let album = prop_cast::<String>(map, "Album")
        .cloned()
        .and_then(sanitize_text);
    // Duration may arrive as u32; fall back to as_u64 if prop_cast fails.
    // Both 0 and 0xFFFFFFFF are BlueZ sentinels meaning "no duration" → None.
    let duration_ms = prop_cast::<u32>(map, "Duration")
        .copied()
        .or_else(|| {
            map.get("Duration")
                .and_then(|v| v.0.as_u64())
                .map(|n| u32::try_from(n).unwrap_or(u32::MAX))
        })
        .and_then(sanitize_duration_ms);
    TrackMetadata {
        title,
        artist,
        album,
        duration_ms,
    }
}

/// Read the current `Status`, `Position`, and `Track` from a MediaPlayer1 object.
/// Returns `None` on D-Bus error (caller records failure).
async fn read_media(
    conn: &Arc<SyncConnection>,
    player_path: &dbus::Path<'static>,
) -> Option<MediaInfo> {
    let proxy = Proxy::new(BLUEZ_DEST, player_path.clone(), DBUS_TIMEOUT, conn.clone());

    let status_str: String = match proxy.get(MEDIAPLAYER1_IFACE, "Status").await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("soundsync: avrcp: get Status error: {e}");
            return None;
        }
    };
    let status = PlaybackStatus::from_bluez(&status_str);

    // Position may be absent when stopped — treat error as None (not fatal).
    let position_ms: Option<u32> = proxy.get::<u32>(MEDIAPLAYER1_IFACE, "Position").await.ok();

    let track: TrackMetadata = match proxy.get::<PropMap>(MEDIAPLAYER1_IFACE, "Track").await {
        Ok(m) => track_from_propmap(&m),
        Err(_) => TrackMetadata::default(), // No track — stopped/idle
    };

    Some(media_info_from(status, &track, position_ms))
}

// ─── Artwork lookup helper ────────────────────────────────────────────────────

/// Per-session artwork cache: the (title, artist) key of the current track plus
/// the last successfully fetched artwork URL for that track.
/// - `None` → no track has been seen yet this session.
/// - `Some((key, None))` → track known, art lookup pending or returned nothing.
/// - `Some((key, Some(url)))` → art found; url is preserved across polls/reconnects.
type ArtCache = Arc<Mutex<Option<((String, String), Option<String>)>>>;

/// Publish `info` (preserving existing art for the same track) and, on a
/// genuine track change, spawn exactly ONE async task to fetch new artwork.
///
/// ## Durability contract
/// - **Same track** (title+artist == cached key): the cached `artwork_url` is
///   re-stamped onto `info` before publishing.  Cover art therefore survives
///   position/status updates and same-track reconnects (even though
///   `set_media(None)` wipes `AppState` on reconnect, the url lives in the
///   `ArtCache` and is recovered on the first read after reconnect).
/// - **New track** (or first read): `artwork_url` = `None`; cache is updated;
///   one fetch task is spawned.  The task race-guards before writing back.
/// - **No lock across await**: every `Mutex`/`RwLock` guard is cloned-and-
///   dropped before any `.await` or `set_media` call.
///
/// `art_cache` is shared across every call site (initial read,
/// PropertiesChanged, poll) and is NOT cleared on reconnect.
async fn publish_and_lookup_art(mut info: MediaInfo, state: AppStateHandle, art_cache: ArtCache) {
    let title = info.title.clone().unwrap_or_default();
    let artist = info.artist.clone().unwrap_or_default();
    let album = info.album.clone().unwrap_or_default();

    // Track key: None when both title and artist are empty (no usable metadata).
    let new_key: Option<(String, String)> = if title.is_empty() && artist.is_empty() {
        None
    } else {
        Some((title.clone(), artist.clone()))
    };

    // --- Read cache; determine track change and preserved url ----------------
    let (is_new_track, preserved_url) = {
        let guard = art_cache.lock().await;
        match (new_key.as_ref(), guard.as_ref()) {
            // We have a key and it matches the cached key → same track.
            (Some(k), Some((cached_key, cached_url))) if k == cached_key => {
                (false, cached_url.clone())
            }
            // Key changed (or first read with a key) → new track.
            (Some(_), _) => (true, None),
            // No usable key → treat as same-track (no lookup, no art).
            (None, _) => (false, None),
        }
    };
    // guard dropped here.

    // --- Same-track: preserve art and publish --------------------------------
    if !is_new_track {
        info.artwork_url = preserved_url;
        state.set_media(Some(info)).await;
        return; // No spawn on same-track polls.
    }

    // --- Genuine track change ------------------------------------------------
    // Update cache: new key, url not yet known.
    {
        let mut guard = art_cache.lock().await;
        // new_key is Some here (is_new_track is only true when new_key.is_some()).
        *guard = new_key.clone().map(|k| (k, None));
    }
    // guard dropped.

    // Publish the new track immediately (artwork_url = None — not yet fetched).
    state.set_media(Some(info)).await;

    // Only spawn a fetch task when we have a usable key.
    let Some(key) = new_key else {
        return;
    };

    // Spawn ONE fetch task for this track.
    tokio::spawn(async move {
        // Fetch — best-effort; any error returns None.
        let art_url = fetch_artwork(artist, album, title).await;

        let Some(url) = art_url else {
            return; // No art found — gradient placeholder remains.
        };

        // Race guard + cache update: only apply if the track key still matches.
        let cache_matches = {
            let mut guard = art_cache.lock().await;
            if let Some((ref cached_key, ref mut cached_url)) = *guard {
                if cached_key == &key {
                    // Store the url in the cache for future reconnects/polls.
                    *cached_url = Some(url.clone());
                    true
                } else {
                    false
                }
            } else {
                false
            }
        };
        // guard dropped.

        if !cache_matches {
            return;
        }

        // Re-read state; stamp the art only if title+artist still match.
        let updated = {
            let state_guard = state.state.read().await;
            if let Some(ref media) = state_guard.media {
                let cur_title = media.title.as_deref().unwrap_or("");
                let cur_artist = media.artist.as_deref().unwrap_or("");
                if (cur_title, cur_artist) != (key.0.as_str(), key.1.as_str()) {
                    return;
                }
                Some(MediaInfo {
                    artwork_url: Some(url),
                    ..media.clone()
                })
            } else {
                None
            }
        };
        // state_guard dropped.

        if let Some(m) = updated {
            state.set_media(Some(m)).await;
        }
    });
}

// ─── Manager ─────────────────────────────────────────────────────────────────

/// Aborts the wrapped task when dropped.
///
/// The AVRCP manager spawns one IOResource task per D-Bus connection to drive
/// its I/O. That task *owns* the connection, so the socket only closes once the
/// task ends. On every reconnect we abandon the old connection — and a detached
/// `JoinHandle` does **not** abort its task, so without this guard each reconnect
/// leaked a live connection (and its system-bus socket) forever. Enough leaks
/// and UID hits dbus-daemon's `max_connections_per_user` (256), after which all
/// new system-bus connections by that user are refused — including unrelated
/// ones like `avahi-browse`, breaking Chromecast discovery (AUD-040).
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// AVRCP media manager: owns a `SyncConnection`, finds the device's
/// `org.bluez.MediaPlayer1`, watches `PropertiesChanged` to keep `AppState`
/// now-playing current, and routes `TransportCommand`s to the player methods.
/// Reconnects on `DbusHealth` threshold (AUD-011). Never panics.
pub async fn run_media_manager(
    mut media_rx: mpsc::Receiver<TransportCommand>,
    state: AppStateHandle,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    // Artwork cache: persists across reconnects so the same track keeps its
    // cover art after a D-Bus cycle without re-fetching.
    let art_cache: ArtCache = Arc::new(Mutex::new(None));

    'reconnect: loop {
        // ── Check shutdown before each reconnect attempt ──────────────────
        if *shutdown.borrow() {
            return;
        }

        // ── Clear stale media on every reconnect iteration ────────────────
        state.set_media(None).await;

        // ── Connect to the system bus ─────────────────────────────────────
        let (resource, conn) = match connection::new_system_sync() {
            Ok(pair) => pair,
            Err(e) => {
                eprintln!("soundsync: avrcp: D-Bus connect error: {e}");
                tokio::select! {
                    biased;
                    _ = wait_for_shutdown(&mut shutdown) => return,
                    _ = tokio::time::sleep(RECONNECT_BACKOFF) => {}
                }
                continue 'reconnect;
            }
        };

        // Spawn the IOResource that drives this connection's I/O. The guard
        // aborts it whenever we leave this iteration (reconnect or shutdown),
        // which closes the connection's socket instead of leaking it (AUD-040).
        let _resource_guard = AbortOnDrop(tokio::spawn(async move {
            let err = resource.await;
            eprintln!("soundsync: avrcp: D-Bus IOResource ended: {err}");
        }));

        // ── Find the MediaPlayer1 object ──────────────────────────────────
        let player_path = loop {
            if *shutdown.borrow() {
                return;
            }
            if let Some(p) = find_active_player(&conn, None).await {
                break p;
            }
            state.set_media(None).await;
            tokio::select! {
                biased;
                _ = wait_for_shutdown(&mut shutdown) => return,
                _ = tokio::time::sleep(PLAYER_POLL) => {}
            }
        };

        eprintln!("soundsync: avrcp: player found at {player_path}");

        // ── Register PropertiesChanged signal match ───────────────────────
        let mr = MatchRule::new_signal("org.freedesktop.DBus.Properties", "PropertiesChanged")
            .with_sender(BLUEZ_DEST)
            .with_path(player_path.clone());
        let msg_match = match conn.add_match(mr).await {
            Ok(m) => m,
            Err(e) => {
                eprintln!("soundsync: avrcp: add_match error: {e}");
                tokio::select! {
                    biased;
                    _ = wait_for_shutdown(&mut shutdown) => return,
                    _ = tokio::time::sleep(RECONNECT_BACKOFF) => {}
                }
                continue 'reconnect;
            }
        };

        // Use the typed stream that auto-parses the three PropertiesChanged args.
        let (msg_match, mut sig_stream) = msg_match.stream::<(String, PropMap, Vec<String>)>();

        // ── Read initial state ────────────────────────────────────────────
        let mut health = DbusHealth::new();
        match read_media(&conn, &player_path).await {
            Some(info) => {
                health.record_success();
                publish_and_lookup_art(info, state.clone(), art_cache.clone()).await;
            }
            None => {
                if health.record_failure() {
                    eprintln!("soundsync: avrcp: initial read failed; reconnecting");
                    // clean up the match before reconnecting
                    if let Err(e) = conn.remove_match(msg_match.token()).await {
                        eprintln!("soundsync: avrcp: remove_match error: {e}");
                    }
                    drop(sig_stream);
                    continue 'reconnect;
                }
            }
        }

        // ── Inner session loop ────────────────────────────────────────────
        'session: loop {
            tokio::select! {
                biased;

                // ── Shutdown ──────────────────────────────────────────────
                _ = wait_for_shutdown(&mut shutdown) => {
                    let _ = conn.remove_match(msg_match.token()).await;
                    drop(sig_stream);
                    return;
                }

                // ── Transport command from web UI ─────────────────────────
                cmd = media_rx.recv() => {
                    match cmd {
                        None => {
                            // All senders dropped — nothing to do, just return.
                            let _ = conn.remove_match(msg_match.token()).await;
                            drop(sig_stream);
                            return;
                        }
                        Some(c) => {
                            let proxy = Proxy::new(
                                BLUEZ_DEST,
                                player_path.clone(),
                                DBUS_TIMEOUT,
                                conn.clone(),
                            );
                            match proxy.method_call::<(), _, _, _>(
                                MEDIAPLAYER1_IFACE,
                                c.bluez_method(),
                                (),
                            ).await {
                                Ok(()) => { health.record_success(); }
                                Err(e) => {
                                    eprintln!("soundsync: avrcp: command {} error: {e}", c.bluez_method());
                                    if health.record_failure() {
                                        eprintln!("soundsync: avrcp: D-Bus health threshold reached; reconnecting");
                                        state.set_media(None).await;
                                        let _ = conn.remove_match(msg_match.token()).await;
                                        drop(sig_stream);
                                        continue 'reconnect;
                                    }
                                }
                            }
                        }
                    }
                }

                // ── PropertiesChanged signal ──────────────────────────────
                maybe_sig = sig_stream.next() => {
                    match maybe_sig {
                        None => {
                            // Stream ended (match removed or conn dropped)
                            continue 'reconnect;
                        }
                        Some((_msg, (iface, _changed, _invalidated))) => {
                            if iface != MEDIAPLAYER1_IFACE {
                                continue 'session;
                            }
                            match read_media(&conn, &player_path).await {
                                Some(info) => {
                                    health.record_success();
                                    publish_and_lookup_art(
                                        info,
                                        state.clone(),
                                        art_cache.clone(),
                                    ).await;
                                }
                                None => {
                                    if health.record_failure() {
                                        eprintln!("soundsync: avrcp: D-Bus health threshold reached; reconnecting");
                                        state.set_media(None).await;
                                        let _ = conn.remove_match(msg_match.token()).await;
                                        drop(sig_stream);
                                        continue 'reconnect;
                                    }
                                }
                            }
                        }
                    }
                }

                // ── Slow player-liveness poll ─────────────────────────────
                // Also re-evaluates which player is the active streaming
                // source; reconnects if the active source changed (e.g. a
                // second device started streaming).
                _ = tokio::time::sleep(PLAYER_POLL) => {
                    // Re-find the best player; if it has changed, reconnect
                    // so we re-bind the signal match to the new target. Passing
                    // the current player makes the choice sticky — two equally
                    // active devices no longer flap (AUD-041).
                    if let Some(best) = find_active_player(&conn, Some(&player_path)).await {
                        if best != player_path {
                            eprintln!(
                                "soundsync: avrcp: active player changed to {best}; reconnecting"
                            );
                            state.set_media(None).await;
                            let _ = conn.remove_match(msg_match.token()).await;
                            drop(sig_stream);
                            continue 'reconnect;
                        }
                    }
                    match read_media(&conn, &player_path).await {
                        Some(info) => {
                            health.record_success();
                            publish_and_lookup_art(
                                info,
                                state.clone(),
                                art_cache.clone(),
                            ).await;
                        }
                        None => {
                            if health.record_failure() {
                                eprintln!("soundsync: avrcp: player poll failed; reconnecting");
                                state.set_media(None).await;
                                let _ = conn.remove_match(msg_match.token()).await;
                                drop(sig_stream);
                                continue 'reconnect;
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bluetooth::avrcp::{PlaybackStatus, TrackMetadata};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    // ── AbortOnDrop (the D-Bus connection-leak guard, AUD-040) ────────────────

    #[tokio::test]
    async fn abort_on_drop_tears_down_the_spawned_task() {
        // A marker whose Drop fires only when the task's future is torn down —
        // i.e. when the task is aborted (or completes). The body never completes
        // on its own, so the marker firing proves the abort happened.
        struct SetOnDrop(Arc<AtomicBool>);
        impl Drop for SetOnDrop {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let torn_down = Arc::new(AtomicBool::new(false));
        let flag = torn_down.clone();
        {
            let _guard = AbortOnDrop(tokio::spawn(async move {
                let _marker = SetOnDrop(flag);
                std::future::pending::<()>().await; // runs until aborted
            }));
            // Let the task start and reach the pending await.
            tokio::time::sleep(Duration::from_millis(20)).await;
            assert!(
                !torn_down.load(Ordering::SeqCst),
                "task should still be running while the guard is alive"
            );
        } // guard drops here → abort

        // Give the runtime a moment to run the abort and drop the future.
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(
            torn_down.load(Ordering::SeqCst),
            "dropping the guard must abort the task (closing the D-Bus socket)"
        );
    }

    // ── select_player (deterministic + sticky selection, AUD-041) ─────────────

    fn p(s: &str) -> dbus::Path<'static> {
        dbus::Path::new(s).unwrap().into_static()
    }

    fn cand(path: &str, device_active: bool, playing: bool) -> PlayerCandidate {
        PlayerCandidate {
            path: p(path),
            device_active,
            playing,
        }
    }

    const DEV_A: &str = "/org/bluez/hci0/dev_44_4A_DB_B4_E7_0D/player0";
    const DEV_B: &str = "/org/bluez/hci0/dev_80_49_71_15_3C_00/player0";

    #[test]
    fn select_player_is_deterministic_when_two_devices_are_active() {
        // Both devices active, no current. Feeding the candidates in EITHER
        // order must yield the same winner — the sorted-first path (DEV_A).
        let forward = select_player(vec![cand(DEV_A, true, true), cand(DEV_B, true, true)], None);
        let reversed = select_player(vec![cand(DEV_B, true, true), cand(DEV_A, true, true)], None);
        assert_eq!(forward, Some(p(DEV_A)));
        assert_eq!(reversed, Some(p(DEV_A)));
    }

    #[test]
    fn select_player_sticks_to_current_when_it_still_qualifies() {
        // Both active; currently bound to DEV_B (not the sorted-first). Stickiness
        // must keep DEV_B rather than flapping back to DEV_A every poll.
        let current = p(DEV_B);
        let chosen = select_player(
            vec![cand(DEV_A, true, true), cand(DEV_B, true, true)],
            Some(&current),
        );
        assert_eq!(chosen, Some(p(DEV_B)));
    }

    #[test]
    fn select_player_switches_when_current_drops_out_of_the_pool() {
        // Bound to DEV_B, but only DEV_A is active now → must move to DEV_A.
        let current = p(DEV_B);
        let chosen = select_player(
            vec![cand(DEV_A, true, false), cand(DEV_B, false, false)],
            Some(&current),
        );
        assert_eq!(chosen, Some(p(DEV_A)));
    }

    #[test]
    fn select_player_prefers_active_device_over_merely_playing() {
        // DEV_B is sorted-last but the only active one; it wins over playing DEV_A.
        let chosen = select_player(
            vec![cand(DEV_A, false, true), cand(DEV_B, true, false)],
            None,
        );
        assert_eq!(chosen, Some(p(DEV_B)));
    }

    #[test]
    fn select_player_falls_back_to_first_when_none_active_or_playing() {
        let chosen = select_player(
            vec![cand(DEV_B, false, false), cand(DEV_A, false, false)],
            None,
        );
        assert_eq!(chosen, Some(p(DEV_A)));
    }

    #[test]
    fn select_player_returns_none_with_no_candidates() {
        assert_eq!(select_player(vec![], None), None);
    }

    // ── device_prefix ────────────────────────────────────────────────────────

    #[test]
    fn device_prefix_extracts_from_player_path() {
        assert_eq!(
            device_prefix("/org/bluez/hci0/dev_44_4A_DB_B4_E7_0D/player0"),
            Some("/org/bluez/hci0/dev_44_4A_DB_B4_E7_0D")
        );
    }

    #[test]
    fn device_prefix_extracts_from_transport_path() {
        assert_eq!(
            device_prefix("/org/bluez/hci0/dev_44_4A_DB_B4_E7_0D/sep1/fd2"),
            Some("/org/bluez/hci0/dev_44_4A_DB_B4_E7_0D")
        );
    }

    #[test]
    fn device_prefix_same_device_for_player_and_transport() {
        let player = device_prefix("/org/bluez/hci0/dev_44_4A_DB_B4_E7_0D/player0").unwrap();
        let transport = device_prefix("/org/bluez/hci0/dev_44_4A_DB_B4_E7_0D/sep1/fd2").unwrap();
        assert_eq!(player, transport);
    }

    #[test]
    fn device_prefix_returns_none_for_path_without_dev() {
        assert_eq!(device_prefix("/org/bluez/hci0"), None);
        assert_eq!(device_prefix("/"), None);
    }

    // ── media_info_from ──────────────────────────────────────────────────────

    #[test]
    fn builds_media_info_from_model() {
        let track = TrackMetadata {
            title: Some("Song".into()),
            artist: Some("Artist".into()),
            album: Some("Album".into()),
            duration_ms: Some(210_000),
        };
        let info = media_info_from(PlaybackStatus::Playing, &track, Some(1500));
        assert_eq!(info.status, "playing");
        assert_eq!(info.title.as_deref(), Some("Song"));
        assert_eq!(info.artist.as_deref(), Some("Artist"));
        assert_eq!(info.album.as_deref(), Some("Album"));
        assert_eq!(info.duration_ms, Some(210_000));
        assert_eq!(info.position_ms, Some(1500));
    }

    #[test]
    fn empty_track_paused_has_no_fields() {
        let info = media_info_from(PlaybackStatus::Paused, &TrackMetadata::default(), None);
        assert_eq!(info.status, "paused");
        assert_eq!(info.title, None);
        assert_eq!(info.duration_ms, None);
        assert_eq!(info.position_ms, None);
    }
}
