//! What happened to one eval task: everything grading and tagging look at.

use crate::client::{Event, TraceEntry};

#[derive(Debug, Clone, Default)]
pub struct Outcome {
    /// The runtime's task id, if the task was created.
    pub task_id: Option<String>,
    /// The last status seen, if the task was created.
    pub status: Option<String>,
    pub result: Option<String>,
    pub error: Option<String>,
    pub approvals: usize,
    pub attempts: u32,
    /// The task had not settled when the runner stopped waiting.
    pub timed_out: bool,
    /// A call to the runtime failed, so the task couldn't be created or followed.
    pub runtime_error: Option<String>,
    pub events: Vec<Event>,
    /// `None` when the trace couldn't be read (see [`crate::client::Runtime::trace`]).
    pub trace: Option<Vec<TraceEntry>>,
    /// From creation until a worker picked the task up, in seconds.
    pub queue_secs: Option<f64>,
    /// From creation until the task settled, in seconds.
    pub total_secs: Option<f64>,
}

impl Outcome {
    pub fn status_is(&self, s: &str) -> bool {
        self.status.as_deref() == Some(s)
    }

    pub fn result_text(&self) -> &str {
        self.result.as_deref().unwrap_or("")
    }
}
