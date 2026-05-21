use tokio::sync::oneshot;

/// Preprocessed log data sent from a worker task to the updater task.
pub struct ParseEvent {
    pub tokens: Vec<String>,
    pub reply: oneshot::Sender<DrainResult>,
}

/// Result returned for each ingested log message.
#[derive(Debug, Clone)]
pub struct DrainResult {
    pub cluster_id: usize,
    pub cluster_size: usize,
    pub template: String,
    pub update_type: String,
}
