//! client: 물리 surface 부모(Claude 등)에게 온 attach 원격 자식의 턴 결과 보관.
//!
//! Codex 대화 부모는 Codex 원장에 기록하지만, 물리 surface 부모에게는 받을 원장이 없다. 그래서 결과 본문을 plugin
//! 데이터 폴더에 먼저 영속 저장하고, 그 부모의 완료 로그에 결과 ID 와 조회 명령을 한 줄 남긴 뒤에야 받았다고 답한다.
//!
//! - 결과의 정체는 route 토큰(그 지시를 보낼 때 client 가 새로 발급한 128비트 값)과 client 완료 키
//!   ([`crate::remote_attach::local_completion_key`]) 두 값이다. mirror surface 는 재연결 때 바뀔 수 있어 정체에 넣지
//!   않는다. 결과 ID 는 정체의 안정 해시이고, 같은 ID 의 저장 기록과 정체를 대조해 같을 때만 같은 결과로 본다. 해시가
//!   겹친 다른 결과는 다음 칸([`PROBES`] 개)에 저장한다. 재전송·재연결로 같은 결과가 다시 오면 같은 ID 라 새로 쌓이지 않는다.
//! - 받기는 결과 폴더의 잠금 파일로 프로세스 사이까지 직렬화한다.
//! - 알림 줄을 남기지 못하면 오류로 답해 host 가 다시 보내게 한다. 다시 오면 저장분을 그대로 두고 알림만 다시 시도한다.
//!   알림 표시를 저장하지 못한 채 다시 오면 같은 ID 의 알림 줄이 한 번 더 갈 수 있다(장애 때 중복 허용).
//! - 부모가 사라졌거나 지시 뒤 부모 세션이 바뀌었으면 본문은 저장하되 알리지 않고 `undelivered` 사유를 남긴다. 다른
//!   대화가 남의 결과를 받지 않게 하기 위해서다. 목록 조회로 찾을 수 있다.
//! - 결과는 자식이 닫혀도 남는다. 마지막으로 쓴 시각(알린 결과는 알린 시각)부터 [`KEPT`] 가 지나거나 [`MAX`] 개를 넘으면
//!   결론 난 것(알렸거나 `undelivered`) 중 오래된 것부터 지운다. 아직 알리지 못한 결과는 지우지 않으므로 상한을 넘을 수
//!   있다. 조회로는 지우지 않는다.
//!
//! 계약: docs/plugins/remote-attach-children.md.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};

use crate::host_call::HostCall;
use crate::remote_attach::{Received, local_completion_key};

/// plugin 데이터 폴더 아래 결과 폴더 이름.
pub const DIR: &str = "attach-results";
/// 결과를 두는 기간.
pub const KEPT: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// 결과 수 상한. 넘으면 결론 난 것 중 오래된 것부터 지운다.
pub const MAX: usize = 1024;
/// 받기를 직렬화하는 잠금 파일.
const LOCK_FILE: &str = ".lock";
/// 쓰다 끊긴 tmp 파일을 지우기까지 기다리는 시간.
const TMP_KEPT: Duration = Duration::from_secs(60 * 60);
/// 해시가 겹칠 때 보는 칸 수.
const PROBES: u32 = 16;
/// 알림 줄에 그대로 쓰는 상태 값. 그 밖의 값은 `unknown` 으로 쓴다(자식이 만든 문자열을 줄에 싣지 않는다).
pub const STATUSES: &[&str] = &["turn_ended", "turn_failed", "exited"];
/// Claude 세션 id 를 담는 surface meta. claude plugin 의 SessionStart 훅이 기록한다. 부모 세션이 바뀌었는지 볼 때 쓴다.
pub const PARENT_SESSION_META: &str = "claude-session-id";

/// 받은 결과 하나의 요약. 알림 줄을 만들 때 쓴다. `status` 는 [`STATUSES`] 중 하나이거나 `unknown` 이다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stored {
    pub id: String,
    pub parent_surface: u32,
    pub child_surface: u32,
    pub status: &'static str,
}

/// 결과 정체의 ID(FNV-1a 64비트, 16 hex). 인증 값이 아니라 파일 이름으로 쓸 수 있는 안정 이름이다. 같은 ID 가
/// 같은 결과라는 보장은 아니므로 저장된 기록의 정체를 대조한다.
fn hash_id(identity: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in identity.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// 결과 ID 형식인가. 조회 인자를 파일 이름으로 쓰기 전에 확인한다.
pub fn is_result_id(s: &str) -> bool {
    s.len() == 16
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// 결과 하나의 정체. 저장된 기록의 같은 필드와 모두 같아야 같은 결과다.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Identity {
    route: String,
    completion_key: String,
}

impl Identity {
    fn candidate(&self, probe: u32) -> String {
        let mut s = format!("{}\n{}", self.route, self.completion_key);
        if probe > 0 {
            s.push_str(&format!("\n#{probe}"));
        }
        hash_id(&s)
    }

    fn matches(&self, record: &Value) -> bool {
        record.get("route").and_then(Value::as_str) == Some(self.route.as_str())
            && record.get("completion_key").and_then(Value::as_str)
                == Some(self.completion_key.as_str())
    }
}

fn file(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.json"))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

pub(crate) fn read(path: &Path) -> Result<Option<Value>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
            .map(Some)
            .map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// tmp 파일에 쓰고 디스크에 내린 뒤 이름을 바꾼다. 중간에 끊겨도 이전 내용이나 새 내용 중 하나가 남는다.
pub(crate) fn write(dir: &Path, path: &Path, value: &Value) -> Result<(), String> {
    use std::io::Write;
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    let mut f = std::fs::File::create(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    f.write_all(value.to_string().as_bytes())
        .and_then(|()| f.sync_all())
        .map_err(|e| format!("{}: {e}", tmp.display()))?;
    drop(f);
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))?;
    #[cfg(unix)]
    if let Err(e) = std::fs::File::open(dir).and_then(|d| d.sync_all()) {
        tracing::warn!("attach results: could not sync {}: {e}", dir.display());
    }
    Ok(())
}

/// 결론 난 결과인가. 알렸거나 알리지 않기로 했다.
fn concluded(record: &Value) -> bool {
    record.get("notified").and_then(Value::as_bool) == Some(true)
        || record.get("undelivered").is_some_and(|u| !u.is_null())
}

/// 기간이 지났거나 상한을 넘은 결론 난 결과와 오래된 tmp 파일을 지운다. 지우지 못해도 받기는 계속한다.
fn prune(dir: &Path, keep: &str) {
    let now = SystemTime::now();
    let age = |m: SystemTime| now.duration_since(m).unwrap_or_default();
    let mut files = records(dir, &age);
    files.sort();
    let mut over = files.len().saturating_sub(MAX);
    for (modified, path) in &files {
        let old = age(*modified) > KEPT;
        let is_keep = path.file_stem().is_some_and(|s| s == keep);
        if (old || over > 0) && !is_keep && remove_concluded(path) {
            over = over.saturating_sub(1);
        }
    }
}

/// 결과 기록 파일과 수정 시각. 오래된 tmp 파일은 여기서 지운다.
fn records(dir: &Path, age: &dyn Fn(SystemTime) -> Duration) -> Vec<(SystemTime, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files = Vec::new();
    for e in entries.filter_map(Result::ok) {
        let path = e.path();
        let Some(modified) = e.metadata().ok().and_then(|m| m.modified().ok()) else {
            continue;
        };
        match path.extension().and_then(|x| x.to_str()) {
            Some("json") => files.push((modified, path)),
            Some("tmp") if age(modified) > TMP_KEPT => {
                if let Err(err) = std::fs::remove_file(&path) {
                    tracing::warn!("attach results: could not prune {}: {err}", path.display());
                }
            }
            _ => {}
        }
    }
    files
}

/// 결론 난 결과면 지운다. 지웠는지 돌려준다.
fn remove_concluded(path: &Path) -> bool {
    // 아직 알리지 못한 결과는 부모가 ID 를 모르므로 지우지 않는다. 읽을 수 없는 기록도 남긴다.
    if !matches!(read(path), Ok(Some(r)) if concluded(&r)) {
        return false;
    }
    match std::fs::remove_file(path) {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!("attach results: could not prune {}: {e}", path.display());
            false
        }
    }
}

/// 같은 정체의 기록이 있는 칸, 없으면 처음 빈 칸. 지운 칸 뒤에 있는 기록도 찾도록 모든 칸을 본다.
fn slot(dir: &Path, identity: &Identity) -> Result<(String, Option<Value>), String> {
    let mut free = None;
    for probe in 0..PROBES {
        let id = identity.candidate(probe);
        match read(&file(dir, &id))? {
            Some(existing) if identity.matches(&existing) => return Ok((id, Some(existing))),
            Some(_) => {}
            None => {
                free.get_or_insert(id);
            }
        }
    }
    free.map(|id| (id, None)).ok_or_else(|| {
        format!(
            "no free result id for {} after {PROBES} probes",
            identity.completion_key
        )
    })
}

/// 결과 폴더 잠금. 파일이 닫히면 풀린다.
pub(crate) fn lock(dir: &Path) -> Result<std::fs::File, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join(LOCK_FILE);
    let f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    f.lock().map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(f)
}

/// 부모에게 알리지 않을 사유. 부모 surface 가 없으면 `parent_gone`, 지시 때 기록한 부모 세션(`issued_session`)과 지금
/// 세션이 다르면 `parent_session_changed` 다. 조회 자체가 실패하면 `Err` 이며 호출자는 다시 받도록 오류로 답한다.
pub fn suppression<H: HostCall>(
    host: &H,
    parent_surface: u32,
    issued_session: Option<&str>,
) -> Result<Option<&'static str>, String> {
    let located = host
        .call("surface.locate", json!({ "surface_id": parent_surface }))
        .map_err(|e| format!("surface.locate s{parent_surface}: {e}"))?;
    if located.get("exists").and_then(Value::as_bool) != Some(true) {
        return Ok(Some("parent_gone"));
    }
    let Some(issued) = issued_session else {
        return Ok(None);
    };
    let now = parent_session(host, parent_surface)?;
    Ok((now.as_deref() != Some(issued)).then_some("parent_session_changed"))
}

/// 부모 surface 의 지금 Claude 세션 id. 값이 없으면(셸 등 Claude 가 아닌 부모) `None` 이다. 조회 자체가 실패하면
/// `Err` 다. 값 없음으로 낮추면 받을 때는 세션이 바뀐 것으로, 발급 때는 세션 보호가 빠진 것으로 잘못 판단한다.
pub fn parent_session<H: HostCall>(
    host: &H,
    parent_surface: u32,
) -> Result<Option<String>, String> {
    let reply = host
        .call(
            "surface.meta.get",
            json!({ "surface_id": parent_surface, "key": PARENT_SESSION_META }),
        )
        .map_err(|e| format!("surface.meta.get s{parent_surface}: {e}"))?;
    Ok(reply
        .get("value")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(String::from))
}

/// 받은 원격 결과(`completion`)를 `parent_surface` 의 결과로 저장하고 `notify` 로 알린 뒤 상태를 준다.
///
/// - 본문이 틀리면 `Rejected` 다.
/// - 처음 받은 결과는 저장 → 알림 → 알림 표시 순서이며 `Recorded` 다.
/// - `suppressed` 사유가 있으면 알리지 않고 그 사유를 저장한 뒤 `Recorded` 다(서버가 다시 보내지 않게 한다).
/// - 이미 결론 난 같은 결과는 `Duplicate` 다. 저장만 되고 알리지 못한 결과는 알림을 다시 시도한다.
/// - 저장이나 알림이 실패하면 `Err` 다. 호출자는 host 가 다시 보내도록 오류로 답한다.
pub fn receive(
    dir: &Path,
    parent_surface: u32,
    child_surface: u32,
    completion: &Value,
    suppressed: Option<&str>,
    notify: impl FnOnce(&Stored) -> Result<(), String>,
) -> Result<Received, String> {
    let Some(remote_key) = completion.get("completion_key").and_then(Value::as_str) else {
        return Ok(Received::Rejected(
            "completion without completion_key".into(),
        ));
    };
    let Some(status) = completion.get("status").and_then(Value::as_str) else {
        return Ok(Received::Rejected("completion without status".into()));
    };
    let identity = Identity {
        route: completion
            .get(crate::remote_attach::ROUTE_FIELD)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        completion_key: local_completion_key(remote_key),
    };
    let _lock = lock(dir)?;
    let (id, existing) = slot(dir, &identity)?;
    let path = file(dir, &id);
    let mut record = match existing {
        Some(existing) => existing,
        None => {
            let mut map = Map::new();
            map.insert("id".into(), json!(id));
            map.insert("completion_key".into(), json!(identity.completion_key));
            map.insert("route".into(), json!(identity.route));
            for field in ["provider", "child_session", "body", "attempt_id"] {
                map.insert(
                    field.into(),
                    completion.get(field).cloned().unwrap_or(Value::Null),
                );
            }
            map.insert("status".into(), json!(status));
            map.insert("parent_surface".into(), json!(parent_surface));
            map.insert("child_surface".into(), json!(child_surface));
            map.insert(
                "remote_child_surface".into(),
                completion
                    .get("child_surface")
                    .cloned()
                    .unwrap_or(Value::Null),
            );
            map.insert("received_at_ms".into(), json!(now_ms()));
            map.insert("notified".into(), json!(false));
            map.insert("undelivered".into(), Value::Null);
            let record = Value::Object(map);
            write(dir, &path, &record)?;
            prune(dir, &id);
            record
        }
    };
    if concluded(&record) {
        return Ok(Received::Duplicate);
    }
    if let Some(reason) = suppressed {
        record["undelivered"] = json!(reason);
        write(dir, &path, &record)?;
        tracing::info!("attach results: {id} stored without notice ({reason})");
        return Ok(Received::Recorded);
    }
    notify(&Stored {
        id: id.clone(),
        parent_surface: u32_of(&record, "parent_surface").unwrap_or(parent_surface),
        child_surface: u32_of(&record, "child_surface").unwrap_or(child_surface),
        status: STATUSES
            .iter()
            .find(|s| **s == status)
            .copied()
            .unwrap_or("unknown"),
    })?;
    record["notified"] = json!(true);
    write(dir, &path, &record)?;
    Ok(Received::Recorded)
}

fn u32_of(v: &Value, key: &str) -> Option<u32> {
    v.get(key)
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
}

/// 조회 실패.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryError {
    /// `id` 가 결과 ID 형식이 아니다.
    BadId(String),
    /// 그 ID 의 결과가 없다(기간이 지나 지워졌거나 받은 적이 없다).
    NotFound(String),
    Io(String),
}

/// `remote_results` 조회. `id` 가 있으면 본문까지 담은 그 결과(`{"result": ...}`), 없으면 본문을 뺀 목록
/// (`{"results": [...]}`, 받은 순서)이다. 목록의 `body_bytes` 는 본문 길이다.
pub fn query(dir: &Path, params: &Value) -> Result<Value, QueryError> {
    if let Some(id) = params.get("id").and_then(Value::as_str) {
        if !is_result_id(id) {
            return Err(QueryError::BadId(id.to_string()));
        }
        return match read(&file(dir, id)).map_err(QueryError::Io)? {
            Some(record) => Ok(json!({ "result": record })),
            None => Err(QueryError::NotFound(id.to_string())),
        };
    }
    let mut results = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(json!({ "results": results }));
        }
        Err(e) => return Err(QueryError::Io(format!("{}: {e}", dir.display()))),
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.extension().is_none_or(|x| x != "json") {
            continue;
        }
        match read(&path) {
            Ok(Some(Value::Object(mut map))) => {
                let bytes = map
                    .remove("body")
                    .and_then(|b| b.as_str().map(str::len))
                    .unwrap_or(0);
                map.insert("body_bytes".into(), json!(bytes));
                results.push(Value::Object(map));
            }
            Ok(_) => {}
            Err(e) => tracing::warn!("attach results: skipped unreadable record: {e}"),
        }
    }
    results.sort_by_key(|r| r.get("received_at_ms").and_then(Value::as_u64).unwrap_or(0));
    Ok(json!({ "results": results }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    const ROUTE: &str = "0123456789abcdef0123456789abcdef";

    fn completion(key: &str, body: &str) -> Value {
        completion_on(ROUTE, key, body)
    }

    fn completion_on(route: &str, key: &str, body: &str) -> Value {
        json!({
            "completion_key": key,
            "provider": "claude",
            "child_surface": 41,
            "child_session": "s-1",
            "status": "turn_ended",
            "body": body,
            "route": route,
        })
    }

    fn id_of(route: &str, remote_key: &str) -> String {
        Identity {
            route: route.into(),
            completion_key: local_completion_key(remote_key),
        }
        .candidate(0)
    }

    fn ok(_: &Stored) -> Result<(), String> {
        Ok(())
    }

    /// 처음 받은 결과는 본문을 저장한 뒤 알리고, 같은 결과의 재전송은 다시 알리지 않는다.
    #[test]
    fn a_result_is_stored_before_it_is_announced_once() {
        let dir = tempfile::tempdir().unwrap();
        let seen = RefCell::new(Vec::new());
        let notify = |s: &Stored| {
            // 알릴 때는 이미 본문을 읽을 수 있어야 한다.
            let stored = query(dir.path(), &json!({ "id": s.id })).unwrap();
            assert_eq!(stored["result"]["body"], "done");
            seen.borrow_mut().push(s.clone());
            Ok(())
        };
        let c = completion("claude:r1:t1", "done");
        assert_eq!(
            receive(dir.path(), 7, 12, &c, None, notify).unwrap(),
            Received::Recorded
        );
        assert_eq!(
            receive(dir.path(), 7, 12, &c, None, |_| panic!(
                "재전송은 알리지 않는다"
            ))
            .unwrap(),
            Received::Duplicate
        );
        let seen = seen.into_inner();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].id, id_of(ROUTE, "claude:r1:t1"));
        assert_eq!((seen[0].parent_surface, seen[0].child_surface), (7, 12));
        let r = &query(dir.path(), &json!({ "id": seen[0].id })).unwrap()["result"];
        assert_eq!(r["remote_child_surface"], 41);
        assert_eq!(r["route"], ROUTE);
        assert_eq!(r["notified"], true);
    }

    /// 알리지 못하면 오류이며, 다시 오면 저장분으로 알림만 다시 시도한다.
    #[test]
    fn a_failed_announcement_is_retried_on_the_next_delivery() {
        let dir = tempfile::tempdir().unwrap();
        let c = completion("claude:r1:t2", "body");
        assert!(receive(dir.path(), 7, 12, &c, None, |_| Err("disk".into())).is_err());
        let calls = RefCell::new(0);
        assert_eq!(
            receive(dir.path(), 7, 12, &c, None, |_| {
                *calls.borrow_mut() += 1;
                Ok(())
            })
            .unwrap(),
            Received::Recorded
        );
        assert_eq!(*calls.borrow(), 1);
    }

    /// 다른 route(다른 지시·다른 서버)에서 온 같은 완료 키는 다른 결과다. 재연결로 mirror surface 가 바뀐 재전송은
    /// 같은 결과라 다시 알리지 않는다.
    #[test]
    fn the_route_not_the_mirror_surface_tells_results_apart() {
        let dir = tempfile::tempdir().unwrap();
        let other = "fedcba9876543210fedcba9876543210";
        let mut ids = std::collections::BTreeSet::new();
        for route in [ROUTE, other] {
            let c = completion_on(route, "claude:same:t1", "x");
            let id = RefCell::new(String::new());
            assert_eq!(
                receive(dir.path(), 7, 12, &c, None, |s| {
                    *id.borrow_mut() = s.id.clone();
                    Ok(())
                })
                .unwrap(),
                Received::Recorded
            );
            ids.insert(id.into_inner());
        }
        assert_eq!(ids.len(), 2);
        let again = completion_on(ROUTE, "claude:same:t1", "x");
        assert_eq!(
            receive(dir.path(), 7, 13, &again, None, |_| panic!("같은 결과")).unwrap(),
            Received::Duplicate
        );
    }

    /// 충돌로 다음 칸에 저장된 결과는 앞 칸이 지워진 뒤 다시 와도 그 기록을 찾는다(빈 칸에 새로 쓰지 않는다).
    #[test]
    fn a_result_behind_a_freed_slot_is_still_found() {
        let dir = tempfile::tempdir().unwrap();
        let first = id_of(ROUTE, "claude:hole:t1");
        let squatter = json!({ "route": ROUTE, "completion_key": "r.other", "notified": true });
        std::fs::write(file(dir.path(), &first), squatter.to_string()).unwrap();
        let c = completion("claude:hole:t1", "body");
        receive(dir.path(), 7, 12, &c, None, ok).unwrap();
        std::fs::remove_file(file(dir.path(), &first)).unwrap();
        assert_eq!(
            receive(dir.path(), 7, 12, &c, None, |_| panic!("이미 알렸다")).unwrap(),
            Received::Duplicate
        );
        assert!(!file(dir.path(), &first).exists());
    }

    /// 알리지 않을 사유가 있으면 본문은 저장하고 사유를 남기며, 다시 와도 알리지 않는다.
    #[test]
    fn a_suppressed_result_is_kept_without_a_notice() {
        let dir = tempfile::tempdir().unwrap();
        let c = completion("claude:gone:t1", "body");
        assert_eq!(
            receive(dir.path(), 7, 12, &c, Some("parent_gone"), |_| panic!(
                "알리지 않는다"
            ))
            .unwrap(),
            Received::Recorded
        );
        assert_eq!(
            receive(dir.path(), 7, 12, &c, None, |_| panic!("이미 결론 났다")).unwrap(),
            Received::Duplicate
        );
        let id = id_of(ROUTE, "claude:gone:t1");
        let r = &query(dir.path(), &json!({ "id": id })).unwrap()["result"];
        assert_eq!(r["undelivered"], "parent_gone");
        assert_eq!(r["notified"], false);
        assert_eq!(r["body"], "body");
    }

    /// 알림 줄의 상태는 허용 값만 쓴다.
    #[test]
    fn an_unknown_status_is_not_copied_into_the_notice() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = completion("claude:s:t1", "b");
        c["status"] = json!("turn_ended; rm -rf /");
        let seen = RefCell::new("");
        receive(dir.path(), 7, 12, &c, None, |s| {
            *seen.borrow_mut() = s.status;
            Ok(())
        })
        .unwrap();
        assert_eq!(*seen.borrow(), "unknown");
    }

    /// 해시가 겹친 다른 결과의 기록이 그 ID 에 있으면 덮어쓰거나 중복으로 보지 않고 다음 ID 에 저장한다.
    #[test]
    fn a_hash_collision_is_not_taken_for_the_same_result() {
        let dir = tempfile::tempdir().unwrap();
        let first = id_of(ROUTE, "claude:r9:t1");
        let squatter = json!({ "id": first, "route": ROUTE,
            "completion_key": "r.someone-else", "notified": true, "body": "theirs" });
        std::fs::write(file(dir.path(), &first), squatter.to_string()).unwrap();
        let id = RefCell::new(String::new());
        let c = completion("claude:r9:t1", "mine");
        assert_eq!(
            receive(dir.path(), 7, 12, &c, None, |s| {
                *id.borrow_mut() = s.id.clone();
                Ok(())
            })
            .unwrap(),
            Received::Recorded
        );
        let id = id.into_inner();
        assert_ne!(id, first);
        assert_eq!(
            query(dir.path(), &json!({ "id": id })).unwrap()["result"]["body"],
            "mine"
        );
        assert_eq!(
            query(dir.path(), &json!({ "id": first })).unwrap()["result"]["body"],
            "theirs"
        );
    }

    /// 독립 턴은 각자 다른 ID 이고 목록은 본문 없이 받은 순서다.
    #[test]
    fn separate_turns_get_separate_ids_and_the_list_omits_bodies() {
        let dir = tempfile::tempdir().unwrap();
        for t in 1..=3 {
            let c = completion(&format!("codex:x:t{t}"), &"b".repeat(t));
            receive(dir.path(), 7, 12, &c, None, ok).unwrap();
        }
        let list = query(dir.path(), &json!({})).unwrap();
        let list = list["results"].as_array().unwrap();
        assert_eq!(list.len(), 3);
        assert!(list.iter().all(|r| r.get("body").is_none()));
        let ids: std::collections::BTreeSet<_> =
            list.iter().map(|r| r["id"].as_str().unwrap()).collect();
        assert_eq!(ids.len(), 3);
        assert!(list.iter().any(|r| r["body_bytes"] == 3));
    }

    /// 형식이 틀린 결과는 거절하고, 조회 ID 는 형식을 먼저 확인한다.
    #[test]
    fn malformed_results_and_ids_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            receive(
                dir.path(),
                7,
                12,
                &json!({ "status": "turn_ended" }),
                None,
                ok
            ),
            Ok(Received::Rejected(_))
        ));
        assert_eq!(
            query(dir.path(), &json!({ "id": "../x" })),
            Err(QueryError::BadId("../x".into()))
        );
        let missing = hash_id("nothing");
        assert_eq!(
            query(dir.path(), &json!({ "id": missing })),
            Err(QueryError::NotFound(missing))
        );
        assert_eq!(query(dir.path(), &json!({})).unwrap()["results"], json!([]));
    }

    fn put_old(dir: &Path, i: usize, notified: bool) -> PathBuf {
        let p = dir.join(format!("{i:016x}.json"));
        std::fs::write(&p, json!({ "notified": notified }).to_string()).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&p)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(60 + i as u64))
            .unwrap();
        p
    }

    /// 상한을 넘으면 알린 것 중 오래된 것부터 지우고, 아직 알리지 못한 결과와 방금 받은 결과는 남긴다.
    #[test]
    fn pruning_over_the_cap_keeps_unannounced_results() {
        let dir = tempfile::tempdir().unwrap();
        // 가장 오래된 두 개: 하나는 아직 알리지 못했다.
        let unannounced = put_old(dir.path(), MAX + 1, false);
        let announced = put_old(dir.path(), MAX, true);
        for i in 0..MAX - 2 {
            put_old(dir.path(), i, true);
        }
        let c = completion("claude:new:t1", "x");
        receive(dir.path(), 7, 12, &c, None, ok).unwrap();
        assert!(
            unannounced.exists(),
            "아직 알리지 못한 결과는 지우지 않는다"
        );
        assert!(!announced.exists(), "알린 결과는 오래된 것부터 지운다");
        assert!(file(dir.path(), &id_of(ROUTE, "claude:new:t1")).exists());
    }

    /// 보존 기간이 지나도 알리지 못한 결과는 남는다.
    #[test]
    fn expired_unannounced_results_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        let old = SystemTime::now() - KEPT - Duration::from_secs(60);
        let mut paths = Vec::new();
        for (i, notified) in [(1, false), (2, true)] {
            let p = put_old(dir.path(), i, notified);
            std::fs::File::options()
                .write(true)
                .open(&p)
                .unwrap()
                .set_modified(old)
                .unwrap();
            paths.push(p);
        }
        receive(dir.path(), 7, 12, &completion("claude:x:t1", "x"), None, ok).unwrap();
        assert!(paths[0].exists());
        assert!(!paths[1].exists());
    }

    /// 부모 surface 를 조회하는 가짜 host. `meta` 가 `Err` 면 세션 조회가 실패한다.
    struct ParentHost {
        meta: Result<Value, ()>,
    }

    impl HostCall for ParentHost {
        fn call(&self, method: &str, _: Value) -> Result<Value, tasty_plugin_sdk::PluginError> {
            match method {
                "surface.locate" => Ok(json!({ "exists": true })),
                _ => self
                    .meta
                    .clone()
                    .map_err(|()| tasty_plugin_sdk::PluginError::HostCall {
                        method: method.into(),
                        message: "busy".into(),
                        code: Some(-32000),
                    }),
            }
        }
    }

    #[test]
    fn a_failed_session_lookup_is_an_error_not_a_changed_session() {
        let host = ParentHost { meta: Err(()) };
        assert!(parent_session(&host, 7).is_err());
        // 받을 때 조회가 실패하면 영구 억제(`parent_session_changed`)가 아니라 다시 받도록 오류다.
        assert!(suppression(&host, 7, Some("s-1")).is_err());
    }

    #[test]
    fn a_missing_session_value_is_none_and_a_matching_one_is_not_suppressed() {
        let host = ParentHost {
            meta: Ok(json!({ "value": null })),
        };
        assert_eq!(parent_session(&host, 7), Ok(None));
        assert_eq!(
            suppression(&host, 7, Some("s-1")),
            Ok(Some("parent_session_changed"))
        );
        let host = ParentHost {
            meta: Ok(json!({ "value": "s-1" })),
        };
        assert_eq!(parent_session(&host, 7), Ok(Some("s-1".into())));
        assert_eq!(suppression(&host, 7, Some("s-1")), Ok(None));
    }
}
