use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::drain::Drain;
use crate::masking::LogMasker;
use crate::parse_event::{DrainResult, ParseEvent};

/// Concurrent wrapper around [`Drain`].
///
/// N caller tasks preprocess logs in parallel (tokenization happens on the
/// calling task), then send [`ParseEvent`]s through an MPSC channel to a single
/// updater task that owns the [`Drain`] tree and applies mutations sequentially.
pub struct ConcurrentDrain {
    sender: mpsc::Sender<ParseEvent>,
    updater_handle: JoinHandle<()>,
    extra_delimiters: Vec<String>,
    masker: Option<LogMasker>,
}

impl ConcurrentDrain {
    /// Create a new concurrent drain pipeline.
    ///
    /// * `drain` — a fully configured [`Drain`] instance (the updater takes ownership).
    /// * `channel_capacity` — bounded MPSC channel size.
    pub fn new(drain: Drain, channel_capacity: usize) -> Self {
        let extra_delimiters = drain.extra_delimiters().to_vec();
        let masker = drain.masker().cloned();
        let (tx, rx) = mpsc::channel::<ParseEvent>(channel_capacity);

        let updater_handle = tokio::spawn(Self::updater_loop(drain, rx));

        Self {
            sender: tx,
            updater_handle,
            extra_delimiters,
            masker,
        }
    }

    /// The single-writer updater loop that owns the Drain tree.
    async fn updater_loop(mut drain: Drain, mut rx: mpsc::Receiver<ParseEvent>) {
        while let Some(event) = rx.recv().await {
            let (cluster, update_type) = drain.add_log_message_from_tokens(event.tokens);
            let result = DrainResult {
                cluster_id: cluster.cluster_id,
                cluster_size: cluster.size,
                template: cluster.get_template(),
                update_type: update_type.as_str().to_string(),
            };
            // If the receiver was dropped, just continue draining the channel.
            let _ = event.reply.send(result);
        }
    }

    /// Preprocess and ingest a log message.
    ///
    /// Tokenization runs on the calling task; only the token vector is sent to
    /// the updater task for tree mutation.
    pub async fn add_log_message(&self, content: &str) -> DrainResult {
        let masked = match &self.masker {
            Some(m) => m.mask(content),
            None => content.to_string(),
        };
        let tokens = self.tokenize(&masked);
        let (tx, rx) = oneshot::channel();
        let event = ParseEvent { tokens, reply: tx };
        self.sender
            .send(event)
            .await
            .expect("updater task has stopped");
        rx.await.expect("updater dropped reply sender")
    }

    /// Tokenize content using the same logic as [`Drain::get_content_as_tokens`].
    fn tokenize(&self, content: &str) -> Vec<String> {
        let mut content = content.trim().to_string();
        for delim in &self.extra_delimiters {
            content = content.replace(delim.as_str(), " ");
        }
        content.split_whitespace().map(|s| s.to_string()).collect()
    }

    /// Gracefully shut down the pipeline.
    ///
    /// Drops the sender (so the updater loop exits after processing remaining
    /// events) and awaits the updater task.
    pub async fn shutdown(self) {
        drop(self.sender);
        self.updater_handle.await.expect("updater task panicked");
    }
}
