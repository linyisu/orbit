use orbit_agent::events::AgentEvent;
use orbit_agent::tool::FileChange;
use orbit_model::types::ChatMessage;
use serde::{Deserialize, Serialize};

pub use orbit_protocol::Session;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Run {
    #[serde(default)]
    pub images: Vec<orbit_protocol::MessageImage>,
    pub prompt: String,
    pub status: String,
    /// Wall-clock duration measured by the writer, in milliseconds. Runs saved
    /// before this column existed retain `None` rather than a fabricated value.
    pub elapsed_ms: Option<u64>,
    pub events: Vec<AgentEvent>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub session: Session,
    pub history: Vec<ChatMessage>,
    pub runs: Vec<Run>,
    #[serde(default)]
    pub recovery: Option<RecoveryInfo>,
}

/// Runtime projection, distinct from the complete audit snapshot. File activity
/// and calibration have dedicated state instead of retaining tool output bodies.
#[derive(Clone, Debug)]
pub struct RuntimeSnapshot {
    snapshot: Snapshot,
    pub files: orbit_agent::memory::FileActivity,
    pub calibration: std::collections::BTreeMap<String, orbit_agent::calibration::TokenCalibration>,
}

impl From<Snapshot> for RuntimeSnapshot {
    fn from(mut snapshot: Snapshot) -> Self {
        let files = orbit_agent::memory::FileActivity::from_events(
            snapshot
                .runs
                .iter()
                .flat_map(|run| &run.events),
        );
        let mut calibration =
            std::collections::BTreeMap::<String, orbit_agent::calibration::TokenCalibration>::new();
        for event in snapshot
            .runs
            .iter()
            .flat_map(|run| &run.events)
        {
            if let AgentEvent::RequestMetrics {
                profile,
                estimated_input,
                usage,
                ..
            } = event
            {
                calibration
                    .entry(profile.clone())
                    .or_default()
                    .observe(
                        *estimated_input,
                        usage
                            .as_ref()
                            .and_then(|usage| usage.prompt_tokens),
                    );
            }
        }
        let last_completed = snapshot
            .runs
            .iter()
            .rposition(|run| run.status == "completed");
        for (index, run) in snapshot.runs.iter_mut().enumerate() {
            run.events.retain(|event| {
                Some(index) == last_completed
                    && matches!(event, AgentEvent::CompactionRecorded { .. })
            });
        }
        Self {
            snapshot,
            files,
            calibration,
        }
    }
}

impl RuntimeSnapshot {
    pub(crate) fn absorb(&mut self, evidence: crate::event_log::Evidence) {
        self.files.merge(&evidence.files);
        for (profile, samples) in evidence.calibration {
            self.calibration
                .entry(profile)
                .or_default()
                .merge(&samples);
        }
    }

    pub(crate) fn append(&mut self, mut newer: Self) {
        newer.files.merge(&self.files);
        let mut calibration = std::mem::take(&mut self.calibration);
        for (profile, samples) in &newer.calibration {
            calibration
                .entry(profile.clone())
                .or_default()
                .merge(samples);
        }
        newer.calibration = calibration;
        let mut runs = std::mem::take(&mut self.snapshot.runs);
        if newer
            .runs
            .iter()
            .any(|run| run.status == "completed")
        {
            for run in &mut runs {
                run.events.clear();
            }
        }
        runs.append(&mut newer.snapshot.runs);
        newer.snapshot.runs = runs;
        *self = newer;
    }
}

impl std::ops::Deref for RuntimeSnapshot {
    type Target = Snapshot;
    fn deref(&self) -> &Snapshot {
        &self.snapshot
    }
}

impl std::ops::DerefMut for RuntimeSnapshot {
    fn deref_mut(&mut self) -> &mut Snapshot {
        &mut self.snapshot
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecoveryInfo {
    pub turns: usize,
    pub tool_calls: usize,
    pub unresolved_tools: usize,
}

/// A successfully committed workspace edit reconstructed from the durable
/// event stream. The source text is held in Orbit's private central database,
/// never in a project-side checkpoint file.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceChange {
    pub run_id: i64,
    pub run_status: String,
    pub call_id: String,
    pub tool: String,
    pub change: FileChange,
    /// A local `/undo` restored this exact checkpoint. The original audit
    /// receipt remains visible, but it is no longer eligible for another undo.
    pub undone: bool,
}

/// An actual completed command reconstructed from the event ledger.
/// It is evidence of execution, not a claim that the model made in prose.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct VerificationRecord {
    pub run_id: i64,
    pub run_status: String,
    pub command: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub cancelled: bool,
    pub elapsed_ms: u64,
    #[serde(default)]
    pub evidence: Option<orbit_runtime::verification::VerificationEvidence>,
}

/// Project-scoped work that survives directory moves and conversation changes.
pub use orbit_protocol::project::{
    MemoryCategory, ProjectPlugin, ProjectTask, SessionMemory, TaskStatus,
};
