//! `TuiApprover` — the `Approver` impl the TUI plugs into the
//! agent. When the policy returns `Decision::Ask`, the agent loop
//! awaits `approver.approve(...)`; this impl pushes a
//! `UiEvent::ApprovalRequested` into the render channel and waits
//! on a oneshot for the user's keystroke.
//!
//! Session-scoped allow / deny lists short-circuit the round-trip
//! when a fingerprint matches a prior `[a]llow` or `[d]eny`. The
//! fingerprint is `tool::<canonical-json>` so identical repeat
//! requests auto-resolve; anything different prompts again. We
//! deliberately do *not* match by tool alone — a user approving
//! `Bash command="cargo test"` should not silently approve
//! `Bash command="rm -rf /"`.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;

use crate::policy::Approver;
use crate::tui::event::{ApprovalResponse, UiEvent};

/// Slot the render task uses to find the in-flight approval
/// request's response sender. Single-slot because the agent loop
/// dispatches tools sequentially within a turn — we never have
/// two pending approvals at once.
pub type PendingApproval = Arc<Mutex<Option<oneshot::Sender<ApprovalResponse>>>>;

pub struct TuiApprover {
    ui_tx: UnboundedSender<UiEvent>,
    pending: PendingApproval,
    /// Fingerprints the user has flagged "always allow this
    /// session." Lives until the TUI exits.
    allow: Arc<Mutex<HashSet<String>>>,
    /// Counterpart for "always deny this session."
    deny: Arc<Mutex<HashSet<String>>>,
}

impl TuiApprover {
    pub fn new(ui_tx: UnboundedSender<UiEvent>, pending: PendingApproval) -> Self {
        Self {
            ui_tx,
            pending,
            allow: Arc::new(Mutex::new(HashSet::new())),
            deny: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    fn fingerprint(tool: &str, args: &Value) -> String {
        format!("{}::{}", tool, args)
    }
}

#[async_trait]
impl Approver for TuiApprover {
    async fn approve(&self, tool: &str, args: &Value, reason: &str) -> bool {
        let fp = Self::fingerprint(tool, args);
        if self.allow.lock().unwrap().contains(&fp) {
            return true;
        }
        if self.deny.lock().unwrap().contains(&fp) {
            return false;
        }

        let (tx, rx) = oneshot::channel();
        // Stash the sender so the render task can find it on
        // user keystroke. If a previous approval was somehow
        // still pending we drop its sender — the awaiting agent
        // sees the channel close and returns false (deny).
        // Sequential dispatch means this should never happen in
        // practice, but the conservative drop is safer than
        // letting a stale sender steal the next response.
        *self.pending.lock().unwrap() = Some(tx);

        let _ = self.ui_tx.send(UiEvent::ApprovalRequested {
            tool: tool.to_string(),
            args: args.clone(),
            reason: reason.to_string(),
        });

        match rx.await {
            Ok(ApprovalResponse::Yes) => true,
            Ok(ApprovalResponse::No) => false,
            Ok(ApprovalResponse::AlwaysAllow) => {
                self.allow.lock().unwrap().insert(fp);
                true
            }
            Ok(ApprovalResponse::AlwaysDeny) => {
                self.deny.lock().unwrap().insert(fp);
                false
            }
            // Channel closed (TUI shutdown mid-approval). Be
            // conservative: deny.
            Err(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::sync::mpsc;

    fn setup() -> (
        TuiApprover,
        mpsc::UnboundedReceiver<UiEvent>,
        PendingApproval,
    ) {
        let (tx, rx) = mpsc::unbounded_channel();
        let pending = Arc::new(Mutex::new(None));
        let approver = TuiApprover::new(tx, pending.clone());
        (approver, rx, pending)
    }

    #[tokio::test]
    async fn yes_response_returns_true() {
        let (approver, mut rx, pending) = setup();
        let request_fut = tokio::spawn({
            let approver = Arc::new(approver);
            let a = approver.clone();
            async move { a.approve("Edit", &json!({"file_path":"x"}), "edit x").await }
        });
        // Wait for the request to land in the channel.
        let ev = rx.recv().await.expect("approval event");
        assert!(matches!(ev, UiEvent::ApprovalRequested { .. }));
        // Respond.
        let sender = pending.lock().unwrap().take().expect("sender stashed");
        let _ = sender.send(ApprovalResponse::Yes);
        let allowed = request_fut.await.unwrap();
        assert!(allowed);
    }

    #[tokio::test]
    async fn no_response_returns_false() {
        let (approver, mut rx, pending) = setup();
        let request_fut = tokio::spawn({
            let approver = Arc::new(approver);
            let a = approver.clone();
            async move { a.approve("Edit", &json!({"file_path":"x"}), "edit x").await }
        });
        let _ = rx.recv().await;
        let sender = pending.lock().unwrap().take().unwrap();
        let _ = sender.send(ApprovalResponse::No);
        assert!(!request_fut.await.unwrap());
    }

    #[tokio::test]
    async fn always_allow_short_circuits_subsequent_identical_requests() {
        let (approver, mut rx, pending) = setup();
        let approver = Arc::new(approver);

        // First request — user says always-allow.
        let a1 = approver.clone();
        let f1 = tokio::spawn(async move {
            a1.approve("Edit", &json!({"file_path":"x"}), "edit x").await
        });
        let _ = rx.recv().await;
        pending
            .lock()
            .unwrap()
            .take()
            .unwrap()
            .send(ApprovalResponse::AlwaysAllow)
            .unwrap();
        assert!(f1.await.unwrap());

        // Second identical request — should NOT push a UiEvent;
        // the approver auto-resolves from the allow set.
        let a2 = approver.clone();
        let f2 = tokio::spawn(async move {
            a2.approve("Edit", &json!({"file_path":"x"}), "edit x").await
        });
        // The future resolves quickly without us sending anything.
        let resolved = tokio::time::timeout(std::time::Duration::from_millis(100), f2)
            .await
            .expect("should auto-resolve")
            .expect("task ok");
        assert!(resolved);
        // No new event should have arrived in the channel.
        let try_recv = rx.try_recv();
        assert!(
            try_recv.is_err(),
            "expected no new approval request, got {:?}",
            try_recv
        );
    }

    #[tokio::test]
    async fn always_deny_short_circuits_subsequent_identical_requests() {
        let (approver, mut rx, pending) = setup();
        let approver = Arc::new(approver);

        let a1 = approver.clone();
        let f1 = tokio::spawn(async move {
            a1.approve("Bash", &json!({"command":"rm -rf /"}), "danger").await
        });
        let _ = rx.recv().await;
        pending
            .lock()
            .unwrap()
            .take()
            .unwrap()
            .send(ApprovalResponse::AlwaysDeny)
            .unwrap();
        assert!(!f1.await.unwrap());

        let a2 = approver.clone();
        let f2 = tokio::spawn(async move {
            a2.approve("Bash", &json!({"command":"rm -rf /"}), "danger").await
        });
        let denied = tokio::time::timeout(std::time::Duration::from_millis(100), f2)
            .await
            .unwrap()
            .unwrap();
        assert!(!denied);
    }

    #[tokio::test]
    async fn different_args_do_not_share_an_allow_decision() {
        // Allow for `cargo test` does not allow `rm -rf /`.
        let (approver, mut rx, pending) = setup();
        let approver = Arc::new(approver);

        let a1 = approver.clone();
        let f1 = tokio::spawn(async move {
            a1.approve("Bash", &json!({"command":"cargo test"}), "ok").await
        });
        let _ = rx.recv().await;
        pending
            .lock()
            .unwrap()
            .take()
            .unwrap()
            .send(ApprovalResponse::AlwaysAllow)
            .unwrap();
        assert!(f1.await.unwrap());

        // Different command — must prompt again.
        let a2 = approver.clone();
        let f2 = tokio::spawn(async move {
            a2.approve("Bash", &json!({"command":"rm -rf /"}), "boom").await
        });
        // Verify a fresh ApprovalRequested arrives.
        let ev = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv())
            .await
            .expect("event")
            .expect("some");
        assert!(matches!(ev, UiEvent::ApprovalRequested { .. }));
        // Send a No so the future resolves.
        pending
            .lock()
            .unwrap()
            .take()
            .unwrap()
            .send(ApprovalResponse::No)
            .unwrap();
        assert!(!f2.await.unwrap());
    }

    #[tokio::test]
    async fn channel_close_returns_false_conservatively() {
        let (approver, mut rx, pending) = setup();
        let approver = Arc::new(approver);
        let a1 = approver.clone();
        let f1 = tokio::spawn(async move {
            a1.approve("Edit", &json!({"file_path":"x"}), "edit x").await
        });
        let _ = rx.recv().await;
        // Drop the sender without sending. This simulates a TUI
        // shutdown mid-approval.
        drop(pending.lock().unwrap().take().unwrap());
        let result = f1.await.unwrap();
        assert!(!result, "channel close must default to deny");
    }
}
