//! Live reload off the shared journal.
//!
//! Every surface in degen-paint writes to one project directory and appends to one
//! `history.jsonl`. That is the property that lets an agent and a human work on the same
//! document, and the viewport should *show* it: when an op lands — from the CLI, from MCP,
//! from the Studio — the window redraws with it.
//!
//! The mechanism is a poll rather than a filesystem watcher, because the thing that
//! matters is the newest sequence number, not the event that produced it: a poll cannot
//! miss a write, cannot fire twice for one write, and needs no platform-specific code.
//! The cost is one `stat` twice a second, and a re-read only when that `stat` moved.

use dpaint_core::{Result, Workspace};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// Twice a second: fast enough that an agent's edit appears while you are still looking
/// at the window, slow enough to be free.
pub const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Cheap change signal for the journal file, so the common case (nothing happened) costs
/// one `stat` instead of a parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    pub len: u64,
    pub modified: Option<SystemTime>,
}

pub fn stamp(path: &Path) -> Option<Stamp> {
    let md = std::fs::metadata(path).ok()?;
    Some(Stamp {
        len: md.len(),
        modified: md.modified().ok(),
    })
}

/// Watches one project's journal and reports when the viewport needs rebuilding.
#[derive(Debug)]
pub struct JournalWatch {
    root: PathBuf,
    seq: Option<u64>,
    stamp: Option<Stamp>,
    next_poll: Instant,
    reloads: u32,
    forced: bool,
}

impl JournalWatch {
    /// `seq` is the newest sequence number at the moment the viewport loaded the project,
    /// so the state already on screen does not count as a change.
    pub fn new(root: impl Into<PathBuf>, seq: Option<u64>) -> Self {
        let root = root.into();
        let stamp = stamp(&journal_path(&root));
        Self {
            root,
            seq,
            stamp,
            next_poll: Instant::now() + POLL_INTERVAL,
            reloads: 0,
            forced: false,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn seq(&self) -> Option<u64> {
        self.seq
    }

    /// How many rebuilds this watcher has triggered — shown in the status readout so the
    /// shared-journal behaviour is visible rather than merely claimed.
    pub fn reloads(&self) -> u32 {
        self.reloads
    }

    pub fn due(&self, now: Instant) -> bool {
        now >= self.next_poll
    }

    pub fn schedule_next(&mut self, now: Instant) {
        self.next_poll = now + POLL_INTERVAL;
    }

    /// The decision, with the I/O taken out: does this newest sequence number mean the
    /// viewport is stale? Exactly one `true` per change, whatever the poll rate.
    pub fn observe(&mut self, newest: Option<u64>) -> bool {
        if newest == self.seq {
            return false;
        }
        self.seq = newest;
        self.reloads += 1;
        true
    }

    /// Force the next [`poll`](Self::poll) to re-read and rebuild, whatever the journal
    /// looks like — what the `r` key does.
    pub fn invalidate(&mut self) {
        self.forced = true;
        self.seq = None;
    }

    /// Poll the project. Returns the freshly opened workspace when something changed,
    /// and `None` when it is not yet time or nothing moved.
    ///
    /// A project that vanishes or fails to parse mid-write is not an error the viewport
    /// should die of: the stamp is left untouched so the next poll tries again.
    pub fn poll(&mut self, now: Instant) -> Result<Option<Workspace>> {
        if !self.due(now) {
            return Ok(None);
        }
        self.schedule_next(now);

        let path = journal_path(&self.root);
        let fresh = stamp(&path);
        if fresh == self.stamp && !self.forced {
            return Ok(None);
        }
        self.forced = false;
        self.stamp = fresh;

        let mut ws = Workspace::open(&self.root)?;
        let newest = ws.journal.load()?.last().map(|e| e.seq);
        if self.observe(newest) {
            Ok(Some(ws))
        } else {
            Ok(None)
        }
    }
}

pub fn journal_path(root: &Path) -> PathBuf {
    root.join("history.jsonl")
}

/// The newest sequence number in a workspace's journal, for priming a watcher.
pub fn newest_seq(ws: &mut Workspace) -> Result<Option<u64>> {
    Ok(ws.journal.load()?.last().map(|e| e.seq))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sequence_bump_triggers_exactly_one_rebuild() {
        let mut w = JournalWatch::new("/nonexistent", Some(3));
        assert!(!w.observe(Some(3)), "the state on screen is not a change");
        assert!(w.observe(Some(4)), "an agent appended an op");
        assert!(!w.observe(Some(4)), "polling again must not rebuild again");
        assert!(!w.observe(Some(4)));
        assert_eq!(w.reloads(), 1);
        assert_eq!(w.seq(), Some(4));
    }

    #[test]
    fn several_ops_between_polls_are_one_rebuild() {
        let mut w = JournalWatch::new("/nonexistent", Some(1));
        assert!(w.observe(Some(9)), "a burst of ops is still one rebuild");
        assert_eq!(w.reloads(), 1);
    }

    #[test]
    fn a_project_with_no_history_yet_starts_quiet_then_wakes_on_the_first_op() {
        let mut w = JournalWatch::new("/nonexistent", None);
        assert!(!w.observe(None));
        assert_eq!(w.reloads(), 0);
        assert!(w.observe(Some(1)));
        assert_eq!(w.reloads(), 1);
    }

    #[test]
    fn undo_rewrites_the_journal_and_that_counts_as_a_change() {
        // Undo does not append; it flips an entry and can shorten the applied history,
        // so the newest sequence number can go *down*. Direction must not matter.
        let mut w = JournalWatch::new("/nonexistent", Some(7));
        assert!(w.observe(Some(6)));
        assert_eq!(w.seq(), Some(6));
    }

    #[test]
    fn polling_respects_the_interval() {
        let w = JournalWatch::new("/nonexistent", Some(1));
        let now = Instant::now();
        assert!(!w.due(now), "a fresh watcher does not poll immediately");
        assert!(w.due(now + POLL_INTERVAL + Duration::from_millis(1)));
    }

    #[test]
    fn a_forced_reload_rebuilds_even_when_the_file_has_not_moved() {
        let mut w = JournalWatch::new("/nonexistent", Some(4));
        w.invalidate();
        assert!(w.observe(Some(4)), "`r` must rebuild from disk regardless");
    }

    #[test]
    fn a_missing_journal_has_no_stamp_and_does_not_panic() {
        assert_eq!(stamp(Path::new("/nonexistent/history.jsonl")), None);
    }

    #[test]
    fn a_forced_poll_of_a_missing_project_reports_the_error_instead_of_exploding() {
        let mut w = JournalWatch::new("/nonexistent-degen-paint-project", None);
        w.invalidate();
        assert!(w.poll(Instant::now() + POLL_INTERVAL * 2).is_err());
    }

    #[test]
    fn an_op_appended_by_another_process_is_picked_up_on_the_next_poll() {
        use dpaint_core::doc::Document;
        use dpaint_core::{Actor, DocId, Journal, Project, RasterDoc, Workspace};

        let tmp = tempfile::tempdir().expect("temp dir");
        let root = tmp.path().join("scratch.dpaint");
        let doc = DocId::from("doc_a");
        let project = Project::new(
            "scratch",
            Document::Raster(RasterDoc::new(doc, "canvas", 64, 64)),
        );
        Workspace::create(&root, project.clone()).expect("create");

        let mut w = JournalWatch::new(&root, None);
        let mut now = Instant::now() + POLL_INTERVAL * 2;
        assert!(
            w.poll(now).expect("poll").is_none(),
            "nothing has happened yet"
        );

        // What another process does: append an entry to the shared journal.
        let before = serde_json::to_value(&project).expect("before");
        let mut after = project.clone();
        after.name = "renamed by an agent".into();
        let after_value = serde_json::to_value(&after).expect("after");
        let mut journal = Journal::new(root.join("history.jsonl"));
        journal
            .record(
                "doc.rename",
                serde_json::json!({ "name": "renamed by an agent" }),
                &before,
                &after_value,
                None,
                Actor::Agent,
            )
            .expect("record");
        std::fs::write(
            root.join("project.json"),
            serde_json::to_string_pretty(&after).expect("json"),
        )
        .expect("write project");

        now += POLL_INTERVAL * 2;
        let reloaded = w.poll(now).expect("poll").expect("the op must be seen");
        assert_eq!(reloaded.project.name, "renamed by an agent");
        assert_eq!(w.seq(), Some(1));
        assert_eq!(w.reloads(), 1);

        now += POLL_INTERVAL * 2;
        assert!(
            w.poll(now).expect("poll").is_none(),
            "a quiet journal must not rebuild a second time"
        );
        assert_eq!(w.reloads(), 1);
    }
}
