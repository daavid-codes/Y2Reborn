# Y2Reborn

The native music player for the **Innioasis Y2** running
[Y2Linux](https://github.com/SchulzCode/Y2Linux). It is written in Rust, decodes
with FFmpeg 9.0.2, draws with OpenGL ES on the Y2's GPU (DRM + Lima), plays
through ALSA (wired) or BlueALSA (Bluetooth), and keeps its library in SQLite.

> **Status: 1.0.0-beta.8.43, part of Y2Linux 1.0 Beta 8.43 (Baseline 164, 2026-10-28), public beta.**
> Git tag `baseline-164` in both repositories. Reborn has no installer of its own: it is built
> into the Y2Linux images. Install them from the
> [Y2Linux releases](https://github.com/SchulzCode/Y2Linux/releases) with Y2Linux's
> [INSTALL](https://github.com/SchulzCode/Y2Linux/blob/main/docs/INSTALL.md) guide; Reborn then
> updates with the system over Wi-Fi (Settings → About & Updates).

## What it does

- Library of artists, albums and songs from Internal Storage and the SD card ([how storage
  works](https://github.com/SchulzCode/Y2Linux/blob/main/docs/STORAGE.md)),
  with cover art from the album folder first and the embedded picture otherwise (thumbnails
  cached in `/data/reborn/cache`); Albums as Cover Flow or a grid, and Genres.
- Wired playback at 44.1 kHz S16 stereo, as on stock; 48 kHz and other rates are decoded and converted.
  Bluetooth playback; the codec is chosen automatically (SBC, AAC and LDAC in the public images;
  aptX/aptX HD only in owner-private builds). The output follows the headphones: Bluetooth
  headphones take over from the jack as soon as they connect (playback continues there), the
  jack takes over again when they disconnect (playback pauses). Choosing the jack by hand sticks
  until the headphones reconnect. AAC headphones get the encoder's high-quality mode at the highest
  rate they accept (AirPods: 256 kbit/s), and the rate steps down by itself on a weak link
  (service menu: AAC Quality, AAC Auto Rate; SBC XQ for SBC-only headphones).
- Internet radio (Home → Radio): Favorites, Popular, Countries, Genres and Search from the free
  community directory radio-browser.info, plus your own .m3u/.pls lists in a `Radio` folder on
  Internal Storage or the SD card. MP3, AAC and Ogg streams over Wi-Fi with the station's song
  title on Now Playing and automatic reconnect; HLS-only stations are not supported yet. Station
  logos, Station Info, lists A to Z with each country's ten most listened stations on top, and
  Previous/Next between stations. FFmpeg
  stays built without networking: `curl` receives the stream and hands Reborn plain audio.
- **Last.fm scrobbling** (Settings → Last.fm): connect by scanning a QR code with your phone
  (password sign-in as a fallback). A library song counts once heard for half its length or
  4 minutes (pauses and skipping ahead don't count; radio and untagged files never do), Now
  Playing shows on your profile, plays are kept on the player while offline and sent in batches
  when Wi-Fi is back, and an optional Rockbox-format `.scrobbler.log` works without an account.
  Nothing is recorded until you opt in; Disconnect and Reset Settings forget the sign-in and its
  waiting plays. Full guide, privacy and security:
  [Last.fm scrobbling](https://github.com/SchulzCode/Y2Linux/blob/main/docs/LASTFM.md).
- **Spotify** (Home → Spotify, Premium account): connect with a QR code on your phone, then
  Playlists, Liked Songs, Albums and Artists, played through Reborn's own sound path (equalizer,
  volume, Bluetooth). The player is also a Spotify Connect device. Needs extra programs in the
  Y2Linux image. See [Spotify](#spotify).
- **Jellyfin** (Home → Jellyfin): music from your own Jellyfin server, found on the home network
  or by its address. Sign in with Quick Connect (a QR code), then Playlists, Favorites, Albums and
  Artists in Reborn's own queue. See [Jellyfin](#jellyfin).
- **Listening stats** (Home → Stats): minutes listened, top artists, albums and songs with their
  minutes, when you listen and how many days in a row, for this week, month, year or all time,
  and a recap of each finished year. Kept on the player only. See [Listening stats](#listening-stats).
- Sound and playback: five-band equalizer with presets, headphone profiles (47 common
  headphones and IEMs from AutoEQ built in, more as `ParametricEQ.txt` files in an `EQ` folder; up to 10
  filters with a clip-safe preamp),
  headphone output level (Low/Medium/High) and DAC filter, Normalize Volume (ReplayGain), gapless
  playback, crossfade (up to 12 s), shuffle and repeat.
- Search (Home → Search) across artists, albums and songs, with results shown live while typing:
  every typed word must begin a word of the name (case, accents and a leading "The" ignored).
- Storage (Settings → Storage): free space of Internal Storage and the SD card, Scan for Music, and
  the setup of Internal Storage when Y2Linux found a large old Innioasis library (it asks once
  and keeps the old files readable until you choose *Set Up Internal Storage*). A damaged SD card
  says *Needs repair · Check it on a computer*.
- Playlists (Home → Playlists): .m3u/.m3u8 files from Internal Storage and the SD card, made and
  changed on the player (add from any song, album or Now Playing; remove; move with the wheel;
  rename, duplicate, delete), a Favorites playlist with a heart, and playlists exported by a
  computer (Windows, macOS or Linux paths) matched to your library. The file stays the playlist:
  edits are written back into it, songs the player cannot find are kept.
- Pause and Play continue at once: a paused song keeps its file and audio output open for up to
  three minutes (a station stops; it is live). Holding Previous/Next seeks, and Previous/Next work
  even while a song is still starting.
- A status-bar clock in the player's time zone: found automatically over Wi-Fi (once per network)
  or chosen under Settings → Display → Time Zone (country first; built-in zone rules with
  daylight saving; shown once the player's clock is set).
- A full-screen keyboard for search, station search, Wi-Fi passwords and the Last.fm password
  sign-in, driven by the buttons:
  Back, Play/Pause, Previous and Next move up, down, left and right; Select types; hold Select
  when done, hold Back to cancel. Pages for capitals, digits, symbols and accented letters.
- Click-wheel UI (Product UI v4, Open Sans) with motion at 60 frames per second: gliding focus,
  smooth scrolling, sliding screens, and the playing song as a wave on Now Playing. A status bar
  with the screen's name, clock, Wi-Fi, Bluetooth, a USB symbol while a computer is connected
  and the battery; short messages (a computer connected, files copying, songs added, problems)
  appear in it. Home with Now Playing, Music, Playlists, Radio, Search and Settings; Bluetooth
  pairing, low battery, hold-to-turn-off, startup and shutdown screens; Software Update in one
  step (Update Now: Download · Check · Install with progress, speed and time left; Cancel during
  the download). A to Z lists ignore case, accents and a leading "The".
- Skins: Settings → Skin switches between Reborn's own look and skins that
  restyle every screen (colours, fonts, pictures, layouts). Sixteen skins ship with it (Classic, Nano Glass, Walkman, Terminal, Winamp, Paper, Big Type,
  Cassette, Neon Night, Zune, Bunny Bow and five Mini colours);
  more are added by copying a folder into `Skins` on the player. See [skins](docs/skins.md).
- Error reports (Settings → About & Updates → Error Reports), opt-in after a one-time question:
  sent over Wi-Fi when something goes wrong or with Send Report Now, or saved to Internal Storage
  or the SD card (Save Report) to attach to a GitHub issue. The player's logs and technical
  details with personal data removed, in about ten seconds, with the progress on screen
  ([Error reports](https://github.com/SchulzCode/Y2Linux/blob/main/docs/ERROR-REPORTS.md)).
- Works with the platform: sleep/wake preparation, workload hints to the CPU governor,
  `rebornctl` and a control socket at `/run/reborn/control.sock`.

Known limits (details in [current state](docs/CURRENT_REBORN_STATE.md)): native
high-rate output is gated; AAC is negotiated with AirPods Pro but not yet listened to; saved
Bluetooth headphones are not connected on play; the player cannot tell whether wired headphones are
plugged in (no jack detection yet), so "jack" means "not Bluetooth". Crossfade and EQ have not been checked on hardware.
Last.fm scrobbling works on the player (QR sign-in and real scrobbles confirmed); sending plays
that waited offline across sleep is still to be checked.

## How scrobbling stays safe

- **Your password:** with the QR sign-in it is typed only into Last.fm's own page on your phone;
  the fallback sends it once over HTTPS and never stores, logs or displays it.
- **Your sign-in:** the player keeps only Last.fm's session key, in one root-only file in
  `/data/reborn/state` (not reachable over USB, never in logs, diagnostics or update images). It
  can scrobble but cannot read or change your password or e-mail; revoke it any time on last.fm
  (Settings → Applications).
- **The connection:** HTTPS with certificate checks only; every request is signed as Last.fm
  requires; keys and data travel in the request body on curl's standard input, never on a command
  line.
- **The app key:** Y2Reborn's Last.fm API key and secret are not in this repository. Y2Linux's
  release build installs them as a root-only file (`/usr/share/reborn/lastfm-api.json`); a build
  without that file simply has no Last.fm. Like every client app, a released image necessarily
  contains the key; it identifies the app but gives no access to any account, and it can be
  replaced with an update if ever misused. Forks: register your own key.

Code: `crates/reborn-core/src/scrobble.rs` (what counts), `crates/reborn-platform/src/scrobble.rs`
(offline queue, log, clock), `crates/reborn-platform/src/lastfm.rs` (Last.fm API, signing),
`app/reborn/src/scrobbling.rs` (the player's side) and `crates/reborn-ui/src/qr.rs` (QR codes).

## Spotify

Listen to a Spotify **Premium** account on the player. Ported from
[PiPod-OS](https://github.com/daavid-codes/PiPod-OS). Not yet tried on a Y2.

### What you need

- A Spotify Premium account (Spotify streams to other players only for Premium).
- Wi-Fi, and the player's clock set (the sign-in checks certificates).
- A Y2Linux image with `spotifyd` and `reborn-spotify-metadata` installed (see
  [tools/spotify/README.md](tools/spotify/README.md)). Without them, Spotify isn't on Home and its
  page says *Spotify Isn't Installed*.

### Connecting

1. Home → **Spotify** → **Connect with Your Phone**.
2. The player shows a QR code. Scan it with your phone's camera, or go to `spotify.com/pair` on
   any device and type the code shown under it.
3. Approve Y2Reborn in Spotify's page. The player continues by itself.

No password is typed on the player. The sign-in lasts until you choose **Disconnect** on the
Spotify page.

### Using it

- **Library:** Playlists (the ones you made and the ones you follow), Liked Songs, Albums and
  Artists. An artist opens Top Songs, Albums and All Songs. Long lists end in **More**.
- **Playing:** choose a song and it plays within the playlist or album it came from, so Previous
  and Next stay there. Playlists, albums and Liked Songs also have **Play All**.
- **Controls:** Play/Pause, Previous and Next work as usual. Holding Previous/Next, or the wheel
  when Customize → Now Playing Wheel is *Seek*, moves 10 seconds at a time. The volume, equalizer,
  headphone profile, Bluetooth and the jack all apply, because the sound plays through Reborn.
- **Now Playing:** the song, its cover and position, a green **SPOTIFY** label with the playlist
  or album, and *SPOTIFY · 320 KBPS*. Hold Select for **Spotify** (back to the page),
  **Sleep Timer** and **Stop Spotify**.
- **Spotify Connect:** once Spotify has started on the player, it is listed as *Y2Reborn* among
  your account's devices. The phone app can start, pause and skip it; the player follows within
  about two seconds. Starting music on the player pauses Spotify, and the other way round.

### What the player keeps

Everything is in `/data/reborn/state/spotify/` (root-only, not reachable over USB, never in
logs, diagnostics or update images):

- `account.json`: Spotify's refresh token for this player.
- `cache/`: spotifyd's own reusable credentials. Songs are never stored (no audio cache).
- `spotifyd.conf` and `audio.pcm` (the pipe the sound travels through), recreated each start.

**Disconnect** removes the sign-in and spotifyd's credentials. Your Spotify password is only ever
typed into Spotify's own page.

### Limits

- Reborn's Last.fm scrobbling doesn't count Spotify songs; connect Spotify in your Last.fm
  settings instead.
- No search, and Spotify's own shuffle and repeat aren't on the player yet.
- Spotify starts the first time you open its page after switching on (a few seconds of
  *Starting Spotify…*).

### If something goes wrong

- *Spotify can't be reached. Check Wi-Fi.*: the player is offline or Spotify didn't answer.
- *Spotify needs you to connect again.*: the sign-in was revoked or expired; connect again.
- *Couldn't Load This List*: choose **Try Again**; the library helper didn't answer.
- If spotifyd stops, the player starts it again by itself (after 5 s, waiting longer each time,
  up to 5 min).
- Logs use the subsystem `spotify` (no token is ever logged).

### How it works (developers)

- **Sign-in:** OAuth 2.0 device authorization (RFC 8628) with the client id librespot uses. The
  access token is written where spotifyd and the library helper start from; both turn it into
  reusable credentials of their own.
- **Playback:** spotifyd runs as Reborn's child process with the `pipe` backend into a FIFO. A
  pump thread hands its PCM to Reborn's decoder as a streamed WAV, and stops reading while no
  decoder listens, so spotifyd waits. spotifyd is controlled over MPRIS (and its patched
  `rs.spotifyd.Controls.PlayContextTrack`) on the system bus.
- **Library:** `reborn-spotify-metadata` (housecat, Spotify's own client protocol) prints one JSON
  list per call.
- **Code:** `crates/reborn-platform/src/spotify.rs` (sign-in, spotifyd, the pump, MPRIS, the
  helper), `app/reborn/src/spotify.rs` (the player's side), `crates/reborn-ui` (`SpotifyView`,
  the pages), `tools/spotify` (spotifyd patch, D-Bus policy, build notes) and
  `tools/spotify-metadata` (the helper, its own Cargo workspace).

## Jellyfin

Listen to music from your own [Jellyfin](https://jellyfin.org) server. Tested against
Jellyfin 10.11.0; not yet tried on a Y2.

### What you need

- A Jellyfin server with a music library (tested with 10.11.0).
- **Quick Connect** switched on: Dashboard → General → Quick Connect (on by default).
- The player on the same network as the server, or the server reachable by an address (a
  reverse proxy or a VPN). A server at home needs no internet, only Wi-Fi.

### Connecting

1. Home → **Jellyfin**. Servers on the home network are listed by themselves (choose
   **Search Again** to look once more). For any other server choose **Enter Address…** and type
   it, for example `192.168.1.20:8096`, `jellyfin.local` or `https://music.example.org`. A bare
   name or address gets `http://` and Jellyfin's port 8096.
2. Choosing a server starts the sign-in: the player shows a QR code and a six-digit code.
3. Scan the QR code with a phone already signed in to Jellyfin in its browser and tap
   **Authorize**. Or open **Quick Connect** in Jellyfin's user menu (web or app) and enter the
   code. The player continues by itself.

No password is typed on the player. The sign-in lasts until you choose **Disconnect**, or remove
the device *Innioasis Y2* (app *Y2Reborn*) in the server's Dashboard → Devices. **Change Server** forgets the server
and its sign-in.

### Using it

- **Library:** Playlists, Favorites (songs you marked in Jellyfin), Albums and Artists. An artist
  opens Albums and All Songs. Long lists end in **More**.
- **Playing:** choose a song and the rest of its list follows; **Play All** and **Shuffle** start a
  whole list. The list becomes Reborn's own queue: Previous/Next, Shuffle, Repeat, seeking, Up Next
  and the sleep timer work as for songs on the player.
- **Sound:** FLAC, MP3, Ogg, Opus, WAV and AAC arrive as they are (lossless stays lossless). Other
  formats (M4A/ALAC, WMA and others) are converted to MP3 by the server, and so is a song after a
  seek (at 320 kbit/s, from that point on).
- **Now Playing:** the song, the album's cover from the server, a purple **JELLYFIN** label with
  the list's name, and the format (*JELLYFIN · FLAC*). Hold Select for Up Next, Sleep Timer,
  Shuffle, Repeat and **Jellyfin** (back to the page).
- **Your server knows:** play counts, "last played" and the dashboard's *Now Playing* follow what
  the player plays, and the server's scrobbling plugins (Last.fm, ListenBrainz) see it.

### What the player keeps

`/data/reborn/state/jellyfin/account.json` (root-only, not reachable over USB, never in logs,
diagnostics or update images): the server's address and name, your user name, the access token
and this player's device id. The token is passed to `curl` on its standard input, never on a
command line. Use an `https://` address for a server reached over the internet: with `http://`
the token travels unencrypted.

### Limits

- Downloads for listening away from the server are planned, not there yet.
- No search yet.
- Reborn's own Last.fm scrobbling doesn't count Jellyfin songs; use the server's plugins.
- Servers are only found on the same network: not across subnets or VLANs, and not when the
  server runs in Docker without host networking (or UDP port 7359 published). Type the address.
- Gapless playback between streamed songs is still to be checked on a Y2.

### If something goes wrong

- *No server answered on this network*: see Limits above, or enter the address.
- *That doesn't look like a Jellyfin server*: check the address and port.
- *Quick Connect is off on this server*: switch it on under Dashboard → General.
- *The code expired*: choose **Sign In with Quick Connect** for a new one (codes last a few
  minutes).
- *Jellyfin needs you to sign in again*: the device was removed on the server; sign in again.
- *The Jellyfin server can't be reached*: the server is off, or the player is on another network.
- Logs use the subsystem `jellyfin` (no token is ever logged).

### How it works (developers)

- **Discovery:** a UDP broadcast, *Who is JellyfinServer?*, to port 7359.
- **Sign-in:** `POST /QuickConnect/Initiate`, the approval page `/web/#/quickconnect?code=…`,
  `GET /QuickConnect/Connect` every 3 s, then `POST /Users/AuthenticateWithQuickConnect`.
- **Library:** `/Items` (with `userId`), `/Playlists/{id}/Items` and `/Artists/AlbumArtists`, 200
  items a page.
- **Streaming:** `/Audio/{id}/universal` from the start of a song (direct when Reborn reads the
  format from a pipe, else MP3), `/Audio/{id}/stream.mp3?static=false&startTimeTicks=…` after a
  seek. `curl` receives it and Reborn's decoder reads the pipe; the decoder is told where the
  stream starts in the song. Play reports go to `/Sessions/Playing`, `…/Progress` and
  `…/Stopped`.
- **Code:** `crates/reborn-platform/src/jellyfin.rs` (discovery, Quick Connect, library, streams,
  reports), `app/reborn/src/jellyfin.rs` (the player's side), `crates/reborn-ui` (`JellyfinView`,
  the pages).
- **Testing without a server:** `tools/jellyfin-test/run.sh` starts Jellyfin in Docker with a
  generated test library and runs `crates/reborn-platform/tests/jellyfin_live.rs` against it:
  sign-in, every list, streams in each format, a seek checked by the tone that arrives, play
  reports. It needs Docker, ffmpeg, curl, python3 and cargo; `KEEP=1` leaves the server running
  (user `david`, password `test`). CI runs the same test (`.github/workflows/jellyfin.yml`).

## Listening stats

Home → **Stats** shows what you listen to, like a year-in-review that is always up to date.
Everything is counted and kept on the player; nothing is sent anywhere.

### What counts

- Songs from the player's library, from Jellyfin and from Spotify. Radio and audiobooks don't.
- Only time actually heard: pausing, seeking ahead or a stalled stream adds nothing. A song heard
  for less than 10 seconds (a skip) isn't kept; one heard for 30 seconds or more counts as a play.
- A song with several artists counts for the first one named.

### Using it

- **Period** steps through This Week (from Monday), This Month, This Year and All Time, in the
  player's time zone.
- The top of the page shows the minutes listened, plays and artists, your top artist with its
  minutes, and days in a row (now, and the longest).
- **Top Artists**, **Top Albums** and **Top Songs**: the 50 most heard of the period, each with its
  minutes (and plays). **When You Listen**: minutes by hour of the day.
- **Your 2025** (one row per finished year): that year's minutes, top artist, song and album,
  busiest month, plays, days you listened and the longest run of days, with its own rankings.
- **Keep Listening Stats** pauses the counting (what was kept stays). **Reset Listening Stats**
  forgets everything, after asking.

Until the player's clock is set (Wi-Fi, or Settings → Display → Date & Time) the page shows
*Adding Up…*: listens are kept, but which week or month they belong to needs the date.

### What the player keeps

One line per listen in `/data/reborn/state/listening.jsonl` (root-only, not reachable over USB,
never in diagnostics or update images): when it started, how long it was heard, the song, artist,
album and where it came from. About 2 MB for a year of daily listening.

### How it works (developers)

`reborn_core::stats` (pure): `Meter` measures each play's heard time as the scrobbling rules do,
`summarize` adds up a period, `recap_years` lists the finished years. `reborn_platform::listening`
keeps the file (append-only; a line cut off by power loss is skipped). `app/reborn/src/stats.rs`
feeds the meter every pass, keeps listens once the clock is trustworthy, and adds the summaries up
only while a Stats screen is open (again when a listen arrives or the day changes).

## Development

Use Rust **1.99.0**. `rust-toolchain.toml` pins it together with the
`armv7-unknown-linux-gnueabihf` target. All crates are vendored in `vendor/`,
so builds work offline. Host builds link the system's development libraries
through `pkg-config`: FFmpeg (`libavformat`, `libavcodec`, `libavutil`,
`libavfilter`, `libswresample`, `libswscale`), ALSA, D-Bus, SQLite, libdrm, GBM,
EGL and GLES v2.

```sh
cargo fmt --all -- --check
cargo test --workspace --locked --offline
cargo clippy --workspace --all-targets --locked --offline -- -D warnings
```

UI previews render the production drawing code on the host
(`tools/preview/render_previews.py`, output in `docs/ui/previews/v3/`).

The ARM binary is built only through Y2Linux's Buildroot, which provides the
cross toolchain, sysroot, FFmpeg, SQLite, ALSA, D-Bus and Mesa. Check out this
repository **next to** Y2Linux (`../Y2Reborn`). `tools/build/cross.sh` is
what the Buildroot recipe calls. Host tests don't prove anything about the real Y2:
display, audio and power behaviour need the device.

| Path | Contents |
| --- | --- |
| `app/reborn` | The player binary, `rebornctl` and `reborn-bench` |
| `crates/` | `reborn-core` (state), `reborn-ui`, `reborn-graphics`, `reborn-audio`, `reborn-media` (FFmpeg membrane), `reborn-library` (SQLite), `reborn-platform` (Y2Linux API), `reborn-control`, `reborn-observability` |
| `assets/` | Fonts, UI art and media test fixtures |
| `tools/` | Cross build, previews, asset generation, qualification helpers |
| `vendor/` | Vendored, checksummed crate sources |
| `docs/` | Documentation; start at [docs/README.md](docs/README.md) |

On the device the player uses `/data/reborn` for its state (including the Last.fm session and
waiting plays in `state/`), plays music from
`/media/internal/music` (or `/data/music` before the internal music partition is
set up) and `/media/sd`.

## Licence

MIT (see [LICENSE](LICENSE)). The Open Sans glyph data is under the SIL Open Font
Licence (`assets/fonts/OpenSans-OFL.txt`). The spotifyd patch in `tools/spotify` is under spotifyd's GPL-3.0;
the vendored housecat crates in `tools/spotify-metadata` are MIT. No firmware, keys (including the Last.fm API key),
Bluetooth bonds or private device data belong in this repository.
