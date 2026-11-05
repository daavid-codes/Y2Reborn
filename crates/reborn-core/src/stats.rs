//! Listening stats: what was heard, for how long, and what that adds up to.
//!
//! One `Listen` per play: the song, its artist and album, where it came from
//! and how long it was actually *heard*. Heard time is measured as the
//! scrobbling rules measure it (`scrobble`): small forward steps of the
//! playback position no larger than the time that passed, so pauses, seeks
//! and stalls add nothing. Songs from the library, Jellyfin and Spotify
//! count; radio and audiobooks don't.
//!
//! `summarize` turns the listens of a period (a week, a month, a year, all
//! time, or one year's recap) into minutes, top artists, albums and songs,
//! the hours of the day, and streaks of days. Days and hours are the
//! player's local time (`clock::Rule`). Pure logic: no I/O.
use crate::{clock::Rule, Track};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A play shorter than this (heard time) isn't kept: a skip.
pub const MIN_LISTEN_MS: u64 = 10_000;
/// A play counts as one play of its song once heard this long.
pub const PLAY_MS: u64 = 30_000;
/// Position steps up to this much larger than the elapsed time still count.
const SLACK_MS: u64 = 1_500;
/// A position this close to the start, after the play had moved on, is a
/// new play of the same entry (repeat, Previous).
const RESTART_MS: u64 = 2_000;
/// How many names each top list keeps.
pub const TOP: usize = 50;

/// Where a listen came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Source {
    #[serde(rename = "l")]
    Library,
    #[serde(rename = "j")]
    Jellyfin,
    #[serde(rename = "s")]
    Spotify,
}

/// One play, as kept on the player (one JSON line each; short keys keep a
/// year of listening around 2 MB).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Listen {
    /// When it started (unix seconds, UTC).
    #[serde(rename = "t")]
    pub at: i64,
    /// Heard time.
    pub ms: u64,
    #[serde(rename = "ti")]
    pub title: String,
    #[serde(rename = "ar")]
    pub artist: String,
    #[serde(rename = "al", default, skip_serializing_if = "String::is_empty")]
    pub album: String,
    /// The album's artist, when it differs from the song's.
    #[serde(rename = "aa", default, skip_serializing_if = "String::is_empty")]
    pub album_artist: String,
    #[serde(rename = "s")]
    pub source: Source,
}
impl Listen {
    /// The artist the play counts for: the first one named.
    pub fn main_artist(&self) -> &str {
        crate::split_artists(&self.artist)
            .into_iter()
            .next()
            .unwrap_or(self.artist.as_str())
            .trim()
    }
    fn album_artist(&self) -> &str {
        if self.album_artist.is_empty() {
            self.main_artist()
        } else {
            &self.album_artist
        }
    }
}

/// Where a track's listening counts, or `None` when it doesn't (radio,
/// audiobooks, a file without an artist or title).
pub fn source_of(track: &Track) -> Option<Source> {
    if track.book || track.is_radio() {
        return None;
    }
    if track.title.trim().is_empty() || track.artist.trim().is_empty() {
        return None;
    }
    Some(if track.is_spotify() {
        Source::Spotify
    } else if track.is_jellyfin() {
        Source::Jellyfin
    } else {
        // A title that is just the file name is no song to rank.
        if track.title == track.filename {
            return None;
        }
        Source::Library
    })
}

/// A listen whose start may not be known yet (no trustworthy clock).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Heard {
    pub listen: Listen,
    pub started_unix: Option<i64>,
    pub started_mono_ms: u64,
}
impl Heard {
    /// The listen with its start time, once the clock is known.
    pub fn stamp(mut self, now_mono_ms: u64, now_unix: Option<i64>) -> Option<Listen> {
        self.listen.at = match self.started_unix {
            Some(t) => t,
            None => {
                let ago = now_mono_ms.saturating_sub(self.started_mono_ms) / 1000;
                now_unix? - ago as i64
            }
        };
        Some(self.listen)
    }
}

#[derive(Clone, Debug)]
struct Current {
    entry: u64,
    /// Title and artist: a Spotify entry stays while its songs change.
    song: (String, String),
    listen: Option<Listen>,
    started_unix: Option<i64>,
    started_mono_ms: u64,
    last_position_ms: u64,
    last_mono_ms: u64,
    heard_ms: u64,
}
impl Current {
    fn finish(self) -> Option<Heard> {
        let mut listen = self.listen?;
        if self.heard_ms < MIN_LISTEN_MS {
            return None;
        }
        listen.ms = self.heard_ms;
        Some(Heard {
            listen,
            started_unix: self.started_unix,
            started_mono_ms: self.started_mono_ms,
        })
    }
}

/// Follows what plays and hands over each listen when it ends.
#[derive(Clone, Debug, Default)]
pub struct Meter {
    current: Option<Current>,
}
impl Meter {
    /// One observation, as `scrobble::Tracker::observe` takes it. Returns the
    /// listen that just ended (another song, the same from its start again,
    /// or nothing playing any more), if it was long enough to keep.
    pub fn observe(
        &mut self,
        now: Option<(u64, &Track)>,
        playing: bool,
        position_ms: u64,
        now_mono_ms: u64,
        now_unix: Option<i64>,
    ) -> Option<Heard> {
        let Some((entry, track)) = now else {
            return self.current.take().and_then(Current::finish);
        };
        let same_song =
            |c: &Current| c.entry == entry && c.song.0 == track.title && c.song.1 == track.artist;
        let restart = self.current.as_ref().is_some_and(|c| {
            same_song(c)
                && position_ms < RESTART_MS
                && c.last_position_ms >= RESTART_MS + SLACK_MS
                && position_ms < c.last_position_ms
        });
        if restart || self.current.as_ref().is_none_or(|c| !same_song(c)) {
            let ended = self.current.take().and_then(Current::finish);
            self.current = Some(Current {
                entry,
                song: (track.title.clone(), track.artist.clone()),
                listen: source_of(track).map(|source| Listen {
                    at: 0,
                    ms: 0,
                    title: track.title.trim().to_owned(),
                    artist: track.artist.trim().to_owned(),
                    album: track.album.trim().to_owned(),
                    album_artist: {
                        let aa = track.tagged_album_artist().trim();
                        if aa == track.artist.trim() {
                            String::new()
                        } else {
                            aa.to_owned()
                        }
                    },
                    source,
                }),
                started_unix: now_unix.map(|t| t - (position_ms / 1000) as i64),
                started_mono_ms: now_mono_ms.saturating_sub(position_ms),
                last_position_ms: position_ms,
                last_mono_ms: now_mono_ms,
                heard_ms: 0,
            });
            return ended;
        }
        let c = self.current.as_mut()?;
        let elapsed = now_mono_ms.saturating_sub(c.last_mono_ms);
        if playing && position_ms > c.last_position_ms {
            let step = position_ms - c.last_position_ms;
            if step <= elapsed + SLACK_MS {
                c.heard_ms += step;
            }
        }
        c.last_position_ms = position_ms;
        c.last_mono_ms = now_mono_ms;
        None
    }
    /// The listen in progress, ended now (the player is shutting down).
    pub fn finish(&mut self) -> Option<Heard> {
        self.current.take().and_then(Current::finish)
    }
}

// ----------------------------------------------------------------- periods

/// The stretch of time a summary covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Period {
    /// Since Monday.
    Week,
    #[default]
    Month,
    Year,
    All,
    /// One whole calendar year (a recap).
    Recap(i32),
}
impl Period {
    pub const CHOICES: [Period; 4] = [Period::Week, Period::Month, Period::Year, Period::All];
    pub fn label(self) -> String {
        match self {
            Self::Week => "This Week".into(),
            Self::Month => "This Month".into(),
            Self::Year => "This Year".into(),
            Self::All => "All Time".into(),
            Self::Recap(year) => format!("{year}"),
        }
    }
    /// The next choice, as one press on its row steps through them.
    pub fn next(self) -> Self {
        match self {
            Self::Week => Self::Month,
            Self::Month => Self::Year,
            Self::Year => Self::All,
            Self::All | Self::Recap(_) => Self::Week,
        }
    }
    /// [from, to) in unix seconds, for "now" in zone `rule`.
    pub fn range(self, now_unix: i64, rule: &Rule) -> (i64, i64) {
        let today = rule.local(now_unix);
        let start_of = |year: i32, month: u8, day: u8| {
            rule.utc_of(&crate::clock::Local {
                year,
                month,
                day,
                hour: 0,
                minute: 0,
            })
        };
        match self {
            Self::All => (i64::MIN, i64::MAX),
            Self::Year => (start_of(today.year, 1, 1), i64::MAX),
            Self::Month => (start_of(today.year, today.month, 1), i64::MAX),
            Self::Week => {
                let midnight = start_of(today.year, today.month, today.day);
                let days = local_day(now_unix, rule);
                // 1970-01-01 was a Thursday; Monday is day 0 of a week.
                let since_monday = (days + 3).rem_euclid(7);
                (midnight - since_monday * 86_400, i64::MAX)
            }
            Self::Recap(year) => (start_of(year, 1, 1), start_of(year + 1, 1, 1)),
        }
    }
}

/// Days since 1970-01-01 in local time.
pub fn local_day(utc: i64, rule: &Rule) -> i64 {
    (utc + i64::from(rule.offset_at(utc))).div_euclid(86_400)
}

// ----------------------------------------------------------------- summary

/// One name of a top list: an artist, an album or a song.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ranked {
    pub name: String,
    /// The album's or song's artist; empty for artists.
    pub by: String,
    pub ms: u64,
    pub plays: u32,
}

/// What a period's listening adds up to.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub ms: u64,
    pub plays: u32,
    /// Different songs, artists and albums heard.
    pub songs: usize,
    pub artists: usize,
    pub albums: usize,
    /// Most heard first, ties by plays then name; at most `TOP` each.
    pub top_artists: Vec<Ranked>,
    pub top_albums: Vec<Ranked>,
    pub top_songs: Vec<Ranked>,
    /// Heard time by local hour of the day.
    pub hours: [u64; 24],
    /// Heard time by local month (a recap's busiest month).
    pub months: [u64; 12],
    /// Days with any listening, and the longest run of days in a row.
    pub days: usize,
    pub longest_streak: usize,
    /// The run of days ending today (or yesterday), for the current periods.
    pub current_streak: usize,
    /// Heard time by source: library, Jellyfin, Spotify.
    pub by_source: [u64; 3],
}
impl Summary {
    pub fn busiest_hour(&self) -> Option<usize> {
        (0..24)
            .filter(|&h| self.hours[h] > 0)
            .max_by_key(|&h| (self.hours[h], std::cmp::Reverse(h)))
    }
    pub fn busiest_month(&self) -> Option<usize> {
        (0..12)
            .filter(|&m| self.months[m] > 0)
            .max_by_key(|&m| (self.months[m], std::cmp::Reverse(m)))
    }
}

fn ranked(map: HashMap<(String, String), (u64, u32)>) -> Vec<Ranked> {
    let mut list: Vec<Ranked> = map
        .into_iter()
        .map(|((name, by), (ms, plays))| Ranked {
            name,
            by,
            ms,
            plays,
        })
        .collect();
    list.sort_by(|a, b| {
        b.ms.cmp(&a.ms)
            .then(b.plays.cmp(&a.plays))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    list.truncate(TOP);
    list
}

/// Names compare case-blind: "the beatles" and "The Beatles" are one artist.
fn key(name: &str) -> String {
    name.trim().to_lowercase()
}

/// The listens in `period` (as of `now_unix`, in zone `rule`), summed up.
pub fn summarize(listens: &[Listen], period: Period, now_unix: i64, rule: &Rule) -> Summary {
    let (from, to) = period.range(now_unix, rule);
    let mut s = Summary::default();
    // Case-blind keys, each with the spelling heard first.
    let mut artists: HashMap<String, (String, u64, u32)> = HashMap::new();
    let mut albums: HashMap<(String, String), (String, String, u64, u32)> = HashMap::new();
    let mut songs: HashMap<(String, String), (String, String, u64, u32)> = HashMap::new();
    let mut days = std::collections::BTreeSet::new();
    for l in listens.iter().filter(|l| l.at >= from && l.at < to) {
        let play = u32::from(l.ms >= PLAY_MS);
        s.ms += l.ms;
        s.plays += play;
        let local = rule.local(l.at);
        s.hours[usize::from(local.hour.min(23))] += l.ms;
        s.months[usize::from(local.month.clamp(1, 12) - 1)] += l.ms;
        s.by_source[match l.source {
            Source::Library => 0,
            Source::Jellyfin => 1,
            Source::Spotify => 2,
        }] += l.ms;
        days.insert(local_day(l.at, rule));
        let artist = l.main_artist();
        let a = artists
            .entry(key(artist))
            .or_insert_with(|| (artist.to_owned(), 0, 0));
        a.1 += l.ms;
        a.2 += play;
        if !l.album.is_empty() {
            let by = l.album_artist();
            let a = albums
                .entry((key(&l.album), key(by)))
                .or_insert_with(|| (l.album.clone(), by.to_owned(), 0, 0));
            a.2 += l.ms;
            a.3 += play;
        }
        let song = songs
            .entry((key(&l.title), key(artist)))
            .or_insert_with(|| (l.title.clone(), artist.to_owned(), 0, 0));
        song.2 += l.ms;
        song.3 += play;
    }
    s.songs = songs.len();
    s.artists = artists.len();
    s.albums = albums.len();
    s.top_artists = ranked(
        artists
            .into_values()
            .map(|(name, ms, plays)| ((name, String::new()), (ms, plays)))
            .collect(),
    );
    s.top_albums = ranked(
        albums
            .into_values()
            .map(|(name, by, ms, plays)| ((name, by), (ms, plays)))
            .collect(),
    );
    s.top_songs = ranked(
        songs
            .into_values()
            .map(|(name, by, ms, plays)| ((name, by), (ms, plays)))
            .collect(),
    );
    s.days = days.len();
    let mut run = 0;
    let mut previous = None;
    for &day in &days {
        run = if previous == Some(day - 1) {
            run + 1
        } else {
            1
        };
        s.longest_streak = s.longest_streak.max(run);
        previous = Some(day);
    }
    // A run still going: it ends today, or yesterday (today not yet heard).
    let today = local_day(now_unix, rule);
    if !matches!(period, Period::Recap(_)) && matches!(previous, Some(d) if d >= today - 1) {
        s.current_streak = run;
    }
    s
}

/// Finished years with listening, newest first (each has a recap).
pub fn recap_years(listens: &[Listen], now_unix: i64, rule: &Rule) -> Vec<i32> {
    let this_year = rule.local(now_unix).year;
    let mut years: Vec<i32> = listens
        .iter()
        .map(|l| rule.local(l.at).year)
        .filter(|&y| y < this_year)
        .collect();
    years.sort_unstable_by(|a, b| b.cmp(a));
    years.dedup();
    years
}

/// "12 min", "3 h 05 min", "1 254 h": heard time as people say it.
pub fn duration_text(ms: u64) -> String {
    let minutes = ms / 60_000;
    match minutes {
        0 if ms > 0 => "Under a minute".into(),
        0..60 => format!("{minutes} min"),
        60..6000 => format!("{} h {:02} min", minutes / 60, minutes % 60),
        _ => format!("{} h", minutes / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::{Local, Rule};

    fn at(rule: &Rule, y: i32, mo: u8, d: u8, h: u8) -> i64 {
        rule.utc_of(&Local {
            year: y,
            month: mo,
            day: d,
            hour: h,
            minute: 0,
        })
    }

    fn listen(at: i64, minutes: u64, title: &str, artist: &str, album: &str) -> Listen {
        Listen {
            at,
            ms: minutes * 60_000,
            title: title.into(),
            artist: artist.into(),
            album: album.into(),
            album_artist: String::new(),
            source: Source::Library,
        }
    }

    fn song(title: &str, artist: &str) -> Track {
        Track {
            id: 1,
            source_id: "internal".into(),
            title: title.into(),
            artist: artist.into(),
            album: "Album".into(),
            album_artist: artist.into(),
            duration_ms: 200_000,
            online: true,
            ..Default::default()
        }
    }

    #[test]
    fn only_heard_time_counts_and_skips_are_dropped() {
        let mut meter = Meter::default();
        let t = song("So What", "Miles Davis");
        // Plays 0..60 s in 1 s steps, then a seek forward of 60 s (not heard).
        let mut ms = 1_000_000;
        for pos in 0..=60u64 {
            assert!(meter
                .observe(Some((1, &t)), true, pos * 1000, ms, Some(1_000))
                .is_none());
            ms += 1000;
        }
        meter.observe(Some((1, &t)), true, 120_000, ms, Some(1_000));
        // Paused for a minute: nothing is added.
        ms += 60_000;
        meter.observe(Some((1, &t)), false, 120_000, ms, Some(1_000));
        // The next song: the first one ends with 60 s heard.
        let next = song("Freddie Freeloader", "Miles Davis");
        let heard = meter
            .observe(Some((2, &next)), true, 0, ms, Some(1_100))
            .expect("the first listen ended");
        assert_eq!(heard.listen.ms, 60_000);
        assert_eq!(heard.listen.title, "So What");
        assert_eq!(heard.started_unix, Some(1_000));
        // 5 s of the second, then nothing plays: a skip, not kept.
        meter.observe(Some((2, &next)), true, 5_000, ms + 5_000, Some(1_105));
        assert!(meter
            .observe(None, false, 0, ms + 6_000, Some(1_106))
            .is_none());
    }

    #[test]
    fn a_spotify_entry_counts_each_song_and_radio_never() {
        let mut meter = Meter::default();
        let mut t = Track::spotify("spotify:playlist:x", "Song A");
        t.artist = "Artist".into();
        meter.observe(Some((7, &t)), true, 0, 0, Some(0));
        meter.observe(Some((7, &t)), true, 20_000, 20_000, Some(20));
        // The same entry, the next song (spotifyd moved on).
        t.title = "Song B".into();
        let heard = meter
            .observe(Some((7, &t)), true, 0, 20_000, Some(20))
            .unwrap();
        assert_eq!(
            (heard.listen.title.as_str(), heard.listen.source),
            ("Song A", Source::Spotify)
        );
        let station = Track::radio(&crate::RadioStation {
            name: "Jazz FM".into(),
            url: "http://x".into(),
            ..Default::default()
        });
        assert_eq!(source_of(&station), None);
        let mut book = song("Chapter 1", "Author");
        book.book = true;
        assert_eq!(source_of(&book), None);
    }

    #[test]
    fn a_listen_without_a_clock_is_stamped_later() {
        let heard = Heard {
            listen: listen(0, 1, "a", "b", ""),
            started_unix: None,
            started_mono_ms: 10_000,
        };
        assert_eq!(heard.clone().stamp(70_000, None), None);
        assert_eq!(heard.stamp(70_000, Some(1_000)).unwrap().at, 940);
    }

    #[test]
    fn periods_start_at_local_midnight_on_monday_the_first_and_new_year() {
        let utc = Rule::UTC;
        // Wednesday 2026-11-04 15:00 UTC.
        let now = at(&utc, 2026, 11, 4, 15);
        assert_eq!(Period::Week.range(now, &utc).0, at(&utc, 2026, 11, 2, 0));
        assert_eq!(Period::Month.range(now, &utc).0, at(&utc, 2026, 11, 1, 0));
        assert_eq!(Period::Year.range(now, &utc).0, at(&utc, 2026, 1, 1, 0));
        assert_eq!(
            Period::Recap(2025).range(now, &utc),
            (at(&utc, 2025, 1, 1, 0), at(&utc, 2026, 1, 1, 0))
        );
        // A zone east of UTC: its Monday starts earlier in UTC.
        let tokyo = crate::clock::zone("Asia/Tokyo").unwrap();
        assert_eq!(Period::Week.range(now, &tokyo).0, at(&utc, 2026, 11, 1, 15));
    }

    #[test]
    fn a_summary_ranks_artists_albums_and_songs() {
        let utc = Rule::UTC;
        let now = at(&utc, 2026, 11, 4, 20);
        let listens = vec![
            listen(
                at(&utc, 2026, 11, 2, 8),
                5,
                "So What",
                "Miles Davis",
                "Kind of Blue",
            ),
            listen(
                at(&utc, 2026, 11, 3, 8),
                9,
                "So What",
                "miles davis",
                "Kind of Blue",
            ),
            listen(
                at(&utc, 2026, 11, 3, 21),
                6,
                "Naima",
                "John Coltrane, Wynton Kelly",
                "Giant Steps",
            ),
            listen(
                at(&utc, 2026, 11, 4, 21),
                4,
                "Take Five",
                "Dave Brubeck",
                "Time Out",
            ),
            // A skip-length listen counts minutes but not a play.
            Listen {
                ms: 20_000,
                ..listen(
                    at(&utc, 2026, 11, 4, 22),
                    0,
                    "Take Five",
                    "Dave Brubeck",
                    "Time Out",
                )
            },
            // Last month: outside This Week and This Month.
            listen(
                at(&utc, 2026, 10, 30, 8),
                60,
                "Blue Train",
                "John Coltrane",
                "Blue Train",
            ),
        ];
        let week = summarize(&listens, Period::Week, now, &utc);
        assert_eq!(week.ms, 24 * 60_000 + 20_000);
        assert_eq!(week.plays, 4);
        assert_eq!(week.artists, 3, "case-blind; the first artist named");
        assert_eq!(week.top_artists[0].name, "Miles Davis");
        assert_eq!(week.top_artists[0].ms, 14 * 60_000);
        assert_eq!(week.top_artists[0].plays, 2);
        assert_eq!(week.top_albums[0].name, "Kind of Blue");
        assert_eq!(week.top_albums[0].by, "Miles Davis");
        assert_eq!(week.top_songs[0].name, "So What");
        assert_eq!(week.busiest_hour(), Some(8));
        assert_eq!(week.days, 3);
        assert_eq!(week.longest_streak, 3);
        assert_eq!(week.current_streak, 3);
        let all = summarize(&listens, Period::All, now, &utc);
        assert_eq!(all.top_artists[0].name, "John Coltrane");
        assert_eq!(all.longest_streak, 3, "Oct 30 doesn't join Nov 2");
        assert_eq!(all.by_source[0], all.ms);
        assert_eq!(recap_years(&listens, now, &utc), Vec::<i32>::new());
    }

    #[test]
    fn a_recap_is_one_whole_year() {
        let utc = Rule::UTC;
        let now = at(&utc, 2026, 1, 3, 12);
        let listens = vec![
            listen(at(&utc, 2024, 6, 1, 12), 30, "a", "Old", ""),
            listen(at(&utc, 2025, 3, 1, 12), 10, "b", "Spring", ""),
            listen(at(&utc, 2025, 7, 1, 12), 50, "c", "Summer", ""),
            listen(at(&utc, 2025, 7, 2, 12), 5, "c", "Summer", ""),
            listen(at(&utc, 2026, 1, 2, 12), 5, "d", "New", ""),
        ];
        assert_eq!(recap_years(&listens, now, &utc), vec![2025, 2024]);
        let recap = summarize(&listens, Period::Recap(2025), now, &utc);
        assert_eq!(recap.ms, 65 * 60_000);
        assert_eq!(recap.top_artists[0].name, "Summer");
        assert_eq!(recap.busiest_month(), Some(6), "July");
        assert_eq!(recap.longest_streak, 2);
        assert_eq!(recap.current_streak, 0, "a recap has no running streak");
    }

    #[test]
    fn listens_round_trip_as_short_json() {
        let l = Listen {
            album_artist: "Various".into(),
            source: Source::Jellyfin,
            ..listen(1_700_000_000, 3, "Song", "Artist", "Album")
        };
        let line = serde_json::to_string(&l).unwrap();
        assert_eq!(
            line,
            r#"{"t":1700000000,"ms":180000,"ti":"Song","ar":"Artist","al":"Album","aa":"Various","s":"j"}"#
        );
        assert_eq!(serde_json::from_str::<Listen>(&line).unwrap(), l);
    }

    #[test]
    fn durations_read_naturally() {
        assert_eq!(duration_text(0), "0 min");
        assert_eq!(duration_text(20_000), "Under a minute");
        assert_eq!(duration_text(12 * 60_000), "12 min");
        assert_eq!(duration_text(185 * 60_000), "3 h 05 min");
        assert_eq!(duration_text(7_000 * 60_000), "116 h");
        let order: Vec<Period> = std::iter::successors(Some(Period::Week), |p| {
            Some(p.next()).filter(|n| *n != Period::Week)
        })
        .collect();
        assert_eq!(order, Period::CHOICES);
    }
}
