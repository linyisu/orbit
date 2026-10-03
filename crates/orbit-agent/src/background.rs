use std::{
    collections::{BTreeMap, HashMap, HashSet},
    future::Future,
    panic::AssertUnwindSafe,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

use futures_util::FutureExt;
use orbit_model::types::ToolCall;
use serde::Serialize;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::tool::{ToolError, ToolOutput};

pub const MAX_BACKGROUND_COMMANDS: usize = 8;
const MAX_RETAINED_JOBS: usize = 128;
static JOB_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Running,
    Cancelling,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Serialize)]
pub struct JobSnapshot {
    pub job_id: String,
    pub call_id: String,
    pub command: String,
    pub status: JobStatus,
    pub elapsed_ms: u64,
    pub output_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
}

struct Job {
    call: ToolCall,
    label: String,
    generation: u64,
    cancellation: CancellationToken,
    started: Instant,
    elapsed_ms: Option<u64>,
    output_id: Option<String>,
    result: Option<Result<ToolOutput, ToolError>>,
    cancelled: bool,
    observed: bool,
    delivered: bool,
}

#[derive(Default)]
struct State {
    jobs: BTreeMap<String, Job>,
    outputs: HashMap<String, String>,
    generation: u64,
    active: bool,
}

#[derive(Default)]
struct Inner {
    state: Mutex<State>,
    changed: Notify,
}

/// Shared by one session's tools and Agent. Processes never write conversation
/// history; the Agent acknowledges completions after its journal checkpoint.
#[derive(Clone, Default)]
pub struct BackgroundJobs(Arc<Inner>);

pub(crate) struct RunGuard {
    jobs: BackgroundJobs,
    generation: u64,
}

impl RunGuard {
    pub(crate) async fn cancel_and_wait(&self) {
        {
            let state = self
                .jobs
                .0
                .state
                .lock()
                .expect("background jobs lock poisoned");
            for job in state
                .jobs
                .values()
                .filter(|job| job.generation == self.generation && job.result.is_none())
            {
                job.cancellation.cancel();
            }
        }
        let wait = async {
            loop {
                let notified = self.jobs.0.changed.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if !self
                    .jobs
                    .0
                    .state
                    .lock()
                    .expect("background jobs lock poisoned")
                    .jobs
                    .values()
                    .any(|job| job.generation == self.generation && job.result.is_none())
                {
                    return;
                }
                notified.await;
            }
        };
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), wait).await;
    }
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        let mut state = self
            .jobs
            .0
            .state
            .lock()
            .expect("background jobs lock poisoned");
        for job in state
            .jobs
            .values_mut()
            .filter(|job| job.generation == self.generation)
        {
            job.observed = true;
            job.delivered = true;
            if job.result.is_none() {
                job.cancellation.cancel();
            }
        }
        state.active = false;
        self.jobs.0.changed.notify_waiters();
    }
}

#[derive(Clone)]
pub(crate) struct Completion {
    pub snapshot: JobSnapshot,
    pub call: ToolCall,
    pub result: Result<ToolOutput, ToolError>,
    pub observed: bool,
}

impl BackgroundJobs {
    pub(crate) fn begin_run(&self) -> Result<RunGuard, ToolError> {
        let mut state = self.0.state.lock().expect("background jobs lock poisoned");
        if state.active {
            return Err(ToolError::ExecutionFailed(
                "this session already has an active Agent run".into(),
            ));
        }
        state.generation += 1;
        state.active = true;
        state.outputs.clear();
        Ok(RunGuard {
            jobs: self.clone(),
            generation: state.generation,
        })
    }

    pub fn track_output(&self, call_id: &str, output_id: &str) {
        self.0
            .state
            .lock()
            .expect("background jobs lock poisoned")
            .outputs
            .insert(call_id.into(), output_id.into());
    }

    pub fn output_id_for_call(&self, call_id: &str) -> Option<String> {
        self.0
            .state
            .lock()
            .expect("background jobs lock poisoned")
            .outputs
            .get(call_id)
            .cloned()
    }

    pub fn start<F, Fut>(
        &self,
        call: ToolCall,
        label: String,
        output_id: Option<String>,
        parent: CancellationToken,
        work: F,
    ) -> Result<JobSnapshot, ToolError>
    where
        F: FnOnce(CancellationToken) -> Fut + Send + 'static,
        Fut: Future<Output = Result<ToolOutput, ToolError>> + Send + 'static,
    {
        let cancellation = parent.child_token();
        let id = format!(
            "job-{}-{}",
            std::process::id(),
            JOB_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        let snapshot = {
            let mut state = self.0.state.lock().expect("background jobs lock poisoned");
            if parent.is_cancelled() {
                return Err(ToolError::Cancelled);
            }
            if state
                .jobs
                .values()
                .filter(|job| job.result.is_none())
                .count()
                >= MAX_BACKGROUND_COMMANDS
            {
                return Err(ToolError::ExecutionFailed(format!(
                    "at most {MAX_BACKGROUND_COMMANDS} background commands may run per session; wait for an existing job"
                )));
            }
            while state.jobs.len() >= MAX_RETAINED_JOBS {
                let oldest = state
                    .jobs
                    .iter()
                    .filter(|(_, job)| job.result.is_some() && job.delivered)
                    .min_by_key(|(_, job)| job.started)
                    .map(|(id, _)| id.clone());
                let Some(oldest) = oldest else {
                    return Err(ToolError::ExecutionFailed(
                        "background command results must be delivered before starting more jobs"
                            .into(),
                    ));
                };
                state.jobs.remove(&oldest);
            }
            let job = Job {
                call,
                label: bounded(&label, 512),
                generation: state.generation,
                cancellation: cancellation.clone(),
                started: Instant::now(),
                elapsed_ms: None,
                output_id,
                result: None,
                cancelled: false,
                observed: false,
                delivered: false,
            };
            let snapshot = job.snapshot(&id, false);
            state.jobs.insert(id.clone(), job);
            snapshot
        };
        let jobs = self.clone();
        tokio::spawn(async move {
            let result = AssertUnwindSafe(async { work(cancellation).await })
                .catch_unwind()
                .await
                .unwrap_or_else(|_| {
                    Err(ToolError::ExecutionFailed(
                        "background command task panicked".into(),
                    ))
                });
            let mut state = jobs.0.state.lock().expect("background jobs lock poisoned");
            if let Some(job) = state.jobs.get_mut(&id) {
                job.elapsed_ms = Some(millis(job.started.elapsed()));
                job.cancelled = job.cancellation.is_cancelled();
                job.result = Some(result);
            }
            drop(state);
            jobs.0.changed.notify_waiters();
        });
        Ok(snapshot)
    }

    pub fn snapshots(
        &self,
        ids: &[String],
        include_result: bool,
    ) -> Result<Vec<JobSnapshot>, ToolError> {
        let state = self.0.state.lock().expect("background jobs lock poisoned");
        let ids = selected_ids(&state, ids)?;
        Ok(ids
            .iter()
            .map(|id| state.jobs[id].snapshot(id, include_result))
            .collect())
    }

    pub fn cancel(&self, ids: &[String]) -> Result<Vec<JobSnapshot>, ToolError> {
        let state = self.0.state.lock().expect("background jobs lock poisoned");
        let ids = selected_ids(&state, ids)?;
        for id in &ids {
            if state.jobs[id].result.is_none() {
                state.jobs[id].cancellation.cancel();
            }
        }
        self.0.changed.notify_waiters();
        Ok(ids
            .iter()
            .map(|id| state.jobs[id].snapshot(id, false))
            .collect())
    }

    /// Register the notification before inspecting state so completion between
    /// the check and await cannot be lost. Waiting never invokes the model.
    pub async fn wait(
        &self,
        ids: &[String],
        all: bool,
        cancellation: CancellationToken,
    ) -> Result<Vec<JobSnapshot>, ToolError> {
        let ids = {
            let state = self.0.state.lock().expect("background jobs lock poisoned");
            if ids.is_empty() {
                state
                    .jobs
                    .iter()
                    .filter(|(_, job)| {
                        job.generation == state.generation && !job.delivered && !job.observed
                    })
                    .map(|(id, _)| id.clone())
                    .collect()
            } else {
                selected_ids(&state, ids)?
            }
        };
        loop {
            let notified = self.0.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if cancellation.is_cancelled() {
                return Err(ToolError::Cancelled);
            }
            {
                let mut state = self.0.state.lock().expect("background jobs lock poisoned");
                let ready = ids
                    .iter()
                    .filter(|id| state.jobs.get(*id).is_some_and(|job| job.result.is_some()))
                    .count();
                if ids.is_empty() || (all && ready == ids.len()) || (!all && ready > 0) {
                    let mut snapshots = Vec::new();
                    for id in &ids {
                        let job = state.jobs.get_mut(id).ok_or_else(|| unknown(id))?;
                        if job.result.is_some() {
                            job.observed = true;
                        }
                        snapshots.push(job.snapshot(id, true));
                    }
                    return Ok(snapshots);
                }
                if ids.iter().any(|id| !state.jobs.contains_key(id)) {
                    return Err(ToolError::ExecutionFailed(
                        "a waited command result was evicted".into(),
                    ));
                }
            }
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => return Err(ToolError::Cancelled),
                _ = notified => {},
            }
        }
    }

    pub(crate) fn pending_ids(&self) -> Vec<String> {
        let state = self.0.state.lock().expect("background jobs lock poisoned");
        state
            .jobs
            .iter()
            .filter(|(_, job)| job.generation == state.generation && !job.delivered)
            .map(|(id, _)| id.clone())
            .collect()
    }

    pub(crate) async fn wait_for_completion(
        &self,
        cancellation: CancellationToken,
    ) -> Result<(), ToolError> {
        loop {
            let notified = self.0.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if cancellation.is_cancelled() {
                return Err(ToolError::Cancelled);
            }
            {
                let state = self.0.state.lock().expect("background jobs lock poisoned");
                if !state
                    .jobs
                    .values()
                    .any(|job| job.generation == state.generation && !job.delivered)
                    || state.jobs.values().any(|job| {
                        job.generation == state.generation && !job.delivered && job.result.is_some()
                    })
                {
                    return Ok(());
                }
            }
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => return Err(ToolError::Cancelled),
                _ = notified => {},
            }
        }
    }

    pub(crate) fn completions(&self) -> Vec<Completion> {
        let state = self.0.state.lock().expect("background jobs lock poisoned");
        state
            .jobs
            .iter()
            .filter(|(_, job)| job.generation == state.generation && !job.delivered)
            .filter_map(|(id, job)| {
                Some(Completion {
                    snapshot: job.snapshot(id, true),
                    call: job.call.clone(),
                    result: job.result.clone()?,
                    observed: job.observed,
                })
            })
            .collect()
    }

    pub(crate) fn acknowledge(&self, id: &str) {
        if let Some(job) = self
            .0
            .state
            .lock()
            .expect("background jobs lock poisoned")
            .jobs
            .get_mut(id)
        {
            job.delivered = true;
        }
    }
}

impl Job {
    fn snapshot(&self, id: &str, include_result: bool) -> JobSnapshot {
        let status = match &self.result {
            None if self.cancellation.is_cancelled() => JobStatus::Cancelling,
            None => JobStatus::Running,
            Some(_) if self.cancelled => JobStatus::Cancelled,
            Some(Err(ToolError::Cancelled)) => JobStatus::Cancelled,
            Some(Err(_)) => JobStatus::Failed,
            Some(Ok(output)) => {
                let result = serde_json::from_str::<serde_json::Value>(&output.content).ok();
                if result
                    .as_ref()
                    .is_some_and(|value| value["cancelled"] == true)
                {
                    JobStatus::Cancelled
                } else if result.as_ref().is_some_and(|value| {
                    value["timed_out"] == true
                        || value.get("exit_code").is_some_and(|code| code != 0)
                }) {
                    JobStatus::Failed
                } else {
                    JobStatus::Completed
                }
            }
        };
        let result = if include_result {
            self.result.as_ref().map(|result| match result {
                Ok(output) => compact_output(&output.content),
                Err(error) => serde_json::json!({ "error": error.to_string() }),
            })
        } else {
            None
        };
        JobSnapshot {
            job_id: id.into(),
            call_id: self.call.id.clone(),
            command: self.label.clone(),
            status,
            elapsed_ms: self
                .elapsed_ms
                .unwrap_or_else(|| millis(self.started.elapsed())),
            output_id: self.output_id.clone(),
            result,
        }
    }
}

fn selected_ids(state: &State, ids: &[String]) -> Result<Vec<String>, ToolError> {
    if ids.is_empty() {
        return Ok(state
            .jobs
            .iter()
            .filter(|(_, job)| job.generation == state.generation)
            .map(|(id, _)| id.clone())
            .collect());
    }
    if ids.len() > MAX_BACKGROUND_COMMANDS || ids.iter().collect::<HashSet<_>>().len() != ids.len()
    {
        return Err(ToolError::ExecutionFailed(format!(
            "specify up to {MAX_BACKGROUND_COMMANDS} distinct command job IDs"
        )));
    }
    for id in ids {
        if !state.jobs.contains_key(id) {
            return Err(unknown(id));
        }
    }
    Ok(ids.to_vec())
}

fn unknown(id: &str) -> ToolError {
    ToolError::ExecutionFailed(format!(
        "unknown command job {id}; jobs belong to this session and do not survive an Orbit restart"
    ))
}

fn millis(duration: std::time::Duration) -> u64 {
    duration.as_millis().min(u64::MAX as u128) as u64
}

fn bounded(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.into();
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[preview truncated]", &text[..end])
}

fn compact_output(content: &str) -> serde_json::Value {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(content) else {
        return serde_json::Value::String(bounded(content, 4096));
    };
    for field in ["stdout", "stderr"] {
        if let Some(text) = value.get(field).and_then(serde_json::Value::as_str) {
            let preview = bounded(text, 2048);
            if preview != text {
                value["completion_preview_truncated"] = true.into();
            }
            value[field] = preview.into();
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::oneshot;

    fn gated(jobs: &BackgroundJobs, name: &str) -> (JobSnapshot, oneshot::Sender<()>) {
        let (release, gate) = oneshot::channel();
        let snapshot = jobs
            .start(
                ToolCall::function(name, "run_command", serde_json::json!({"command":name})),
                name.into(),
                None,
                CancellationToken::new(),
                |token| async move {
                    tokio::select! {
                        biased;
                        _ = token.cancelled() => Err(ToolError::Cancelled),
                        _ = gate => Ok(ToolOutput::text("{\"exit_code\":0}")),
                    }
                },
            )
            .unwrap();
        (snapshot, release)
    }

    #[tokio::test]
    async fn wait_any_and_all_handle_already_finished_jobs_and_validate_all_ids() {
        let jobs = BackgroundJobs::default();
        let _run = jobs.begin_run().unwrap();
        let (first, first_release) = gated(&jobs, "orbit-first");
        let (second, second_release) = gated(&jobs, "orbit-second");
        let ids = vec![first.job_id, second.job_id];
        first_release.send(()).unwrap();
        let result = jobs
            .wait(&ids, false, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(result[0].status, JobStatus::Completed);
        assert_eq!(result[1].status, JobStatus::Running);
        let waiting = tokio::spawn({
            let jobs = jobs.clone();
            let ids = ids.clone();
            async move { jobs.wait(&ids, true, CancellationToken::new()).await }
        });
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished());
        second_release.send(()).unwrap();
        assert!(
            waiting
                .await
                .unwrap()
                .unwrap()
                .iter()
                .all(|job| job.status == JobStatus::Completed)
        );
        assert!(
            jobs.wait(
                &[ids[0].clone(), "foreign-job".into()],
                false,
                CancellationToken::new()
            )
            .await
            .is_err()
        );
        assert!(jobs.cancel(&[ids[0].clone(), ids[0].clone()]).is_err());
        assert!(
            jobs.completions()
                .iter()
                .all(|completion| completion.observed)
        );
    }

    #[tokio::test]
    async fn cancellation_is_not_completion_until_cleanup_finishes() {
        let jobs = BackgroundJobs::default();
        let (cancelled, cancellation_seen) = oneshot::channel();
        let (release, cleanup) = oneshot::channel();
        let job = jobs
            .start(
                ToolCall::function("orbit-cleanup", "run_command", serde_json::json!({})),
                "orbit-cleanup".into(),
                None,
                CancellationToken::new(),
                |token| async move {
                    token.cancelled().await;
                    cancelled.send(()).unwrap();
                    cleanup.await.unwrap();
                    Err(ToolError::Cancelled)
                },
            )
            .unwrap();
        let ids = vec![job.job_id];
        assert_eq!(jobs.cancel(&ids).unwrap()[0].status, JobStatus::Cancelling);
        cancellation_seen.await.unwrap();
        let waiting = tokio::spawn({
            let jobs = jobs.clone();
            let ids = ids.clone();
            async move { jobs.wait(&ids, true, CancellationToken::new()).await }
        });
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished());
        release.send(()).unwrap();
        assert_eq!(
            waiting.await.unwrap().unwrap()[0].status,
            JobStatus::Cancelled
        );
    }

    #[tokio::test]
    async fn cancelling_the_run_preserves_already_completed_results() {
        let jobs = BackgroundJobs::default();
        let parent = CancellationToken::new();
        let job = jobs
            .start(
                ToolCall::function("orbit-finished", "run_command", serde_json::json!({})),
                "orbit-finished".into(),
                None,
                parent.clone(),
                |_| async { Ok(ToolOutput::text("{\"exit_code\":0}")) },
            )
            .unwrap();
        let ids = [job.job_id];
        jobs.wait(&ids, true, CancellationToken::new())
            .await
            .unwrap();
        parent.cancel();
        assert_eq!(
            jobs.snapshots(&ids, false).unwrap()[0].status,
            JobStatus::Completed
        );
        assert_eq!(jobs.cancel(&ids).unwrap()[0].status, JobStatus::Completed);
    }

    #[tokio::test]
    async fn run_generation_and_session_manager_prevent_late_delivery() {
        let jobs = BackgroundJobs::default();
        let first_run = jobs.begin_run().unwrap();
        assert!(jobs.begin_run().is_err());
        let (first, _release) = gated(&jobs, "orbit-old");
        let other = BackgroundJobs::default();
        assert!(
            other
                .snapshots(std::slice::from_ref(&first.job_id), false)
                .is_err()
        );
        drop(first_run);
        let _second_run = jobs.begin_run().unwrap();
        jobs.wait(&[first.job_id], true, CancellationToken::new())
            .await
            .unwrap();
        assert!(jobs.completions().is_empty());
        assert!(jobs.pending_ids().is_empty());
    }

    #[tokio::test]
    async fn capacity_and_panics_have_bounded_recoverable_results() {
        let jobs = BackgroundJobs::default();
        let run = jobs.begin_run().unwrap();
        let mut ids = Vec::new();
        let mut releases = Vec::new();
        for index in 0..MAX_BACKGROUND_COMMANDS {
            let (job, release) = gated(&jobs, &format!("orbit-{index}"));
            ids.push(job.job_id);
            releases.push(release);
        }
        assert!(
            jobs.start(
                ToolCall::function("overflow", "run_command", serde_json::json!({})),
                "orbit-overflow".into(),
                None,
                CancellationToken::new(),
                |_| async { Ok(ToolOutput::text("unused")) }
            )
            .is_err()
        );
        jobs.cancel(&ids).unwrap();
        let result = jobs
            .wait(&ids, true, CancellationToken::new())
            .await
            .unwrap();
        assert!(result.iter().all(|job| job.status == JobStatus::Cancelled));
        let panic = jobs
            .start(
                ToolCall::function("panic", "run_command", serde_json::json!({})),
                "orbit-panic".into(),
                None,
                CancellationToken::new(),
                |_| async { panic!("Orbit test panic") },
            )
            .unwrap();
        let result = jobs
            .wait(&[panic.job_id], true, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(result[0].status, JobStatus::Failed);
        assert!(
            result[0].result.as_ref().unwrap()["error"]
                .as_str()
                .unwrap()
                .contains("panicked")
        );
        drop(run);
    }

    #[test]
    fn completion_preview_preserves_utf8_and_exit_metadata() {
        let value = compact_output(
            &serde_json::json!({
                "exit_code": 7, "stdout": "轨道".repeat(3000), "stderr": "轨道".repeat(3000),
                "output_id": "cmd-orbit", "timed_out": false,
            })
            .to_string(),
        );
        assert_eq!(value["exit_code"], 7);
        assert_eq!(value["output_id"], "cmd-orbit");
        assert_eq!(value["completion_preview_truncated"], true);
        assert!(value["stdout"].as_str().unwrap().len() < 2100);
        assert!(value["stderr"].as_str().unwrap().len() < 2100);
    }
}
