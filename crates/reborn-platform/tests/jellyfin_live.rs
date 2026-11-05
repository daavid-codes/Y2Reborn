//! Reborn's Jellyfin client against a real Jellyfin server.
//!
//! Ignored by default: it needs a server. `tools/jellyfin-test/run.sh`
//! starts one in Docker with a generated test library and runs this. By
//! hand: set `REBORN_JELLYFIN_TEST_SERVER` (its address) and
//! `REBORN_JELLYFIN_TEST_TOKEN` (an administrator's access token, which
//! approves Quick Connect as a phone would), then
//! `cargo test -p reborn-platform --test jellyfin_live -- --ignored`.
//!
//! The library it expects (what the script generates): an album "Tone
//! Album" of three FLAC songs, and an album "Other Album" with an M4A and an
//! MP3 song. Every song plays 440 Hz for 10 s, then 1000 Hz for 10 s, so the
//! tone heard tells which part of a song arrived. Decoding uses the `ffmpeg`
//! command.

use reborn_platform::jellyfin::{self as jf, Account, Client, Error, Item, Report, Stream};
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

fn server() -> (String, String) {
    let server = std::env::var("REBORN_JELLYFIN_TEST_SERVER").expect("REBORN_JELLYFIN_TEST_SERVER");
    let token = std::env::var("REBORN_JELLYFIN_TEST_TOKEN").expect("REBORN_JELLYFIN_TEST_TOKEN");
    (jf::base_url(&server).expect("an address"), token)
}

/// The administrator, standing in for the phone that taps Authorize.
fn admin(server: &str, token: &str) -> Client {
    Client::new(Account {
        server: server.into(),
        device_id: "reborn-test-admin".into(),
        token: token.into(),
        user_id: "0".into(),
        ..Default::default()
    })
}

/// Sign in the way the player does: Quick Connect, approved by the admin.
fn sign_in(server: &str, token: &str) -> Client {
    let probe = Client::new(Account {
        server: server.into(),
        device_id: jf::new_device_id(),
        ..Default::default()
    });
    let (_, id, version) = probe.public_info().expect("a Jellyfin server");
    eprintln!("Jellyfin {version} ({id})");
    let code = probe.quick_connect().expect("Quick Connect on");
    assert_eq!(code.code.len(), 6, "a six-digit code");
    assert_eq!(
        code.url,
        format!("{server}/web/#/quickconnect?code={}", code.code)
    );
    assert!(!probe.quick_connect_approved(code.secret()).unwrap());
    admin(server, token)
        .post(
            &format!("/QuickConnect/Authorize?code={}", code.code),
            &serde_json::Value::Null,
        )
        .expect("the admin approves");
    assert!(probe.quick_connect_approved(code.secret()).unwrap());
    let account = probe
        .sign_in(code.secret())
        .expect("a token for the secret");
    assert!(account.signed_in());
    jf::use_account(Some(account.clone()));
    Client::new(account)
}

/// Everything a list holds, page by page.
fn all(client: &Client, key: &str) -> Vec<Item> {
    let mut items = vec![];
    let mut start = 0;
    loop {
        let page = client
            .list(key, start)
            .unwrap_or_else(|e| panic!("{key}: {e:?}"));
        items.extend(page.items);
        match page.next {
            Some(next) => start = next,
            None => return items,
        }
    }
}

fn song<'a>(items: &'a [Item], name: &str) -> &'a Item {
    items
        .iter()
        .find(|i| i.name == name)
        .unwrap_or_else(|| panic!("no song {name}"))
}

/// What a stream delivers: its bytes, as Reborn's decoder would read them.
fn receive(item: &str, start_ms: u64) -> Vec<u8> {
    let stream = Stream::open(item, start_ms).expect("a stream");
    let mut pipe = std::fs::File::from(stream.reader().try_clone_to_owned().unwrap());
    let mut bytes = vec![];
    pipe.read_to_end(&mut bytes).unwrap();
    bytes
}

/// Decoded with ffmpeg as mono 8 kHz samples, its format name and the
/// dominant tone of each second.
fn listen(bytes: &[u8]) -> (String, Vec<u32>) {
    let probe = run(
        "ffprobe",
        &[
            "-v",
            "error",
            "-show_entries",
            "format=format_name",
            "-of",
            "csv=p=0",
            "-i",
            "pipe:0",
        ],
        bytes,
    );
    let pcm = run(
        "ffmpeg",
        &[
            "-v", "error", "-i", "pipe:0", "-f", "s16le", "-ac", "1", "-ar", "8000", "pipe:1",
        ],
        bytes,
    );
    let samples: Vec<i16> = pcm
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&b| i16::from_le_bytes(b))
        .collect();
    // Zero crossings per second / 2 = frequency.
    let tones = samples
        .chunks(8000)
        .filter(|s| s.len() == 8000)
        .map(|s| s.windows(2).filter(|w| (w[0] < 0) != (w[1] < 0)).count() as u32 / 2)
        .collect();
    (String::from_utf8_lossy(&probe).trim().to_owned(), tones)
}

fn run(tool: &str, args: &[&str], input: &[u8]) -> Vec<u8> {
    let mut child = Command::new(tool)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap_or_else(|e| panic!("{tool}: {e}"));
    let mut stdin = child.stdin.take().unwrap();
    let input = input.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let mut out = vec![];
    child.stdout.take().unwrap().read_to_end(&mut out).unwrap();
    let _ = writer.join();
    child.wait().unwrap();
    out
}

fn near(heard: u32, tone: u32) -> bool {
    heard.abs_diff(tone) <= tone / 20
}

#[test]
#[ignore = "needs a Jellyfin server: tools/jellyfin-test/run.sh"]
fn a_real_server_signs_in_lists_and_streams() {
    let (server, token) = server();

    // The home network finds it (the script runs it with host networking).
    let found = jf::discover(Duration::from_secs(2));
    eprintln!("discovered: {found:?}");

    let client = sign_in(&server, &token);

    // The library, as the Jellyfin page lists it.
    let albums = all(&client, "albums");
    let names: Vec<&str> = albums.iter().map(|a| a.name.as_str()).collect();
    assert!(
        names.contains(&"Tone Album") && names.contains(&"Other Album"),
        "{names:?}"
    );
    let tone_album = albums.iter().find(|a| a.name == "Tone Album").unwrap();
    assert_eq!(tone_album.album_artist, "Test Artist");
    let artists = all(&client, "artists");
    assert!(
        artists.iter().any(|a| a.name == "Test Artist"),
        "{artists:?}"
    );
    let artist = artists.iter().find(|a| a.name == "Other Artist").unwrap();
    let artist_songs = all(&client, &format!("artist-songs:{}", artist.id));
    assert_eq!(artist_songs.len(), 2, "{artist_songs:?}");
    let artist_albums = all(&client, &format!("artist-albums:{}", artist.id));
    assert_eq!(artist_albums.len(), 1);

    let songs = all(&client, &format!("album:{}", tone_album.id));
    let titles: Vec<&str> = songs.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(titles, ["Tone 1", "Tone 2", "Tone 3"], "in track order");
    let flac = &songs[0];
    assert_eq!(flac.container, "flac");
    assert!(
        flac.duration_ms.abs_diff(20_000) < 500,
        "{}",
        flac.duration_ms
    );
    assert_eq!(flac.sample_rate, 44_100);
    let other = all(&client, &format!("album:{}", artist_albums[0].id));
    let m4a = song(&other, "Moov Song");
    let mp3 = song(&other, "MP3 Song");

    // A playlist and a favorite, made on the server, show in their lists.
    let user = client.account.user_id.clone();
    client
        .post(
            "/Playlists",
            &serde_json::json!({"Name": "Reborn Test", "Ids": [songs[2].id, mp3.id],
                "UserId": user, "MediaType": "Audio"}),
        )
        .expect("a playlist");
    client
        .post(
            &format!("/UserFavoriteItems/{}?userId={user}", songs[1].id),
            &serde_json::Value::Null,
        )
        .expect("a favorite");
    let playlists = all(&client, "playlists");
    let playlist = playlists
        .iter()
        .find(|p| p.name == "Reborn Test")
        .unwrap_or_else(|| panic!("{playlists:?}"));
    let in_playlist = all(&client, &format!("playlist:{}", playlist.id));
    let order: Vec<&str> = in_playlist.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(order, ["Tone 3", "MP3 Song"], "in the playlist's order");
    let favorites = all(&client, "favorites");
    assert!(
        favorites.iter().any(|s| s.name == "Tone 2"),
        "{favorites:?}"
    );

    // Streams: FLAC and MP3 arrive as they are, M4A converted to MP3, and
    // a seek starts where it was asked to (the second tone).
    let (format, tones) = listen(&receive(&flac.id, 0));
    eprintln!("FLAC from 0: {format}, tones {tones:?}");
    assert_eq!(format, "flac");
    assert!((19..=20).contains(&tones.len()), "a 20 s song: {tones:?}");
    assert!(near(tones[2], 440) && near(tones[15], 1000), "{tones:?}");

    let (format, tones) = listen(&receive(&flac.id, 12_000));
    eprintln!("FLAC from 12 s: {format}, tones {tones:?}");
    assert_eq!(format, "mp3");
    assert!((7..=8).contains(&tones.len()), "8 s left: {tones:?}");
    assert!(near(tones[1], 1000), "the second tone: {tones:?}");

    let (format, tones) = listen(&receive(&m4a.id, 0));
    eprintln!("M4A from 0: {format}, tones {tones:?}");
    assert_eq!(format, "mp3", "converted: a pipe can't read MP4's index");
    assert!(near(tones[3], 440) && near(tones[14], 1000), "{tones:?}");

    let (format, tones) = listen(&receive(&mp3.id, 0));
    eprintln!("MP3 from 0: {format}, tones {tones:?}");
    assert_eq!(format, "mp3");
    assert!(near(tones[4], 440), "{tones:?}");

    // The server hears what plays.
    for report in [
        Report::Started {
            item: flac.id.clone(),
        },
        Report::Progress {
            item: flac.id.clone(),
            position_ms: 5_000,
            paused: false,
        },
        Report::Progress {
            item: flac.id.clone(),
            position_ms: 6_000,
            paused: true,
        },
        Report::Stopped {
            item: flac.id.clone(),
            position_ms: 6_000,
        },
    ] {
        client
            .report(&report)
            .unwrap_or_else(|e| panic!("{report:?}: {e:?}"));
    }

    // A wrong token is "sign in again", and signing out ends the token.
    let wrong = Client::new(Account {
        token: "0".repeat(32),
        ..client.account.clone()
    });
    assert_eq!(wrong.list("albums", 0), Err(Error::Unauthorized));
    client.sign_out();
    assert_eq!(client.list("albums", 0), Err(Error::Unauthorized));
    jf::use_account(None);
}
