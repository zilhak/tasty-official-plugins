//! 구독자별 제한된 큐에 이벤트를 전달한다. 큐의 여유를 기다리지 않고 포화 시 버린 수를 센다.
//! 연속 누락이 DROP_STREAK_LIMIT에 도달하면 구독을 끊어 재연결을 유도한다.
//! 소비자는 Last-Event-ID로 버퍼에 남은 구간을 재개할 수 있다.

use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde_json::{Value, json};

use crate::record::EventKind;
use crate::sse::frame;

/// 구독자 한 명의 큐에 보관할 수 있는 이벤트 수.
pub const SUBSCRIBER_QUEUE_CAP: usize = 256;

/// 이 횟수만큼 **연속으로** 버려지면 그 구독을 끊는다.
pub const DROP_STREAK_LIMIT: u64 = 64;

/// 구독 파라미터. 어느 것도 주지 않으면 "모든 surface, thinking 제외" 다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SubOptions {
    /// 이 surface 의 이벤트만 받는다. `None` 이면 watch 중인 전부.
    pub filter_surface: Option<u32>,
    /// 사고 블록(`thinking`) 포함 여부. 기본은 **제외** — 노출을 최소로 시작한다.
    pub include_thinking: bool,
}

impl SubOptions {
    fn wants(&self, event: &Published) -> bool {
        if !self.include_thinking && event.kind == EventKind::Thinking {
            return false;
        }
        self.filter_surface.is_none_or(|s| s == event.surface_id)
    }
}

/// 방출 준비가 끝난 이벤트 하나 — 직렬화는 구독자 수와 무관하게 **한 번만** 한다.
#[derive(Debug)]
pub struct Published {
    pub seq: u64,
    pub surface_id: u32,
    pub kind: EventKind,
    /// 완성된 SSE 프레임(`id:` / `event:` / `data:` / 빈 줄).
    pub frame: String,
}

impl Published {
    /// 이벤트 JSON 을 SSE 프레임으로 감싼다. `seq` 가 곧 SSE 의 `id` 라, 소비자가
    /// `Last-Event-ID` 로 돌려주면 그대로 `after_seq` 커서가 된다.
    pub fn new(seq: u64, surface_id: u32, kind: EventKind, payload: &Value) -> Self {
        Self {
            seq,
            surface_id,
            kind,
            frame: frame::encode(seq, kind.as_str(), &payload.to_string()),
        }
    }
}

/// 구독자 하나의 관측값 — `serve_info` 응답용(토큰 등 비밀은 담지 않는다).
#[derive(Debug, Clone)]
pub struct SubStat {
    pub id: u64,
    pub connected_ms: u64,
    pub sent: u64,
    pub dropped: u64,
    pub opts: SubOptions,
}

impl SubStat {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "connected_ms": self.connected_ms,
            "sent": self.sent,
            "dropped": self.dropped,
            "filter_surface": self.opts.filter_surface,
            "thinking": self.opts.include_thinking,
        })
    }
}

#[derive(Debug)]
struct Sub {
    id: u64,
    tx: SyncSender<Arc<Published>>,
    opts: SubOptions,
    sent: u64,
    dropped: u64,
    drop_streak: u64,
    connected_at: Instant,
}

#[derive(Debug, Default)]
struct HubInner {
    next_id: u64,
    subs: Vec<Sub>,
    /// 끊긴 구독자의 것까지 합산한 누적 drop — 개별 통계가 사라져도 총량은 남는다.
    total_dropped: u64,
}

const INNER_WHAT: &str = "the SSE subscriber registry";
static INNER_POISON_REPORTED: AtomicBool = AtomicBool::new(false);

/// 구독자 fan-out 허브. 이벤트 생산자(tail 스레드)와 HTTP 연결 스레드가 공유한다.
#[derive(Debug, Default)]
pub struct SseHub {
    inner: Mutex<HubInner>,
}

impl SseHub {
    /// 모든 허브 접근에서 사용하는 잠금. poison을 알리고 기존 상태를 재사용한다.
    /// 패닉 전의 부분 갱신을 되돌리거나 집계값의 정확성을 보장하지는 않는다.
    fn lock_inner(&self) -> std::sync::MutexGuard<'_, HubInner> {
        tasty_utils::poison::recover_mutex(self.inner.lock(), INNER_WHAT, &INNER_POISON_REPORTED)
    }

    /// 구독자가 하나도 없는가. 생산자가 **직렬화 자체를 건너뛰기 위한** 빠른 검사다.
    pub fn is_idle(&self) -> bool {
        self.lock_inner().subs.is_empty()
    }

    /// 구독을 등록하고 수신단을 돌려준다. 반환값이 drop 되면 구독이 해제된다.
    pub fn subscribe(self: &Arc<Self>, opts: SubOptions) -> Subscription {
        let (tx, rx) = sync_channel(SUBSCRIBER_QUEUE_CAP);
        let mut inner = self.lock_inner();
        inner.next_id += 1;
        let id = inner.next_id;
        inner.subs.push(Sub {
            id,
            tx,
            opts,
            sent: 0,
            dropped: 0,
            drop_streak: 0,
            connected_at: Instant::now(),
        });
        Subscription {
            id,
            rx,
            hub: self.clone(),
        }
    }

    /// 매칭되는 구독자의 큐에 넣는다. 큐의 여유는 기다리지 않는다.
    pub fn publish(&self, event: Arc<Published>) {
        let mut inner = self.lock_inner();
        // 끊긴 구독자의 통계는 목록에서 사라지므로, 그 누적 drop 만 총량으로 옮긴다
        // (살아 있는 구독자의 drop 은 `stats` 가 그때그때 합산한다 — 이중 계산 방지).
        let mut retired = 0u64;
        inner.subs.retain_mut(|sub| {
            let keep = deliver(sub, &event);
            if !keep {
                retired += sub.dropped;
            }
            keep
        });
        inner.total_dropped += retired;
    }

    fn unsubscribe(&self, id: u64) {
        let mut inner = self.lock_inner();
        if let Some(pos) = inner.subs.iter().position(|s| s.id == id) {
            let gone = inner.subs.remove(pos);
            inner.total_dropped += gone.dropped;
        }
    }

    /// 모든 구독의 송신단을 닫는다. 수신단은 남은 큐를 읽은 뒤 종료를 확인한다.
    pub fn close_all(&self) {
        let mut inner = self.lock_inner();
        let carried: u64 = inner.subs.drain(..).map(|s| s.dropped).sum();
        inner.total_dropped += carried;
    }

    /// 현재 구독자 통계 + 끊긴 구독자까지 합산한 누적 drop.
    pub fn stats(&self) -> (Vec<SubStat>, u64) {
        let inner = self.lock_inner();
        let live_dropped: u64 = inner.subs.iter().map(|s| s.dropped).sum();
        let stats = inner
            .subs
            .iter()
            .map(|s| SubStat {
                id: s.id,
                connected_ms: s.connected_at.elapsed().as_millis() as u64,
                sent: s.sent,
                dropped: s.dropped,
                opts: s.opts,
            })
            .collect();
        (stats, inner.total_dropped + live_dropped)
    }
}

/// 구독자 하나에 이벤트를 넣는다. 반환값은 "이 구독을 유지하는가".
fn deliver(sub: &mut Sub, event: &Arc<Published>) -> bool {
    if !sub.opts.wants(event) {
        return true;
    }
    match sub.tx.try_send(event.clone()) {
        Ok(()) => {
            sub.sent += 1;
            sub.drop_streak = 0;
            true
        }
        Err(TrySendError::Full(_)) => {
            sub.dropped += 1;
            sub.drop_streak += 1;
            if sub.drop_streak >= DROP_STREAK_LIMIT {
                tracing::warn!(
                    "agent-stream: subscriber {} dropped {} events in a row — closing it so the consumer reconnects and resumes with Last-Event-ID",
                    sub.id,
                    sub.drop_streak
                );
                return false;
            }
            true
        }
        // 연결 스레드가 이미 끝났다(클라이언트가 끊음).
        Err(TrySendError::Disconnected(_)) => false,
    }
}

/// 살아 있는 구독 하나. drop 되면 허브에서 자동으로 빠진다.
#[derive(Debug)]
pub struct Subscription {
    pub id: u64,
    pub rx: Receiver<Arc<Published>>,
    hub: Arc<SseHub>,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.hub.unsubscribe(self.id);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn event(seq: u64, surface_id: u32, kind: EventKind) -> Arc<Published> {
        Arc::new(Published::new(
            seq,
            surface_id,
            kind,
            &json!({ "seq": seq }),
        ))
    }

    #[test]
    fn a_full_queue_drops_instead_of_blocking_the_producer() {
        let hub = Arc::new(SseHub::default());
        let sub = hub.subscribe(SubOptions::default());

        // 큐를 정확히 가득 채운다 — 여기까지는 전부 들어간다.
        for seq in 1..=SUBSCRIBER_QUEUE_CAP as u64 {
            hub.publish(event(seq, 1, EventKind::Text));
        }
        let (stats, _) = hub.stats();
        assert_eq!(stats[0].sent, SUBSCRIBER_QUEUE_CAP as u64);
        assert_eq!(stats[0].dropped, 0);

        // 가득 찬 큐의 송신은 기다리지 않고 누락 수만 올려야 한다.
        for seq in 1..=10u64 {
            hub.publish(event(1000 + seq, 1, EventKind::Text));
        }
        let (stats, total) = hub.stats();
        assert_eq!(stats[0].sent, SUBSCRIBER_QUEUE_CAP as u64);
        assert_eq!(stats[0].dropped, 10);
        assert_eq!(total, 10);

        // 큐에 담긴 것은 그대로 살아 있다 — drop 은 뒤에서 잘린다(앞을 밀어내지 않는다).
        let first = sub.rx.recv().expect("queued event");
        assert_eq!(first.seq, 1);
    }

    #[test]
    fn a_subscriber_that_keeps_dropping_is_closed_so_the_gap_surfaces_as_a_reconnect() {
        let hub = Arc::new(SseHub::default());
        let sub = hub.subscribe(SubOptions::default());
        for seq in 1..=(SUBSCRIBER_QUEUE_CAP as u64 + DROP_STREAK_LIMIT) {
            hub.publish(event(seq, 1, EventKind::Text));
        }
        let (stats, total) = hub.stats();
        assert!(stats.is_empty(), "the stuck subscriber must be closed");
        assert_eq!(total, DROP_STREAK_LIMIT);
        // 송신단이 사라졌으므로 연결 스레드는 큐를 비운 뒤 즉시 종료를 관측한다.
        while sub.rx.recv().is_ok() {}
    }

    #[test]
    fn thinking_is_excluded_unless_the_subscription_asks_for_it() {
        let hub = Arc::new(SseHub::default());
        let plain = hub.subscribe(SubOptions::default());
        let full = hub.subscribe(SubOptions {
            include_thinking: true,
            ..SubOptions::default()
        });
        hub.publish(event(1, 1, EventKind::Thinking));
        hub.publish(event(2, 1, EventKind::Text));

        assert_eq!(plain.rx.recv().expect("text").seq, 2);
        assert!(plain.rx.try_recv().is_err());
        assert_eq!(full.rx.recv().expect("thinking").seq, 1);
        assert_eq!(full.rx.recv().expect("text").seq, 2);
    }

    #[test]
    fn a_surface_filter_only_admits_that_surface() {
        let hub = Arc::new(SseHub::default());
        let sub = hub.subscribe(SubOptions {
            filter_surface: Some(9),
            ..SubOptions::default()
        });
        hub.publish(event(1, 3, EventKind::Text));
        hub.publish(event(2, 9, EventKind::Text));
        assert_eq!(sub.rx.recv().expect("filtered").seq, 2);
        assert!(sub.rx.try_recv().is_err());
    }

    #[test]
    fn dropping_a_subscription_removes_it_and_idle_goes_back_to_true() {
        let hub = Arc::new(SseHub::default());
        assert!(hub.is_idle());
        let sub = hub.subscribe(SubOptions::default());
        assert!(!hub.is_idle());
        drop(sub);
        assert!(hub.is_idle());
    }

    /// poison 상태에서도 구독자를 유지하고 이벤트를 전달하며 복구를 한 번은 알려야 한다.
    #[test]
    fn a_poisoned_registry_still_delivers_to_live_subscribers() {
        let hub = Arc::new(SseHub::default());
        let sub = hub.subscribe(SubOptions::default());

        // 구독자가 있는 상태에서 잠금을 보유한 스레드에 패닉을 일으킨다.
        let poisoner = Arc::clone(&hub);
        std::thread::spawn(move || {
            let _guard = poisoner.inner.lock().expect("poison 전 잠금");
            panic!("시험을 위해 잠금 보유 중 패닉을 일으킨다");
        })
        .join()
        .expect_err("패닉한 스레드는 Err 로 join 된다");
        assert!(hub.inner.lock().is_err(), "poison 이 실제로 걸려야 한다");

        // 기존 구독자가 있다고 보고해야 한다.
        assert!(
            !hub.is_idle(),
            "poison 상태에서도 기존 구독자가 있음을 알려야 한다"
        );

        // 전달에 실패해도 시험이 멈추지 않도록 수신에 제한 시간을 둔다.
        hub.publish(event(1, 1, EventKind::Text));
        let got = sub
            .rx
            .recv_timeout(Duration::from_secs(5))
            .expect("poison 뒤에도 도달해야 한다");
        assert_eq!(got.seq, 1);

        // 나머지 접근자도 같은 경로를 지난다.
        let (stats, _) = hub.stats();
        assert_eq!(stats.len(), 1);
        hub.close_all();
        assert!(hub.is_idle());

        assert!(
            INNER_POISON_REPORTED.load(std::sync::atomic::Ordering::Relaxed),
            "poison 복구를 한 번은 로그로 알려야 한다"
        );
    }

    #[test]
    fn close_all_ends_every_open_subscription() {
        let hub = Arc::new(SseHub::default());
        let a = hub.subscribe(SubOptions::default());
        let b = hub.subscribe(SubOptions::default());
        hub.close_all();
        assert!(hub.is_idle());
        assert!(a.rx.recv().is_err());
        assert!(b.rx.recv().is_err());
    }
}
