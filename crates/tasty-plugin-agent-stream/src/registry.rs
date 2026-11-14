//! 추적 대상·이벤트 버퍼와 재시작용 스냅샷.
//! 재시작하면 파일 끝이 아니라 마지막으로 저장한 offset부터 읽어 중복을 허용한다.
//! 실행 중에도 최근 uuid만 기억하므로 오래된 기록이나 uuid 없는 기록은 다시 나올 수 있다.
//! 이벤트 버퍼와 열린 턴은 저장하지 않는다. 따라서 소비자에게 모든 이벤트가
//! 적어도 한 번 전달된다는 보장은 아니며, 버퍼에서 사라진 구간은 SSE 재개 시 gap으로 알린다.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::record::{
    EventKind, REASON_REWATCHED, REASON_SESSION_ENDED, REASON_TURN_TIMEOUT, StreamEvent,
};
use crate::sse::ServeConfig;
use crate::sse::hub::{Published, SseHub, SubOptions};
use crate::tail::TailState;

/// 이벤트 버퍼 상한. 넘치면 가장 오래된 것부터 버리고 `dropped` 로 알린다.
pub(crate) const EVENT_BUFFER_CAP: usize = 4096;

/// 추적 대상별로 중복 확인에 사용할 최근 uuid 수.
const DEDUPE_CAP: usize = 4096;

/// 영속 스냅샷 포맷 버전. 형식이 바뀌면 올려서 옛 파일을 무시한다.
const SNAPSHOT_VERSION: u64 = 1;

/// correlation 턴의 기본 비활동 타임아웃(초). `turn_start` 가 `timeout_secs` 로
/// 덮어쓸 수 있다.
pub(crate) const DEFAULT_TURN_TIMEOUT_SECS: u64 = 600;
/// `timeout_secs` 로 받을 수 있는 범위. 너무 짧으면 정상 턴을 끊고, 너무 길면 막힌 턴이
/// 오래 남아 후속 요청을 막는다.
pub(crate) const MIN_TURN_TIMEOUT_SECS: u64 = 10;
pub(crate) const MAX_TURN_TIMEOUT_SECS: u64 = 86_400;

/// surface 이벤트에 request_id를 붙이는 열린 턴.
/// turn_end 또는 비활성 시간 초과로 닫는다.
#[derive(Debug)]
struct TurnState {
    request_id: String,
    /// 이 턴에서 이벤트가 마지막으로 나온 시각. 타임아웃 sweep 의 기준.
    last_activity: Instant,
    /// 이 턴의 비활동 타임아웃. `turn_start` 인자 또는 기본값.
    timeout: Duration,
}

/// `turn_start` 가 거부되는 이유.
#[derive(Debug, PartialEq, Eq)]
pub enum TurnError {
    /// 대상 surface 가 watch 중이 아니다 — 태깅할 이벤트가 애초에 나오지 않는다.
    NotWatched,
    /// 이미 열린 턴이 있다. 앞 턴이 끝나기 전에는 새 턴을 열지 않는다.
    AlreadyOpen { request_id: String },
}

/// 최근 본 레코드 uuid 를 유한 개수만 기억하는 FIFO 집합.
#[derive(Debug, Default)]
struct DedupeCache {
    seen: HashSet<String>,
    order: VecDeque<String>,
}

impl DedupeCache {
    /// 기억하고 있는 uuid면 false, 그 외에는 기록하고 true를 반환한다.
    fn insert(&mut self, uuid: &str) -> bool {
        if !self.seen.insert(uuid.to_string()) {
            return false;
        }
        self.order.push_back(uuid.to_string());
        if self.order.len() > DEDUPE_CAP
            && let Some(evicted) = self.order.pop_front()
        {
            self.seen.remove(&evicted);
        }
        true
    }
}

/// tail 중인 대상 하나.
#[derive(Debug)]
pub struct Watch {
    pub surface_id: u32,
    pub session_id: String,
    pub transcript: PathBuf,
    /// tail 진행 상태. pump 가 파일 I/O 동안 레지스트리 락 **밖으로 꺼내가면** 잠시
    /// `None` 이다([`StreamRegistry::check_out`]).
    tail: Option<TailState>,
    /// 마지막으로 관측된 읽기 위치. `tail` 이 꺼내져 있는 동안에도 `list`/스냅샷이
    /// 참조해야 하므로 별도로 들고 있는다.
    offset: u64,
    /// 대상 변경 시 이 레지스트리가 새로 발급하는 번호. 꺼내간 tail이 아직 유효한지 확인한다.
    generation: u64,
    dedupe: DedupeCache,
}

impl Watch {
    /// 레코드 uuid 기준 중복 판정. uuid 가 없는 레코드는 중복 제거 대상이 아니다
    /// (식별자가 없어 같음을 주장할 수 없다 — 통과시킨다).
    pub fn accept_record(&mut self, uuid: Option<&str>) -> bool {
        match uuid {
            Some(u) => self.dedupe.insert(u),
            None => true,
        }
    }

    /// 마지막으로 관측된 읽기 위치.
    pub fn offset(&self) -> u64 {
        self.offset
    }

    fn to_json(&self, status: &str) -> Value {
        json!({
            "surface_id": self.surface_id,
            "session_id": self.session_id,
            "transcript": self.transcript.to_string_lossy(),
            "offset": self.offset,
            "status": status,
        })
    }
}

/// 파일 I/O 동안 잠금 밖으로 꺼낸 tail 상태. IPC가 파일 읽기를 기다리지 않게 한다.
#[derive(Debug)]
pub struct TailCheckout {
    pub surface_id: u32,
    pub session_id: String,
    pub transcript: PathBuf,
    pub tail: TailState,
    generation: u64,
}

/// 버퍼에 남아 있는 이벤트를 재전송한다.
/// 커서가 오래돼 이미 사라진 구간은 gap으로 알려 소비자가 누락을 알 수 있게 한다.
#[derive(Debug, Default)]
pub struct Replay {
    /// 재전송할 수 없는 `(첫 seq, 마지막 seq)` 구간. 없으면 `None`.
    pub gap: Option<(u64, u64)>,
    pub events: Vec<Arc<Published>>,
}

/// 이벤트와 수집 순서 번호. 번호는 스냅샷 저장 시 함께 남긴다.
#[derive(Debug)]
struct BufferedEvent {
    seq: u64,
    surface_id: u32,
    session_id: String,
    /// 이 이벤트가 나올 때 그 surface 에 열려 있던 correlation 턴의 `request_id`.
    /// 턴 밖에서 나온 이벤트는 `None`.
    request_id: Option<String>,
    event: StreamEvent,
}

impl BufferedEvent {
    fn to_json(&self) -> Value {
        event_json(
            self.seq,
            self.surface_id,
            &self.session_id,
            self.request_id.as_deref(),
            &self.event,
        )
    }
}

/// poll과 SSE가 공유하는 이벤트 JSON.
/// 열린 턴의 이벤트에는 호출자가 준 request_id를 붙이고, 턴 밖에서는 필드를 생략한다.
pub(crate) fn event_json(
    seq: u64,
    surface_id: u32,
    session_id: &str,
    request_id: Option<&str>,
    event: &StreamEvent,
) -> Value {
    let mut value = event.to_json();
    let map = value.as_object_mut().expect("StreamEvent::to_json object");
    map.insert("seq".into(), Value::from(seq));
    map.insert("surface_id".into(), Value::from(surface_id));
    map.insert("session_id".into(), Value::from(session_id.to_string()));
    if let Some(id) = request_id {
        map.insert("request_id".into(), Value::from(id.to_string()));
    }
    value
}

/// plugin 전체의 스트림 상태. IPC 핸들러(worker 스레드)와 tail 루프가 공유한다.
#[derive(Debug, Default)]
pub struct StreamRegistry {
    watches: Vec<Watch>,
    events: VecDeque<BufferedEvent>,
    next_seq: u64,
    dropped: u64,
    /// 스냅샷 저장 위치. `TASTY_PLUGIN_DATA_DIR` 미주입 시 `None` — 영속화를 건너뛴다
    /// (조용히 다른 경로에 쓰지 않는다).
    snapshot_path: Option<PathBuf>,
    /// 마지막 저장 이후 offset/대상이 바뀌었는가.
    dirty: bool,
    /// [`Watch::generation`] 발급기. 재사용되지 않아야 꺼내간 tail 의 되돌리기 판정이
    /// 정확하다(0 은 "아직 발급 안 됨" 이므로 실제 발급은 1 부터).
    next_generation: u64,
    /// surface 별로 열려 있는 correlation 턴. `turn_start` 가 넣고, 그 surface 의
    /// `turn_end` 이벤트(어느 경로에서 왔든)가 뺀다. 재시작으로 사라지는 **휘발 상태**라
    /// 스냅샷에 남기지 않는다 — 재시작 시점의 in-flight 턴은 복구 대상이 아니다.
    turns: HashMap<u32, TurnState>,
    /// SSE 구독자에게 이벤트를 전달하는 허브.
    hub: Arc<SseHub>,
    /// 재시작 후 같은 주소로 열기 위해 저장하는 SSE 설정.
    serve: Option<ServeConfig>,
}

impl StreamRegistry {
    pub fn new(data_dir: Option<&Path>) -> Self {
        Self {
            snapshot_path: data_dir.map(|d| d.join("watches.json")),
            ..Self::default()
        }
    }

    #[cfg(test)]
    pub fn is_watched(&self, surface_id: u32) -> bool {
        self.watches.iter().any(|w| w.surface_id == surface_id)
    }

    /// 대상을 등록한다. 기존 등록을 교체하면 그 턴을 stream:rewatched로 닫고 true를 반환한다.
    pub fn insert(&mut self, mut watch: Watch) -> bool {
        self.next_generation += 1;
        watch.generation = self.next_generation;
        let replaced = match self
            .watches
            .iter()
            .position(|w| w.surface_id == watch.surface_id)
        {
            Some(pos) => {
                let previous = self.watches.remove(pos);
                self.push_event(
                    previous.surface_id,
                    &previous.session_id,
                    StreamEvent::turn_end(REASON_REWATCHED),
                );
                true
            }
            None => false,
        };
        self.watches.push(watch);
        self.watches.sort_by_key(|w| w.surface_id);
        self.dirty = true;
        replaced
    }

    /// tail 상태를 락 밖으로 꺼낸다 — 파일 I/O 를 레지스트리 락 안에서 하지 않기 위한 것.
    /// 대상이 없거나 이미 꺼내져 있으면 `None`.
    pub fn check_out(&mut self, surface_id: u32) -> Option<TailCheckout> {
        let watch = self
            .watches
            .iter_mut()
            .find(|w| w.surface_id == surface_id)?;
        let tail = watch.tail.take()?;
        Some(TailCheckout {
            surface_id,
            session_id: watch.session_id.clone(),
            transcript: watch.transcript.clone(),
            generation: watch.generation,
            tail,
        })
    }

    /// 꺼내간 tail 을 되돌린다. 그 사이 대상이 바뀌었으면(unwatch · 세션 교체 · 재-watch)
    /// 되돌리지 않고 버린다 — 그 tail 은 더 이상 존재하지 않는 대상의 진행 상태다.
    /// 되돌리는 데 성공하면 `true`.
    pub fn check_in(&mut self, checkout: TailCheckout) -> bool {
        let Some(watch) = self
            .watches
            .iter_mut()
            .find(|w| w.surface_id == checkout.surface_id)
        else {
            return false;
        };
        if watch.generation != checkout.generation {
            return false;
        }
        watch.offset = checkout.tail.offset();
        watch.tail = Some(checkout.tail);
        true
    }

    /// 미해결이던 transcript 경로를 확정한다. `expect_session` 이 지금 대상의 세션과
    /// 다르면(그 사이 세션이 바뀌었으면) 아무 것도 하지 않는다. 반영했으면 `true`.
    pub fn set_transcript(
        &mut self,
        surface_id: u32,
        expect_session: &str,
        transcript: PathBuf,
    ) -> bool {
        self.next_generation += 1;
        let generation = self.next_generation;
        let Some(watch) = self.watches.iter_mut().find(|w| w.surface_id == surface_id) else {
            return false;
        };
        if watch.session_id != expect_session || watch.transcript == transcript {
            return false;
        }
        watch.transcript = transcript;
        // 새로 찾은 파일은 처음부터 읽는다 — 이 세션의 내용 전부가 대상이다.
        watch.tail = Some(TailState::resume_at(&watch.transcript, 0));
        watch.offset = 0;
        watch.generation = generation;
        self.dirty = true;
        true
    }

    /// 대상을 해제하고 종료 이벤트를 남긴다. 등록돼 있지 않았으면 `false`.
    pub fn remove(&mut self, surface_id: u32, reason: &str) -> bool {
        let Some(pos) = self.watches.iter().position(|w| w.surface_id == surface_id) else {
            return false;
        };
        let watch = self.watches.remove(pos);
        self.push_event(surface_id, &watch.session_id, StreamEvent::turn_end(reason));
        self.dirty = true;
        true
    }

    /// tail 루프가 순회할 대상 목록 (surface_id, session_id, 경로).
    pub fn targets(&self) -> Vec<(u32, String, PathBuf)> {
        self.watches
            .iter()
            .map(|w| (w.surface_id, w.session_id.clone(), w.transcript.clone()))
            .collect()
    }

    pub fn watch_mut(&mut self, surface_id: u32) -> Option<&mut Watch> {
        self.watches.iter_mut().find(|w| w.surface_id == surface_id)
    }

    /// 세션이 바뀌면 이전 턴을 닫고 읽을 대상을 교체한다.
    pub fn switch_session(&mut self, surface_id: u32, session_id: String, transcript: PathBuf) {
        self.next_generation += 1;
        let generation = self.next_generation;
        let Some(watch) = self.watch_mut(surface_id) else {
            return;
        };
        let previous = std::mem::replace(&mut watch.session_id, session_id);
        watch.transcript = transcript;
        // 새 세션 파일은 처음부터 읽는다 — 백로그가 곧 그 세션의 전부다.
        watch.tail = Some(TailState::resume_at(&watch.transcript, 0));
        watch.offset = 0;
        watch.generation = generation;
        watch.dedupe = DedupeCache::default();
        self.push_event(
            surface_id,
            &previous,
            StreamEvent::turn_end(REASON_SESSION_ENDED),
        );
        self.dirty = true;
    }

    /// 이벤트를 버퍼에 넣고 상한을 넘으면 오래된 것부터 뺀다.
    /// 열린 턴이 있으면 request_id와 활동 시각을 반영하며 turn_end는 그 턴을 닫는다.
    pub fn push_event(&mut self, surface_id: u32, session_id: &str, event: StreamEvent) {
        self.next_seq += 1;
        // 다음 저장에서 이벤트 순서 번호도 갱신한다.
        self.dirty = true;
        let request_id = self.turns.get(&surface_id).map(|t| t.request_id.clone());
        if let Some(turn) = self.turns.get_mut(&surface_id) {
            turn.last_activity = Instant::now();
        }
        let closes_turn = event.kind == EventKind::TurnEnd;
        self.publish_to_subscribers(
            self.next_seq,
            surface_id,
            session_id,
            request_id.as_deref(),
            &event,
        );
        self.events.push_back(BufferedEvent {
            seq: self.next_seq,
            surface_id,
            session_id: session_id.to_string(),
            request_id,
            event,
        });
        while self.events.len() > EVENT_BUFFER_CAP {
            self.events.pop_front();
            self.dropped += 1;
        }
        if closes_turn {
            self.turns.remove(&surface_id);
        }
    }

    /// 추적 중인 surface에 턴을 연다. 이미 열려 있으면 거절해 request_id가 섞이지 않게 한다.
    pub fn start_turn(
        &mut self,
        surface_id: u32,
        request_id: String,
        timeout: Duration,
    ) -> Result<(), TurnError> {
        if !self.watches.iter().any(|w| w.surface_id == surface_id) {
            return Err(TurnError::NotWatched);
        }
        if let Some(existing) = self.turns.get(&surface_id) {
            return Err(TurnError::AlreadyOpen {
                request_id: existing.request_id.clone(),
            });
        }
        self.turns.insert(
            surface_id,
            TurnState {
                request_id,
                last_activity: Instant::now(),
                timeout,
            },
        );
        Ok(())
    }

    /// 활동이 오래 없는 턴을 stream:turn_timeout으로 닫고 기존 request_id를 붙여 알린다.
    pub fn sweep_stale_turns(&mut self, now: Instant) {
        let stale: Vec<u32> = self
            .turns
            .iter()
            .filter(|(_, t)| now.saturating_duration_since(t.last_activity) >= t.timeout)
            .map(|(surface_id, _)| *surface_id)
            .collect();
        for surface_id in stale {
            let session_id = self
                .watches
                .iter()
                .find(|w| w.surface_id == surface_id)
                .map(|w| w.session_id.clone())
                .unwrap_or_default();
            self.push_event(
                surface_id,
                &session_id,
                StreamEvent::turn_end(REASON_TURN_TIMEOUT),
            );
        }
    }

    #[cfg(test)]
    pub fn has_open_turn(&self, surface_id: u32) -> bool {
        self.turns.contains_key(&surface_id)
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// 구독자 fan-out 핸들. SSE 서버가 같은 허브를 들고 구독을 등록한다.
    pub fn hub(&self) -> Arc<SseHub> {
        self.hub.clone()
    }

    /// SSE 서버 기동 설정(스냅샷에 남는 값).
    pub fn serve_config(&self) -> Option<ServeConfig> {
        self.serve.clone()
    }

    pub fn set_serve_config(&mut self, config: Option<ServeConfig>) {
        if self.serve != config {
            self.serve = config;
            self.dirty = true;
        }
    }

    /// 구독자가 있을 때만 이벤트를 JSON으로 만들고 전달한다.
    fn publish_to_subscribers(
        &self,
        seq: u64,
        surface_id: u32,
        session_id: &str,
        request_id: Option<&str>,
        event: &StreamEvent,
    ) {
        if self.hub.is_idle() {
            return;
        }
        let payload = event_json(seq, surface_id, session_id, request_id, event);
        self.hub.publish(Arc::new(Published::new(
            seq, surface_id, event.kind, &payload,
        )));
    }

    /// SSE 재개에 poll과 같은 이벤트 버퍼를 사용한다.
    /// 커서보다 뒤의 이벤트와 이미 사라져 돌려줄 수 없는 gap을 반환한다.
    pub fn replay_after(&self, after_seq: u64, opts: SubOptions) -> Replay {
        // 버퍼에 남아 있는 가장 오래된 seq. 비어 있으면 "아무 것도 남지 않았다" 는 뜻이라
        // 다음에 발급될 번호를 첫 가용 번호로 본다.
        let first_available = self
            .events
            .front()
            .map(|e| e.seq)
            .unwrap_or(self.next_seq + 1);
        let gap = (after_seq + 1 < first_available).then(|| (after_seq + 1, first_available - 1));
        let events = self
            .events
            .iter()
            .filter(|e| e.seq > after_seq)
            .filter(|e| opts.include_thinking || e.event.kind != EventKind::Thinking)
            .filter(|e| opts.filter_surface.is_none_or(|s| s == e.surface_id))
            .map(|e| {
                Arc::new(Published::new(
                    e.seq,
                    e.surface_id,
                    e.event.kind,
                    &e.to_json(),
                ))
            })
            .collect();
        Replay { gap, events }
    }

    /// `agent_stream.list` 응답. 파일 존재 여부를 status 로 함께 노출한다.
    pub fn list_json(&self) -> Value {
        let watches: Vec<Value> = self
            .watches
            .iter()
            .map(|w| {
                let status = if w.transcript.is_file() {
                    "tailing"
                } else {
                    "awaiting_transcript"
                };
                w.to_json(status)
            })
            .collect();
        json!({ "watches": watches })
    }

    /// `agent_stream.poll` 응답. seq 커서 기반의 **비파괴** 읽기 — 여러 소비자가 각자
    /// 커서를 들고 같은 버퍼를 읽을 수 있다.
    pub fn poll_json(&self, filter_surface: Option<u32>, after_seq: u64, limit: usize) -> Value {
        let selected: Vec<&BufferedEvent> = self
            .events
            .iter()
            .filter(|e| e.seq > after_seq)
            .filter(|e| filter_surface.is_none_or(|s| e.surface_id == s))
            .take(limit)
            .collect();
        let next_seq = selected
            .last()
            .map(|e| e.seq)
            .unwrap_or_else(|| after_seq.max(self.oldest_seq().saturating_sub(1)));
        json!({
            "events": selected.iter().map(|e| e.to_json()).collect::<Vec<_>>(),
            "next_seq": next_seq,
            "latest_seq": self.next_seq,
            "dropped": self.dropped,
        })
    }

    fn oldest_seq(&self) -> u64 {
        self.events.front().map(|e| e.seq).unwrap_or(self.next_seq)
    }

    /// 변경된 스냅샷을 저장한다. 실패하면 로그를 남기고 다음 저장 기회를 기다린다.
    pub fn save_if_dirty(&mut self) {
        if !self.dirty {
            return;
        }
        let Some(path) = self.snapshot_path.clone() else {
            self.dirty = false;
            return;
        };
        match write_snapshot(&path, &self.snapshot_payload()) {
            Ok(()) => self.dirty = false,
            Err(e) => tracing::warn!("agent-stream: cannot persist watches to {path:?}: {e}"),
        }
    }

    fn snapshot_payload(&self) -> Value {
        json!({
            "version": SNAPSHOT_VERSION,
            "next_seq": self.next_seq,
            "serve": self.serve.as_ref().map(|c| json!({
                "bind": c.bind,
                "port": c.port,
                "token": c.token,
            })),
            "watches": self.watches.iter().map(|w| json!({
                "surface_id": w.surface_id,
                "session_id": w.session_id,
                "transcript": w.transcript.to_string_lossy(),
                "offset": w.offset,
            })).collect::<Vec<_>>(),
        })
    }

    /// 저장된 대상·offset·순서 번호로 복구한다. 이벤트 버퍼는 복구하지 않는다.
    /// 마지막 저장 이후의 기록은 다시 읽을 수 있으며, 사라진 파일은 tail 루프에서 다시 찾는다.
    pub fn restore(&mut self) {
        let Some(path) = self.snapshot_path.clone() else {
            return;
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            return;
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            tracing::warn!("agent-stream: watch snapshot at {path:?} is not valid JSON — ignored");
            return;
        };
        if value.get("version").and_then(Value::as_u64) != Some(SNAPSHOT_VERSION) {
            tracing::warn!("agent-stream: watch snapshot version mismatch — ignored");
            return;
        }
        self.next_seq = value
            .get("next_seq")
            .and_then(Value::as_u64)
            .unwrap_or(self.next_seq);
        self.serve = value.get("serve").and_then(restore_serve);
        let Some(entries) = value.get("watches").and_then(Value::as_array) else {
            return;
        };
        for entry in entries {
            if let Some(mut watch) = restore_one(entry) {
                self.next_generation += 1;
                watch.generation = self.next_generation;
                self.watches.push(watch);
            }
        }
        self.watches.sort_by_key(|w| w.surface_id);
    }
}

/// 같은 디렉터리의 임시 파일에 쓴 뒤 rename해 쓰다 만 JSON이 목적지에 남는 것을 줄인다.
/// 저장 오류는 호출자가 기록한다.
fn write_snapshot(path: &Path, payload: &Value) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(payload)?;
    let temp = path.with_extension("json.tmp");
    write_private(&temp, &bytes)?;
    match std::fs::rename(&temp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            // 임시 파일 정리 실패가 원래 rename 오류를 가리지 않게 한다.
            if let Err(cleanup) = std::fs::remove_file(&temp) {
                tracing::warn!(
                    "agent-stream: cannot remove stale snapshot temp {temp:?}: {cleanup}"
                );
            }
            Err(e)
        }
    }
}

/// 구독 토큰이 들어 있는 스냅샷을 쓴다. Unix에서는 새 파일과 기존 파일 모두 0600으로 맞춘다.
/// 다른 플랫폼은 std::fs::write를 사용하며 여기서 별도 ACL을 지정하지 않는다.
#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.write_all(bytes)?;
    file.sync_all()
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)
}

/// 저장된 SSE 설정을 검증한다. 형식이나 조건이 맞지 않으면 복원하지 않는다.
fn restore_serve(entry: &Value) -> Option<ServeConfig> {
    let config = ServeConfig {
        bind: entry.get("bind")?.as_str()?.to_string(),
        port: u16::try_from(entry.get("port")?.as_u64()?).ok()?,
        token: entry
            .get("token")
            .and_then(Value::as_str)
            .map(str::to_string),
    };
    config.validate().ok()?;
    Some(config)
}

fn restore_one(entry: &Value) -> Option<Watch> {
    let surface_id = u32::try_from(entry.get("surface_id")?.as_u64()?).ok()?;
    let session_id = entry.get("session_id")?.as_str()?.to_string();
    let transcript = PathBuf::from(entry.get("transcript")?.as_str()?);
    let offset = entry.get("offset").and_then(Value::as_u64).unwrap_or(0);
    Some(Watch {
        surface_id,
        session_id,
        tail: Some(TailState::resume_at(&transcript, offset)),
        transcript,
        offset,
        // 실제 번호는 `restore` 가 레지스트리 발급기로 덮어쓴다.
        generation: 0,
        dedupe: DedupeCache::default(),
    })
}

/// 새 watch 를 만든다. `from_start` 면 파일 처음부터, 아니면 현재 파일 끝부터.
pub(crate) fn new_watch(
    surface_id: u32,
    session_id: String,
    transcript: PathBuf,
    from_start: bool,
) -> Watch {
    let tail = if from_start {
        TailState::resume_at(&transcript, 0)
    } else {
        TailState::at_end(&transcript)
    };
    Watch {
        surface_id,
        session_id,
        transcript,
        offset: tail.offset(),
        tail: Some(tail),
        // 실제 번호는 `StreamRegistry::insert` 가 발급기로 덮어쓴다.
        generation: 0,
        dedupe: DedupeCache::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{EventKind, REASON_UNWATCHED};

    fn text_event(body: &str, uuid: &str) -> StreamEvent {
        let mut ev = StreamEvent::turn_end("placeholder");
        ev.kind = EventKind::Text;
        ev.reason = None;
        ev.text = Some(body.to_string());
        ev.record_uuid = Some(uuid.to_string());
        ev
    }

    #[test]
    fn dedupe_accepts_a_uuid_once_and_always_accepts_uuidless_records() {
        let mut watch = new_watch(1, "s".into(), PathBuf::from("/nope"), false);
        assert!(watch.accept_record(Some("u1")));
        assert!(!watch.accept_record(Some("u1")));
        assert!(watch.accept_record(Some("u2")));
        assert!(watch.accept_record(None));
        assert!(watch.accept_record(None));
    }

    #[test]
    fn poll_is_non_destructive_and_cursor_driven() {
        let mut reg = StreamRegistry::new(None);
        reg.push_event(7, "s", text_event("a", "u1"));
        reg.push_event(7, "s", text_event("b", "u2"));

        let first = reg.poll_json(None, 0, 10);
        assert_eq!(first["events"].as_array().expect("array").len(), 2);
        assert_eq!(first["next_seq"], 2);
        // 같은 커서로 다시 읽으면 같은 결과 — 소비되지 않는다.
        assert_eq!(reg.poll_json(None, 0, 10), first);
        // 커서를 옮기면 그 뒤만.
        let second = reg.poll_json(None, 2, 10);
        assert!(second["events"].as_array().expect("array").is_empty());
        assert_eq!(second["next_seq"], 2);
    }

    #[test]
    fn poll_can_filter_by_surface() {
        let mut reg = StreamRegistry::new(None);
        reg.push_event(1, "s1", text_event("a", "u1"));
        reg.push_event(2, "s2", text_event("b", "u2"));
        let only_two = reg.poll_json(Some(2), 0, 10);
        let events = only_two["events"].as_array().expect("array");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["surface_id"], 2);
        assert_eq!(events[0]["session_id"], "s2");
    }

    #[test]
    fn poll_honours_the_limit() {
        let mut reg = StreamRegistry::new(None);
        for i in 0..5 {
            reg.push_event(1, "s", text_event("x", &format!("u{i}")));
        }
        let page = reg.poll_json(None, 0, 2);
        assert_eq!(page["events"].as_array().expect("array").len(), 2);
        assert_eq!(page["next_seq"], 2);
        assert_eq!(page["latest_seq"], 5);
    }

    #[test]
    fn buffer_overflow_drops_the_oldest_and_reports_it() {
        let mut reg = StreamRegistry::new(None);
        for i in 0..(EVENT_BUFFER_CAP + 3) {
            reg.push_event(1, "s", text_event("x", &format!("u{i}")));
        }
        let page = reg.poll_json(None, 0, EVENT_BUFFER_CAP + 10);
        assert_eq!(page["dropped"], 3);
        assert_eq!(
            page["events"].as_array().expect("array").len(),
            EVENT_BUFFER_CAP
        );
    }

    #[test]
    fn unwatch_emits_a_terminal_event_so_consumers_stop_waiting() {
        let mut reg = StreamRegistry::new(None);
        reg.insert(new_watch(9, "s".into(), PathBuf::from("/nope"), false));
        assert!(reg.remove(9, REASON_UNWATCHED));
        assert!(!reg.remove(9, REASON_UNWATCHED), "second remove is a no-op");

        let page = reg.poll_json(None, 0, 10);
        let events = page["events"].as_array().expect("array");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["kind"], "turn_end");
        assert_eq!(events[0]["reason"], REASON_UNWATCHED);
    }

    #[test]
    fn session_switch_closes_the_old_turn_and_rebinds_the_target() {
        let dir = tempfile::tempdir().expect("tempdir");
        let old = dir.path().join("old.jsonl");
        let new = dir.path().join("new.jsonl");
        std::fs::write(&old, b"{}\n").expect("write");
        std::fs::write(&new, b"{}\n").expect("write");

        let mut reg = StreamRegistry::new(None);
        reg.insert(new_watch(3, "old-session".into(), old, false));
        reg.switch_session(3, "new-session".into(), new.clone());

        let watch = reg.watch_mut(3).expect("watch");
        assert_eq!(watch.session_id, "new-session");
        assert_eq!(watch.transcript, new);
        assert_eq!(watch.offset(), 0, "new session is read from the start");

        let events = reg.poll_json(None, 0, 10);
        let events = events["events"].as_array().expect("array");
        assert_eq!(events[0]["kind"], "turn_end");
        assert_eq!(events[0]["reason"], REASON_SESSION_ENDED);
        assert_eq!(events[0]["session_id"], "old-session");
    }

    #[test]
    fn rewatching_the_same_surface_closes_the_previous_turn() {
        let mut reg = StreamRegistry::new(None);
        assert!(!reg.insert(new_watch(4, "s1".into(), PathBuf::from("/a"), false)));
        assert!(reg.insert(new_watch(4, "s2".into(), PathBuf::from("/b"), false)));

        let events = reg.poll_json(None, 0, 10);
        let events = events["events"].as_array().expect("array");
        assert_eq!(events.len(), 1, "the replaced registration must be closed");
        assert_eq!(events[0]["reason"], REASON_REWATCHED);
        assert_eq!(events[0]["session_id"], "s1");
    }

    #[test]
    fn a_tail_checked_out_across_a_retarget_is_discarded_not_written_back() {
        let mut reg = StreamRegistry::new(None);
        reg.insert(new_watch(9, "s1".into(), PathBuf::from("/a"), false));
        let checkout = reg.check_out(9).expect("checked out");
        assert!(
            reg.check_out(9).is_none(),
            "a tail is checked out only once"
        );

        // 꺼내간 사이 세션이 바뀌면 그 tail 은 옛 대상의 진행 상태다.
        reg.switch_session(9, "s2".into(), PathBuf::from("/b"));
        assert!(!reg.check_in(checkout), "a stale tail must be rejected");
        assert_eq!(reg.watch_mut(9).expect("watch").session_id, "s2");
        assert!(
            reg.check_out(9).is_some(),
            "the new target keeps its own tail"
        );
    }

    #[test]
    fn snapshot_round_trip_resumes_from_the_persisted_offset() {
        let dir = tempfile::tempdir().expect("tempdir");
        let transcript = dir.path().join("t.jsonl");
        std::fs::write(&transcript, b"{\"a\":1}\n{\"b\":2}\n").expect("write");

        let mut reg = StreamRegistry::new(Some(dir.path()));
        reg.insert(new_watch(5, "sess".into(), transcript.clone(), false));
        reg.save_if_dirty();

        let mut restored = StreamRegistry::new(Some(dir.path()));
        restored.restore();
        let watch = restored.watch_mut(5).expect("restored watch");
        assert_eq!(watch.session_id, "sess");
        assert_eq!(watch.transcript, transcript);
        // 저장된 offset인 파일 끝에서 재개한다.
        assert_eq!(watch.offset(), 16);
    }

    #[test]
    fn the_seq_cursor_keeps_increasing_across_a_restart() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut reg = StreamRegistry::new(Some(dir.path()));
        for _ in 0..3 {
            reg.push_event(1, "s", StreamEvent::turn_end("end_turn"));
        }
        reg.save_if_dirty();
        assert_eq!(reg.poll_json(None, 0, 10)["latest_seq"], 3);

        let mut restarted = StreamRegistry::new(Some(dir.path()));
        restarted.restore();
        restarted.push_event(1, "s", StreamEvent::turn_end("end_turn"));
        let page = restarted.poll_json(None, 3, 10);
        let events = page["events"].as_array().expect("array");
        assert_eq!(
            events.len(),
            1,
            "a consumer at after_seq=3 still sees new events"
        );
        assert_eq!(
            events[0]["seq"], 4,
            "seq must not restart at 1 — that would silently swallow events for an existing cursor"
        );
    }

    #[test]
    fn a_corrupt_snapshot_is_ignored_rather_than_fatal() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("watches.json"), b"{not json").expect("write");
        let mut reg = StreamRegistry::new(Some(dir.path()));
        reg.restore();
        assert!(reg.targets().is_empty());
    }

    #[test]
    fn a_snapshot_from_another_format_version_is_ignored() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("watches.json"),
            br#"{"version":999,"watches":[{"surface_id":1,"session_id":"s","transcript":"/x","offset":0}]}"#,
        )
        .expect("write");
        let mut reg = StreamRegistry::new(Some(dir.path()));
        reg.restore();
        assert!(reg.targets().is_empty());
    }

    #[test]
    fn without_a_data_dir_persistence_is_skipped_not_redirected() {
        let mut reg = StreamRegistry::new(None);
        reg.insert(new_watch(1, "s".into(), PathBuf::from("/nope"), false));
        reg.save_if_dirty();
        reg.restore();
        assert_eq!(reg.targets().len(), 1);
    }

    #[test]
    fn list_reports_whether_the_transcript_exists_yet() {
        let dir = tempfile::tempdir().expect("tempdir");
        let present = dir.path().join("here.jsonl");
        std::fs::write(&present, b"{}\n").expect("write");

        let mut reg = StreamRegistry::new(None);
        reg.insert(new_watch(1, "a".into(), present, false));
        reg.insert(new_watch(
            2,
            "b".into(),
            dir.path().join("absent.jsonl"),
            false,
        ));

        let list = reg.list_json();
        let watches = list["watches"].as_array().expect("array");
        assert_eq!(watches[0]["status"], "tailing");
        assert_eq!(watches[1]["status"], "awaiting_transcript");
    }

    #[test]
    fn a_cursor_behind_the_buffer_reports_the_unrecoverable_range() {
        let mut reg = StreamRegistry::new(None);
        for i in 0..(EVENT_BUFFER_CAP + 5) {
            reg.push_event(1, "s", text_event("x", &format!("u{i}")));
        }
        // 앞의 5 개는 상한에 밀려 사라졌다 — seq 1..=5 는 재전송할 수 없다.
        let replay = reg.replay_after(0, SubOptions::default());
        assert_eq!(replay.gap, Some((1, 5)), "빠진 구간을 그대로 알려야 한다");
        assert_eq!(replay.events.len(), EVENT_BUFFER_CAP);
        assert_eq!(replay.events[0].seq, 6);

        // 버퍼 안에 있는 커서는 갭이 아니다.
        let inside = reg.replay_after(10, SubOptions::default());
        assert_eq!(inside.gap, None);
        // 경계: 남아 있는 가장 오래된 것 바로 앞을 가리키면 빠진 것이 없다.
        assert_eq!(reg.replay_after(5, SubOptions::default()).gap, None);
    }

    #[test]
    fn a_fresh_registry_reports_no_gap() {
        let mut reg = StreamRegistry::new(None);
        assert_eq!(reg.replay_after(0, SubOptions::default()).gap, None);
        reg.push_event(1, "s", text_event("a", "u1"));
        assert_eq!(reg.replay_after(0, SubOptions::default()).gap, None);
    }

    #[test]
    fn the_snapshot_round_trips_the_serve_clause_with_the_cursor_and_watches() {
        let dir = tempfile::tempdir().expect("tempdir");
        let transcript = dir.path().join("t.jsonl");
        std::fs::write(&transcript, b"{}\n").expect("write");

        let mut reg = StreamRegistry::new(Some(dir.path()));
        reg.insert(new_watch(9, "sess".into(), transcript.clone(), true));
        reg.push_event(9, "sess", text_event("a", "u1"));
        reg.set_serve_config(Some(ServeConfig {
            bind: "127.0.0.1".into(),
            port: 8787,
            token: Some("secret".into()),
        }));
        reg.save_if_dirty();

        let mut restored = StreamRegistry::new(Some(dir.path()));
        restored.restore();
        let serve = restored
            .serve_config()
            .expect("serve clause survives a restart");
        assert_eq!(serve.bind, "127.0.0.1");
        assert_eq!(serve.port, 8787);
        assert_eq!(serve.token.as_deref(), Some("secret"));
        // 커서는 이어져야 한다 — 재시작 후 1 부터 다시 세면 소비자가 조용히 건너뛴다.
        assert_eq!(restored.poll_json(None, 0, 10)["latest_seq"], 1);
        assert_eq!(
            restored.watch_mut(9).expect("watch survives").session_id,
            "sess"
        );
    }

    #[test]
    fn a_serve_clause_that_would_not_validate_is_not_restored() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("watches.json");
        // 광역 bind + 토큰 없음 — `validate()` 가 거르는 조합이다.
        std::fs::write(
            &path,
            serde_json::to_vec(&json!({
                "version": SNAPSHOT_VERSION,
                "next_seq": 0,
                "watches": [],
                "serve": {"bind": "0.0.0.0", "port": 8787},
            }))
            .expect("encode"),
        )
        .expect("write");

        let mut reg = StreamRegistry::new(Some(dir.path()));
        reg.restore();
        assert!(reg.serve_config().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn the_snapshot_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let mut reg = StreamRegistry::new(Some(dir.path()));
        reg.set_serve_config(Some(ServeConfig {
            bind: "127.0.0.1".into(),
            port: 8787,
            token: Some("secret".into()),
        }));
        reg.save_if_dirty();

        let path = dir.path().join("watches.json");
        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "토큰이 평문으로 들어 있는 파일이다");

        // 이미 존재하는(느슨한) 파일도 다음 저장에서 조여야 한다.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        reg.mark_dirty();
        reg.save_if_dirty();
        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    // ── correlation (turn_start / 태깅 / 종료) ──────────────────────────

    fn watched(surface_id: u32) -> StreamRegistry {
        let mut reg = StreamRegistry::new(None);
        reg.insert(new_watch(
            surface_id,
            "s".into(),
            PathBuf::from("/nope"),
            false,
        ));
        reg
    }

    fn events_of(reg: &StreamRegistry) -> Vec<Value> {
        reg.poll_json(None, 0, 1000)["events"]
            .as_array()
            .expect("array")
            .clone()
    }

    #[test]
    fn a_turn_tags_its_events_and_the_turn_end_closes_it() {
        let mut reg = watched(1);
        reg.start_turn(1, "req-1".into(), Duration::from_secs(600))
            .expect("turn opens on a watched surface");
        reg.push_event(1, "s", text_event("hi", "u1"));
        reg.push_event(1, "s", StreamEvent::turn_end("stop:end_turn"));
        // 턴이 닫힌 뒤에 나온 이벤트(사용자가 직접 입력한 경우 등)는 태그가 없다.
        reg.push_event(1, "s", text_event("after", "u2"));

        let ev = events_of(&reg);
        assert_eq!(ev[0]["request_id"], "req-1", "in-turn text is tagged");
        assert_eq!(ev[1]["request_id"], "req-1", "the turn_end is tagged too");
        assert_eq!(ev[1]["kind"], "turn_end");
        assert!(
            ev[2].get("request_id").is_none(),
            "an event after the turn closed is untagged: {}",
            ev[2]
        );
        assert!(!reg.has_open_turn(1));
    }

    #[test]
    fn out_of_turn_events_carry_no_request_id() {
        let mut reg = watched(1);
        reg.push_event(1, "s", text_event("solo", "u1"));
        assert!(events_of(&reg)[0].get("request_id").is_none());
    }

    #[test]
    fn a_second_turn_start_while_one_is_open_is_rejected() {
        let mut reg = watched(1);
        reg.start_turn(1, "req-1".into(), Duration::from_secs(600))
            .expect("first opens");
        let err = reg
            .start_turn(1, "req-2".into(), Duration::from_secs(600))
            .expect_err("overlap is rejected");
        assert_eq!(
            err,
            TurnError::AlreadyOpen {
                request_id: "req-1".into()
            }
        );
        // 첫 턴은 그대로 열려 있다.
        assert!(reg.has_open_turn(1));
    }

    #[test]
    fn turn_start_on_an_unwatched_surface_is_rejected() {
        let mut reg = StreamRegistry::new(None);
        let err = reg
            .start_turn(9, "req".into(), Duration::from_secs(600))
            .expect_err("no watch, nothing to tag");
        assert_eq!(err, TurnError::NotWatched);
    }

    #[test]
    fn the_same_request_id_can_open_a_new_turn_after_the_previous_one_closed() {
        let mut reg = watched(1);
        reg.start_turn(1, "req-1".into(), Duration::from_secs(600))
            .expect("opens");
        reg.push_event(1, "s", StreamEvent::turn_end("stop:end_turn"));
        assert!(!reg.has_open_turn(1));
        // 앞 턴이 닫혔으므로 같은 id 로 새 턴을 열 수 있다(겹침이 아니다).
        reg.start_turn(1, "req-1".into(), Duration::from_secs(600))
            .expect("reuse after close is allowed");
        assert!(reg.has_open_turn(1));
    }

    #[test]
    fn unwatching_a_surface_closes_its_open_turn_tagged() {
        let mut reg = watched(1);
        reg.start_turn(1, "req-1".into(), Duration::from_secs(600))
            .expect("opens");
        assert!(reg.remove(1, REASON_UNWATCHED));
        let ev = events_of(&reg);
        let last = ev.last().expect("a terminal event");
        assert_eq!(last["kind"], "turn_end");
        assert_eq!(last["reason"], REASON_UNWATCHED);
        assert_eq!(
            last["request_id"], "req-1",
            "the terminal event that closes an open turn is correlated"
        );
        assert!(!reg.has_open_turn(1));
    }

    #[test]
    fn rewatching_a_surface_closes_the_previous_open_turn_tagged() {
        let mut reg = watched(1);
        reg.start_turn(1, "req-1".into(), Duration::from_secs(600))
            .expect("opens");
        // 같은 surface 재-watch → 이전 등록의 턴을 rewatched 로 닫는다.
        reg.insert(new_watch(1, "s2".into(), PathBuf::from("/other"), false));
        let ev = events_of(&reg);
        let rewatch = ev
            .iter()
            .find(|e| e["reason"] == "stream:rewatched")
            .expect("a rewatched turn_end");
        assert_eq!(rewatch["request_id"], "req-1");
        assert!(!reg.has_open_turn(1));
    }

    #[test]
    fn a_stale_turn_is_swept_with_a_timeout_turn_end() {
        let mut reg = watched(1);
        // 타임아웃 0 — start 직후의 어떤 now 로도 즉시 만료된다(막힌 턴: 이벤트가 없다).
        reg.start_turn(1, "req-1".into(), Duration::ZERO)
            .expect("opens");
        reg.sweep_stale_turns(Instant::now());
        let ev = events_of(&reg);
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0]["kind"], "turn_end");
        assert_eq!(ev[0]["reason"], REASON_TURN_TIMEOUT);
        assert_eq!(ev[0]["request_id"], "req-1");
        assert!(!reg.has_open_turn(1), "the swept turn is closed");
    }

    #[test]
    fn a_turn_with_recent_activity_is_not_swept() {
        let mut reg = watched(1);
        reg.start_turn(1, "req-1".into(), Duration::from_secs(600))
            .expect("opens");
        reg.push_event(1, "s", text_event("working", "u1"));
        reg.sweep_stale_turns(Instant::now());
        assert!(reg.has_open_turn(1), "a fresh turn survives the sweep");
    }

    #[test]
    fn the_request_id_rides_the_sse_payload_too() {
        // poll 과 SSE 는 같은 event_json 을 쓰므로, poll 에 실리면 SSE data 에도 실린다.
        let payload = event_json(5, 1, "s", Some("req-9"), &text_event("x", "u1"));
        assert_eq!(payload["request_id"], "req-9");
        let untagged = event_json(6, 1, "s", None, &text_event("y", "u2"));
        assert!(untagged.get("request_id").is_none());
    }
}
