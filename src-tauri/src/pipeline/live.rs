//! What the pipeline is doing right now, per item: the live activity panel on
//! every review screen, and the retrieval feed on the Full text screen.
//!
//! In memory only and lost on restart: the event log remains the record of
//! what was done and how. This only shows current steps and recent outcomes.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::Serialize;

const RECENT: usize = 40;

#[derive(Default)]
pub struct LiveRetrieval {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    /// (review, stage, item) -> (title, step, started)
    active: HashMap<(String, &'static str, String), (String, String, Instant)>,
    /// (review, item, when)
    recent: VecDeque<(String, RecentItem, Instant)>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ActiveItem {
    /// The screen the item belongs to (a frontend StageId).
    pub stage: &'static str,
    /// The report for retrieval; the study or review for other stages.
    pub report_id: String,
    pub title: String,
    pub step: String,
    pub seconds: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RecentItem {
    pub report_id: String,
    pub title: String,
    pub found: bool,
    pub detail: String,
    #[serde(skip_deserializing)]
    pub seconds_ago: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub active: Vec<ActiveItem>,
    pub recent: Vec<RecentItem>,
}

impl LiveRetrieval {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Starts tracking one report's retrieval; the handle clears it when dropped.
    pub fn track(self: &Arc<Self>, review: &str, report: &str, title: &str) -> Tracked {
        self.track_in(review, "fulltext", report, title)
    }

    /// Starts tracking one item of work on a stage's screen; the handle
    /// clears it when dropped, so a failed or paused job leaves nothing behind.
    pub fn track_in(
        self: &Arc<Self>,
        review: &str,
        stage: &'static str,
        item: &str,
        title: &str,
    ) -> Tracked {
        let t = Tracked {
            live: self.clone(),
            key: (review.to_owned(), stage, item.to_owned()),
            title: title.to_owned(),
        };
        t.step("Starting");
        t
    }

    /// Everything running now for a review, longest-running first.
    pub fn active(&self, review: &str) -> Vec<ActiveItem> {
        let g = self.lock();
        let mut active: Vec<ActiveItem> = g
            .active
            .iter()
            .filter(|((r, _, _), _)| r == review)
            .map(|((_, stage, item), (title, step, started))| ActiveItem {
                stage,
                report_id: item.clone(),
                title: title.clone(),
                step: step.clone(),
                seconds: started.elapsed().as_secs(),
            })
            .collect();
        active.sort_by_key(|a| std::cmp::Reverse(a.seconds));
        active
    }

    /// Retrieval only, for the Full text screen.
    pub fn snapshot(&self, review: &str) -> Snapshot {
        let active = self
            .active(review)
            .into_iter()
            .filter(|a| a.stage == "fulltext")
            .collect();
        let g = self.lock();
        let recent = g
            .recent
            .iter()
            .rev()
            .filter(|(r, _, _)| r == review)
            .map(|(_, item, at)| RecentItem {
                seconds_ago: at.elapsed().as_secs(),
                ..item.clone()
            })
            .collect();
        Snapshot { active, recent }
    }
}

pub struct Tracked {
    live: Arc<LiveRetrieval>,
    key: (String, &'static str, String),
    title: String,
}

impl Tracked {
    pub fn step(&self, step: impl Into<String>) {
        let mut g = self.live.lock();
        let started = g
            .active
            .get(&self.key)
            .map_or_else(Instant::now, |(_, _, s)| *s);
        g.active
            .insert(self.key.clone(), (self.title.clone(), step.into(), started));
    }

    /// Records the outcome for the recent-results feed.
    pub fn done(&self, found: bool, detail: impl Into<String>) {
        let mut g = self.live.lock();
        g.active.remove(&self.key);
        g.recent.push_back((
            self.key.0.clone(),
            RecentItem {
                report_id: self.key.2.clone(),
                title: self.title.clone(),
                found,
                detail: detail.into(),
                seconds_ago: 0,
            },
            Instant::now(),
        ));
        while g.recent.len() > RECENT {
            g.recent.pop_front();
        }
    }
}

impl Drop for Tracked {
    fn drop(&mut self) {
        self.live.lock().active.remove(&self.key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracks_steps_and_outcomes_per_review() {
        let live = Arc::new(LiveRetrieval::default());
        let a = live.track("rev1", "p1", "Paper one");
        a.step("Checking Unpaywall");
        let b = live.track("rev2", "p9", "Other review");
        let snap = live.snapshot("rev1");
        assert_eq!(snap.active.len(), 1);
        assert_eq!(snap.active[0].step, "Checking Unpaywall");
        a.done(true, "Semantic Scholar");
        drop(a);
        drop(b);
        let s = live.track_in("rev1", "synthesis", "rev1", "Synthesis");
        s.step("Coding findings: study 1 of 3");
        assert_eq!(live.active("rev1").len(), 1);
        assert_eq!(live.active("rev1")[0].stage, "synthesis");
        assert!(live.snapshot("rev1").active.is_empty(), "retrieval feed only");
        drop(s);
        let snap = live.snapshot("rev1");
        assert!(snap.active.is_empty());
        assert_eq!(snap.recent.len(), 1);
        assert!(snap.recent[0].found);
        assert!(
            live.snapshot("rev2").recent.is_empty(),
            "dropped without an outcome"
        );
    }
}
