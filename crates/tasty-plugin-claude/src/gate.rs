//! Stop 훅 게이트의 본문·완료 표식·회차 상한을 이름으로 등록한다.
//! 게이트 이름은 프로필 적용에도 사용하므로 같은 이름의 사용자 프로필과 중복 등록할 수 없다.
//!
//! 데이터 디렉터리의 gates/registered/<name>.json에 정의를, gates/bodies/<name>.md에
//! 본문 복사본을 저장한다. 여러 줄 본문을 JSON 문자열에 넣지 않고 별도 파일로 관리한다.
//! 활성 파일과 회차 상태는 checklist 모듈이 관리한다.
//!
//! 기본 continue-checklist는 파일 없이 제공한다. 본문은 번역 카탈로그에서 읽고
//! 완료 표식은 checklist::SENTINEL을 쓰며 회차 상한은 설정값으로 결정한다.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use tasty_plugin_sdk::{IpcMethodError, i18n::Translator};

use crate::checklist::SENTINEL;

/// 프로필과 같은 이름 규칙: 영문 소문자·숫자·하이픈, 최대 32자.
/// 경로에 넣는 이름이므로 슬래시·점 등은 허용하지 않는다.
pub(crate) fn is_valid_short_name(s: &str) -> bool {
    if s.is_empty() || s.len() > 32 {
        return false;
    }
    s.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateError {
    /// 호스트가 `TASTY_PLUGIN_DATA_DIR` 를 주입하지 않은 비정상 기동.
    NoDataDir,
    InvalidShortName(String),
    UnknownGate(String),
    BodyNotReadable {
        path: String,
        message: String,
    },
    /// 본문에 이 게이트의 완료 표식이 없다.
    BodyMissingSentinel {
        sentinel: String,
    },
    /// 빈 표식은 모든 메시지와 일치하므로 허용하지 않는다.
    EmptySentinel,
    /// 라운드 상한 0 — 게이트가 한 번도 block 하지 못한다.
    RoundsBelowOne,
    /// 같은 이름의 사용자 프로필이 있다.
    ProfileNameConflict(String),
    Io {
        path: String,
        message: String,
    },
}

impl GateError {
    pub(crate) fn translate(&self, tr: &Translator) -> String {
        match self {
            Self::NoDataDir => tr.t("claude.gate.no_data_dir").to_string(),
            Self::InvalidShortName(s) => tr.t_fmt("claude.gate.invalid_short_name", s),
            Self::UnknownGate(id) => tr.t_fmt("claude.gate.unknown_gate", id),
            Self::BodyNotReadable { path, message } => {
                tr.t_args("claude.gate.body_not_readable", &[path, message])
            }
            Self::BodyMissingSentinel { sentinel } => {
                tr.t_fmt("claude.gate.body_missing_sentinel", sentinel)
            }
            Self::EmptySentinel => tr.t("claude.gate.empty_sentinel").to_string(),
            Self::RoundsBelowOne => tr.t("claude.gate.rounds_below_one").to_string(),
            // 메시지가 이름을 두 번 쓴다(충돌한 이름 + 해제 명령 예시) — `{}` 는 한 번씩만 채우므로
            // 같은 값을 두 번 넘긴다.
            Self::ProfileNameConflict(name) => {
                tr.t_args("claude.gate.profile_name_conflict", &[name, name])
            }
            Self::Io { path, message } => tr.t_args("claude.gate.io_error", &[path, message]),
        }
    }
}

/// 본문은 별도 파일에 저장하고 정의에는 완료 표식과 회차 상한을 둔다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateDef {
    /// 실제 사용할 완료 표식. 생략해 등록했으면 기본값을 저장한다.
    pub sentinel: String,
    /// 생략하면 실행 시 설정값, DEFAULT_ROUND_LIMIT 순서로 선택한다.
    pub round_limit: Option<u32>,
}

impl GateDef {
    fn to_json(&self) -> Value {
        match self.round_limit {
            Some(n) => json!({ "sentinel": self.sentinel, "round_limit": n }),
            None => json!({ "sentinel": self.sentinel }),
        }
    }

    /// 알려진 필드만 읽어 이전 버전도 읽을 수 있는 부분을 사용한다.
    fn from_json(v: &Value) -> Self {
        Self {
            sentinel: v
                .get("sentinel")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or(SENTINEL)
                .to_string(),
            round_limit: v
                .get("round_limit")
                .and_then(|v| v.as_u64())
                .map(|n| n as u32)
                .filter(|n| *n >= 1),
        }
    }
}

/// 목록에 표시할 요약. 자체 회차 상한이 없으면 값 대신 settings에서 결정함을 알린다.
#[derive(Debug, Clone)]
pub struct GateSummary {
    /// `<owner>/<short>` 형식.
    pub id: String,
    pub owner: &'static str,
    pub sentinel: String,
    pub round_limit: Option<u32>,
    /// `"gate"`(정의가 직접 지정) 또는 `"settings"`(미지정 → Settings 폴백).
    pub round_limit_source: &'static str,
    /// 게이트가 켜져 있는지. 개별 status 호출 없이 전체 상태를 볼 수 있도록 포함한다.
    pub enabled: bool,
}

/// 번역 본문과 기본 표식을 사용하는 내장 게이트. 같은 이름의 사용자 등록이 있으면 그쪽이 우선한다.
/// 번역의 표식 포함 여부는 checklist 시험, 사용자 본문은 register에서 확인한다.
pub(crate) fn host_default_gate(short_name: &str, tr: &Translator) -> Option<(GateDef, String)> {
    match short_name {
        DEFAULT_GATE_NAME => Some((
            GateDef {
                sentinel: SENTINEL.to_string(),
                round_limit: None,
            },
            tr.t("claude.checklist.body").to_string(),
        )),
        _ => None,
    }
}

/// 게이트를 생략했을 때의 기본 이름. 매니페스트와 같은지는 기존 시험이 확인한다.
pub(crate) const DEFAULT_GATE_NAME: &str = "continue-checklist";

/// [`host_default_gate`] 가 아는 이름 전체 — `list` 이 host 항목을 나열할 때 순회한다.
const HOST_DEFAULT_GATE_NAMES: &[&str] = &[DEFAULT_GATE_NAME];

fn registered_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("gates").join("registered")
}

fn bodies_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("gates").join("bodies")
}

fn registered_file(data_dir: &Path, short_name: &str) -> PathBuf {
    registered_dir(data_dir).join(format!("{short_name}.json"))
}

fn body_file(data_dir: &Path, short_name: &str) -> PathBuf {
    bodies_dir(data_dir).join(format!("{short_name}.md"))
}

fn require_data_dir(data_dir: Option<&Path>) -> Result<&Path, GateError> {
    data_dir.ok_or(GateError::NoDataDir)
}

/// 같은 이름의 사용자 게이트가 있는지 확인해 프로필 등록의 충돌을 막는다.
pub(crate) fn is_registered(data_dir: Option<&Path>, short_name: &str) -> bool {
    data_dir.is_some_and(|d| registered_file(d, short_name).is_file())
}

/// 토글 대상이 등록돼 있는지 확인한다. 본문이 손상됐어도 끌 수 있도록 본문은 읽지 않는다.
pub(crate) fn ensure_known(
    data_dir: Option<&Path>,
    short_name: &str,
    tr: &Translator,
) -> Result<(), GateError> {
    if !is_valid_short_name(short_name) {
        return Err(GateError::InvalidShortName(short_name.to_string()));
    }
    match owner_of(data_dir, short_name, tr) {
        Some(_) => Ok(()),
        None => Err(GateError::UnknownGate(format!("user/{short_name}"))),
    }
}

/// 사용자 등록을 먼저 확인하고 없으면 기본 게이트를 조회해 출처를 반환한다.
fn owner_of(data_dir: Option<&Path>, short_name: &str, tr: &Translator) -> Option<&'static str> {
    if !is_valid_short_name(short_name) {
        return None;
    }
    if is_registered(data_dir, short_name) {
        return Some("user");
    }
    host_default_gate(short_name, tr).map(|_| "host")
}

/// 기본 게이트 이름 목록. 프로필 목록에서도 사용한다.
pub(crate) fn host_default_names() -> &'static [&'static str] {
    HOST_DEFAULT_GATE_NAMES
}

/// 사용자 게이트 이름을 정렬해 반환한다.
pub(crate) fn registered_names(data_dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(registered_dir(data_dir))
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.strip_suffix(".json").map(str::to_string)
        })
        .collect();
    names.sort();
    names
}

/// 이름으로 적용할 Stop 훅 설정을 만든다. 실제 본문·표식·상한은 훅 실행 때 다시 읽는다.
/// 이름은 명령에 그대로 들어가므로 is_valid_short_name의 문자 제한을 유지해야 한다.
pub(crate) fn attach_profile(
    data_dir: Option<&Path>,
    short_name: &str,
    tr: &Translator,
) -> Option<(&'static str, Value)> {
    let owner = owner_of(data_dir, short_name, tr)?;
    Some((owner, stop_hook_profile(short_name)))
}

/// 공용 tasty_guarded_command로 게이트 호출 명령을 만들어 Stop 훅 설정에 넣는다.
fn stop_hook_profile(short_name: &str) -> Value {
    let command = crate::install::tasty_guarded_command(&gate_hook_argv(short_name));
    json!({
        "hooks": {
            "Stop": [{
                "matcher": "",
                "hooks": [{ "type": "command", "command": command }]
            }]
        }
    })
}

/// 게이트 호출 명령. Claude Code 의 연속 block 상한은 환경 변수로만 정해지므로
/// 그 값을 `--block-cap` 으로 넘겨 플러그인이 상한에 닿은 block 을 알아보게 한다.
pub(crate) fn gate_hook_argv(short_name: &str) -> String {
    format!(
        "tasty claude checklist-hook --gate {short_name} --block-cap \"${{CLAUDE_CODE_STOP_HOOK_BLOCK_CAP:-}}\""
    )
}

/// 이름·본문·완료 표식·상한을 검증하고 본문 복사본과 정의를 저장한다.
/// 같은 이름으로 재등록하면 두 파일을 덮어쓴다. 입력 검증은 쓰기 전에 끝낸다.
pub(crate) fn register(
    data_dir: Option<&Path>,
    short_name: &str,
    body_path: &Path,
    sentinel: Option<&str>,
    round_limit: Option<u32>,
) -> Result<(), GateError> {
    if !is_valid_short_name(short_name) {
        return Err(GateError::InvalidShortName(short_name.to_string()));
    }
    let data_dir = require_data_dir(data_dir)?;

    // 같은 이름의 사용자 프로필을 가리지 않도록 충돌을 거절한다.
    if crate::profile::is_registered(Some(data_dir), short_name) {
        return Err(GateError::ProfileNameConflict(short_name.to_string()));
    }

    let sentinel = match sentinel {
        Some("") => return Err(GateError::EmptySentinel),
        Some(s) => s.to_string(),
        None => SENTINEL.to_string(),
    };
    if round_limit == Some(0) {
        return Err(GateError::RoundsBelowOne);
    }

    let body = std::fs::read_to_string(body_path).map_err(|e| GateError::BodyNotReadable {
        path: body_path.display().to_string(),
        message: e.to_string(),
    })?;
    if !body.contains(&sentinel) {
        return Err(GateError::BodyMissingSentinel { sentinel });
    }

    let def = GateDef {
        sentinel,
        round_limit,
    };

    let def_dir = registered_dir(data_dir);
    std::fs::create_dir_all(&def_dir).map_err(|e| GateError::Io {
        path: def_dir.display().to_string(),
        message: e.to_string(),
    })?;
    let body_dir = bodies_dir(data_dir);
    std::fs::create_dir_all(&body_dir).map_err(|e| GateError::Io {
        path: body_dir.display().to_string(),
        message: e.to_string(),
    })?;

    // 정의 파일로 등록 여부를 판단하므로 본문을 먼저 쓴다.
    let body_dest = body_file(data_dir, short_name);
    std::fs::write(&body_dest, &body).map_err(|e| GateError::Io {
        path: body_dest.display().to_string(),
        message: e.to_string(),
    })?;
    let def_dest = registered_file(data_dir, short_name);
    let text = serde_json::to_string_pretty(&def.to_json()).unwrap_or_else(|_| "{}".to_string());
    std::fs::write(&def_dest, text).map_err(|e| GateError::Io {
        path: def_dest.display().to_string(),
        message: e.to_string(),
    })?;
    Ok(())
}

/// 정의와 본문을 제거한다. 등록이 없으면 UnknownGate를 반환한다.
/// 같은 이름의 재등록이 이전 상태를 물려받지 않도록 활성 파일·회차도 정리한다.
/// 상태 정리 실패는 경고만 남기고, 정의·본문 삭제의 실패와는 구분한다.
pub(crate) fn unregister(data_dir: Option<&Path>, short_name: &str) -> Result<(), GateError> {
    // 파일을 삭제하기 전에 경로에 넣을 이름을 검증한다.
    if !is_valid_short_name(short_name) {
        return Err(GateError::InvalidShortName(short_name.to_string()));
    }
    let data_dir = require_data_dir(data_dir)?;
    let def = registered_file(data_dir, short_name);
    if !def.is_file() {
        return Err(GateError::UnknownGate(format!("user/{short_name}")));
    }
    std::fs::remove_file(&def).map_err(|e| GateError::Io {
        path: def.display().to_string(),
        message: e.to_string(),
    })?;
    // 뒤의 본문 삭제가 실패하더라도 활성 파일과 회차 상태의 정리는 시도한다.
    crate::checklist::remove_gate_runtime_state(Some(data_dir), short_name);
    let body = body_file(data_dir, short_name);
    // 본문이 이미 없는 상태(수동 삭제 등)는 정상 완료로 본다 — 목표는 "둘 다 없음".
    if body.is_file() {
        std::fs::remove_file(&body).map_err(|e| GateError::Io {
            path: body.display().to_string(),
            message: e.to_string(),
        })?;
    }
    Ok(())
}

/// 정의와 본문, 실제 출처(user 또는 host)를 반환한다.
/// 데이터 디렉터리가 없어도 기본 게이트는 조회할 수 있다.
pub(crate) fn show(
    data_dir: Option<&Path>,
    short_name: &str,
    tr: &Translator,
) -> Result<(&'static str, GateDef, String), GateError> {
    // 파일을 읽기 전에도 경로에 넣을 이름을 검증한다.
    if !is_valid_short_name(short_name) {
        return Err(GateError::InvalidShortName(short_name.to_string()));
    }
    if let Some(data_dir) = data_dir {
        let def_path = registered_file(data_dir, short_name);
        if def_path.is_file() {
            let text = std::fs::read_to_string(&def_path).map_err(|e| GateError::Io {
                path: def_path.display().to_string(),
                message: e.to_string(),
            })?;
            let value: Value = serde_json::from_str(&text).map_err(|e| GateError::Io {
                path: def_path.display().to_string(),
                message: e.to_string(),
            })?;
            let body_path = body_file(data_dir, short_name);
            let body = std::fs::read_to_string(&body_path).map_err(|e| GateError::Io {
                path: body_path.display().to_string(),
                message: e.to_string(),
            })?;
            return Ok(("user", GateDef::from_json(&value), body));
        }
    }
    match host_default_gate(short_name, tr) {
        Some((def, body)) => Ok(("host", def, body)),
        None => Err(GateError::UnknownGate(format!("user/{short_name}"))),
    }
}

/// 기본 게이트를 먼저 나열한다. 같은 이름의 사용자 등록이 있으면 실제 사용할 사용자 항목만 보여준다.
pub(crate) fn list(data_dir: Option<&Path>, tr: &Translator) -> Vec<GateSummary> {
    let user_names: Vec<String> = data_dir.map(registered_names).unwrap_or_default();

    let mut out: Vec<GateSummary> = Vec::new();
    for name in HOST_DEFAULT_GATE_NAMES {
        if user_names.iter().any(|u| u == name) {
            continue;
        }
        if let Some((def, _body)) = host_default_gate(name, tr) {
            let enabled = crate::checklist::marker_present(data_dir, name);
            out.push(summary(format!("host/{name}"), "host", def, enabled));
        }
    }
    for name in &user_names {
        // 정의가 손상돼도 삭제할 이름을 찾을 수 있도록 목록에는 기본값으로 표시한다.
        let def = data_dir
            .map(|d| registered_file(d, name))
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .map(|v| GateDef::from_json(&v))
            .unwrap_or_else(|| GateDef {
                sentinel: SENTINEL.to_string(),
                round_limit: None,
            });
        let enabled = crate::checklist::marker_present(data_dir, name);
        out.push(summary(format!("user/{name}"), "user", def, enabled));
    }
    out
}

fn summary(id: String, owner: &'static str, def: GateDef, enabled: bool) -> GateSummary {
    GateSummary {
        id,
        owner,
        round_limit_source: if def.round_limit.is_some() {
            "gate"
        } else {
            "settings"
        },
        sentinel: def.sentinel,
        round_limit: def.round_limit,
        enabled,
    }
}

fn require_name<'a>(params: &'a Value, tr: &Translator) -> Result<&'a str, IpcMethodError> {
    params
        .get("name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| IpcMethodError::invalid_params(tr.t("claude.params.missing_name")))
}

pub(crate) fn to_ipc_err(e: GateError, tr: &Translator) -> IpcMethodError {
    IpcMethodError::new(tr.t_fmt("claude.gate.error_prefix", &e.translate(tr)))
}

fn summary_to_json(s: &GateSummary) -> Value {
    json!({
        "id": s.id,
        "owner": s.owner,
        "sentinel": s.sentinel,
        "round_limit": s.round_limit,
        "round_limit_source": s.round_limit_source,
        "enabled": s.enabled,
    })
}

/// 게이트 등록 IPC. CLI에서 전달한 body_file은 경로 정규화를 거친다.
pub(crate) fn handle_register(
    data_dir: Option<&Path>,
    params: &Value,
    tr: &Translator,
) -> Result<Value, IpcMethodError> {
    let name = require_name(params, tr)?;
    let body_file = params
        .get("body_file")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| IpcMethodError::invalid_params(tr.t("claude.params.missing_body_file")))?;
    let sentinel = params
        .get("sentinel")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    // 빈 표식을 미지정으로 처리하지 않고 별도로 거절한다.
    if sentinel.is_none()
        && params
            .get("sentinel")
            .and_then(|v| v.as_str())
            .is_some_and(str::is_empty)
    {
        return Err(to_ipc_err(GateError::EmptySentinel, tr));
    }
    let rounds = params
        .get("rounds")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32);
    register(data_dir, name, Path::new(body_file), sentinel, rounds)
        .map_err(|e| to_ipc_err(e, tr))?;
    Ok(json!({ "id": format!("user/{name}") }))
}

/// `claude.gate_unregister` — `--name <short>`.
pub(crate) fn handle_unregister(
    data_dir: Option<&Path>,
    params: &Value,
    tr: &Translator,
) -> Result<Value, IpcMethodError> {
    let name = require_name(params, tr)?;
    unregister(data_dir, name).map_err(|e| to_ipc_err(e, tr))?;
    Ok(json!({ "id": format!("user/{name}") }))
}

/// `claude.gate_list` — 등록 게이트 + host 기본 게이트.
pub(crate) fn handle_list(
    data_dir: Option<&Path>,
    _params: &Value,
    tr: &Translator,
) -> Result<Value, IpcMethodError> {
    let entries: Vec<Value> = list(data_dir, tr).iter().map(summary_to_json).collect();
    Ok(json!({ "gates": entries }))
}

/// `claude.gate_show` — 정의 + 본문 텍스트.
pub(crate) fn handle_show(
    data_dir: Option<&Path>,
    params: &Value,
    tr: &Translator,
) -> Result<Value, IpcMethodError> {
    let name = require_name(params, tr)?;
    let (owner, def, body) = show(data_dir, name, tr).map_err(|e| to_ipc_err(e, tr))?;
    Ok(json!({
        "id": format!("{owner}/{name}"),
        "owner": owner,
        "sentinel": def.sentinel,
        "round_limit": def.round_limit,
        "round_limit_source": if def.round_limit.is_some() { "gate" } else { "settings" },
        "body": body,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_translator() -> Translator {
        let lang_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lang");
        Translator::load(&lang_dir, "en")
    }

    /// 센티넬을 포함한 본문 파일을 만들어 경로를 돌려준다 — 등록이 통과해야 하는
    /// 케이스의 공통 준비.
    fn body_with(tmp: &Path, name: &str, text: &str) -> PathBuf {
        let p = tmp.join(name);
        std::fs::write(&p, text).unwrap();
        p
    }

    fn write_profile(data_dir: &Path, short_name: &str, content: &str) {
        let dir = data_dir.join("profiles").join("registered");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{short_name}.json")), content).unwrap();
    }

    #[test]
    fn register_copies_body_and_show_returns_it() {
        let tmp = tempfile::tempdir().unwrap();
        let src = body_with(tmp.path(), "b.md", &format!("do the thing\n{SENTINEL}\n"));
        register(Some(tmp.path()), "mygate", &src, None, Some(5)).unwrap();

        assert!(registered_file(tmp.path(), "mygate").is_file());
        assert!(body_file(tmp.path(), "mygate").is_file());

        let (owner, def, body) = show(Some(tmp.path()), "mygate", &test_translator()).unwrap();
        assert_eq!(owner, "user");
        assert_eq!(def.sentinel, SENTINEL);
        assert_eq!(def.round_limit, Some(5));
        assert!(body.contains("do the thing"));

        // 원본이 사라져도 게이트는 살아 있다(복사본 방침).
        std::fs::remove_file(&src).unwrap();
        let (_, _, body) = show(Some(tmp.path()), "mygate", &test_translator()).unwrap();
        assert!(body.contains("do the thing"));
    }

    #[test]
    fn register_rejects_body_without_sentinel() {
        let tmp = tempfile::tempdir().unwrap();
        let src = body_with(tmp.path(), "b.md", "no sentinel here\n");
        let err = register(Some(tmp.path()), "bad", &src, None, None).unwrap_err();
        assert!(matches!(err, GateError::BodyMissingSentinel { .. }));
        // 거부됐으면 아무 파일도 남지 않아야 한다.
        assert!(!registered_file(tmp.path(), "bad").exists());
        assert!(!body_file(tmp.path(), "bad").exists());
    }

    #[test]
    fn register_with_custom_sentinel_validates_against_that_sentinel() {
        let tmp = tempfile::tempdir().unwrap();
        // 기본 센티넬은 있지만 커스텀 센티넬은 없는 본문 → 커스텀 기준으로 거부.
        let only_default = body_with(tmp.path(), "a.md", &format!("{SENTINEL}\n"));
        let err = register(
            Some(tmp.path()),
            "custom",
            &only_default,
            Some("<<MINE>>"),
            None,
        )
        .unwrap_err();
        assert!(matches!(err, GateError::BodyMissingSentinel { .. }));

        // 커스텀 센티넬을 담은 본문은 기본 센티넬이 없어도 통과.
        let only_custom = body_with(tmp.path(), "b.md", "finish with <<MINE>>\n");
        register(
            Some(tmp.path()),
            "custom",
            &only_custom,
            Some("<<MINE>>"),
            None,
        )
        .unwrap();
        let (_, def, _) = show(Some(tmp.path()), "custom", &test_translator()).unwrap();
        assert_eq!(def.sentinel, "<<MINE>>");
    }

    #[test]
    fn register_rejects_empty_sentinel() {
        let tmp = tempfile::tempdir().unwrap();
        let src = body_with(tmp.path(), "b.md", "anything\n");
        let err = register(Some(tmp.path()), "empty", &src, Some(""), None).unwrap_err();
        assert_eq!(err, GateError::EmptySentinel);
    }

    #[test]
    fn register_rejects_invalid_short_name() {
        let tmp = tempfile::tempdir().unwrap();
        let src = body_with(tmp.path(), "b.md", &format!("{SENTINEL}\n"));
        let err = register(Some(tmp.path()), "Bad Name!", &src, None, None).unwrap_err();
        assert!(matches!(err, GateError::InvalidShortName(_)));
    }

    /// 이름 검증이 없으면 데이터 디렉터리 밖의 파일에 닿는 입력을 만든다.
    fn escaping_name() -> &'static str {
        "../../../outside"
    }

    #[test]
    fn unregister_rejects_path_traversal_name_and_leaves_outside_file_intact() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().join("data");
        std::fs::create_dir_all(registered_dir(&data_dir)).unwrap();
        std::fs::create_dir_all(bodies_dir(&data_dir)).unwrap();

        // 검증이 빠지면 unregister 가 지웠을 바로 그 두 경로.
        let victim_def = registered_file(&data_dir, escaping_name());
        let victim_body = body_file(&data_dir, escaping_name());
        std::fs::write(&victim_def, "{}").unwrap();
        std::fs::write(&victim_body, "outside body").unwrap();

        let err = unregister(Some(&data_dir), escaping_name()).unwrap_err();
        assert!(matches!(err, GateError::InvalidShortName(_)));
        assert!(victim_def.is_file(), "data_dir 밖 정의 파일이 삭제됐다");
        assert!(victim_body.is_file(), "data_dir 밖 본문 파일이 삭제됐다");
    }

    #[test]
    fn show_rejects_path_traversal_name_and_does_not_leak_outside_file() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().join("data");
        std::fs::create_dir_all(registered_dir(&data_dir)).unwrap();
        std::fs::create_dir_all(bodies_dir(&data_dir)).unwrap();

        // 검증이 빠지면 show 가 읽어 반환했을 바로 그 두 경로.
        std::fs::write(
            registered_file(&data_dir, escaping_name()),
            "{\"sentinel\":\"X\"}",
        )
        .unwrap();
        std::fs::write(body_file(&data_dir, escaping_name()), "SECRET-BODY\n").unwrap();

        let err = show(Some(&data_dir), escaping_name(), &test_translator()).unwrap_err();
        assert!(matches!(err, GateError::InvalidShortName(_)));

        // data_dir 이 없을 때도 host fallback 으로 새지 않는다.
        let err = show(None, escaping_name(), &test_translator()).unwrap_err();
        assert!(matches!(err, GateError::InvalidShortName(_)));
    }

    #[test]
    fn register_rejects_when_profile_with_same_name_exists() {
        let tmp = tempfile::tempdir().unwrap();
        write_profile(tmp.path(), "clash", "{}");
        let src = body_with(tmp.path(), "b.md", &format!("{SENTINEL}\n"));
        let err = register(Some(tmp.path()), "clash", &src, None, None).unwrap_err();
        assert!(matches!(err, GateError::ProfileNameConflict(_)));
    }

    /// 대칭 방향 — 게이트가 먼저 있으면 동명 프로필 등록이 거부된다.
    #[test]
    fn profile_register_rejects_when_gate_with_same_name_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let body = body_with(tmp.path(), "b.md", &format!("{SENTINEL}\n"));
        register(Some(tmp.path()), "clash", &body, None, None).unwrap();

        let profile_src = tmp.path().join("p.json");
        std::fs::write(&profile_src, "{}").unwrap();
        let err = crate::profile::register(Some(tmp.path()), "clash", &profile_src).unwrap_err();
        assert!(matches!(
            err,
            crate::profile::ProfileError::GateNameConflict(_)
        ));
    }

    #[test]
    fn register_rejects_rounds_below_one() {
        let tmp = tempfile::tempdir().unwrap();
        let src = body_with(tmp.path(), "b.md", &format!("{SENTINEL}\n"));
        let err = register(Some(tmp.path()), "zero", &src, None, Some(0)).unwrap_err();
        assert_eq!(err, GateError::RoundsBelowOne);
    }

    #[test]
    fn register_without_data_dir_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let src = body_with(tmp.path(), "b.md", &format!("{SENTINEL}\n"));
        let err = register(None, "x", &src, None, None).unwrap_err();
        assert_eq!(err, GateError::NoDataDir);
    }

    #[test]
    fn reregister_overwrites_definition_and_body() {
        let tmp = tempfile::tempdir().unwrap();
        let first = body_with(tmp.path(), "a.md", &format!("first\n{SENTINEL}\n"));
        register(Some(tmp.path()), "g", &first, None, Some(2)).unwrap();

        let second = body_with(tmp.path(), "b.md", &format!("second\n{SENTINEL}\n"));
        register(Some(tmp.path()), "g", &second, None, None).unwrap();

        let (_, def, body) = show(Some(tmp.path()), "g", &test_translator()).unwrap();
        assert!(body.contains("second"));
        assert!(!body.contains("first"));
        assert_eq!(def.round_limit, None, "재등록이 정의도 함께 갈아치운다");
    }

    #[test]
    fn unregister_removes_definition_and_body() {
        let tmp = tempfile::tempdir().unwrap();
        let src = body_with(tmp.path(), "b.md", &format!("{SENTINEL}\n"));
        register(Some(tmp.path()), "gone", &src, None, None).unwrap();

        unregister(Some(tmp.path()), "gone").unwrap();
        assert!(!registered_file(tmp.path(), "gone").exists());
        assert!(
            !body_file(tmp.path(), "gone").exists(),
            "등록 해제 뒤 본문 파일도 없어야 한다"
        );

        let err = unregister(Some(tmp.path()), "gone").unwrap_err();
        assert!(matches!(err, GateError::UnknownGate(_)));
    }

    #[test]
    fn list_includes_host_default_and_user_gates() {
        let tmp = tempfile::tempdir().unwrap();
        let src = body_with(tmp.path(), "b.md", &format!("{SENTINEL}\n"));
        register(Some(tmp.path()), "mygate", &src, None, Some(5)).unwrap();

        let entries = list(Some(tmp.path()), &test_translator());
        let host = entries
            .iter()
            .find(|e| e.id == "host/continue-checklist")
            .expect("host default gate is listed");
        assert_eq!(host.sentinel, SENTINEL);
        assert_eq!(host.round_limit, None);
        assert_eq!(host.round_limit_source, "settings");

        let user = entries
            .iter()
            .find(|e| e.id == "user/mygate")
            .expect("user gate is listed");
        assert_eq!(user.round_limit, Some(5));
        assert_eq!(user.round_limit_source, "gate");
    }

    /// 전체 목록에서 게이트별 활성 상태를 확인할 수 있어야 한다.
    #[test]
    fn list_reports_each_gates_enabled_state() {
        let tmp = tempfile::tempdir().unwrap();
        let tr = test_translator();
        let src = body_with(tmp.path(), "b.md", &format!("{SENTINEL}\n"));
        register(Some(tmp.path()), "mygate", &src, None, None).unwrap();

        let off = list(Some(tmp.path()), &tr);
        assert!(
            off.iter().all(|e| !e.enabled),
            "켠 적 없는데 on 으로 보인다"
        );

        crate::checklist::handle_enable(Some(tmp.path()), &json!({ "gate": "mygate" }), &tr)
            .unwrap();
        let on = list(Some(tmp.path()), &tr);
        assert!(on.iter().find(|e| e.id == "user/mygate").unwrap().enabled);
        assert!(
            !on.iter()
                .find(|e| e.id == "host/continue-checklist")
                .unwrap()
                .enabled,
            "다른 게이트를 켰는데 host 기본 게이트가 on 으로 보인다"
        );
    }

    /// 마커 조회가 `data_dir` 없이도 답해야 목록이 host 기본 게이트를 계속 보여준다.
    #[test]
    fn list_without_data_dir_reports_gates_as_off() {
        assert!(list(None, &test_translator()).iter().all(|e| !e.enabled));
    }

    #[test]
    fn list_without_data_dir_shows_only_host_default() {
        let entries = list(None, &test_translator());
        assert!(!entries.is_empty());
        assert!(entries.iter().all(|e| e.owner == "host"));
        assert!(entries.iter().any(|e| e.id == "host/continue-checklist"));
    }

    #[test]
    fn show_falls_back_to_host_default_gate() {
        let tmp = tempfile::tempdir().unwrap();
        let tr = test_translator();
        let (owner, def, body) = show(Some(tmp.path()), "continue-checklist", &tr).unwrap();
        assert_eq!(owner, "host");
        assert_eq!(def.sentinel, SENTINEL);
        assert_eq!(def.round_limit, None);
        assert!(
            body.contains(SENTINEL),
            "host 기본 본문은 센티넬을 포함한다"
        );

        // 모르는 이름은 host 폴백도 없다.
        let err = show(Some(tmp.path()), "nope", &tr).unwrap_err();
        assert!(matches!(err, GateError::UnknownGate(_)));
    }

    #[test]
    fn user_gate_shadows_host_default_of_same_name() {
        let tmp = tempfile::tempdir().unwrap();
        let src = body_with(tmp.path(), "b.md", "my own checklist <<MINE>>\n");
        register(
            Some(tmp.path()),
            "continue-checklist",
            &src,
            Some("<<MINE>>"),
            Some(7),
        )
        .unwrap();

        let (owner, def, body) =
            show(Some(tmp.path()), "continue-checklist", &test_translator()).unwrap();
        assert_eq!(owner, "user");
        assert_eq!(def.sentinel, "<<MINE>>");
        assert_eq!(def.round_limit, Some(7));
        assert!(body.contains("my own checklist"));

        // 목록에는 같은 이름이 두 줄로 나오지 않는다 — 실효 항목 하나만.
        let entries = list(Some(tmp.path()), &test_translator());
        let matching: Vec<&GateSummary> = entries
            .iter()
            .filter(|e| e.id.ends_with("/continue-checklist"))
            .collect();
        assert_eq!(matching.len(), 1);
        assert_eq!(matching[0].id, "user/continue-checklist");
    }

    /// 생략한 완료 표식은 정의 파일에 기본값으로 저장한다.
    #[test]
    fn omitted_sentinel_is_materialized_as_the_default() {
        let tmp = tempfile::tempdir().unwrap();
        let src = body_with(tmp.path(), "b.md", &format!("{SENTINEL}\n"));
        register(Some(tmp.path()), "g", &src, None, None).unwrap();
        let text = std::fs::read_to_string(registered_file(tmp.path(), "g")).unwrap();
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["sentinel"], SENTINEL);
        assert!(v.get("round_limit").is_none());
    }
}

#[cfg(test)]
mod message_tests {
    use super::*;

    /// 충돌 이름과 제거 명령의 두 placeholder를 모두 채워야 한다.
    #[test]
    fn name_conflict_messages_fill_every_placeholder() {
        let lang_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lang");
        for locale in tasty_i18n::BUILTIN_CODES {
            let tr = Translator::load(&lang_dir, locale);

            let gate_side = GateError::ProfileNameConflict("mygate".into()).translate(&tr);
            assert!(gate_side.contains("mygate"), "{locale}: {gate_side}");
            assert!(
                !gate_side.contains("{}"),
                "{locale}: 채워지지 않은 placeholder 가 남았다: {gate_side}"
            );

            let profile_side =
                crate::profile::ProfileError::GateNameConflict("mygate".into()).translate(&tr);
            assert!(profile_side.contains("mygate"), "{locale}: {profile_side}");
            assert!(
                !profile_side.contains("{}"),
                "{locale}: 채워지지 않은 placeholder 가 남았다: {profile_side}"
            );
        }
    }
}
