//! The AI bridge: model calls the app would send to a provider wait here
//! until an assistant connected over MCP takes them (`sift_next_task`) and
//! answers them (`sift_submit_task`).
//!
//! The app reaches the bridge through an ordinary OpenAI-compatible provider
//! (preset `mcp_bridge`), so retries, schema validation, the event log and the
//! automation disclosure all work exactly as they do for any other provider.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;
use tokio::sync::{oneshot, Notify};

use crate::llm::{parse_and_validate, JsonSchema};

/// A claimed task nobody answered within this time goes back to the queue,
/// so a client that crashed mid-task does not strand it.
const CLAIM_TTL: Duration = Duration::from_secs(600);

/// What the assistant sees for one waiting model call.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TaskView {
    pub id: String,
    /// Instructions for the call (the app's versioned prompt).
    pub system: Option<String>,
    /// The conversation after the system prompt: `{role, content}` objects.
    pub messages: Vec<Value>,
    /// JSON Schema the answer must match; `None` means plain text.
    pub schema: Option<Value>,
    pub schema_name: Option<String>,
    /// Tasks still waiting behind this one.
    pub queued_behind: usize,
}

/// The answer handed back to the waiting HTTP request.
#[derive(Debug, Clone, PartialEq)]
pub struct Answer {
    pub content: String,
    /// Who answered, e.g. "claude-code (claude-opus-5-5)"; disclosed as the
    /// model that actually replied.
    pub answered_by: String,
}

struct Task {
    view: TaskView,
    reply: oneshot::Sender<Answer>,
    claimed_at: Option<Instant>,
}

#[derive(Default)]
struct Inner {
    order: VecDeque<String>,
    tasks: HashMap<String, Task>,
    /// Last time any client asked for work, for the status line in Settings.
    last_poll: Option<Instant>,
}

#[derive(Default)]
pub struct Bridge {
    inner: Mutex<Inner>,
    arrived: Notify,
}

/// Held by the HTTP request that is waiting for an answer. Dropping it (the
/// request timed out or the connection closed) withdraws the task.
pub struct Ticket {
    id: String,
    bridge: Arc<Bridge>,
    rx: Option<oneshot::Receiver<Answer>>,
}

impl Ticket {
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Waits for the answer, up to `limit`. `None` means nobody answered.
    pub async fn wait(mut self, limit: Duration) -> Option<Answer> {
        let rx = self.rx.take()?;
        tokio::time::timeout(limit, rx).await.ok()?.ok()
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        self.bridge.withdraw(&self.id);
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeStatus {
    pub waiting: usize,
    pub claimed: usize,
    /// Seconds since a client last asked for work; `None` if none has.
    pub last_poll_secs: Option<u64>,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum SubmitError {
    #[error("no task with id {0} is waiting; it may have timed out. Call sift_next_task for current work.")]
    Unknown(String),
    #[error("the answer does not match the task's JSON schema: {0}. Fix it and submit again with the same id.")]
    Invalid(String),
}

impl Bridge {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Queues a model call. The returned ticket resolves when it is answered.
    pub fn post(
        self: &Arc<Self>,
        system: Option<String>,
        messages: Vec<Value>,
        schema: Option<(String, Value)>,
    ) -> Ticket {
        let id = format!("t_{}", uuid::Uuid::now_v7().simple());
        let (tx, rx) = oneshot::channel();
        let (schema_name, schema) = match schema {
            Some((n, s)) => (Some(n), Some(s)),
            None => (None, None),
        };
        {
            let mut inner = self.lock();
            inner.order.push_back(id.clone());
            inner.tasks.insert(
                id.clone(),
                Task {
                    view: TaskView {
                        id: id.clone(),
                        system,
                        messages,
                        schema,
                        schema_name,
                        queued_behind: 0,
                    },
                    reply: tx,
                    claimed_at: None,
                },
            );
        }
        self.arrived.notify_waiters();
        Ticket {
            id,
            bridge: self.clone(),
            rx: Some(rx),
        }
    }

    fn withdraw(&self, id: &str) {
        let mut inner = self.lock();
        inner.tasks.remove(id);
        inner.order.retain(|t| t != id);
    }

    /// Hands out the oldest unclaimed task, if any.
    fn take(&self) -> Option<TaskView> {
        let mut inner = self.lock();
        inner.last_poll = Some(Instant::now());
        let now = Instant::now();
        let Inner { order, tasks, .. } = &mut *inner;
        let pos = order.iter().position(|id| {
            tasks.get(id).is_some_and(|t| {
                t.claimed_at
                    .is_none_or(|at| now.duration_since(at) >= CLAIM_TTL)
            })
        })?;
        let id = order.get(pos)?.clone();
        let behind = order.len().saturating_sub(pos + 1);
        let task = tasks.get_mut(&id)?;
        task.claimed_at = Some(now);
        let mut view = task.view.clone();
        view.queued_behind = behind;
        Some(view)
    }

    /// Waits up to `limit` for a task to arrive.
    pub async fn next(&self, limit: Duration) -> Option<TaskView> {
        let deadline = tokio::time::Instant::now() + limit;
        loop {
            // Created before checking, so a task posted in between still wakes us.
            let arrived = self.arrived.notified();
            if let Some(t) = self.take() {
                return Some(t);
            }
            if tokio::time::timeout_at(deadline, arrived).await.is_err() {
                // One last look: a claim may have expired meanwhile.
                return self.take();
            }
        }
    }

    /// Answers a task. With a schema, the answer is checked first so the
    /// assistant can correct it instead of spending the app's retry.
    pub fn submit(&self, id: &str, content: String, answered_by: String) -> Result<(), SubmitError> {
        let mut inner = self.lock();
        let task = inner
            .tasks
            .get(id)
            .ok_or_else(|| SubmitError::Unknown(id.to_owned()))?;
        if let (Some(schema), Some(name)) = (&task.view.schema, &task.view.schema_name) {
            let schema = JsonSchema {
                name: name.clone(),
                schema: schema.clone(),
            };
            if let Err(e) = parse_and_validate(&content, &schema) {
                return Err(SubmitError::Invalid(e.to_string()));
            }
        }
        let task = inner
            .tasks
            .remove(id)
            .ok_or_else(|| SubmitError::Unknown(id.to_owned()))?;
        inner.order.retain(|t| t != id);
        task.reply
            .send(Answer {
                content,
                answered_by,
            })
            .map_err(|_| SubmitError::Unknown(id.to_owned()))
    }

    pub fn status(&self) -> BridgeStatus {
        let inner = self.lock();
        let claimed = inner
            .tasks
            .values()
            .filter(|t| t.claimed_at.is_some())
            .count();
        BridgeStatus {
            waiting: inner.tasks.len() - claimed,
            claimed,
            last_poll_secs: inner.last_poll.map(|t| t.elapsed().as_secs()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema() -> (String, Value) {
        (
            "screen".into(),
            json!({"type": "object", "properties": {"decision": {"type": "string", "enum": ["include", "exclude"]}},
                   "required": ["decision"], "additionalProperties": false}),
        )
    }

    #[tokio::test]
    async fn a_posted_task_is_taken_answered_and_delivered() {
        let bridge = Bridge::new();
        let ticket = bridge.post(
            Some("screen this".into()),
            vec![json!({"role": "user", "content": "record"})],
            Some(schema()),
        );
        let task = bridge.next(Duration::from_millis(50)).await.unwrap();
        assert_eq!(task.id, ticket.id());
        assert_eq!(task.system.as_deref(), Some("screen this"));
        assert_eq!(bridge.status().claimed, 1);
        // A claimed task is not handed out twice.
        assert!(bridge.next(Duration::from_millis(10)).await.is_none());
        bridge
            .submit(&task.id, r#"{"decision":"include"}"#.into(), "test".into())
            .unwrap();
        let answer = ticket.wait(Duration::from_millis(50)).await.unwrap();
        assert_eq!(answer.content, r#"{"decision":"include"}"#);
        assert_eq!(bridge.status().waiting + bridge.status().claimed, 0);
    }

    #[tokio::test]
    async fn an_answer_that_breaks_the_schema_is_returned_for_correction() {
        let bridge = Bridge::new();
        let ticket = bridge.post(None, vec![], Some(schema()));
        let task = bridge.next(Duration::from_millis(10)).await.unwrap();
        let err = bridge
            .submit(&task.id, r#"{"decision":"maybe"}"#.into(), "t".into())
            .unwrap_err();
        assert!(matches!(err, SubmitError::Invalid(_)));
        // The task stays open; a corrected answer goes through.
        bridge
            .submit(&task.id, r#"{"decision":"exclude"}"#.into(), "t".into())
            .unwrap();
        assert!(ticket.wait(Duration::from_millis(10)).await.is_some());
    }

    #[tokio::test]
    async fn a_waiting_client_wakes_when_a_task_arrives() {
        let bridge = Bridge::new();
        let b = bridge.clone();
        let waiter = tokio::spawn(async move { b.next(Duration::from_secs(5)).await });
        tokio::time::sleep(Duration::from_millis(20)).await;
        let _ticket = bridge.post(None, vec![json!({"role": "user", "content": "hi"})], None);
        let task = waiter.await.unwrap().unwrap();
        assert_eq!(task.messages.len(), 1);
    }

    #[tokio::test]
    async fn dropping_the_ticket_withdraws_the_task() {
        let bridge = Bridge::new();
        let ticket = bridge.post(None, vec![], None);
        let id = ticket.id().to_owned();
        drop(ticket);
        assert!(bridge.next(Duration::from_millis(10)).await.is_none());
        assert_eq!(
            bridge.submit(&id, "x".into(), "t".into()),
            Err(SubmitError::Unknown(id))
        );
    }

    #[tokio::test]
    async fn tasks_come_out_oldest_first_with_queue_depth() {
        let bridge = Bridge::new();
        let a = bridge.post(None, vec![], None);
        let _b = bridge.post(None, vec![], None);
        let first = bridge.next(Duration::from_millis(10)).await.unwrap();
        assert_eq!(first.id, a.id());
        assert_eq!(first.queued_behind, 1);
    }
}
