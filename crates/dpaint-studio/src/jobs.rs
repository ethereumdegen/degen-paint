//! Ops that outlive a click, and the project overview the grounding API reads.
//!
//! `dispatch("op")` stays synchronous — the CLI and MCP want the result, not a handle. The
//! GUI cannot afford that: a render or an `ai.*` call blocks the bridge thread for seconds,
//! and a navigator that sees no answer assumes the app is wedged. So the Studio also runs an
//! op on its own thread and reports it as a job, which is what puts `role=progressbar` and a
//! working Cancel button in the UI.
//!
//! Cancellation is honoured *between* ops, never inside one: killing a half-applied op would
//! be the one way to corrupt a project. A job cancelled before it starts touches nothing —
//! no engine, no journal entry — and that is the guarantee the Cancel button makes.
//!
//! The registry is per process rather than per [`Studio`]: every shell (`dpaint serve`, the
//! Tauri app) holds exactly one Studio for the life of the process, and `state` has to report
//! the busy list without being handed a registry to read.

use crate::api::Studio;
use dpaint_core::{now_iso, AssetStore, Error, Result, Workspace};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// How long a finished job stays readable. Long enough for a UI that polls every second and
/// for an agent that comes back after a detour; short enough that a long session does not
/// accumulate results nobody will read.
const KEEP_FINISHED: Duration = Duration::from_secs(300);

pub(crate) fn handles(method: &str) -> bool {
    matches!(
        method,
        "overview" | "job.start" | "job.status" | "job.cancel"
    )
}

pub(crate) fn dispatch_ext(studio: &Studio, method: &str, params: &Value) -> Option<Result<Value>> {
    match method {
        "overview" => Some(overview(studio, params)),
        "job.start" => Some(start(studio, params)),
        "job.status" => Some(str_param(params, "id").and_then(|id| status(&id))),
        "job.cancel" => Some(str_param(params, "id").and_then(|id| cancel(&id))),
        _ => None,
    }
}

/// The project as `dpaint_overview` reports it, from the same builder.
fn overview(studio: &Studio, params: &Value) -> Result<Value> {
    let root = project_root(studio)?;
    let mut ws = Workspace::open(&root)?;
    let only = params.get("doc").and_then(|d| d.as_str());
    let entries = ws.journal.load()?;
    dpaint_core::overview::overview(&ws.project, entries, &AssetStore::new(&root), only)
}

/// The one place that answers "which project is open". [`Studio`] owns that question; when
/// nothing is open there is no root and every job or quote is invalid rather than guessed.
pub(crate) fn project_root(studio: &Studio) -> Result<PathBuf> {
    studio
        .root()
        .ok_or_else(|| Error::Invalid("no project is open".into()))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum JobState {
    Running,
    Done,
    Error,
    Cancelled,
}

impl JobState {
    fn as_str(self) -> &'static str {
        match self {
            JobState::Running => "running",
            JobState::Done => "done",
            JobState::Error => "error",
            JobState::Cancelled => "cancelled",
        }
    }
}

struct Job {
    op: String,
    label: String,
    started_at: String,
    state: JobState,
    /// Set once the op is in flight. From then on a cancel is recorded but not honoured:
    /// the engine's transaction has to run to completion.
    applying: bool,
    cancel_requested: bool,
    result: Option<Value>,
    error: Option<Value>,
    finished: Option<Instant>,
}

impl Job {
    fn to_value(&self, id: &str) -> Value {
        let mut v = json!({
            "id": id,
            "state": self.state.as_str(),
            "op": self.op,
            "startedAt": self.started_at,
        });
        if let Some(r) = &self.result {
            v["result"] = r.clone();
        }
        if let Some(e) = &self.error {
            v["error"] = e.clone();
        }
        v
    }
}

#[derive(Default)]
struct Jobs {
    next: u64,
    jobs: BTreeMap<String, Job>,
}

impl Jobs {
    fn reap(&mut self) {
        self.jobs.retain(|_, j| match j.finished {
            Some(at) => at.elapsed() < KEEP_FINISHED,
            None => true,
        });
    }
}

/// A poisoned registry would take the whole GUI down with it, and the invariants here are a
/// map and a counter: recover the contents instead.
fn registry() -> MutexGuard<'static, Jobs> {
    static JOBS: LazyLock<Mutex<Jobs>> = LazyLock::new(Mutex::default);
    JOBS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Running jobs, for `state`'s `busy` array and the progressbar's accessible name.
pub fn busy() -> Vec<Value> {
    let mut reg = registry();
    reg.reap();
    reg.jobs
        .iter()
        .filter(|(_, j)| j.state == JobState::Running)
        .map(|(id, j)| json!({ "id": id, "op": j.op, "label": j.label }))
        .collect()
}

fn start(studio: &Studio, params: &Value) -> Result<Value> {
    let op = str_param(params, "op")?;
    let root = project_root(studio)?;
    let request = json!({
        "op": op,
        "args": params.get("args").cloned().unwrap_or_else(|| json!({})),
        "doc": params.get("doc").cloned().unwrap_or(Value::Null),
        "dryRun": params.get("dryRun").and_then(|d| d.as_bool()).unwrap_or(false),
    });
    let id = register(&op);

    let thread_id = id.clone();
    let spawned = std::thread::Builder::new()
        .name(format!("dpaint-{id}"))
        .spawn(move || {
            let outcome = match Studio::open(&root) {
                // The gate is the whole of cancellation: past it the op is in flight and
                // runs to completion, before it nothing has been touched.
                Ok(studio) if claim(&thread_id) => match studio.dispatch("op", &request) {
                    Ok(v) => Outcome::Done(v),
                    Err(e) => Outcome::Failed(e),
                },
                Ok(_) => return,
                Err(e) => Outcome::Failed(e),
            };
            finish(&thread_id, outcome);
        });
    if let Err(e) = spawned {
        // Nobody will ever finish it, and a job stuck in `busy` leaves the UI's progressbar
        // up for the rest of the session.
        registry().jobs.remove(&id);
        return Err(Error::Invalid(format!("cannot start a job thread: {e}")));
    }

    Ok(json!({ "id": id }))
}

fn register(op: &str) -> String {
    let mut reg = registry();
    reg.reap();
    reg.next += 1;
    let id = format!("job_{}", reg.next);
    reg.jobs.insert(
        id.clone(),
        Job {
            op: op.to_string(),
            label: format!("Running {op}"),
            started_at: now_iso(),
            state: JobState::Running,
            applying: false,
            cancel_requested: false,
            result: None,
            error: None,
            finished: None,
        },
    );
    id
}

/// Take the job into flight, unless a cancel arrived first. Both sides of that decision run
/// under the registry lock, so a cancel either stops the op or does not — never both.
fn claim(id: &str) -> bool {
    let mut reg = registry();
    let Some(job) = reg.jobs.get_mut(id) else {
        return false;
    };
    if job.cancel_requested {
        job.state = JobState::Cancelled;
        job.finished = Some(Instant::now());
        return false;
    }
    job.applying = true;
    true
}

enum Outcome {
    Done(Value),
    Failed(Error),
}

fn finish(id: &str, outcome: Outcome) {
    let mut reg = registry();
    let Some(job) = reg.jobs.get_mut(id) else {
        return;
    };
    job.applying = false;
    job.finished = Some(Instant::now());
    match outcome {
        Outcome::Done(v) => {
            job.state = JobState::Done;
            job.result = Some(v);
        }
        Outcome::Failed(e) => {
            job.state = JobState::Error;
            job.error = Some(
                serde_json::to_value(e.detail()).unwrap_or_else(|_| json!({ "code": "invalid" })),
            );
        }
    }
}

fn status(id: &str) -> Result<Value> {
    let mut reg = registry();
    reg.reap();
    reg.jobs
        .get(id)
        .map(|j| j.to_value(id))
        .ok_or_else(|| Error::Invalid(format!("no job '{id}'")))
}

fn cancel(id: &str) -> Result<Value> {
    let mut reg = registry();
    let job = reg
        .jobs
        .get_mut(id)
        .ok_or_else(|| Error::Invalid(format!("no job '{id}'")))?;
    job.cancel_requested = true;
    if job.state == JobState::Running && !job.applying {
        job.state = JobState::Cancelled;
        job.finished = Some(Instant::now());
    }
    Ok(json!({ "id": id, "state": job.state.as_str() }))
}

fn str_param(params: &Value, key: &str) -> Result<String> {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| Error::Invalid(format!("missing '{key}'")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use dpaint_core::doc::{Document, RasterDoc};
    use dpaint_core::{DocId, Project};

    fn studio() -> (tempfile::TempDir, Studio) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("p.dpaint");
        let project = Project::new(
            "demo",
            Document::Raster(RasterDoc::new(DocId::from("doc_main"), "main", 32, 32)),
        );
        Workspace::create(&root, project).unwrap();
        let s = Studio::open(&root).unwrap();
        (tmp, s)
    }

    fn journal_len(studio: &Studio) -> usize {
        let mut ws = Workspace::open(studio.root().unwrap()).unwrap();
        ws.journal.load().unwrap().len()
    }

    fn settle(id: &str) -> Value {
        for _ in 0..500 {
            let s = status(id).unwrap();
            if s["state"] != "running" {
                return s;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("job {id} never finished");
    }

    #[test]
    fn a_job_commits_exactly_one_journal_entry_and_reports_its_result() {
        let (_t, s) = studio();
        let started = s
            .dispatch(
                "job.start",
                &json!({ "op": "raster.layer.add",
                         "args": { "type": "fill", "color": "#ff0000", "name": "bg" } }),
            )
            .unwrap();
        let id = started["id"].as_str().unwrap().to_string();
        assert!(id.starts_with("job_"));

        let done = settle(&id);
        assert_eq!(done["state"], "done", "{done}");
        assert_eq!(done["op"], "raster.layer.add");
        assert_eq!(done["result"]["op"], "raster.layer.add");
        assert_eq!(journal_len(&s), 1);
        assert!(!busy().iter().any(|j| j["id"] == id.as_str()));
    }

    #[test]
    fn a_job_cancelled_before_it_applies_leaves_the_journal_untouched() {
        let (_t, s) = studio();
        let before = journal_len(&s);

        // The gate, driven the way the worker thread drives it: a cancel that lands first
        // must stop the op before the engine is opened at all.
        let id = register("raster.layer.add");
        assert_eq!(
            s.dispatch("job.cancel", &json!({ "id": id })).unwrap()["state"],
            "cancelled"
        );
        assert!(!claim(&id), "a cancelled job must never be claimed");

        let st = s.dispatch("job.status", &json!({ "id": id })).unwrap();
        assert_eq!(st["state"], "cancelled");
        assert!(st.get("result").is_none());
        assert_eq!(
            journal_len(&s),
            before,
            "a cancelled job must not journal anything"
        );
    }

    /// `state`'s busy array is what puts the progressbar's accessible name on screen, so the
    /// label has to name the op a human can cancel.
    #[test]
    fn a_running_job_is_listed_as_busy_with_a_label_that_names_the_op() {
        let id = register("raster.filter.gaussian-blur");
        let row = busy()
            .into_iter()
            .find(|j| j["id"] == id.as_str())
            .expect("a registered job is busy");
        assert_eq!(row["op"], "raster.filter.gaussian-blur");
        assert_eq!(row["label"], "Running raster.filter.gaussian-blur");

        finish(&id, Outcome::Done(json!({})));
        assert!(!busy().iter().any(|j| j["id"] == id.as_str()));
    }

    #[test]
    fn an_op_already_in_flight_is_not_killed_by_a_cancel() {
        let (_t, _s) = studio();
        let id = register("raster.layer.add");
        assert!(claim(&id));
        assert_eq!(cancel(&id).unwrap()["state"], "running");

        finish(&id, Outcome::Done(json!({ "op": "raster.layer.add" })));
        assert_eq!(status(&id).unwrap()["state"], "done");
    }

    #[test]
    fn a_failed_job_carries_the_structured_error() {
        let (_t, s) = studio();
        let id = s
            .dispatch(
                "job.start",
                &json!({ "op": "raster.layer.set", "args": { "target": "#nope", "opacity": 0.5 } }),
            )
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let done = settle(&id);
        assert_eq!(done["state"], "error");
        assert_eq!(done["error"]["code"], "selector_no_match");
        assert_eq!(journal_len(&s), 0);
    }

    #[test]
    fn an_unknown_job_id_is_an_error_not_a_silent_empty_state() {
        let (_t, s) = studio();
        let err = s
            .dispatch("job.status", &json!({ "id": "job_nope" }))
            .unwrap_err();
        assert_eq!(err.code(), "invalid");
        assert!(s
            .dispatch("job.cancel", &json!({ "id": "job_nope" }))
            .is_err());
    }

    #[test]
    fn overview_is_the_same_builder_the_mcp_tool_serves() {
        let (_t, s) = studio();
        let o = s.dispatch("overview", &json!({})).unwrap();
        assert_eq!(o["documents"][0]["id"], "doc_main");
        assert_eq!(o["documents"][0]["kind"], "raster");
        assert_eq!(o["documents"][0]["layers"], 0);
        assert_eq!(o["history"]["entries"], 0);
    }

    /// The Welcome screen is a real state: these methods have to say so, not panic on a
    /// missing root.
    #[test]
    fn with_no_project_open_a_job_or_an_overview_is_refused() {
        let s = Studio::empty();
        for m in ["job.start", "overview"] {
            let err = s
                .dispatch(m, &json!({ "op": "raster.layer.add" }))
                .unwrap_err();
            assert_eq!(err.code(), "invalid", "{m}");
            assert!(err.to_string().contains("no project is open"), "{m}: {err}");
        }
    }
}
