//! 이름으로 등록한 Claude 설정 파일을 세션에 부착한다.
//!
//! 이름은 등록 프로필, 등록·기본 게이트, 내장 훅 순으로 해석한다.
//! 게이트는 Stop 훅 설정으로 변환하며 내장 훅은 조회만 허용한다.
//! 프로필과 등록 게이트의 이름이 겹치면 등록을 거부한다.
//!
//! `TASTY_PLUGIN_DATA_DIR` 아래에 두 종류의 파일을 둔다.
//! - `profiles/registered/<short-name>.json`: 등록할 때 받은 파일의 복사본
//! - `profiles/generated/<sorted-names>.json`: 부착할 때마다 다시 병합한 결과
//!
//! 데이터 경로가 없으면 등록·부착을 거부한다. 다른 경로에 대신 쓰지 않는다.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use tasty_plugin_sdk::{HostHandle, IpcMethodError, i18n::Translator};
use tracing::warn;

use crate::install::MANAGED_HOOKS;
use crate::profile_merge::{MergeError, merge_contents};

/// 파일명으로 쓰므로 소문자·숫자·하이픈만 허용하고 최대 32자로 제한한다.
fn is_valid_short_name(s: &str) -> bool {
    if s.is_empty() || s.len() > 32 {
        return false;
    }
    s.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileError {
    /// 호스트가 `TASTY_PLUGIN_DATA_DIR` 를 주입하지 않은 비정상 기동.
    NoDataDir,
    InvalidShortName(String),
    /// 이름 목록이 비어 있음(`--profile ""` 등).
    EmptyNames,
    UnknownProfile(String),
    /// 조회 전용 항목(내장 훅)은 이름으로 부착할 수 없다.
    NotAttachable(String),
    /// 같은 이름의 게이트가 있어 프로필을 등록할 수 없다.
    GateNameConflict(String),
    SourceNotReadable {
        path: String,
        message: String,
    },
    SourceNotJsonObject {
        path: String,
    },
    Merge(MergeError),
    Io {
        path: String,
        message: String,
    },
}

impl ProfileError {
    pub(crate) fn translate(&self, tr: &Translator) -> String {
        match self {
            Self::NoDataDir => tr.t("claude.profile.no_data_dir").to_string(),
            Self::InvalidShortName(s) => tr.t_fmt("claude.profile.invalid_short_name", s),
            Self::EmptyNames => tr.t("claude.profile.empty_names").to_string(),
            Self::UnknownProfile(id) => tr.t_fmt("claude.profile.unknown_profile", id),
            Self::NotAttachable(id) => tr.t_fmt("claude.profile.not_attachable", id),
            // t_fmt는 첫 자리만 채우므로 두 자리 모두 직접 치환한다.
            Self::GateNameConflict(name) => {
                tr.t_args("claude.profile.gate_name_conflict", &[name, name])
            }
            Self::SourceNotReadable { path, message } => {
                tr.t_args("claude.profile.source_not_readable", &[path, message])
            }
            Self::SourceNotJsonObject { path } => {
                tr.t_fmt("claude.profile.source_not_json_object", path)
            }
            Self::Merge(e) => e.translate(tr),
            Self::Io { path, message } => tr.t_args("claude.profile.io_error", &[path, message]),
        }
    }
}

/// `claude profile list` 응답에 넣는 항목 요약.
#[derive(Debug, Clone)]
pub struct ProfileSummary {
    /// `<owner>/<short>` 형식.
    pub id: String,
    pub owner: &'static str,
    /// 이름으로 부착할 수 있는지 여부. 내장 훅은 false다.
    pub attachable: bool,
    pub description: Option<String>,
}

fn registered_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("profiles").join("registered")
}

fn generated_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("profiles").join("generated")
}

fn registered_file(data_dir: &Path, short_name: &str) -> PathBuf {
    registered_dir(data_dir).join(format!("{short_name}.json"))
}

fn require_data_dir(data_dir: Option<&Path>) -> Result<&Path, ProfileError> {
    data_dir.ok_or(ProfileError::NoDataDir)
}

/// 게이트 등록 시 같은 이름의 프로필이 있는지 확인한다.
pub(crate) fn is_registered(data_dir: Option<&Path>, short_name: &str) -> bool {
    data_dir.is_some_and(|d| registered_file(d, short_name).is_file())
}

fn parse_names(names_csv: &str) -> Result<Vec<String>, ProfileError> {
    let names: Vec<String> = names_csv
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    if names.is_empty() {
        return Err(ProfileError::EmptyNames);
    }
    for n in &names {
        if !is_valid_short_name(n) {
            return Err(ProfileError::InvalidShortName(n.clone()));
        }
    }
    Ok(names)
}

/// JSON 객체인지 확인한 뒤 등록 파일로 복사한다. 같은 이름이 있으면 덮어쓴다.
/// 등록 뒤에는 사용자가 준 원본 파일의 이동·삭제에 영향을 받지 않는다.
pub(crate) fn register(
    data_dir: Option<&Path>,
    short_name: &str,
    source_path: &Path,
) -> Result<(), ProfileError> {
    if !is_valid_short_name(short_name) {
        return Err(ProfileError::InvalidShortName(short_name.to_string()));
    }
    let data_dir = require_data_dir(data_dir)?;
    if crate::gate::is_registered(Some(data_dir), short_name) {
        return Err(ProfileError::GateNameConflict(short_name.to_string()));
    }
    let text =
        std::fs::read_to_string(source_path).map_err(|e| ProfileError::SourceNotReadable {
            path: source_path.display().to_string(),
            message: e.to_string(),
        })?;
    let value: Value =
        serde_json::from_str(&text).map_err(|e| ProfileError::SourceNotReadable {
            path: source_path.display().to_string(),
            message: e.to_string(),
        })?;
    if !value.is_object() {
        return Err(ProfileError::SourceNotJsonObject {
            path: source_path.display().to_string(),
        });
    }
    let dir = registered_dir(data_dir);
    std::fs::create_dir_all(&dir).map_err(|e| ProfileError::Io {
        path: dir.display().to_string(),
        message: e.to_string(),
    })?;
    let dest = registered_file(data_dir, short_name);
    std::fs::write(&dest, text).map_err(|e| ProfileError::Io {
        path: dest.display().to_string(),
        message: e.to_string(),
    })?;
    Ok(())
}

/// `short_name` 등록을 해제한다. 없으면 `UnknownProfile`.
pub(crate) fn unregister(data_dir: Option<&Path>, short_name: &str) -> Result<(), ProfileError> {
    let data_dir = require_data_dir(data_dir)?;
    let path = registered_file(data_dir, short_name);
    if !path.is_file() {
        return Err(ProfileError::UnknownProfile(format!("user/{short_name}")));
    }
    std::fs::remove_file(&path).map_err(|e| ProfileError::Io {
        path: path.display().to_string(),
        message: e.to_string(),
    })?;
    Ok(())
}

/// 등록 파일이나 게이트 설정과 실제 출처(user/host)를 반환한다.
pub(crate) fn show_registered(
    data_dir: Option<&Path>,
    short_name: &str,
    tr: &Translator,
) -> Result<(&'static str, Value), ProfileError> {
    let data_dir = require_data_dir(data_dir)?;
    let path = registered_file(data_dir, short_name);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        // 등록 프로필이 없으면 게이트를 조회한다.
        Err(_) => {
            if let Some((owner, value)) =
                crate::gate::attach_profile(Some(data_dir), short_name, tr)
            {
                return Ok((owner, value));
            }
            return Err(ProfileError::UnknownProfile(format!("user/{short_name}")));
        }
    };
    let value = serde_json::from_str(&text).map_err(|e| ProfileError::Io {
        path: path.display().to_string(),
        message: e.to_string(),
    })?;
    Ok(("user", value))
}

/// 프로필·게이트·내장 훅을 출처와 ID 순으로 나열한다.
pub(crate) fn list(data_dir: Option<&Path>, tr: &Translator) -> Vec<ProfileSummary> {
    let mut out: Vec<ProfileSummary> = MANAGED_HOOKS
        .iter()
        .map(|(claude_event, token, matcher)| ProfileSummary {
            id: format!("host/{token}"),
            owner: "host",
            attachable: false,
            description: Some(if matcher.is_empty() {
                tr.t_fmt("claude.profile.builtin_always_installed", claude_event)
            } else {
                tr.t_args(
                    "claude.profile.builtin_always_installed_with_matcher",
                    &[claude_event, matcher],
                )
            }),
        })
        .collect();

    for name in crate::gate::host_default_names() {
        out.push(ProfileSummary {
            id: format!("host/{name}"),
            owner: "host",
            attachable: true,
            description: Some(tr.t("claude.profile.gate_attachable").to_string()),
        });
    }

    if let Some(data_dir) = data_dir {
        let profile_names = short_names_in(&registered_dir(data_dir));
        for short in &profile_names {
            out.push(ProfileSummary {
                id: format!("user/{short}"),
                owner: "user",
                attachable: true,
                description: None,
            });
        }
        // 부착할 수 있는 게이트도 설명을 붙여 목록에 포함한다.
        for short in crate::gate::registered_names(data_dir) {
            // 동명 항목이 있으면 실제 부착 순서와 같이 프로필을 우선한다.
            if profile_names.contains(&short) {
                continue;
            }
            out.push(ProfileSummary {
                id: format!("user/{short}"),
                owner: "user",
                attachable: true,
                description: Some(tr.t("claude.profile.gate_attachable").to_string()),
            });
        }
    }
    out
}

/// 디렉토리의 `<이름>.json` 파일 이름을 정렬해 돌려준다.
fn short_names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
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

/// 입력 순서대로 프로필을 병합하고 generated/ 아래에 쓴다.
/// 이름이 하나여도 다시 읽어 쓰므로 재부착 시 등록 내용의 변경을 반영한다.
pub(crate) fn resolve_names(
    data_dir: Option<&Path>,
    names_csv: &str,
    tr: &Translator,
) -> Result<PathBuf, ProfileError> {
    let data_dir = require_data_dir(data_dir)?;
    let names = parse_names(names_csv)?;

    let mut contents = Vec::with_capacity(names.len());
    for name in &names {
        let path = registered_file(data_dir, name);
        if !path.is_file() {
            if let Some((owner, value)) = crate::gate::attach_profile(Some(data_dir), name, tr) {
                contents.push((format!("{owner}/{name}"), value));
                continue;
            }
            // 내장 훅은 목록에서 조회만 할 수 있다.
            if MANAGED_HOOKS.iter().any(|(_, token, _)| token == name) {
                return Err(ProfileError::NotAttachable(format!("host/{name}")));
            }
            return Err(ProfileError::UnknownProfile(format!("user/{name}")));
        }
        let text = std::fs::read_to_string(&path).map_err(|e| ProfileError::Io {
            path: path.display().to_string(),
            message: e.to_string(),
        })?;
        let value: Value = serde_json::from_str(&text).map_err(|e| ProfileError::Io {
            path: path.display().to_string(),
            message: e.to_string(),
        })?;
        contents.push((format!("user/{name}"), value));
    }

    let (merged, warnings) = merge_contents(&contents).map_err(ProfileError::Merge)?;
    for w in &warnings {
        warn!("claude profile resolve({names_csv}): {w}");
    }

    let mut sorted_names = names.clone();
    sorted_names.sort();
    let key = sorted_names.join("+");
    let dir = generated_dir(data_dir);
    std::fs::create_dir_all(&dir).map_err(|e| ProfileError::Io {
        path: dir.display().to_string(),
        message: e.to_string(),
    })?;
    let out_path = dir.join(format!("{key}.json"));
    let text = serde_json::to_string_pretty(&merged).unwrap_or_else(|_| "{}".to_string());
    std::fs::write(&out_path, text).map_err(|e| ProfileError::Io {
        path: out_path.display().to_string(),
        message: e.to_string(),
    })?;
    Ok(out_path)
}

fn require_name<'a>(params: &'a Value, tr: &Translator) -> Result<&'a str, IpcMethodError> {
    params
        .get("name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| IpcMethodError::invalid_params(tr.t("claude.params.missing_name")))
}

pub(crate) fn to_ipc_err(e: ProfileError, tr: &Translator) -> IpcMethodError {
    IpcMethodError::new(tr.t_fmt("claude.profile.error_prefix", &e.translate(tr)))
}

fn summary_to_json(s: &ProfileSummary) -> Value {
    json!({
        "id": s.id,
        "owner": s.owner,
        "attachable": s.attachable,
        "description": s.description,
    })
}

/// 파일을 프로필로 등록한다. CLI 경로는 path_kind="file"로 정규화된다.
pub(crate) fn handle_register(
    data_dir: Option<&Path>,
    params: &Value,
    tr: &Translator,
) -> Result<Value, IpcMethodError> {
    let name = require_name(params, tr)?;
    let file = params
        .get("file")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| IpcMethodError::invalid_params(tr.t("claude.params.missing_file")))?;
    register(data_dir, name, Path::new(file)).map_err(|e| to_ipc_err(e, tr))?;
    Ok(json!({ "id": format!("user/{name}") }))
}

/// `claude.profile_unregister` — `--name <short>`.
pub(crate) fn handle_unregister(
    data_dir: Option<&Path>,
    params: &Value,
    tr: &Translator,
) -> Result<Value, IpcMethodError> {
    let name = require_name(params, tr)?;
    unregister(data_dir, name).map_err(|e| to_ipc_err(e, tr))?;
    Ok(json!({ "id": format!("user/{name}") }))
}

/// `claude.profile_list` — 등록된 프로필 + 내장 훅 listing 항목 전체.
pub(crate) fn handle_list(
    data_dir: Option<&Path>,
    _params: &Value,
    tr: &Translator,
) -> Result<Value, IpcMethodError> {
    let entries: Vec<Value> = list(data_dir, tr).iter().map(summary_to_json).collect();
    Ok(json!({ "profiles": entries }))
}

/// `claude.profile_show` — `--name <short>` 로 등록된 원본 JSON 내용을 그대로 반환.
pub(crate) fn handle_show(
    data_dir: Option<&Path>,
    params: &Value,
    tr: &Translator,
) -> Result<Value, IpcMethodError> {
    let name = require_name(params, tr)?;
    let (owner, content) = show_registered(data_dir, name, tr).map_err(|e| to_ipc_err(e, tr))?;
    Ok(json!({ "id": format!("{owner}/{name}"), "content": content }))
}

/// 세션의 부착 기록과 내장 훅 목록을 반환한다.
pub(crate) fn handle_current(
    data_dir: Option<&Path>,
    host: &HostHandle,
    params: &Value,
    tr: &Translator,
) -> Result<Value, IpcMethodError> {
    let surface_id = crate::handlers::require_target_surface(params, tr)?;
    let attached = crate::reboot::attached_profile_summary(host, surface_id);
    let builtins: Vec<Value> = list(data_dir, tr)
        .iter()
        .filter(|s| !s.attachable)
        .map(summary_to_json)
        .collect();
    Ok(json!({
        "surface_id": surface_id,
        "attached_names": attached.names,
        "attached_path": attached.path,
        "builtin": builtins,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_profile(dir: &Path, name: &str, content: &str) {
        std::fs::create_dir_all(registered_dir(dir)).unwrap();
        std::fs::write(registered_file(dir, name), content).unwrap();
    }

    /// 게이트를 실제 등록 경로로 만든다 — 본문이 센티넬을 포함해야 통과한다.
    fn register_gate(dir: &Path, name: &str) {
        let body = dir.join(format!("{name}-body.md"));
        std::fs::write(&body, format!("{name} 본문\n[[{name}-DONE]]\n")).unwrap();
        crate::gate::register(
            Some(dir),
            name,
            &body,
            Some(&format!("[[{name}-DONE]]")),
            Some(2),
        )
        .unwrap();
    }

    /// 해석 결과 파일에서 Stop 훅 command 문자열들을 뽑는다.
    fn stop_commands(path: &Path) -> Vec<String> {
        let content: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        content["hooks"]["Stop"]
            .as_array()
            .expect("Stop 훅 배열")
            .iter()
            .flat_map(|entry| entry["hooks"].as_array().cloned().unwrap_or_default())
            .filter_map(|h| h["command"].as_str().map(str::to_string))
            .collect()
    }

    fn test_translator() -> Translator {
        let lang_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lang");
        Translator::load(&lang_dir, "en")
    }

    #[test]
    fn register_copies_source_and_show_returns_it() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src.json");
        std::fs::write(&src, r#"{"env":{"A":"1"}}"#).unwrap();
        register(Some(tmp.path()), "myprofile", &src).unwrap();
        let (owner, shown) =
            show_registered(Some(tmp.path()), "myprofile", &test_translator()).unwrap();
        assert_eq!(owner, "user");
        assert_eq!(shown["env"]["A"], "1");
    }

    #[test]
    fn register_rejects_non_object_json() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src.json");
        std::fs::write(&src, r#"["not","object"]"#).unwrap();
        let err = register(Some(tmp.path()), "bad", &src).unwrap_err();
        assert!(matches!(err, ProfileError::SourceNotJsonObject { .. }));
    }

    #[test]
    fn register_rejects_invalid_short_name() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src.json");
        std::fs::write(&src, r#"{}"#).unwrap();
        let err = register(Some(tmp.path()), "Bad Name!", &src).unwrap_err();
        assert!(matches!(err, ProfileError::InvalidShortName(_)));
    }

    #[test]
    fn register_without_data_dir_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src.json");
        std::fs::write(&src, r#"{}"#).unwrap();
        let err = register(None, "x", &src).unwrap_err();
        assert_eq!(err, ProfileError::NoDataDir);
    }

    #[test]
    fn unregister_removes_and_second_call_errors() {
        let tmp = tempfile::tempdir().unwrap();
        write_profile(tmp.path(), "gone", "{}");
        unregister(Some(tmp.path()), "gone").unwrap();
        let err = unregister(Some(tmp.path()), "gone").unwrap_err();
        assert!(matches!(err, ProfileError::UnknownProfile(_)));
    }

    #[test]
    fn list_includes_builtin_hooks_and_user_profiles() {
        let tmp = tempfile::tempdir().unwrap();
        write_profile(tmp.path(), "myprofile", "{}");
        let entries = list(Some(tmp.path()), &test_translator());
        assert!(entries.iter().any(|e| e.id == "host/stop" && !e.attachable));
        assert!(
            entries
                .iter()
                .any(|e| e.id == "user/myprofile" && e.attachable)
        );
    }

    #[test]
    fn list_without_data_dir_still_shows_builtins() {
        let entries = list(None, &test_translator());
        assert!(!entries.is_empty());
        assert!(entries.iter().all(|e| e.owner == "host"));
    }

    #[test]
    fn resolve_single_name_materializes_generated_file() {
        let tmp = tempfile::tempdir().unwrap();
        write_profile(tmp.path(), "solo", r#"{"env":{"A":"1"}}"#);
        let path = resolve_names(Some(tmp.path()), "solo", &test_translator()).unwrap();
        assert!(path.starts_with(generated_dir(tmp.path())));
        let content: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(content["env"]["A"], "1");
    }

    #[test]
    fn resolve_two_names_merges_both() {
        let tmp = tempfile::tempdir().unwrap();
        write_profile(tmp.path(), "one", r#"{"env":{"A":"1"}}"#);
        write_profile(tmp.path(), "two", r#"{"env":{"B":"2"}}"#);
        let path = resolve_names(Some(tmp.path()), "one,two", &test_translator()).unwrap();
        let content: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(content["env"]["A"], "1");
        assert_eq!(content["env"]["B"], "2");
    }

    #[test]
    fn resolve_is_order_independent_for_generated_filename() {
        let tmp = tempfile::tempdir().unwrap();
        write_profile(tmp.path(), "one", "{}");
        write_profile(tmp.path(), "two", "{}");
        let a = resolve_names(Some(tmp.path()), "one,two", &test_translator()).unwrap();
        let b = resolve_names(Some(tmp.path()), "two,one", &test_translator()).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn resolve_unknown_name_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let err = resolve_names(Some(tmp.path()), "nope", &test_translator()).unwrap_err();
        assert!(matches!(err, ProfileError::UnknownProfile(_)));
    }

    #[test]
    fn resolve_builtin_hook_name_is_not_attachable() {
        let tmp = tempfile::tempdir().unwrap();
        let err = resolve_names(Some(tmp.path()), "stop", &test_translator()).unwrap_err();
        assert!(matches!(err, ProfileError::NotAttachable(_)));
    }

    #[test]
    fn resolve_reflects_latest_registered_content_on_reattach() {
        let tmp = tempfile::tempdir().unwrap();
        write_profile(tmp.path(), "changing", r#"{"env":{"A":"1"}}"#);
        let first = resolve_names(Some(tmp.path()), "changing", &test_translator()).unwrap();
        let first_content: Value =
            serde_json::from_str(&std::fs::read_to_string(&first).unwrap()).unwrap();
        assert_eq!(first_content["env"]["A"], "1");

        write_profile(tmp.path(), "changing", r#"{"env":{"A":"2"}}"#);
        let second = resolve_names(Some(tmp.path()), "changing", &test_translator()).unwrap();
        let second_content: Value =
            serde_json::from_str(&std::fs::read_to_string(&second).unwrap()).unwrap();
        assert_eq!(second_content["env"]["A"], "2");
    }

    #[test]
    fn resolve_host_default_profile_by_name() {
        let tmp = tempfile::tempdir().unwrap();
        let path =
            resolve_names(Some(tmp.path()), "continue-checklist", &test_translator()).unwrap();
        let content: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(content["hooks"]["Stop"].is_array());
        // host 기본 게이트도 다른 게이트와 같은 경로로 부착된다.
        assert_eq!(
            stop_commands(&path),
            vec![crate::install::tasty_guarded_command(
                &crate::gate::gate_hook_argv("continue-checklist")
            )]
        );
    }

    #[test]
    fn resolve_registered_gate_by_name() {
        let tmp = tempfile::tempdir().unwrap();
        register_gate(tmp.path(), "mygate");
        let path = resolve_names(Some(tmp.path()), "mygate", &test_translator()).unwrap();
        let content: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(
            content["hooks"]["Stop"].is_array(),
            "등록 게이트가 Stop 훅으로 해석되지 않았다: {content}"
        );
    }

    #[test]
    fn generated_hook_command_contains_gate_flag() {
        let tmp = tempfile::tempdir().unwrap();
        register_gate(tmp.path(), "mygate");
        let path = resolve_names(Some(tmp.path()), "mygate", &test_translator()).unwrap();
        let commands = stop_commands(&path);
        assert_eq!(commands.len(), 1);
        assert!(
            commands[0].contains("checklist-hook --gate mygate"),
            "훅 명령에 게이트 이름이 실리지 않으면 host 기본 게이트로 판정된다: {}",
            commands[0]
        );
    }

    /// 설치 경로와 동일한 명령 생성 함수를 사용하는지 확인한다.
    #[test]
    fn generated_hook_command_uses_guarded_form() {
        let tmp = tempfile::tempdir().unwrap();
        register_gate(tmp.path(), "mygate");
        let path = resolve_names(Some(tmp.path()), "mygate", &test_translator()).unwrap();
        assert_eq!(
            stop_commands(&path),
            vec![crate::install::tasty_guarded_command(
                &crate::gate::gate_hook_argv("mygate")
            )]
        );
    }

    #[test]
    fn registered_profile_shadows_gate_of_same_name() {
        let tmp = tempfile::tempdir().unwrap();
        register_gate(tmp.path(), "mygate");
        // 등록 충돌 검사를 거치지 않고 동명 파일을 직접 만든다.
        write_profile(tmp.path(), "mygate", r#"{"env":{"FROM_PROFILE":"1"}}"#);

        let path = resolve_names(Some(tmp.path()), "mygate", &test_translator()).unwrap();
        let content: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(content["env"]["FROM_PROFILE"], "1");
        assert!(content["hooks"].is_null(), "게이트가 프로필을 덮었다");

        let (owner, shown) =
            show_registered(Some(tmp.path()), "mygate", &test_translator()).unwrap();
        assert_eq!(owner, "user");
        assert_eq!(shown["env"]["FROM_PROFILE"], "1");

        // 목록에도 한 줄로만 나온다.
        let entries = list(Some(tmp.path()), &test_translator());
        assert_eq!(entries.iter().filter(|e| e.id == "user/mygate").count(), 1);
    }

    #[test]
    fn show_registered_reports_user_owner_for_registered_gate() {
        let tmp = tempfile::tempdir().unwrap();
        register_gate(tmp.path(), "mygate");
        let (owner, content) =
            show_registered(Some(tmp.path()), "mygate", &test_translator()).unwrap();
        assert_eq!(owner, "user", "등록 게이트인데 host 로 보고했다");
        assert!(content["hooks"]["Stop"].is_array());
    }

    #[test]
    fn list_shows_registered_gates_as_attachable() {
        let tmp = tempfile::tempdir().unwrap();
        register_gate(tmp.path(), "mygate");
        write_profile(tmp.path(), "myprofile", "{}");
        let entries = list(Some(tmp.path()), &test_translator());

        let gate = entries
            .iter()
            .find(|e| e.id == "user/mygate")
            .expect("등록 게이트가 목록에 없다");
        assert!(gate.attachable);
        assert!(
            gate.description.is_some(),
            "게이트는 등록 프로필과 구분되는 설명을 가져야 한다"
        );
        let profile = entries.iter().find(|e| e.id == "user/myprofile").unwrap();
        assert!(profile.attachable && profile.description.is_none());
    }

    /// 게이트 둘을 함께 부착하면 각각의 Stop 훅이 남아야 한다.
    #[test]
    fn resolve_two_gates_registers_two_stop_hooks() {
        let tmp = tempfile::tempdir().unwrap();
        register_gate(tmp.path(), "gate-a");
        let path = resolve_names(
            Some(tmp.path()),
            "gate-a,continue-checklist",
            &test_translator(),
        )
        .unwrap();
        let commands = stop_commands(&path);
        assert_eq!(commands.len(), 2, "Stop 훅이 하나로 합쳐졌다: {commands:?}");
        assert!(commands.iter().any(|c| c.contains("--gate gate-a")));
        assert!(
            commands
                .iter()
                .any(|c| c.contains("--gate continue-checklist"))
        );
    }

    #[test]
    fn resolve_prefers_user_registration_over_host_default() {
        let tmp = tempfile::tempdir().unwrap();
        write_profile(
            tmp.path(),
            "continue-checklist",
            r#"{"env":{"OVERRIDDEN":"1"}}"#,
        );
        let path =
            resolve_names(Some(tmp.path()), "continue-checklist", &test_translator()).unwrap();
        let content: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(content["env"]["OVERRIDDEN"], "1");
        assert!(content["hooks"].is_null());
    }

    #[test]
    fn show_registered_falls_back_to_host_default() {
        let tmp = tempfile::tempdir().unwrap();
        let (owner, content) =
            show_registered(Some(tmp.path()), "continue-checklist", &test_translator()).unwrap();
        assert_eq!(owner, "host");
        assert!(content["hooks"]["Stop"].is_array());
    }

    #[test]
    fn list_includes_attachable_host_default_profile() {
        let entries = list(None, &test_translator());
        assert!(
            entries
                .iter()
                .any(|e| e.id == "host/continue-checklist" && e.attachable)
        );
    }
}
