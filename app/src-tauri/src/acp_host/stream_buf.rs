use serde_json::{json, Value};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::Instant;
use uuid::Uuid;

const FLUSH_INTERVAL: Duration = Duration::from_millis(50);
const FLUSH_BYTES: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    Text,
    Thought,
}

impl BlockKind {
    fn event_kind(self) -> &'static str {
        match self {
            BlockKind::Text => "text_delta",
            BlockKind::Thought => "thought_delta",
        }
    }
}

struct ActiveBlock {
    kind: BlockKind,
    turn_key: String,
    message_id: Option<String>,
    content_index: Option<u64>,
    buf: String,
    started_at: Instant,
    emitted_any: bool,
    first_pending_at: Option<Instant>,
}

pub struct StreamCoalescer {
    active: Option<ActiveBlock>,
}

impl StreamCoalescer {
    pub fn new() -> Self {
        Self { active: None }
    }

    #[cfg(test)]
    pub fn has_pending(&self) -> bool {
        self.active.as_ref().is_some_and(|b| !b.buf.is_empty())
    }

    pub fn next_flush_deadline(&self) -> Option<Instant> {
        self.active.as_ref().and_then(|b| {
            if b.buf.is_empty() {
                None
            } else {
                b.first_pending_at.map(|t| t + FLUSH_INTERVAL)
            }
        })
    }

    pub async fn push_text(
        &mut self,
        delta: &str,
        message_id: Option<&str>,
        content_index: Option<u64>,
        sink: &mpsc::Sender<Value>,
        session_id: &str,
    ) {
        self.push(
            BlockKind::Text,
            delta,
            message_id,
            content_index,
            sink,
            session_id,
        )
        .await;
    }

    pub async fn push_thought(
        &mut self,
        delta: &str,
        message_id: Option<&str>,
        content_index: Option<u64>,
        sink: &mpsc::Sender<Value>,
        session_id: &str,
    ) {
        self.push(
            BlockKind::Thought,
            delta,
            message_id,
            content_index,
            sink,
            session_id,
        )
        .await;
    }

    async fn push(
        &mut self,
        kind: BlockKind,
        delta: &str,
        message_id: Option<&str>,
        content_index: Option<u64>,
        sink: &mpsc::Sender<Value>,
        session_id: &str,
    ) {
        if delta.is_empty() {
            return;
        }
        let clean_mid = message_id.map(str::trim).filter(|s| !s.is_empty());
        let same_block = self.active.as_ref().is_some_and(|b| {
            b.kind == kind
                && b.message_id.as_deref() == clean_mid
                && b.content_index == content_index
        });
        if !same_block {
            self.finish_active(sink, session_id).await;
            self.active = Some(ActiveBlock {
                kind,
                turn_key: Uuid::new_v4().to_string(),
                message_id: clean_mid.map(str::to_string),
                content_index,
                buf: String::new(),
                started_at: Instant::now(),
                emitted_any: false,
                first_pending_at: None,
            });
        }

        let Some(ref mut b) = self.active else {
            return;
        };
        b.buf.push_str(delta);

        // 首 token 0 延迟立即刷新；后续增量按 50ms 或 256 字节微批合并
        if !b.emitted_any || b.buf.len() >= FLUSH_BYTES {
            let chunk = std::mem::take(&mut b.buf);
            b.emitted_any = true;
            b.first_pending_at = None;
            let _ = sink
                .send(json!({
                    "type": "session_update",
                    "session_id": session_id,
                    "kind": b.kind.event_kind(),
                    "turn_key": b.turn_key,
                    "delta": chunk,
                    "done": false,
                }))
                .await;
        } else if b.first_pending_at.is_none() {
            b.first_pending_at = Some(Instant::now());
        }
    }

    pub async fn flush_due(&mut self, sink: &mpsc::Sender<Value>, session_id: &str) {
        let Some(ref mut b) = self.active else {
            return;
        };
        if b.buf.is_empty() {
            return;
        }
        if b.first_pending_at
            .is_some_and(|t| t.elapsed() >= FLUSH_INTERVAL)
        {
            let chunk = std::mem::take(&mut b.buf);
            b.emitted_any = true;
            b.first_pending_at = None;
            let _ = sink
                .send(json!({
                    "type": "session_update",
                    "session_id": session_id,
                    "kind": b.kind.event_kind(),
                    "turn_key": b.turn_key,
                    "delta": chunk,
                    "done": false,
                }))
                .await;
        }
    }

    pub async fn finish_active(&mut self, sink: &mpsc::Sender<Value>, session_id: &str) {
        let Some(mut b) = self.active.take() else {
            return;
        };
        if b.buf.is_empty() && !b.emitted_any {
            return;
        }
        let tail = std::mem::take(&mut b.buf);
        let mut ev = json!({
            "type": "session_update",
            "session_id": session_id,
            "kind": b.kind.event_kind(),
            "turn_key": b.turn_key,
            "delta": tail,
            "done": true,
        });
        if b.kind == BlockKind::Thought {
            ev["duration_ms"] = json!(b.started_at.elapsed().as_millis() as u64);
        }
        let _ = sink.send(ev).await;
    }

    pub async fn finish_kind(
        &mut self,
        kind: BlockKind,
        authoritative_text: Option<&str>,
        sink: &mpsc::Sender<Value>,
        session_id: &str,
    ) {
        let matches = self.active.as_ref().is_some_and(|b| b.kind == kind);
        if matches {
            let mut b = self.active.take().unwrap();
            let auth = authoritative_text.filter(|s| !s.is_empty());
            if let Some(full) = auth {
                let mut ev = json!({
                    "type": "session_update",
                    "session_id": session_id,
                    "kind": b.kind.event_kind(),
                    "turn_key": b.turn_key,
                    "text": full,
                    "done": true,
                });
                if b.kind == BlockKind::Thought {
                    ev["duration_ms"] = json!(b.started_at.elapsed().as_millis() as u64);
                }
                let _ = sink.send(ev).await;
            } else if !b.buf.is_empty() || b.emitted_any {
                let tail = std::mem::take(&mut b.buf);
                let mut ev = json!({
                    "type": "session_update",
                    "session_id": session_id,
                    "kind": b.kind.event_kind(),
                    "turn_key": b.turn_key,
                    "delta": tail,
                    "done": true,
                });
                if b.kind == BlockKind::Thought {
                    ev["duration_ms"] = json!(b.started_at.elapsed().as_millis() as u64);
                }
                let _ = sink.send(ev).await;
            }
            return;
        }

        if let Some(full) = authoritative_text.filter(|s| !s.is_empty()) {
            self.finish_active(sink, session_id).await;
            let mut ev = json!({
                "type": "session_update",
                "session_id": session_id,
                "kind": kind.event_kind(),
                "turn_key": Uuid::new_v4().to_string(),
                "text": full,
                "done": true,
            });
            if kind == BlockKind::Thought {
                ev["duration_ms"] = json!(0);
            }
            let _ = sink.send(ev).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn coalescer_flushes_first_chunk_immediately_coalesces_deltas_and_rotates_keys() {
        let (tx, mut rx) = mpsc::channel::<Value>(32);
        let mut c = StreamCoalescer::new();

        // 1. 思考首 token 立即发送
        c.push_thought("Think ", None, Some(0), &tx, "s1").await;
        let e1 = rx.try_recv().unwrap();
        assert_eq!(e1["kind"], "thought_delta");
        assert_eq!(e1["delta"], "Think ");
        assert_eq!(e1["done"], false);
        let k_thought = e1["turn_key"].as_str().unwrap().to_string();

        // 后续小增量先缓冲，等 50ms 到期后合并发出
        c.push_thought("step1 ", None, Some(0), &tx, "s1").await;
        c.push_thought("step2", None, Some(0), &tx, "s1").await;
        assert!(rx.try_recv().is_err());
        assert!(c.has_pending());
        assert!(c.next_flush_deadline().is_some());

        tokio::time::sleep(Duration::from_millis(60)).await;
        c.flush_due(&tx, "s1").await;
        let e2 = rx.try_recv().unwrap();
        assert_eq!(e2["turn_key"], k_thought);
        assert_eq!(e2["delta"], "step1 step2");
        assert_eq!(e2["done"], false);
        assert!(!c.has_pending());

        // 2. 切到正文块时先关闭思考块（带 duration_ms），再以新 turn_key 发正文首包
        c.push_text("Hello", Some("m1"), None, &tx, "s1").await;
        let e3 = rx.try_recv().unwrap();
        assert_eq!(e3["kind"], "thought_delta");
        assert_eq!(e3["turn_key"], k_thought);
        assert_eq!(e3["done"], true);
        assert!(e3["duration_ms"].is_u64());

        let e4 = rx.try_recv().unwrap();
        assert_eq!(e4["kind"], "text_delta");
        assert_eq!(e4["delta"], "Hello");
        assert_eq!(e4["done"], false);
        let k_text1 = e4["turn_key"].as_str().unwrap().to_string();
        assert_ne!(k_text1, k_thought);

        // 3. 大增量超过 256 字节立即触发刷新
        let big = "a".repeat(260);
        c.push_text(&big, Some("m1"), None, &tx, "s1").await;
        let e5 = rx.try_recv().unwrap();
        assert_eq!(e5["turn_key"], k_text1);
        assert_eq!(e5["delta"], big);
        assert_eq!(e5["done"], false);

        // 4. message_id 变化切分新 turn_key
        c.push_text("Next msg", Some("m2"), None, &tx, "s1").await;
        let e6 = rx.try_recv().unwrap();
        assert_eq!(e6["turn_key"], k_text1);
        assert_eq!(e6["done"], true);

        let e7 = rx.try_recv().unwrap();
        assert_eq!(e7["delta"], "Next msg");
        let k_text2 = e7["turn_key"].as_str().unwrap().to_string();
        assert_ne!(k_text2, k_text1);

        // 5. finish_kind 使用权威文本收尾
        c.finish_kind(BlockKind::Text, Some("Next msg final"), &tx, "s1")
            .await;
        let e8 = rx.try_recv().unwrap();
        assert_eq!(e8["turn_key"], k_text2);
        assert_eq!(e8["text"], "Next msg final");
        assert_eq!(e8["done"], true);
    }
}
