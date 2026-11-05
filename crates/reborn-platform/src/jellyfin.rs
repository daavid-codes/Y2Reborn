//! Jellyfin: the listener's own music server.
//!
//! - **Finding it:** a UDP broadcast ("Who is JellyfinServer?", port 7359)
//!   finds servers on the home network; an address can be typed instead.
//! - **Sign-in:** Quick Connect. The player asks the server for a six-digit
//!   code and shows a QR code for the server's own approval page with the code
//!   filled in (`/web/#/quickconnect?code=…`); the listener taps Authorize on
//!   a phone already signed in to Jellyfin, and the player trades the secret
//!   for an access token. No password on the player. The token lasts until it
//!   is revoked (Dashboard → Devices, or Disconnect here).
//! - **Library and playback:** Jellyfin's REST API over `curl`, as Last.fm and
//!   radio do. A song plays as a stream into Reborn's decoder: untouched when
//!   Reborn can decode it from a pipe, else converted to MP3 by the server.
//!   Seeking asks the server for the song from that point on.
//!
//! The token travels only in curl's configuration on its standard input,
//! never on a command line, in a log or in diagnostics. The account file is
//! root-only (0600) in `/data/reborn/state/jellyfin`.

use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Identifies Jellyfin songs in the play queue (`Track::source_id`).
pub const SOURCE_ID: &str = reborn_core::JELLYFIN_SOURCE;
const CURL: &str = "/usr/bin/curl";
const CLIENT: &str = "Y2Reborn";
const DEVICE: &str = "Innioasis Y2";
const VERSION: &str = env!("CARGO_PKG_VERSION");
const DISCOVERY_PORT: u16 = 7359;
/// Containers Reborn's stream decoder reads from a pipe as they are. MP4/M4A
/// is left out: its index may sit at the end, which a pipe never reaches,
/// so the server converts those.
const DIRECT: &str = "flac,mp3,ogg,opus,wav,aac";
/// What the server converts everything else to.
const TRANSCODE_BITRATE: u32 = 320_000;
/// A page of a list; More fetches the next.
pub const PAGE: usize = 200;

fn curl() -> String {
    std::env::var("REBORN_CURL").unwrap_or_else(|_| CURL.into())
}

// ----------------------------------------------------------------- account

/// The server and the sign-in, as stored.
#[derive(Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Account {
    /// The server's base address, without a trailing slash.
    pub server: String,
    #[serde(default)]
    pub server_name: String,
    #[serde(default)]
    pub server_id: String,
    /// Empty until signed in.
    #[serde(default)]
    pub user_id: String,
    #[serde(default)]
    pub user_name: String,
    #[serde(default)]
    pub token: String,
    /// This player, as the server lists it under Devices.
    pub device_id: String,
}
impl Account {
    pub fn signed_in(&self) -> bool {
        !self.token.is_empty() && !self.user_id.is_empty()
    }
}
impl std::fmt::Debug for Account {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Account {{ server: {:?}, user: {:?}, .. }}",
            self.server, self.user_name
        )
    }
}

pub fn account_path(root: &Path) -> PathBuf {
    root.join("state/jellyfin/account.json")
}

pub fn load_account(path: &Path) -> Option<Account> {
    let a: Account = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
    (base_url(&a.server).is_some() && !a.device_id.is_empty()).then_some(a)
}

pub fn save_account(path: &Path, account: &Account) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    {
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(&serde_json::to_vec(account).map_err(io::Error::other)?)?;
        file.sync_all()?;
    }
    fs::rename(tmp, path)
}

pub fn forget_account(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// A new device id: 16 random bytes as hex.
pub fn new_device_id() -> String {
    format!("{:016x}{:016x}", crate::random_u64(), crate::random_u64())
}

/// "jelly.local:8096", "https://music.example.org/" → a base address, or
/// None when it can't be one. A bare host gets http:// and, without a port,
/// Jellyfin's 8096.
pub fn base_url(typed: &str) -> Option<String> {
    let typed = typed.trim().trim_end_matches('/');
    if typed.is_empty()
        || typed.len() > 512
        || typed.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return None;
    }
    let lower = typed.to_ascii_lowercase();
    let (scheme, rest) = if let Some(rest) = lower.strip_prefix("https://") {
        ("https://", &typed[typed.len() - rest.len()..])
    } else if let Some(rest) = lower.strip_prefix("http://") {
        ("http://", &typed[typed.len() - rest.len()..])
    } else if lower.contains("://") {
        return None;
    } else {
        ("http://", typed)
    };
    let host = rest.split('/').next().unwrap_or("");
    if host.is_empty() || host.contains(['@', '?', '#']) {
        return None;
    }
    // A port is digits after a name ("http:" alone is no host).
    if let Some((name, port)) = host.rsplit_once(':') {
        if name.is_empty() || port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
    }
    let with_port = scheme == "http://" && !host.contains(':') && rest == host;
    Some(if with_port {
        format!("{scheme}{rest}:8096")
    } else {
        format!("{scheme}{rest}")
    })
}

// ----------------------------------------------------------------- requests

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The server can't be reached (or curl is missing).
    Offline(String),
    /// 401: the token was revoked or Quick Connect is off.
    Unauthorized,
    /// Another HTTP status.
    Status(u16),
    /// A reply that isn't what Jellyfin sends.
    Unreadable,
}
impl Error {
    pub fn message(&self) -> String {
        match self {
            Self::Offline(_) => "The Jellyfin server can't be reached.".into(),
            Self::Unauthorized => "Jellyfin needs you to sign in again.".into(),
            Self::Status(404) => "Jellyfin doesn't have that any more.".into(),
            Self::Status(code) => format!("The Jellyfin server answered {code}."),
            Self::Unreadable => "That doesn't look like a Jellyfin server.".into(),
        }
    }
}

/// Jellyfin's client header: who asks, and with which token.
pub fn authorization(device_id: &str, token: &str) -> String {
    let quote = |s: &str| s.replace(['"', '\\'], "");
    let mut header = format!(
        "MediaBrowser Client=\"{CLIENT}\", Device=\"{DEVICE}\", DeviceId=\"{}\", Version=\"{VERSION}\"",
        quote(device_id)
    );
    if !token.is_empty() {
        header.push_str(&format!(", Token=\"{}\"", quote(token)));
    }
    header
}

/// A curl configuration line's quoted value (curl's own escaping).
fn config_value(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// curl's configuration for one request: the address, the headers and the
/// body. It goes to curl on stdin (`-K -`), so the token is never on a
/// command line.
pub fn curl_config(url: &str, auth: &str, body: Option<&str>) -> String {
    let mut config = format!(
        "url = {}\nheader = {}\nheader = \"Accept: application/json\"\n",
        config_value(url),
        config_value(&format!("Authorization: {auth}"))
    );
    if let Some(body) = body {
        config.push_str("header = \"Content-Type: application/json\"\n");
        config.push_str(&format!("data-binary = {}\n", config_value(body)));
    }
    config
}

/// One request; the reply's body and HTTP status.
fn request(config: &str, timeout_s: u32) -> Result<(u16, Vec<u8>), Error> {
    request_with(&curl(), config, timeout_s)
}

fn request_with(curl: &str, config: &str, timeout_s: u32) -> Result<(u16, Vec<u8>), Error> {
    let mut child = Command::new(curl)
        .args(["-sS", "--proto", "=http,https", "--max-time"])
        .arg(timeout_s.to_string())
        .args(["--connect-timeout", "8", "-A"])
        .arg(format!("{CLIENT}/{VERSION}"))
        .args(["-w", "\n%{http_code}", "-K", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| Error::Offline(e.to_string()))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(config.as_bytes())
            .map_err(|e| Error::Offline(e.to_string()))?;
    }
    let mut reply = vec![];
    if let Some(mut out) = child.stdout.take() {
        let _ = out.by_ref().take(8 << 20).read_to_end(&mut reply);
    }
    let status = child.wait().map_err(|e| Error::Offline(e.to_string()))?;
    let split = reply.iter().rposition(|&b| b == b'\n');
    let code = split
        .and_then(|i| std::str::from_utf8(&reply[i + 1..]).ok())
        .and_then(|s| s.trim().parse::<u16>().ok())
        .unwrap_or(0);
    if code == 0 {
        return Err(Error::Offline(format!(
            "curl {}",
            status.code().unwrap_or(-1)
        )));
    }
    reply.truncate(split.unwrap_or(0));
    Ok((code, reply))
}

fn json(result: Result<(u16, Vec<u8>), Error>) -> Result<serde_json::Value, Error> {
    let (code, body) = result?;
    match code {
        200..=299 if body.is_empty() => Ok(serde_json::Value::Null),
        200..=299 => serde_json::from_slice(&body).map_err(|_| Error::Unreadable),
        401 | 403 => Err(Error::Unauthorized),
        code => Err(Error::Status(code)),
    }
}

/// A signed-in (or, before sign-in, anonymous) connection to one server.
#[derive(Clone)]
pub struct Client {
    pub account: Account,
}
impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Client {{ {:?} }}", self.account)
    }
}
impl Client {
    pub fn new(account: Account) -> Self {
        Self { account }
    }
    fn auth(&self) -> String {
        authorization(&self.account.device_id, &self.account.token)
    }
    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.account.server)
    }
    pub fn get(&self, path: &str) -> Result<serde_json::Value, Error> {
        json(request(
            &curl_config(&self.url(path), &self.auth(), None),
            30,
        ))
    }
    pub fn post(&self, path: &str, body: &serde_json::Value) -> Result<serde_json::Value, Error> {
        let body = body.to_string();
        json(request(
            &curl_config(&self.url(path), &self.auth(), Some(&body)),
            30,
        ))
    }

    /// The server's name, id and version: is this address a Jellyfin server?
    pub fn public_info(&self) -> Result<(String, String, String), Error> {
        let v = self.get("/System/Info/Public")?;
        let text = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_owned();
        let id = text("Id");
        if id.is_empty() {
            return Err(Error::Unreadable);
        }
        Ok((text("ServerName"), id, text("Version")))
    }

    // ----------------------------------------------------------- Quick Connect

    /// Start Quick Connect: the code to approve and the secret to poll with.
    pub fn quick_connect(&self) -> Result<QuickConnect, Error> {
        // POST since 10.9; GET before.
        let v = match self.post("/QuickConnect/Initiate", &serde_json::Value::Null) {
            Err(Error::Status(404 | 405)) => self.get("/QuickConnect/Initiate")?,
            other => other?,
        };
        parse_quick_connect(&v, &self.account.server)
    }
    /// Approved yet?
    pub fn quick_connect_approved(&self, secret: &str) -> Result<bool, Error> {
        let v = self.get(&format!("/QuickConnect/Connect?secret={}", encode(secret)))?;
        Ok(v.get("Authenticated").and_then(|a| a.as_bool()) == Some(true))
    }
    /// Trade an approved secret for the account's token.
    pub fn sign_in(&self, secret: &str) -> Result<Account, Error> {
        let v = self.post(
            "/Users/AuthenticateWithQuickConnect",
            &serde_json::json!({ "Secret": secret }),
        )?;
        let token = v.get("AccessToken").and_then(|t| t.as_str()).unwrap_or("");
        let user = v.get("User").unwrap_or(&serde_json::Value::Null);
        let field = |k: &str| user.get(k).and_then(|x| x.as_str()).unwrap_or("");
        if token.is_empty() || field("Id").is_empty() {
            return Err(Error::Unreadable);
        }
        Ok(Account {
            token: token.to_owned(),
            user_id: field("Id").to_owned(),
            user_name: field("Name").to_owned(),
            ..self.account.clone()
        })
    }
    /// Sign out on the server too (the device leaves its list). Best effort.
    pub fn sign_out(&self) {
        let _ = self.post("/Sessions/Logout", &serde_json::Value::Null);
    }

    // ----------------------------------------------------------------- library

    /// One page of list `key` (see `query_for`), starting at `start`.
    pub fn list(&self, key: &str, start: usize) -> Result<Listing, Error> {
        let path = query_for(key, &self.account.user_id, start).ok_or(Error::Status(400))?;
        let v = self.get(&path)?;
        Ok(parse_listing(&v, start))
    }

    // ----------------------------------------------------------------- playback

    /// Where song `item` streams from, from `start_ms` on.
    pub fn stream_url(&self, item: &str, start_ms: u64) -> String {
        stream_url(&self.account, item, start_ms)
    }
    /// The server's play reports: started, progress, stopped.
    pub fn report(&self, report: &Report) -> Result<(), Error> {
        let (path, body) = report.request();
        self.post(path, &body).map(|_| ())
    }
}

/// A Quick Connect request waiting for approval.
#[derive(Clone)]
pub struct QuickConnect {
    pub code: String,
    secret: String,
    /// The server's own approval page with the code filled in.
    pub url: String,
}
impl QuickConnect {
    pub fn secret(&self) -> &str {
        &self.secret
    }
}
impl std::fmt::Debug for QuickConnect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "QuickConnect {{ code: {:?}, .. }}", self.code)
    }
}

pub fn parse_quick_connect(v: &serde_json::Value, server: &str) -> Result<QuickConnect, Error> {
    let text = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_owned();
    let (code, secret) = (text("Code"), text("Secret"));
    if code.is_empty() || secret.is_empty() || !code.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(Error::Unreadable);
    }
    Ok(QuickConnect {
        url: format!("{server}/web/#/quickconnect?code={code}"),
        code,
        secret,
    })
}

fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Ids are Jellyfin's 32-digit hex (with or without dashes).
fn id_ok(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
}

const SONG_FIELDS: &str = "Fields=MediaSources&EnableImages=false&EnableUserData=false";

/// The request behind each list key. Keys: playlists, favorites, albums,
/// artists, playlist:<id>, album:<id>, artist-albums:<id>, artist-songs:<id>.
pub fn query_for(key: &str, user: &str, start: usize) -> Option<String> {
    let (kind, id) = key.split_once(':').unwrap_or((key, ""));
    if !id_ok(user) || (!id.is_empty() && !id_ok(id)) {
        return None;
    }
    let page = format!("StartIndex={start}&Limit={PAGE}");
    let items = format!("/Items?userId={user}&Recursive=true&{page}");
    Some(match (kind, id.is_empty()) {
        ("playlists", true) => format!(
            "{items}&IncludeItemTypes=Playlist&MediaTypes=Audio&SortBy=SortName&EnableImages=false"
        ),
        ("favorites", true) => format!(
            "{items}&IncludeItemTypes=Audio&Filters=IsFavorite&SortBy=SortName&{SONG_FIELDS}"
        ),
        ("albums", true) => format!(
            "{items}&IncludeItemTypes=MusicAlbum&SortBy=AlbumArtist,SortName&EnableImages=false"
        ),
        ("artists", true) => format!(
            "/Artists/AlbumArtists?userId={user}&{page}&SortBy=SortName&EnableImages=false"
        ),
        ("playlist", false) => format!("/Playlists/{id}/Items?userId={user}&{page}&{SONG_FIELDS}"),
        ("album", false) => format!(
            "{items}&ParentId={id}&IncludeItemTypes=Audio&SortBy=ParentIndexNumber,IndexNumber,SortName&{SONG_FIELDS}"
        ),
        ("artist-albums", false) => format!(
            "{items}&AlbumArtistIds={id}&IncludeItemTypes=MusicAlbum&SortBy=ProductionYear,SortName&EnableImages=false"
        ),
        ("artist-songs", false) => format!(
            "{items}&ArtistIds={id}&IncludeItemTypes=Audio&SortBy=Album,ParentIndexNumber,IndexNumber,SortName&{SONG_FIELDS}"
        ),
        _ => return None,
    })
}

/// One row of a Jellyfin list: a song, an album, an artist or a playlist.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Item {
    pub id: String,
    pub name: String,
    /// Audio, MusicAlbum, MusicArtist, Playlist.
    pub kind: String,
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    pub album_id: String,
    pub duration_ms: u64,
    pub track: u32,
    pub disc: u32,
    pub year: u32,
    /// The file's container ("flac", "mp3"): Now Playing's quality tag.
    pub container: String,
    pub bitrate: u64,
    pub sample_rate: u32,
    /// Songs in a playlist or album, when the server says.
    pub count: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Listing {
    pub items: Vec<Item>,
    /// Where the next page starts, if there is one.
    pub next: Option<usize>,
}

pub fn parse_listing(v: &serde_json::Value, start: usize) -> Listing {
    let items: Vec<Item> = v
        .get("Items")
        .and_then(|i| i.as_array())
        .map(|a| a.iter().filter_map(parse_item).collect())
        .unwrap_or_default();
    let total = v
        .get("TotalRecordCount")
        .and_then(|t| t.as_u64())
        .unwrap_or(0) as usize;
    let seen = start
        + v.get("Items")
            .and_then(|i| i.as_array())
            .map_or(0, Vec::len);
    Listing {
        items,
        next: (seen < total && seen > start).then_some(seen),
    }
}

fn parse_item(v: &serde_json::Value) -> Option<Item> {
    let text = |k: &str| {
        v.get(k)
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .trim()
            .to_owned()
    };
    let number = |k: &str| v.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
    let id = text("Id");
    let name = text("Name");
    if !id_ok(&id) || name.is_empty() {
        return None;
    }
    let artists: Vec<String> = v
        .get("Artists")
        .and_then(|a| a.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let source = v
        .get("MediaSources")
        .and_then(|m| m.as_array())
        .and_then(|m| m.first());
    let stream = source
        .and_then(|s| s.get("MediaStreams"))
        .and_then(|m| m.as_array())
        .and_then(|m| {
            m.iter()
                .find(|s| s.get("Type").and_then(|t| t.as_str()) == Some("Audio"))
        });
    let container = source
        .and_then(|s| s.get("Container"))
        .or_else(|| v.get("Container"))
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    Some(Item {
        album_artist: text("AlbumArtist"),
        artist: if artists.is_empty() {
            text("AlbumArtist")
        } else {
            artists.join(", ")
        },
        album: text("Album"),
        album_id: text("AlbumId"),
        kind: text("Type"),
        duration_ms: number("RunTimeTicks") / 10_000,
        track: number("IndexNumber") as u32,
        disc: number("ParentIndexNumber") as u32,
        year: number("ProductionYear") as u32,
        container,
        bitrate: source
            .and_then(|s| s.get("Bitrate"))
            .and_then(|b| b.as_u64())
            .unwrap_or(0),
        sample_rate: stream
            .and_then(|s| s.get("SampleRate"))
            .and_then(|r| r.as_u64())
            .unwrap_or(0) as u32,
        count: number("ChildCount") as u32,
        id,
        name,
    })
}

/// A song from the start: Jellyfin's universal endpoint sends the file as it
/// is when Reborn can read it from a pipe, else converts it. From a later
/// point: the server converts from there (a pipe can't seek).
pub fn stream_url(account: &Account, item: &str, start_ms: u64) -> String {
    let (server, user, device) = (&account.server, &account.user_id, &account.device_id);
    if start_ms == 0 {
        format!(
            "{server}/Audio/{item}/universal?UserId={user}&DeviceId={device}&Container={DIRECT}\
             &TranscodingContainer=mp3&TranscodingProtocol=http&AudioCodec=mp3\
             &MaxStreamingBitrate=100000000&EnableRedirection=false&EnableRemoteMedia=false"
        )
    } else {
        format!(
            "{server}/Audio/{item}/stream.mp3?static=false&audioCodec=mp3&audioBitRate={TRANSCODE_BITRATE}\
             &startTimeTicks={}&deviceId={device}&userId={user}",
            start_ms * 10_000
        )
    }
}

/// The cover of an album or song (served without a token).
pub fn image_url(server: &str, item: &str) -> String {
    format!(
        "{server}/Items/{item}/Images/Primary?fillWidth=400&fillHeight=400&quality=90&format=Jpg"
    )
}

/// What the server hears about playback (its play counts, "last played",
/// the dashboard's Now Playing and scrobbling plugins follow these).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Report {
    Started {
        item: String,
    },
    Progress {
        item: String,
        position_ms: u64,
        paused: bool,
    },
    Stopped {
        item: String,
        position_ms: u64,
    },
}
impl Report {
    pub fn request(&self) -> (&'static str, serde_json::Value) {
        let ticks = |ms: &u64| ms * 10_000;
        match self {
            Self::Started { item } => (
                "/Sessions/Playing",
                serde_json::json!({"ItemId": item, "PositionTicks": 0, "CanSeek": true,
                    "PlayMethod": "DirectStream", "IsPaused": false}),
            ),
            Self::Progress {
                item,
                position_ms,
                paused,
            } => (
                "/Sessions/Playing/Progress",
                serde_json::json!({"ItemId": item, "PositionTicks": ticks(position_ms),
                    "CanSeek": true, "PlayMethod": "DirectStream", "IsPaused": paused}),
            ),
            Self::Stopped { item, position_ms } => (
                "/Sessions/Playing/Stopped",
                serde_json::json!({"ItemId": item, "PositionTicks": ticks(position_ms)}),
            ),
        }
    }
}

// ------------------------------------------------------------------ stream

/// The signed-in account playback opens streams with (kept here, not in the
/// queue, so the token never reaches the stored session).
static SESSION: std::sync::Mutex<Option<Account>> = std::sync::Mutex::new(None);

/// Set or clear the account songs stream with.
pub fn use_account(account: Option<Account>) {
    if let Ok(mut slot) = SESSION.lock() {
        *slot = account.filter(Account::signed_in);
    }
}

/// One song being received: curl's output is the decoder's pipe. Dropping
/// it stops the transfer.
pub struct Stream {
    child: Child,
}
impl Stream {
    /// Song `item` from `start_ms` on, with the account in use.
    pub fn open(item: &str, start_ms: u64) -> io::Result<Self> {
        let account = SESSION.lock().ok().and_then(|a| a.clone()).ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotConnected, "not signed in to Jellyfin")
        })?;
        Self::start(&account, item, start_ms)
    }
    pub fn start(account: &Account, item: &str, start_ms: u64) -> io::Result<Self> {
        if !id_ok(item) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a Jellyfin id",
            ));
        }
        let url = stream_url(account, item, start_ms);
        let auth = authorization(&account.device_id, &account.token);
        let mut child = Command::new(curl())
            .args(["-sS", "--fail", "--proto", "=http,https"])
            .args([
                "--connect-timeout",
                "10",
                "--speed-time",
                "45",
                "--speed-limit",
                "1",
            ])
            .arg("-A")
            .arg(format!("{CLIENT}/{VERSION}"))
            .args(["-K", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(curl_config(&url, &auth, None).as_bytes())?;
        }
        Ok(Self { child })
    }
    /// The read end for `Decoder::open_stream`.
    pub fn reader(&self) -> BorrowedFd<'_> {
        self.child.stdout.as_ref().expect("piped").as_fd()
    }
}
impl Drop for Stream {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// --------------------------------------------------------------- discovery

/// A server that answered the broadcast.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub name: String,
    pub address: String,
    pub id: String,
}

/// Ask the home network which Jellyfin servers there are (about `wait`).
pub fn discover(wait: Duration) -> Vec<Found> {
    let Ok(socket) = std::net::UdpSocket::bind(("0.0.0.0", 0)) else {
        return vec![];
    };
    if socket.set_broadcast(true).is_err()
        || socket
            .send_to(
                b"Who is JellyfinServer?",
                ("255.255.255.255", DISCOVERY_PORT),
            )
            .is_err()
    {
        return vec![];
    }
    let deadline = Instant::now() + wait;
    let mut found: Vec<Found> = vec![];
    let mut buffer = [0u8; 2048];
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        if left.is_zero() || socket.set_read_timeout(Some(left)).is_err() {
            break;
        }
        match socket.recv_from(&mut buffer) {
            Ok((n, _)) => {
                if let Some(server) = parse_found(&buffer[..n]) {
                    if !found.iter().any(|f| f.id == server.id) {
                        found.push(server);
                    }
                }
            }
            Err(_) => break,
        }
    }
    found
}

pub fn parse_found(reply: &[u8]) -> Option<Found> {
    let v: serde_json::Value = serde_json::from_slice(reply).ok()?;
    let text = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_owned();
    let address = base_url(&text("Address"))?;
    Some(Found {
        name: text("Name"),
        id: text("Id"),
        address,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const USER: &str = "0123456789abcdef0123456789abcdef";
    const ITEM: &str = "fedcba9876543210fedcba9876543210";

    fn account() -> Account {
        Account {
            server: "http://jelly.local:8096".into(),
            user_id: USER.into(),
            token: "secret-token".into(),
            device_id: "dev1".into(),
            ..Default::default()
        }
    }

    #[test]
    fn typed_addresses_become_base_urls() {
        assert_eq!(
            base_url("jelly.local").as_deref(),
            Some("http://jelly.local:8096")
        );
        assert_eq!(
            base_url("192.168.1.5:8920").as_deref(),
            Some("http://192.168.1.5:8920")
        );
        assert_eq!(
            base_url("https://music.example.org/").as_deref(),
            Some("https://music.example.org")
        );
        assert_eq!(
            base_url("HTTPS://Example.org/jellyfin").as_deref(),
            Some("https://Example.org/jellyfin")
        );
        assert_eq!(
            base_url("http://host/jellyfin").as_deref(),
            Some("http://host/jellyfin")
        );
        for bad in [
            "",
            "ftp://x",
            "a b",
            "user@host",
            "http://",
            "x\ty",
            "host:",
            "host:80a",
        ] {
            assert_eq!(base_url(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_token_stays_in_the_config_and_the_header_never_breaks() {
        let auth = authorization("dev\"1", "tok\\en");
        assert!(auth.starts_with("MediaBrowser Client=\"Y2Reborn\""));
        assert!(auth.contains("DeviceId=\"dev1\""));
        assert!(auth.ends_with("Token=\"token\""));
        assert!(!authorization("d", "").contains("Token"));
        let config = curl_config("http://h/x?a=\"b\"", &auth, Some("{\"Secret\":\"s\"}"));
        assert!(config.contains("url = \"http://h/x?a=\\\"b\\\"\""));
        assert!(config.contains("data-binary = \"{\\\"Secret\\\":\\\"s\\\"}\""));
        assert_eq!(config.lines().count(), 5);
        assert!(!format!("{:?}", Client::new(account())).contains("secret-token"));
        assert!(!format!("{:?}", account()).contains("secret-token"));
    }

    #[test]
    fn quick_connect_gives_a_code_and_the_approval_page() {
        let v = serde_json::json!({"Code":"123456","Secret":"abc","Authenticated":false});
        let qc = parse_quick_connect(&v, "https://jf.example").unwrap();
        assert_eq!(qc.code, "123456");
        assert_eq!(qc.secret(), "abc");
        assert_eq!(qc.url, "https://jf.example/web/#/quickconnect?code=123456");
        assert!(!format!("{qc:?}").contains("abc"));
        assert!(parse_quick_connect(&serde_json::json!({"Code":"1"}), "x").is_err());
        assert!(
            parse_quick_connect(&serde_json::json!({"Code":"1&x=2","Secret":"s"}), "x").is_err()
        );
    }

    #[test]
    fn lists_ask_for_the_right_items_and_refuse_odd_ids() {
        let q = |k: &str| query_for(k, USER, 0).unwrap();
        assert!(q("playlists").contains("IncludeItemTypes=Playlist"));
        assert!(q("favorites").contains("Filters=IsFavorite"));
        assert!(q("albums").contains("IncludeItemTypes=MusicAlbum"));
        assert!(q("artists").starts_with("/Artists/AlbumArtists?"));
        assert!(q(&format!("playlist:{ITEM}")).starts_with(&format!("/Playlists/{ITEM}/Items")));
        assert!(q(&format!("album:{ITEM}")).contains(&format!("ParentId={ITEM}")));
        assert!(q(&format!("artist-albums:{ITEM}")).contains("AlbumArtistIds="));
        assert!(q(&format!("artist-songs:{ITEM}")).contains("ArtistIds="));
        assert!(query_for("albums", USER, 200)
            .unwrap()
            .contains("StartIndex=200&Limit=200"));
        assert_eq!(query_for("album:../x", USER, 0), None);
        assert_eq!(query_for("albums:abc", USER, 0), None);
        assert_eq!(query_for("search", USER, 0), None);
        assert_eq!(query_for("albums", "", 0), None);
    }

    #[test]
    fn listings_parse_songs_and_pages() {
        let v = serde_json::json!({
            "Items": [
                {"Id": ITEM, "Name": "So What", "Type": "Audio", "Artists": ["Miles Davis"],
                 "AlbumArtist": "Miles Davis", "Album": "Kind of Blue", "AlbumId": USER,
                 "RunTimeTicks": 5_620_000_000u64, "IndexNumber": 1, "ParentIndexNumber": 1,
                 "MediaSources": [{"Container": "FLAC", "Bitrate": 900000,
                    "MediaStreams": [{"Type": "Audio", "SampleRate": 44100}]}]},
                {"Id": "not hex", "Name": "Bad"},
                {"Id": USER, "Name": ""}
            ],
            "TotalRecordCount": 450
        });
        let l = parse_listing(&v, 200);
        assert_eq!(l.items.len(), 1);
        let song = &l.items[0];
        assert_eq!(song.duration_ms, 562_000);
        assert_eq!(song.artist, "Miles Davis");
        assert_eq!(song.container, "flac");
        assert_eq!(song.sample_rate, 44_100);
        assert_eq!((song.track, song.disc), (1, 1));
        assert_eq!(l.next, Some(203));
        let last = parse_listing(&serde_json::json!({"Items": [], "TotalRecordCount": 3}), 3);
        assert_eq!(last.next, None);
    }

    #[test]
    fn streams_start_untouched_and_seek_through_the_server() {
        let a = account();
        let from_start = stream_url(&a, ITEM, 0);
        assert!(from_start.contains(&format!("/Audio/{ITEM}/universal?")));
        assert!(from_start.contains("Container=flac,mp3,ogg,opus,wav,aac"));
        assert!(from_start.contains("EnableRedirection=false"));
        assert!(!from_start.contains("secret-token"));
        let later = stream_url(&a, ITEM, 61_500);
        assert!(later.contains("startTimeTicks=615000000"));
        assert!(later.contains("static=false"));
    }

    #[test]
    fn reports_carry_ticks() {
        let (path, body) = Report::Progress {
            item: ITEM.into(),
            position_ms: 1500,
            paused: true,
        }
        .request();
        assert_eq!(path, "/Sessions/Playing/Progress");
        assert_eq!(body["PositionTicks"], 15_000_000);
        assert_eq!(body["IsPaused"], true);
        assert_eq!(
            Report::Stopped {
                item: ITEM.into(),
                position_ms: 0
            }
            .request()
            .0,
            "/Sessions/Playing/Stopped"
        );
    }

    #[test]
    fn discovery_replies_name_their_server() {
        let reply = br#"{"Address":"http://192.168.1.20:8096","Id":"abc","Name":"Home","EndpointAddress":null}"#;
        let found = parse_found(reply).unwrap();
        assert_eq!(found.address, "http://192.168.1.20:8096");
        assert_eq!(found.name, "Home");
        assert!(parse_found(b"Who is JellyfinServer?").is_none());
    }

    #[test]
    fn the_account_file_is_private_and_forgettable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("reborn-jellyfin-{}", std::process::id()));
        let path = account_path(&dir);
        save_account(&path, &account()).unwrap();
        assert_eq!(load_account(&path), Some(account()));
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        forget_account(&path).unwrap();
        assert_eq!(load_account(&path), None);
        forget_account(&path).unwrap();
        let _ = fs::remove_dir_all(dir);
    }

    /// A stand-in curl that prints its configuration (from stdin) and an
    /// HTTP status, as `-w` would.
    fn fake_curl(dir: &Path, status: u16) -> PathBuf {
        let path = dir.join("curl");
        fs::write(&path, format!("#!/bin/sh\ncat\nprintf '\\n{status}'\n")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn requests_pass_the_token_on_stdin_and_classify_statuses() {
        let dir = std::env::temp_dir().join(format!("reborn-jellyfin-curl-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let config = curl_config("http://h/x", &authorization("d", "tok"), None);
        let run = |status: u16| {
            let curl = fake_curl(&dir, status);
            request_with(curl.to_str().unwrap(), &config, 5)
        };
        let (code, body) = run(200).unwrap();
        assert_eq!(code, 200);
        assert!(String::from_utf8(body)
            .unwrap()
            .contains("Token=\\\"tok\\\""));
        assert_eq!(json(run(401)), Err(Error::Unauthorized));
        assert_eq!(json(run(503)), Err(Error::Status(503)));
        let missing = dir.join("missing");
        assert!(matches!(
            request_with(missing.to_str().unwrap(), &config, 5),
            Err(Error::Offline(_))
        ));
        let _ = fs::remove_dir_all(dir);
    }
}
