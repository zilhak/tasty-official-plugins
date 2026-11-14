//! 수신자와 완료 이벤트의 영속 원장.
//!
//! 이벤트는 `events/<event_id>.json`에 본문과 전달 상태를 함께 둔다. 같은 수신자의 같은
//! 완료 키는 `claims/<receiver_id>/<키 해시>`를 배타 생성해 한 번만 기록한다.
//!
//! 아직 할 일이 남은 이벤트(보내기 전, 보류, 송신 중, 확인 전 unknown)는
//! `pending/<thread>/<기록 시각>-<event_id>` 표지로 색인한다. 송신과 복구는 이 색인만 읽고
//! 지난 이벤트 본문을 훑지 않는다.
//!
//! 기록 순서는 표지 → 이벤트 → claim이다. 어디서 중단돼도 시작 시 복구가 표지에서 이벤트를
//! 찾아 claim을 채우거나, 이미 다른 이벤트가 claim한 중복을 지운다. claim이 비었거나 없는
//! 이벤트를 가리키면 손상으로 보고 남은 이벤트 쪽으로 고친다. 이벤트는 지우지 않는다.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::store;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReceiverKind {
    /// Tasty 세션 helper가 띄운 서버의 thread.
    Owned,
    /// 사용자의 공유 daemon에 로드된 thread.
    External,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct Receiver {
    pub receiver_id: String,
    pub thread: String,
    pub kind: ReceiverKind,
    #[serde(default)]
    pub codex_home: Option<PathBuf>,
    #[serde(default)]
    pub execution: Option<crate::execution::CodexExecution>,
    pub created_at_ms: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DeliveryState {
    /// 기록만 했고 아직 보내지 않았다. 다시 보내도 중복이 아니다.
    Captured,
    /// 부모 TUI가 붙어 있지 않아 다음 연결까지 보류했다.
    Held,
    /// 송신 중. 이 상태로 재시작되면 수락 여부를 알 수 없다.
    Sending,
    /// 서버가 대기열에 넣었다. 부모가 결과를 읽었다는 뜻은 아니다.
    Accepted,
    /// 서버가 거절했다.
    Rejected,
    /// 외부 부모 thread가 서버에 로드돼 있지 않다.
    NotLoaded,
    /// 응답을 잃어 수락 여부를 확정할 수 없다. 자동으로 다시 보내지 않는다.
    Unknown,
}

impl DeliveryState {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Captured => "captured",
            Self::Held => "held",
            Self::Sending => "sending",
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::NotLoaded => "not_loaded",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct Delivery {
    pub state: DeliveryState,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub submission_id: Option<String>,
    #[serde(default)]
    pub error_code: Option<i64>,
    #[serde(default)]
    pub attempts: u32,
    /// unknown을 대기열 조회로 한 번 확인했다.
    #[serde(default)]
    pub unknown_checked: bool,
    /// 보내지 못한 이벤트를 다시 보낼 수 있는 시각(ms). 0이면 바로 보낸다.
    #[serde(default)]
    pub next_attempt_at_ms: u64,
    pub updated_at_ms: u64,
}

/// provider가 보고한 턴 경계. 작업 품질의 성공·실패 판정이 아니다.
pub(crate) const STATUSES: &[&str] = &["turn_ended", "turn_failed", "exited", "interrupted"];
pub(crate) const PROVIDERS: &[&str] = &["codex", "claude"];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct Event {
    pub event_id: String,
    pub receiver_id: String,
    pub thread: String,
    pub completion_key: String,
    pub provider: String,
    pub child_surface: u32,
    pub child_session: Option<String>,
    pub status: String,
    pub body: Option<String>,
    pub body_truncated: bool,
    pub attempt_id: Option<String>,
    pub captured_at_ms: u64,
    pub delivery: Delivery,
    #[serde(default)]
    pub read_count: u64,
    #[serde(default)]
    pub first_read_at_ms: Option<u64>,
}

/// 원장에 기록할 완료 하나.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Completion {
    pub completion_key: String,
    pub provider: String,
    pub child_surface: u32,
    pub child_session: Option<String>,
    pub status: String,
    pub body: Option<String>,
    pub attempt_id: Option<String>,
}

/// 본문 상한(1MiB). 넘으면 문자 경계에서 자르고 표시한다.
pub(crate) const BODY_MAX: usize = 1024 * 1024;

pub(crate) fn truncate_body(body: String) -> (String, bool) {
    if body.len() <= BODY_MAX {
        return (body, false);
    }
    let mut end = BODY_MAX;
    while !body.is_char_boundary(end) {
        end -= 1;
    }
    (body[..end].to_string(), true)
}

pub(super) fn receiver_file(root: &Path, id: &str) -> PathBuf {
    root.join("receivers").join(format!("{id}.json"))
}

pub(crate) fn event_file(root: &Path, id: &str) -> PathBuf {
    root.join("events").join(format!("{id}.json"))
}

fn claim_file(root: &Path, receiver_id: &str, key_file: &str) -> PathBuf {
    root.join("claims").join(receiver_id).join(key_file)
}

fn child_file(root: &Path, surface: u32) -> PathBuf {
    root.join("children").join(format!("{surface}.json"))
}

fn receiver_index_file(
    root: &Path,
    thread: &str,
    kind: ReceiverKind,
    home: Option<&Path>,
) -> PathBuf {
    let identity = format!(
        "{}|{:?}|{}",
        thread.to_ascii_lowercase(),
        kind,
        home.map(|h| h.to_string_lossy().into_owned())
            .unwrap_or_default()
    );
    root.join("receivers")
        .join("index")
        .join(crate::ids::sha256_hex(&identity))
}

/// thread·전송 경로가 같은 수신자를 다시 쓴다. 없거나 색인이 손상됐으면 새로 만든다. 같은
/// 부모를 여러 번 준비해도 수신자가 늘지 않고 중복 판정 범위도 바뀌지 않는다.
pub(crate) fn receiver_for(
    root: &Path,
    thread: &str,
    kind: ReceiverKind,
    codex_home: Option<PathBuf>,
) -> std::io::Result<Receiver> {
    let index = receiver_index_file(root, thread, kind, codex_home.as_deref());
    if let Some(id) = store::read_text(&index)?
        && let Some(existing) = read_receiver(root, id.trim())?
        && existing.thread.eq_ignore_ascii_case(thread)
        && existing.kind == kind
        && existing.codex_home == codex_home
    {
        return Ok(existing);
    }
    let receiver = Receiver {
        receiver_id: crate::ids::new_id()?,
        thread: thread.to_ascii_lowercase(),
        kind,
        codex_home,
        execution: None,
        created_at_ms: store::now_ms(),
    };
    store::write_json(&receiver_file(root, &receiver.receiver_id), &receiver)?;
    store::replace_text(&index, &receiver.receiver_id)?;
    Ok(receiver)
}

pub(crate) fn read_receiver(root: &Path, id: &str) -> std::io::Result<Option<Receiver>> {
    if !crate::ids::is_id(id) {
        return Ok(None);
    }
    store::read_json(&receiver_file(root, id))
}

pub(crate) fn read_event(root: &Path, id: &str) -> std::io::Result<Option<Event>> {
    if !crate::ids::is_id(id) {
        return Ok(None);
    }
    store::read_json(&event_file(root, id))
}

pub(crate) fn write_event(root: &Path, event: &Event) -> std::io::Result<()> {
    store::write_json(&event_file(root, &event.event_id), event)
}

fn pending_dir(root: &Path, thread: &str) -> PathBuf {
    root.join("pending").join(thread.to_ascii_lowercase())
}

fn pending_name(event: &Event) -> String {
    format!("{:016}-{}", event.captured_at_ms, event.event_id)
}

/// 할 일이 남은 이벤트로 색인한다.
fn add_pending(root: &Path, event: &Event) -> std::io::Result<()> {
    let path = pending_dir(root, &event.thread).join(pending_name(event));
    store::create_claim(&path, "").map(|_| ())
}

/// 할 일이 끝난 이벤트를 색인에서 뺀다.
pub(crate) fn remove_pending(root: &Path, event: &Event) -> std::io::Result<()> {
    store::remove_file(&pending_dir(root, &event.thread).join(pending_name(event)))
}

/// 색인에 남은 thread 목록.
pub(crate) fn pending_threads(root: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(root.join("pending")) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|t| crate::ids::is_uuid(t))
        .collect()
}

/// thread의 색인된 이벤트 ID를 기록 순서대로 돌려준다. 본문은 읽지 않는다.
pub(crate) fn pending_ids(root: &Path, thread: &str) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(pending_dir(root, thread)) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with('.'))
        .collect();
    names.sort();
    names
        .into_iter()
        .filter_map(|n| {
            let id = n.split_once('-')?.1;
            crate::ids::is_id(id).then(|| id.to_string())
        })
        .collect()
}

/// claim 파일이 가리키는 상태.
enum ClaimState {
    Missing,
    /// 유효한 이벤트를 가리킨다.
    Event(Box<Event>),
    /// 비었거나 형식이 틀리거나 없는 이벤트를 가리킨다.
    Broken,
}

fn read_claim(root: &Path, claim: &Path) -> std::io::Result<ClaimState> {
    let Some(text) = store::read_text(claim)? else {
        return Ok(ClaimState::Missing);
    };
    Ok(match read_event(root, text.trim())? {
        Some(event) => ClaimState::Event(Box::new(event)),
        None => ClaimState::Broken,
    })
}

/// 완료를 기록한다. 같은 수신자의 같은 완료 키가 이미 있으면 그 이벤트와 true를 돌려준다.
pub(crate) fn capture(
    root: &Path,
    receiver: &Receiver,
    completion: Completion,
) -> std::io::Result<(Event, bool)> {
    let key_file = crate::ids::completion_key_file_name(&completion.completion_key)
        .ok_or_else(|| std::io::Error::other("invalid completion key"))?;
    let claim = claim_file(root, &receiver.receiver_id, &key_file);
    let broken = match read_claim(root, &claim)? {
        ClaimState::Event(existing) => return Ok((*existing, true)),
        ClaimState::Missing => false,
        ClaimState::Broken => true,
    };
    let (body, body_truncated) = match completion.body {
        Some(b) => {
            let (b, t) = truncate_body(b);
            (Some(b), t)
        }
        None => (None, false),
    };
    let now = store::now_ms();
    let event = Event {
        event_id: crate::ids::new_id()?,
        receiver_id: receiver.receiver_id.clone(),
        thread: receiver.thread.clone(),
        completion_key: completion.completion_key,
        provider: completion.provider,
        child_surface: completion.child_surface,
        child_session: completion.child_session,
        status: completion.status,
        body,
        body_truncated,
        attempt_id: completion.attempt_id,
        captured_at_ms: now,
        delivery: Delivery {
            state: DeliveryState::Captured,
            detail: None,
            submission_id: None,
            error_code: None,
            attempts: 0,
            unknown_checked: false,
            next_attempt_at_ms: 0,
            updated_at_ms: now,
        },
        read_count: 0,
        first_read_at_ms: None,
    };
    add_pending(root, &event)?;
    write_event(root, &event)?;
    if broken {
        // 손상된 claim은 원래 이벤트를 찾을 수 없으니 이 이벤트로 고친다.
        store::replace_text(&claim, &event.event_id)?;
        return Ok((event, false));
    }
    if store::create_claim(&claim, &event.event_id)? {
        return Ok((event, false));
    }
    // 다른 기록이 먼저 claim했다. 방금 쓴 이벤트와 표지는 버린다.
    discard(root, &event)?;
    match read_claim(root, &claim)? {
        ClaimState::Event(existing) => Ok((*existing, true)),
        _ => Err(std::io::Error::other("claimed event is missing")),
    }
}

fn discard(root: &Path, event: &Event) -> std::io::Result<()> {
    store::remove_file(&event_file(root, &event.event_id))?;
    remove_pending(root, event)
}

/// 색인된 이벤트의 claim을 고친다. claim이 없으면 만들고, 손상됐으면 이 이벤트로 바꾼다.
/// 다른 유효한 이벤트가 이미 claim했으면 이 이벤트는 중복이라 지운다. 이벤트가 남았으면
/// true다.
pub(crate) fn repair_claim(root: &Path, event: &Event) -> std::io::Result<bool> {
    let Some(key_file) = crate::ids::completion_key_file_name(&event.completion_key) else {
        return Ok(true);
    };
    let claim = claim_file(root, &event.receiver_id, &key_file);
    match read_claim(root, &claim)? {
        ClaimState::Event(other) if other.event_id == event.event_id => Ok(true),
        ClaimState::Event(_) => discard(root, event).map(|_| false),
        ClaimState::Missing => {
            if store::create_claim(&claim, &event.event_id)? {
                return Ok(true);
            }
            // 그 사이에 claim이 생겼으면 다시 판단한다.
            repair_claim(root, event)
        }
        ClaimState::Broken => store::replace_text(&claim, &event.event_id).map(|_| true),
    }
}

/// 종료 상태 이벤트의 보존 기간. 그 뒤에는 `tasty codex result`로 읽을 수 없다.
pub(crate) const RETENTION: std::time::Duration = std::time::Duration::from_secs(30 * 24 * 60 * 60);

/// 보존 기간이 지난 이벤트와 claim을 지운다. 파일 수정 시각만 보고 본문은 읽지 않는다.
/// 색인에 남은 이벤트는 지우지 않는다.
fn claim_points_at_an_event(root: &Path, claim: &Path) -> bool {
    match store::read_text(claim) {
        Ok(Some(id)) => matches!(read_event(root, id.trim()), Ok(Some(_))),
        // 읽지 못하면 지우지 않는다.
        Ok(None) => false,
        Err(_) => true,
    }
}

pub(crate) fn prune(root: &Path, now: std::time::SystemTime) {
    let mut keep = std::collections::HashSet::new();
    for thread in pending_threads(root) {
        keep.extend(pending_ids(root, &thread));
    }
    prune_events(root, &keep, now);
    prune_claims(root, now);
    prune_ended_children(root);
    prune_attach_routes(root, now);
}

/// 보존 기간이 지난 파일인가.
fn is_old(path: &Path, now: std::time::SystemTime) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| now.duration_since(t).ok())
        .is_some_and(|age| age > RETENTION)
}

/// `keep`에 없는 오래된 이벤트를 지운다.
fn prune_events(root: &Path, keep: &std::collections::HashSet<String>, now: std::time::SystemTime) {
    if let Ok(entries) = std::fs::read_dir(root.join("events")) {
        for entry in entries.filter_map(Result::ok) {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(id) = name.strip_suffix(".json") else {
                continue;
            };
            if !keep.contains(id)
                && is_old(&entry.path(), now)
                && let Err(e) = store::remove_file(&entry.path())
            {
                tracing::warn!("codex ledger: prune event {id}: {e}");
            }
        }
    }
}

/// 지운 이벤트를 가리키는 오래된 claim을 지운다.
fn prune_claims(root: &Path, now: std::time::SystemTime) {
    let Ok(receivers) = std::fs::read_dir(root.join("claims")) else {
        return;
    };
    for dir in receivers.filter_map(Result::ok) {
        let Ok(claims) = std::fs::read_dir(dir.path()) else {
            continue;
        };
        for claim in claims.filter_map(Result::ok) {
            // 이벤트가 남아 있는 claim은 그 키의 중복 판정 근거라 나이와 관계없이 둔다. 지운
            // 이벤트를 가리키는 오래된 claim만 지운다.
            if is_old(&claim.path(), now)
                && !claim_points_at_an_event(root, &claim.path())
                && let Err(e) = store::remove_file(&claim.path())
            {
                tracing::warn!("codex ledger: prune claim: {e}");
            }
        }
    }
}

/// 자식 surface의 결과 반환 경로. 파일이 없으면 반환하지 않는다.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct ChildReturn {
    pub receiver_id: String,
    /// 호스트가 부모 아래에서 매긴 자식 번호. index로 지정하는 명령의 workspace를 찾는 데 쓴다.
    #[serde(default)]
    pub child_index: Option<u32>,
    /// attach 원격 출처 부모(`attach:o<n>`). 있으면 결과는 원장이 아니라 그 부모에게 돌려보내고
    /// `receiver_id` 자리에는 client 가 그 지시에 준 route 토큰을 둔다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attach_parent: Option<String>,
    /// 원격 자식을 닫은 시각(ms). 서버에 남은 결과가 늦게 와도 기록하도록 [`ENDED_KEPT`] 동안 경로를 둔다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at_ms: Option<u64>,
}

fn attach_route_file(root: &Path, token: &str) -> PathBuf {
    root.join("attach-routes").join(format!("{token}.json"))
}

/// client: 원격 자식에게 보낸 지시 하나의 route 토큰과 그때의 수신처. 수신처는 Codex 대화(`receiver_id`)이거나
/// 물리 surface 부모(`parent_surface`, 발급 때 그 surface 의 Claude 세션 `parent_session`)다.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct AttachRoute {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub receiver_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_surface: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session: Option<String>,
}

/// client: 원격 자식에게 보내는 spawn·tell 하나의 route 토큰을 발급한다. 서버는 그 지시로 시작한 턴의 결과에 토큰을
/// 실어 돌려주고, 이 기록으로 그때의 수신처를 찾는다. 경로를 풀거나 다시 묶어도 지우지 않으며 [`RETENTION`] 이
/// 지나면 [`prune`] 이 지운다. 서버에는 토큰만 보낸다.
pub(crate) fn issue_attach_route(root: &Path, to: &AttachRoute) -> std::io::Result<String> {
    let token = tasty_plugin_agent_common::remote_attach::new_route_token();
    store::write_json(&attach_route_file(root, &token), to)?;
    Ok(token)
}

/// Codex 대화 수신처의 route 기록.
pub(crate) fn codex_route(receiver_id: &str) -> AttachRoute {
    AttachRoute {
        receiver_id: receiver_id.into(),
        parent_surface: None,
        parent_session: None,
    }
}

/// route 토큰의 기록. 형식이 틀리거나 기록이 없으면 None 이다.
pub(crate) fn attach_route(root: &Path, token: &str) -> std::io::Result<Option<AttachRoute>> {
    if !tasty_plugin_agent_common::remote_attach::is_route_token(token) {
        return Ok(None);
    }
    store::read_json::<AttachRoute>(&attach_route_file(root, token))
}

fn prune_attach_routes(root: &Path, now: std::time::SystemTime) {
    let Ok(entries) = std::fs::read_dir(root.join("attach-routes")) else {
        return;
    };
    for e in entries.filter_map(Result::ok) {
        if is_old(&e.path(), now)
            && let Err(err) = store::remove_file(&e.path())
        {
            tracing::warn!("codex ledger: prune attach route: {err}");
        }
    }
}

/// 닫은 원격 자식의 반환 경로를 남겨 두는 기간.
pub(crate) const ENDED_KEPT: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// 원격 자식을 닫았다. 경로는 [`ENDED_KEPT`] 동안 남겨 늦은 결과를 받고 [`prune`] 이 지운다.
pub(crate) fn end_child_receiver(root: &Path, surface: u32) -> std::io::Result<()> {
    match child_receiver(root, surface)? {
        Some(mut r) if r.ended_at_ms.is_none() => {
            r.ended_at_ms = Some(store::now_ms());
            write_child_return(root, surface, &r)
        }
        _ => Ok(()),
    }
}

fn prune_ended_children(root: &Path) {
    let limit = ENDED_KEPT.as_millis() as u64;
    let now = store::now_ms();
    for (surface, r) in child_records(root) {
        if r.ended_at_ms
            .is_some_and(|at| now.saturating_sub(at) > limit)
            && let Err(e) = clear_child_receiver(root, surface)
        {
            tracing::warn!("codex ledger: prune ended child {surface}: {e}");
        }
    }
}

/// Codex 자식 surface의 결과를 받을 수신자를 기록한다.
pub(crate) fn set_child_receiver(
    root: &Path,
    surface: u32,
    receiver_id: &str,
    child_index: Option<u32>,
) -> std::io::Result<()> {
    store::write_json(
        &child_file(root, surface),
        &ChildReturn {
            receiver_id: receiver_id.into(),
            child_index,
            attach_parent: None,
            ended_at_ms: None,
        },
    )
}

/// 반환 기록을 그대로 쓴다. 되돌리기처럼 기록 전체를 복원할 때 쓴다.
pub(crate) fn write_child_return(
    root: &Path,
    surface: u32,
    record: &ChildReturn,
) -> std::io::Result<()> {
    store::write_json(&child_file(root, surface), record)
}

/// attach 원격 출처 부모의 자식으로 기록한다. 결과는 `route` 토큰을 실어 그 부모에게 돌아간다.
pub(crate) fn set_attach_child(
    root: &Path,
    surface: u32,
    parent_key: &str,
    route: &str,
    child_index: Option<u32>,
) -> std::io::Result<()> {
    write_child_return(
        root,
        surface,
        &ChildReturn {
            receiver_id: route.into(),
            child_index,
            attach_parent: Some(parent_key.into()),
            ended_at_ms: None,
        },
    )
}

pub(crate) fn child_receiver(root: &Path, surface: u32) -> std::io::Result<Option<ChildReturn>> {
    store::read_json(&child_file(root, surface))
}

/// 반환 경로를 지운다. 이미 기록한 이벤트는 `events/`에 따로 있어 그대로 남는다.
pub(crate) fn clear_child_receiver(root: &Path, surface: u32) -> std::io::Result<()> {
    store::remove_file(&child_file(root, surface))
}

/// 반환 경로가 있는 모든 자식 surface.
pub(crate) fn child_records(root: &Path) -> Vec<(u32, ChildReturn)> {
    let Ok(entries) = std::fs::read_dir(root.join("children")) else {
        return Vec::new();
    };
    let mut out: Vec<(u32, ChildReturn)> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let surface = name.strip_suffix(".json")?.parse().ok()?;
            Some((surface, child_receiver(root, surface).ok()??))
        })
        .collect();
    out.sort_by_key(|(s, _)| *s);
    out
}

#[cfg(test)]
pub(crate) fn all_events(root: &Path) -> Vec<Event> {
    let mut events: Vec<Event> = std::fs::read_dir(root.join("events"))
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().into_owned();
                    read_event(root, name.strip_suffix(".json")?).ok().flatten()
                })
                .collect()
        })
        .unwrap_or_default();
    events.sort_by_key(|e| e.captured_at_ms);
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    const THREAD: &str = "01a12f74-dab9-7a00-8000-0123456789ab";

    fn completion(key: &str, body: Option<&str>) -> Completion {
        Completion {
            completion_key: key.into(),
            provider: "codex".into(),
            child_surface: 5,
            child_session: None,
            status: "turn_ended".into(),
            body: body.map(str::to_string),
            attempt_id: None,
        }
    }

    fn receiver(root: &Path, thread: &str) -> Receiver {
        receiver_for(root, thread, ReceiverKind::Owned, None).unwrap()
    }

    #[test]
    fn the_same_completion_key_is_captured_once_per_receiver() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let r1 = receiver(root, THREAD);
        let r2 = receiver(root, "01a12f74-dab9-7a00-8000-0123456789cd");
        let (a, dup_a) = capture(root, &r1, completion("codex:s:t1", Some("x"))).unwrap();
        let (b, dup_b) = capture(root, &r1, completion("codex:s:t1", Some("y"))).unwrap();
        assert!(!dup_a && dup_b);
        assert_eq!(a.event_id, b.event_id);
        assert_eq!(b.body.as_deref(), Some("x"), "첫 기록을 유지한다");
        let (c, dup_c) = capture(root, &r2, completion("codex:s:t1", None)).unwrap();
        assert!(!dup_c, "다른 수신자는 별도 이벤트");
        assert_ne!(c.event_id, a.event_id);
        assert_eq!(c.body, None);
        assert_eq!(all_events(root).len(), 2);
        // 대소문자만 다른 키는 다른 완료다.
        let (d, dup_d) = capture(root, &r1, completion("codex:S:t1", None)).unwrap();
        assert!(!dup_d);
        assert_ne!(d.event_id, a.event_id);
    }

    /// 같은 thread·전송 경로는 같은 수신자를 다시 쓴다. 경로가 다르면 다른 수신자다.
    #[test]
    fn a_receiver_is_reused_for_the_same_thread_and_transport() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let a = receiver(root, THREAD);
        let b = receiver(root, &THREAD.to_ascii_uppercase());
        assert_eq!(a.receiver_id, b.receiver_id);
        let ext = receiver_for(root, THREAD, ReceiverKind::External, None).unwrap();
        let ext_home =
            receiver_for(root, THREAD, ReceiverKind::External, Some("/h".into())).unwrap();
        assert_ne!(ext.receiver_id, a.receiver_id);
        assert_ne!(ext.receiver_id, ext_home.receiver_id);
        let n = std::fs::read_dir(root.join("receivers"))
            .unwrap()
            .filter(|e| e.as_ref().unwrap().path().is_file())
            .count();
        assert_eq!(n, 3);
    }

    #[test]
    fn a_long_body_is_cut_at_a_character_boundary_and_flagged() {
        let (body, cut) = truncate_body("가".repeat(BODY_MAX));
        assert!(cut && body.len() <= BODY_MAX);
        assert!(body.chars().all(|c| c == '가'));
        assert_eq!(truncate_body("short".into()), ("short".into(), false));
    }

    fn claim_path(root: &Path, r: &Receiver, key: &str) -> PathBuf {
        claim_file(
            root,
            &r.receiver_id,
            &crate::ids::completion_key_file_name(key).unwrap(),
        )
    }

    /// claim을 만들다 중단돼 빈 claim이나 반쯤 쓴 claim이 남아도 유일한 이벤트를 지우지 않는다.
    #[test]
    fn an_empty_or_partial_claim_never_deletes_the_only_event() {
        for content in ["", "0123", "zz"] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path();
            let r = receiver(root, THREAD);
            let (only, _) = capture(root, &r, completion("k:1", Some("x"))).unwrap();
            std::fs::write(claim_path(root, &r, "k:1"), content).unwrap();
            assert!(repair_claim(root, &only).unwrap(), "{content:?}");
            assert!(read_event(root, &only.event_id).unwrap().is_some());
            let (again, dup) = capture(root, &r, completion("k:1", None)).unwrap();
            assert!(dup && again.event_id == only.event_id, "{content:?}");
        }
        // 이벤트를 쓰기 전에 손상된 claim만 남았으면 새 기록이 claim을 고친다.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let r = receiver(root, THREAD);
        let path = claim_path(root, &r, "k:2");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "").unwrap();
        let (fresh, dup) = capture(root, &r, completion("k:2", None)).unwrap();
        assert!(!dup);
        let (again, dup) = capture(root, &r, completion("k:2", None)).unwrap();
        assert!(dup && again.event_id == fresh.event_id);
    }

    #[test]
    fn a_duplicate_written_before_its_claim_is_dropped_and_an_unclaimed_event_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let r = receiver(root, THREAD);
        let (kept, _) = capture(root, &r, completion("k:1", None)).unwrap();
        // claim 전에 중단된 같은 키의 두 번째 이벤트.
        let mut orphan = kept.clone();
        orphan.event_id = crate::ids::new_id().unwrap();
        write_event(root, &orphan).unwrap();
        assert!(!repair_claim(root, &orphan).unwrap());
        assert!(read_event(root, &orphan.event_id).unwrap().is_none());
        // claim이 없는 이벤트.
        let mut unclaimed = kept.clone();
        unclaimed.event_id = crate::ids::new_id().unwrap();
        unclaimed.completion_key = "k:2".into();
        write_event(root, &unclaimed).unwrap();
        assert!(repair_claim(root, &unclaimed).unwrap());
        let (again, dup) = capture(root, &r, completion("k:2", None)).unwrap();
        assert!(dup);
        assert_eq!(again.event_id, unclaimed.event_id);
    }

    #[test]
    fn the_pending_index_lists_ids_in_capture_order_without_reading_bodies() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let r = receiver(root, THREAD);
        let mut ids = Vec::new();
        for i in 0..3 {
            let (e, _) = capture(root, &r, completion(&format!("k:{i}"), Some("x"))).unwrap();
            ids.push(e.event_id);
        }
        assert_eq!(pending_threads(root), vec![THREAD.to_string()]);
        let listed = pending_ids(root, THREAD);
        let mut sorted = listed.clone();
        sorted.sort_by_key(|id| ids.iter().position(|x| x == id));
        assert_eq!(listed.len(), 3);
        let first = read_event(root, &ids[0]).unwrap().unwrap();
        remove_pending(root, &first).unwrap();
        assert_eq!(pending_ids(root, THREAD).len(), 2);
    }

    #[test]
    fn only_old_finished_events_are_pruned() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let r = receiver(root, THREAD);
        let (done, _) = capture(root, &r, completion("k:1", None)).unwrap();
        let (open, _) = capture(root, &r, completion("k:2", None)).unwrap();
        remove_pending(root, &done).unwrap();
        prune(root, std::time::SystemTime::now());
        assert!(
            read_event(root, &done.event_id).unwrap().is_some(),
            "기간 전"
        );
        let later = std::time::SystemTime::now() + RETENTION + std::time::Duration::from_secs(60);
        prune(root, later);
        assert!(read_event(root, &done.event_id).unwrap().is_none());
        assert!(
            read_event(root, &open.event_id).unwrap().is_some(),
            "색인에 남은 이벤트는 지우지 않는다"
        );
        // 남은 이벤트의 claim도 나이와 관계없이 남아 같은 키를 다시 보내도 중복이다.
        let (again, dup) = capture(root, &r, completion("k:2", None)).unwrap();
        assert!(dup && again.event_id == open.event_id);
        // 지운 이벤트의 오래된 claim은 지운다.
        assert!(!claim_path(root, &r, "k:1").exists());
    }

    #[test]
    fn invalid_keys_and_ids_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let r = receiver(root, THREAD);
        assert!(capture(root, &r, completion("a/b", None)).is_err());
        assert!(read_event(root, "../x").unwrap().is_none());
        assert!(read_receiver(root, "nothex").unwrap().is_none());
    }
}
