//! Jellyfin on the player: the server, Quick Connect, the lists, and its
//! songs in Reborn's own queue.
//!
//! Unlike Spotify, Jellyfin hands over songs, not a player: a list chosen
//! becomes Reborn's queue (`Track` entries whose `filename` is the item id),
//! so Previous/Next, shuffle, repeat, seeking and gapless playback are
//! Reborn's own. `playback::BoundDecoder` streams each song from the server
//! (`reborn_platform::jellyfin::Stream`). The server hears what plays, so its
//! play counts, "last played" and scrobbling plugins follow.
//!
//! Network work runs in short worker threads that answer on a channel, as
//! Last.fm's and Spotify's do. The token is never logged.
use crate::Runtime;
use reborn_core::{JellyfinRequest, PlaybackState, QueueEntryId, Track};
use reborn_observability::Level;
use reborn_platform::jellyfin::{self as jf, Account, Client, Error, Found, Item, QuickConnect};
use reborn_ui::ServiceRow;
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

/// How often a Quick Connect asks whether it was approved, and for how long
/// (the server forgets a code after a few minutes; a new one is a tap away).
const POLL_EVERY: Duration = Duration::from_secs(3);
const APPROVE_FOR: Duration = Duration::from_secs(5 * 60);
/// How long the network is listened to for servers.
const DISCOVER_FOR: Duration = Duration::from_millis(1500);
/// The server hears the position this often while a song plays.
const PROGRESS_EVERY: Duration = Duration::from_secs(10);

pub enum Reply {
    Found(Vec<Found>),
    /// The address checked: (base address, server name, server id).
    Checked(Result<(String, String, String), Error>),
    Code(Result<QuickConnect, Error>),
    Polled(Result<bool, Error>),
    SignedIn(Result<Account, Error>),
    Listing {
        key: String,
        more: bool,
        result: Result<jf::Listing, Error>,
    },
}

/// A worker's answer, tagged with the request sequence it belongs to.
type Tagged = (u64, Reply);

pub struct Jellyfin {
    path: PathBuf,
    /// The server chosen, signed in or not yet.
    account: Option<Account>,
    found: Vec<Found>,
    searching: bool,
    /// A Quick Connect waiting for approval, and when it started.
    approval: Option<(QuickConnect, Instant)>,
    next_poll: Instant,
    polling: bool,
    busy: bool,
    problem: Option<String>,
    /// Each list's items as received: what its rows play.
    items: HashMap<String, Vec<Item>>,
    sequence: u64,
    requests: HashMap<String, u64>,
    replies: (Sender<Tagged>, Receiver<Tagged>),
    /// The song the server was told plays: (queue entry, item id).
    reported: Option<(QueueEntryId, String)>,
    reported_paused: bool,
    last_progress: Instant,
    /// The position last known for the reported song (for "stopped").
    reported_at_ms: u64,
}
impl Jellyfin {
    pub fn load(root: &std::path::Path) -> Self {
        let path = jf::account_path(root);
        let account = jf::load_account(&path);
        jf::use_account(account.clone());
        Self {
            path,
            account,
            found: vec![],
            searching: false,
            approval: None,
            next_poll: Instant::now(),
            polling: false,
            busy: false,
            problem: None,
            items: HashMap::new(),
            sequence: 0,
            requests: HashMap::new(),
            replies: channel(),
            reported: None,
            reported_paused: false,
            last_progress: Instant::now(),
            reported_at_ms: 0,
        }
    }
    fn client(&self) -> Option<Client> {
        self.account.clone().map(Client::new)
    }
    fn signed_in(&self) -> bool {
        self.account.as_ref().is_some_and(Account::signed_in)
    }
}

/// A song as a queue entry. The id is negative (never a library row) and
/// stable per song, so Now Playing sees a new song as new.
pub fn track_of(item: &Item) -> Track {
    let key = reborn_core::logo_key(&item.id) as i64 & 0x3fff_ffff_ffff_ffff;
    Track {
        id: -(key + 16),
        source_id: reborn_core::JELLYFIN_SOURCE.into(),
        path: PathBuf::from(format!("jellyfin:{}", item.id)),
        filename: item.id.clone(),
        title: item.name.clone(),
        artist: item.artist.clone(),
        album: item.album.clone(),
        album_artist: if item.album_artist.is_empty() {
            item.artist.clone()
        } else {
            item.album_artist.clone()
        },
        track: item.track,
        disc: item.disc,
        duration_ms: item.duration_ms,
        codec: item.container.clone(),
        sample_rate: item.sample_rate,
        channels: 2,
        bitrate: item.bitrate,
        online: true,
        ..Default::default()
    }
}

/// A list row: songs show their artist, albums their artist (and year),
/// playlists how many songs they hold.
pub fn row_of(item: &Item) -> ServiceRow {
    let detail = match item.kind.as_str() {
        "Audio" => item.artist.clone(),
        "MusicAlbum" if item.year > 0 && !item.album_artist.is_empty() => {
            format!("{} · {}", item.album_artist, item.year)
        }
        "MusicAlbum" => item.album_artist.clone(),
        "Playlist" if item.count == 1 => "1 song".into(),
        "Playlist" if item.count > 0 => format!("{} songs", item.count),
        _ => String::new(),
    };
    ServiceRow {
        title: item.name.clone(),
        detail,
        uri: String::new(),
        id: item.id.clone(),
    }
}

/// The songs of a list as a queue: the tracks and where `row` lands.
pub fn queue_of(items: &[Item], row: usize) -> (Vec<Track>, usize) {
    let mut start = 0;
    let mut tracks = vec![];
    for (i, item) in items.iter().enumerate() {
        if item.kind != "Audio" {
            continue;
        }
        if i < row {
            start += 1;
        }
        tracks.push(track_of(item));
    }
    (tracks, start)
}

impl Runtime {
    fn jellyfin_spawn(&self, name: &str, job: impl FnOnce() -> Reply + Send + 'static) -> bool {
        let reply = self.jellyfin.replies.0.clone();
        let sequence = self.jellyfin.sequence;
        std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                let _ = reply.send((sequence, job()));
            })
            .is_ok()
    }
    fn jellyfin_online(&self) -> bool {
        // A server at home needs the network, not the internet.
        self.ui.radio.online
    }

    /// What the Jellyfin page shows.
    pub(crate) fn jellyfin_view(&mut self) {
        let j = &self.jellyfin;
        let approval = j.approval.as_ref().map(|(q, _)| q.url.clone());
        let code = if approval == self.ui.jellyfin.approval {
            self.ui.jellyfin.code.clone()
        } else {
            approval
                .as_deref()
                .and_then(|url| reborn_ui::qr::encode(url.as_bytes()))
        };
        let view = &mut self.ui.jellyfin;
        view.server = j.account.as_ref().map(|a| {
            let name = if a.server_name.is_empty() {
                a.server.clone()
            } else {
                a.server_name.clone()
            };
            (name, a.server.clone())
        });
        view.user = j
            .account
            .as_ref()
            .filter(|a| a.signed_in())
            .map(|a| a.user_name.clone());
        view.found = j
            .found
            .iter()
            .map(|f| {
                let name = if f.name.is_empty() {
                    f.address.clone()
                } else {
                    f.name.clone()
                };
                (name, f.address.clone())
            })
            .collect();
        view.searching = j.searching;
        view.user_code = j
            .approval
            .as_ref()
            .map(|(q, _)| q.code.clone())
            .unwrap_or_default();
        view.approval = approval;
        view.code = code;
        view.busy = j.busy;
        view.problem = j.problem.clone();
        self.dirty.mark_render();
    }

    pub(crate) fn jellyfin_request(&mut self, request: JellyfinRequest) -> Result<(), String> {
        let result = self.jellyfin_request_inner(request);
        self.jellyfin_view();
        result
    }

    fn jellyfin_request_inner(&mut self, request: JellyfinRequest) -> Result<(), String> {
        use JellyfinRequest as R;
        let local = matches!(request, R::Cancel | R::ChangeServer | R::Disconnect)
            || matches!(request, R::Play { .. } | R::PlayAll { .. });
        if !local && !self.jellyfin_online() {
            return Err("Connect to Wi-Fi first".into());
        }
        match request {
            R::Discover => {
                if self.jellyfin.searching {
                    return Ok(());
                }
                self.jellyfin.searching = self
                    .jellyfin_spawn("jellyfin-find", || Reply::Found(jf::discover(DISCOVER_FOR)));
                Ok(())
            }
            R::UseServer(index) => {
                let found = self
                    .jellyfin
                    .found
                    .get(index)
                    .cloned()
                    .ok_or("That server is gone; search again")?;
                self.jellyfin_check(&found.address)
            }
            R::UseAddress(typed) => self.jellyfin_check(&typed),
            R::ChangeServer => {
                self.jellyfin_forget(true);
                self.jellyfin_discover_quietly();
                Ok(())
            }
            R::Connect if self.jellyfin.busy => Ok(()),
            R::Connect => {
                let client = self.jellyfin.client().ok_or("Choose a server first")?;
                self.jellyfin.problem = None;
                self.jellyfin.busy = self
                    .jellyfin_spawn("jellyfin-code", move || Reply::Code(client.quick_connect()));
                Ok(())
            }
            R::Cancel => {
                self.jellyfin.approval = None;
                Ok(())
            }
            R::Disconnect => {
                self.jellyfin_forget(false);
                self.ui.flash("Signed out of Jellyfin");
                Ok(())
            }
            R::Load(key) => self.jellyfin_load(key, 0),
            R::More(key) => {
                let start = self.jellyfin.items.get(&key).map_or(0, Vec::len);
                self.jellyfin_load(key, start)
            }
            R::Play { list, index } => {
                let items = self
                    .jellyfin
                    .items
                    .get(&list)
                    .ok_or("Jellyfin list not found")?;
                let (tracks, start) = queue_of(items, index);
                self.jellyfin_play(&list, tracks, start, false)
            }
            R::PlayAll { list, shuffle } => {
                let items = self
                    .jellyfin
                    .items
                    .get(&list)
                    .ok_or("Jellyfin list not found")?;
                let (tracks, _) = queue_of(items, 0);
                self.jellyfin_play(&list, tracks, 0, shuffle)
            }
        }
    }

    /// Is this address a Jellyfin server? (then sign in there)
    fn jellyfin_check(&mut self, typed: &str) -> Result<(), String> {
        let address = jf::base_url(typed).ok_or("That isn't a server address")?;
        self.jellyfin.problem = None;
        self.jellyfin.busy = true;
        let probe = Client::new(Account {
            server: address.clone(),
            device_id: jf::new_device_id(),
            ..Default::default()
        });
        let started = self.jellyfin_spawn("jellyfin-check", move || {
            Reply::Checked(probe.public_info().map(|(name, id, _)| (address, name, id)))
        });
        if !started {
            self.jellyfin.busy = false;
            return Err("Couldn't ask the server".into());
        }
        Ok(())
    }

    /// Look for servers when the Jellyfin page opens without one.
    pub(crate) fn jellyfin_discover_quietly(&mut self) {
        if self.jellyfin.account.is_none()
            && !self.jellyfin.searching
            && self.jellyfin.found.is_empty()
            && self.jellyfin_online()
        {
            let _ = self.jellyfin_request_inner(JellyfinRequest::Discover);
            self.jellyfin_view();
        }
    }

    fn jellyfin_load(&mut self, key: String, start: usize) -> Result<(), String> {
        let client = self
            .jellyfin
            .client()
            .filter(|c| c.account.signed_in())
            .ok_or("Sign in to Jellyfin first")?;
        if jf::query_for(&key, &client.account.user_id, start).is_none() {
            return Err("unknown Jellyfin list".into());
        }
        self.jellyfin.sequence += 1;
        self.jellyfin
            .requests
            .insert(key.clone(), self.jellyfin.sequence);
        let started = self.jellyfin_spawn("jellyfin-list", move || Reply::Listing {
            result: client.list(&key, start),
            key,
            more: start > 0,
        });
        if !started {
            return Err("Couldn't ask the server".into());
        }
        Ok(())
    }

    /// A list's songs become the queue, from `start` (or shuffled).
    fn jellyfin_play(
        &mut self,
        list: &str,
        mut tracks: Vec<Track>,
        mut start: usize,
        shuffle: bool,
    ) -> Result<(), String> {
        if tracks.is_empty() {
            return Err("No songs to play here".into());
        }
        if !self.jellyfin.signed_in() {
            return Err("Sign in to Jellyfin first".into());
        }
        tracks.truncate(reborn_core::MAX_QUEUE);
        start = start.min(tracks.len() - 1);
        let natural: Vec<usize>;
        if shuffle {
            // The list's own order stays known: Shuffle off brings it back.
            let mut order: Vec<usize> = (0..tracks.len()).collect();
            crate::shuffle_from(&mut order, 0, crate::shuffle_seed());
            let mut position = vec![0; tracks.len()];
            for (at, &original) in order.iter().enumerate() {
                position[original] = at;
            }
            natural = position;
            tracks = order.iter().map(|&i| tracks[i].clone()).collect();
            start = 0;
        } else {
            natural = (0..tracks.len()).collect();
        }
        self.save_book();
        let title = self
            .ui
            .jellyfin
            .lists
            .get(list)
            .map(|l| l.title.clone())
            .unwrap_or_default();
        self.model.replace_queue(tracks, start)?;
        self.model
            .set_source(format!("Jellyfin: {title}"), Some(&natural));
        self.model.position_ms = 0;
        self.art = false;
        self.log.emit(
            Level::Info,
            "jellyfin",
            "play_requested",
            "Jellyfin playback requested",
            None,
            json!({"songs": self.model.queue.len(), "shuffle": shuffle}),
        );
        self.load()
    }

    /// Sign out (keeping the server), or forget the server too.
    fn jellyfin_forget(&mut self, server: bool) {
        if self.model.current().is_some_and(Track::is_jellyfin) {
            self.jellyfin_report_stop();
            self.model.invalidate();
            self.playback.stop(self.model.generation);
            self.model.playback = PlaybackState::Stopped;
        }
        if let Some(client) = self.jellyfin.client().filter(|c| c.account.signed_in()) {
            let _ = std::thread::Builder::new()
                .name("jellyfin-logout".into())
                .spawn(move || client.sign_out());
        }
        self.jellyfin.approval = None;
        self.jellyfin.items.clear();
        self.ui.jellyfin.lists.clear();
        self.ui.jellyfin.song = None;
        self.jellyfin.account = if server {
            None
        } else {
            self.jellyfin.account.take().map(|a| Account {
                token: String::new(),
                user_id: String::new(),
                user_name: String::new(),
                ..a
            })
        };
        jf::use_account(None);
        let saved = match &self.jellyfin.account {
            Some(account) => jf::save_account(&self.jellyfin.path, account),
            None => jf::forget_account(&self.jellyfin.path),
        };
        if let Err(e) = saved {
            self.log.emit(
                Level::Warn,
                "jellyfin",
                "forget_failed",
                "Jellyfin sign-in not removed",
                None,
                json!({"error": e.to_string()}),
            );
        }
        self.log.emit(
            Level::Info,
            "jellyfin",
            "signed_out",
            "Jellyfin signed out",
            None,
            json!({"server_forgotten": server}),
        );
    }

    /// Replies, the approval poll, and what the server hears.
    pub(crate) fn jellyfin_tick(&mut self) {
        let replies: Vec<Tagged> = self.jellyfin.replies.1.try_iter().collect();
        for (sequence, reply) in replies {
            self.jellyfin_reply(sequence, reply);
        }
        if let Some(started) = self.jellyfin.approval.as_ref().map(|a| a.1) {
            if started.elapsed() > APPROVE_FOR {
                self.jellyfin.approval = None;
                self.jellyfin.problem = Some("The code expired. Try again.".into());
                self.jellyfin_view();
            } else if !self.jellyfin.polling
                && Instant::now() >= self.jellyfin.next_poll
                && self.jellyfin_online()
            {
                let secret = self
                    .jellyfin
                    .approval
                    .as_ref()
                    .map(|a| a.0.secret().to_owned());
                if let (Some(client), Some(secret)) = (self.jellyfin.client(), secret) {
                    self.jellyfin.polling = self.jellyfin_spawn("jellyfin-poll", move || {
                        Reply::Polled(client.quick_connect_approved(&secret))
                    });
                    self.jellyfin.next_poll = Instant::now() + POLL_EVERY;
                }
            }
        }
        if self.model.screen == reborn_core::Screen::Jellyfin {
            self.jellyfin_discover_quietly();
        }
        self.jellyfin_follow();
    }

    /// The song playing: Now Playing's cover and line, and the server's
    /// reports (started, progress, paused, stopped).
    fn jellyfin_follow(&mut self) {
        let current = self.model.current().filter(|t| t.is_jellyfin()).map(|t| {
            (
                t.filename.clone(),
                t.title.clone(),
                t.artist.clone(),
                t.album.clone(),
            )
        });
        let entry = self.model.current_entry_id();
        let playing = matches!(
            self.model.playback,
            PlaybackState::Playing | PlaybackState::Buffering
        );
        let Some((item, title, artist, _)) = current else {
            if self.jellyfin.reported.is_some() {
                self.jellyfin_report_stop();
            }
            if self.ui.jellyfin.song.take().is_some() {
                self.dirty.mark_render();
            }
            return;
        };
        if self.ui.jellyfin.song.as_ref() != Some(&(title.clone(), artist.clone())) {
            self.ui.jellyfin.song = Some((title, artist));
            self.dirty.mark_render();
        }
        let key = entry.map(|e| (e, item.clone()));
        if self.jellyfin.reported.as_ref().map(|r| &r.1) != key.as_ref().map(|k| &k.1)
            || self.jellyfin.reported.as_ref().map(|r| r.0) != key.as_ref().map(|k| k.0)
        {
            if !playing {
                return;
            }
            self.jellyfin_report_stop();
            // The cover: its album's, else the song's own.
            if let Some(account) = &self.jellyfin.account {
                let album = self
                    .model
                    .current()
                    .and_then(|t| {
                        self.jellyfin
                            .items
                            .values()
                            .flatten()
                            .find(|i| i.id == t.filename)
                            .map(|i| i.album_id.clone())
                    })
                    .filter(|a| !a.is_empty())
                    .unwrap_or_else(|| item.clone());
                let url = jf::image_url(&account.server, &album);
                self.stream_cover = Some((reborn_core::logo_key(&url), url));
                self.art = false;
                self.radio_art_pending = None;
            }
            self.jellyfin.reported = key;
            self.jellyfin.reported_paused = false;
            self.jellyfin.last_progress = Instant::now();
            self.jellyfin_send(jf::Report::Started { item });
            return;
        }
        self.jellyfin.reported_at_ms = self.model.position_ms;
        let paused = !playing;
        if paused != self.jellyfin.reported_paused
            || (playing && self.jellyfin.last_progress.elapsed() >= PROGRESS_EVERY)
        {
            self.jellyfin.reported_paused = paused;
            self.jellyfin.last_progress = Instant::now();
            self.jellyfin_send(jf::Report::Progress {
                item,
                position_ms: self.model.position_ms,
                paused,
            });
        }
    }

    fn jellyfin_report_stop(&mut self) {
        if let Some((_, item)) = self.jellyfin.reported.take() {
            let position_ms = self.jellyfin.reported_at_ms;
            self.jellyfin_send(jf::Report::Stopped { item, position_ms });
        }
        if self
            .stream_cover
            .as_ref()
            .is_some_and(|_| !self.model.current().is_some_and(Track::is_streamed))
        {
            self.stream_cover = None;
        }
    }

    /// Reports are best effort: a lost one costs a play count, nothing more.
    fn jellyfin_send(&self, report: jf::Report) {
        let Some(client) = self.jellyfin.client().filter(|c| c.account.signed_in()) else {
            return;
        };
        let _ = std::thread::Builder::new()
            .name("jellyfin-report".into())
            .spawn(move || {
                let _ = client.report(&report);
            });
    }

    fn jellyfin_reply(&mut self, sequence: u64, reply: Reply) {
        match reply {
            Reply::Found(found) => {
                self.jellyfin.searching = false;
                self.jellyfin.found = found;
            }
            Reply::Checked(result) => {
                self.jellyfin.busy = false;
                match result {
                    Ok((server, server_name, server_id)) => {
                        let account = Account {
                            server,
                            server_name,
                            server_id,
                            device_id: jf::new_device_id(),
                            ..Default::default()
                        };
                        if let Err(e) = jf::save_account(&self.jellyfin.path, &account) {
                            self.ui.flash(format!("Couldn't save the server: {e}"));
                        }
                        self.log.emit(
                            Level::Info,
                            "jellyfin",
                            "server_chosen",
                            "Jellyfin server chosen",
                            None,
                            json!({}),
                        );
                        self.jellyfin.account = Some(account);
                        // Straight on to the sign-in.
                        let _ = self.jellyfin_request_inner(JellyfinRequest::Connect);
                    }
                    Err(e) => self.jellyfin.problem = Some(e.message()),
                }
            }
            Reply::Code(result) => {
                self.jellyfin.busy = false;
                match result {
                    Ok(code) => {
                        self.jellyfin.next_poll = Instant::now() + POLL_EVERY;
                        self.jellyfin.approval = Some((code, Instant::now()));
                    }
                    // 401 here: Quick Connect is switched off on the server.
                    Err(Error::Unauthorized) => {
                        self.jellyfin.problem = Some(
                            "Quick Connect is off on this server. Turn it on under Dashboard › General."
                                .into(),
                        )
                    }
                    Err(e) => self.jellyfin.problem = Some(e.message()),
                }
            }
            Reply::Polled(result) => {
                self.jellyfin.polling = false;
                match result {
                    Ok(false) | Err(Error::Offline(_)) => {}
                    Ok(true) => {
                        let secret = self
                            .jellyfin
                            .approval
                            .take()
                            .map(|a| a.0.secret().to_owned());
                        if let (Some(client), Some(secret)) = (self.jellyfin.client(), secret) {
                            self.jellyfin.busy = self
                                .jellyfin_spawn("jellyfin-sign-in", move || {
                                    Reply::SignedIn(client.sign_in(&secret))
                                });
                        }
                    }
                    // The server forgot the code.
                    Err(_) => {
                        self.jellyfin.approval = None;
                        self.jellyfin.problem = Some("The code expired. Try again.".into());
                    }
                }
            }
            Reply::SignedIn(result) => {
                self.jellyfin.busy = false;
                match result {
                    Ok(account) => {
                        if let Err(e) = jf::save_account(&self.jellyfin.path, &account) {
                            self.ui
                                .flash(format!("Couldn't save the Jellyfin sign-in: {e}"));
                            return self.jellyfin_view();
                        }
                        self.ui
                            .flash(format!("Signed in to Jellyfin as {}", account.user_name));
                        self.log.emit(
                            Level::Info,
                            "jellyfin",
                            "signed_in",
                            "Jellyfin signed in",
                            None,
                            json!({}),
                        );
                        jf::use_account(Some(account.clone()));
                        self.jellyfin.account = Some(account);
                        self.jellyfin.problem = None;
                        self.jellyfin.items.clear();
                        self.ui.jellyfin.lists.clear();
                    }
                    Err(e) => self.jellyfin.problem = Some(e.message()),
                }
            }
            Reply::Listing { key, more, result } => {
                if self.jellyfin.requests.get(&key) != Some(&sequence) {
                    return;
                }
                if result == Err(Error::Unauthorized) {
                    // Revoked on the server (Dashboard › Devices).
                    self.jellyfin_forget(false);
                    self.jellyfin.problem = Some(Error::Unauthorized.message());
                    return self.jellyfin_view();
                }
                let list = self.ui.jellyfin.lists.entry(key.clone()).or_default();
                list.loading = false;
                match result {
                    Ok(listing) => {
                        let items = self.jellyfin.items.entry(key).or_default();
                        if !more {
                            items.clear();
                            list.rows.clear();
                        }
                        list.rows.extend(listing.items.iter().map(row_of));
                        items.extend(listing.items);
                        list.next = listing.next.map(|n| n.to_string());
                        list.error = None;
                    }
                    Err(e) => list.error = Some(e.message()),
                }
            }
        }
        self.jellyfin_view();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song(id: &str, name: &str) -> Item {
        Item {
            id: id.into(),
            name: name.into(),
            kind: "Audio".into(),
            artist: "Artist".into(),
            album: "Album".into(),
            duration_ms: 200_000,
            container: "flac".into(),
            ..Default::default()
        }
    }

    #[test]
    fn songs_become_queue_entries_without_secrets_and_with_stable_ids() {
        let item = song("abcdef0123", "Song");
        let t = track_of(&item);
        assert!(t.is_jellyfin() && t.is_streamed() && !t.is_live());
        assert!(t.id < 0);
        assert_eq!(t.id, track_of(&item).id);
        assert_ne!(t.id, track_of(&song("abcdef0124", "Song")).id);
        assert_eq!(t.filename, "abcdef0123");
        assert_eq!(t.album_artist, "Artist");
        assert_eq!(t.duration_ms, 200_000);
        // What the session stores: no address, no token.
        let stored = serde_json::to_string(&t).unwrap();
        assert!(!stored.contains("http"));
    }

    #[test]
    fn a_row_plays_from_its_place_among_the_songs() {
        let mut folder = song("ff", "A folder");
        folder.kind = "Folder".into();
        let items = vec![
            song("a1", "One"),
            folder,
            song("a2", "Two"),
            song("a3", "Three"),
        ];
        let (tracks, start) = queue_of(&items, 2);
        assert_eq!(tracks.len(), 3);
        assert_eq!(start, 1);
        assert_eq!(tracks[start].title, "Two");
    }

    #[test]
    fn rows_say_what_each_item_is() {
        assert_eq!(row_of(&song("a", "S")).detail, "Artist");
        let mut album = song("b", "Kind of Blue");
        album.kind = "MusicAlbum".into();
        album.album_artist = "Miles Davis".into();
        album.year = 1959;
        assert_eq!(row_of(&album).detail, "Miles Davis · 1959");
        let mut playlist = song("c", "Mix");
        playlist.kind = "Playlist".into();
        playlist.count = 12;
        assert_eq!(row_of(&playlist).detail, "12 songs");
    }
}
