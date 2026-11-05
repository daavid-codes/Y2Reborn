//! Listening stats on the player: following what plays, keeping each listen,
//! and adding them up for Home → Stats.
//!
//! `reborn_core::stats::Meter` measures the heard time of every play, as the
//! scrobbling rules do; a listen ends when another song starts, the same one
//! starts over, playback ends or the player shuts down. Listens wait in
//! memory until the clock is trustworthy, then go to
//! `/data/reborn/state/listening.jsonl`. Nothing leaves the player. The
//! summaries are added up only while a Stats screen is open, and again when
//! a listen arrives or the day changes.
use crate::Runtime;
use reborn_core::stats::{self, Heard, Listen, Meter, Period};
use reborn_core::{PlaybackState, Screen};
use reborn_observability::Level;
use reborn_platform::listening;
use serde_json::json;
use std::path::PathBuf;

/// Listens counted before the clock was known, kept in memory at most.
const UNSTAMPED_MAX: usize = 500;

pub struct Stats {
    path: PathBuf,
    listens: Vec<Listen>,
    meter: Meter,
    unstamped: Vec<Heard>,
    /// What the summaries on screen were added up from: (listens, local
    /// day, time zone). Anything else and they are added up again.
    shown: Option<(usize, i64, String)>,
}
impl Stats {
    pub fn load(root: &std::path::Path) -> Self {
        let path = listening::path(root);
        Self {
            listens: listening::load(&path),
            path,
            meter: Meter::default(),
            unstamped: vec![],
            shown: None,
        }
    }
}

impl Runtime {
    /// Every pass: follow what plays, keep listens that ended, and keep the
    /// Stats screens current.
    pub(crate) fn stats_tick(&mut self) {
        let now_ms = reborn_platform::scrobble::boottime_ms();
        let unix = self.trusted_unix();
        if self.model.settings.listening_stats {
            let playing = self.model.playback == PlaybackState::Playing;
            let position = self.model.queue_position;
            let current = self
                .model
                .queue_entry_ids
                .get(position)
                .map(|e| e.0)
                .zip(self.model.queue.get(position));
            if let Some(heard) =
                self.stats
                    .meter
                    .observe(current, playing, self.model.position_ms, now_ms, unix)
            {
                self.stats_heard(heard);
            }
        } else {
            self.stats.meter = Meter::default();
            self.stats.unstamped.clear();
        }
        if unix.is_some() && !self.stats.unstamped.is_empty() {
            for heard in std::mem::take(&mut self.stats.unstamped) {
                if let Some(listen) = heard.stamp(now_ms, unix) {
                    self.stats_keep(listen);
                }
            }
        }
        if matches!(
            self.model.screen,
            Screen::Stats | Screen::StatsList | Screen::StatsRecap
        ) {
            self.stats_view();
        } else {
            self.stats.shown = None;
        }
    }

    fn stats_heard(&mut self, heard: Heard) {
        if self.stats.unstamped.len() >= UNSTAMPED_MAX {
            self.stats.unstamped.remove(0);
        }
        self.stats.unstamped.push(heard);
    }

    fn stats_keep(&mut self, listen: Listen) {
        if let Err(e) = listening::append(&self.stats.path, &listen) {
            self.log.emit(
                Level::Warn,
                "stats",
                "keep_failed",
                "A listen wasn't kept",
                None,
                json!({"error": e.to_string()}),
            );
        }
        self.stats.listens.push(listen);
    }

    /// The song playing when Reborn stops counts up to here.
    pub(crate) fn stats_finish(&mut self) {
        let now_ms = reborn_platform::scrobble::boottime_ms();
        let unix = self.trusted_unix();
        if let Some(heard) = self.stats.meter.finish() {
            self.stats_heard(heard);
        }
        for heard in std::mem::take(&mut self.stats.unstamped) {
            if let Some(listen) = heard.stamp(now_ms, unix) {
                self.stats_keep(listen);
            }
        }
    }

    /// Add up every period and recap year for the Stats screens.
    fn stats_view(&mut self) {
        let Some(now) = self.trusted_unix() else {
            // Which week or month a listen falls in needs the date.
            if self.ui.stats.listens != self.stats.listens.len() {
                self.ui.stats.listens = self.stats.listens.len();
                self.ui.stats.ready = false;
                self.dirty.mark_render();
            }
            return;
        };
        let zone = self.model.settings.time_zone.clone();
        let rule = reborn_core::clock::zone(&zone).unwrap_or(reborn_core::clock::Rule::UTC);
        let key = (self.stats.listens.len(), stats::local_day(now, &rule), zone);
        if self.stats.shown.as_ref() == Some(&key) {
            return;
        }
        let listens = &self.stats.listens;
        let years = stats::recap_years(listens, now, &rule);
        let view = &mut self.ui.stats;
        view.summaries.clear();
        for period in Period::CHOICES
            .into_iter()
            .chain(years.iter().map(|&y| Period::Recap(y)))
        {
            view.summaries
                .insert(period, stats::summarize(listens, period, now, &rule));
        }
        view.years = years;
        view.listens = listens.len();
        view.ready = true;
        self.stats.shown = Some(key);
        self.dirty.mark_render();
    }

    pub(crate) fn stats_set(&mut self, on: bool) {
        if !on {
            // What was heard so far still counts.
            self.stats_finish();
        }
        self.model.settings.listening_stats = on;
        self.checkpoint();
        self.dirty.mark_both();
    }

    pub(crate) fn stats_reset(&mut self) -> Result<(), String> {
        listening::clear(&self.stats.path).map_err(|e| format!("Couldn't reset the stats: {e}"))?;
        self.stats.listens.clear();
        self.stats.unstamped.clear();
        self.stats.meter = Meter::default();
        self.stats.shown = None;
        self.ui.stats = Default::default();
        self.log.emit(
            Level::Info,
            "stats",
            "reset",
            "Listening stats reset",
            None,
            json!({}),
        );
        self.ui.flash("Listening stats reset");
        self.dirty.mark_both();
        Ok(())
    }
}
