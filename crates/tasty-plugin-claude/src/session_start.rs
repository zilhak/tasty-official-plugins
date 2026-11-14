//! SessionStart 훅의 처리: 시작 상태(입력 대기면 idle)와 프로필 복원 계획.

use std::path::Path;

use serde_json::Value;

use tasty_plugin_agent_common::host_call::HostCall as HostCallSink;
use tasty_plugin_sdk::i18n::Translator;

use crate::hook::{HostCall, RESTORE_COMMAND_META_KEY};
use crate::profile_attach::{self, AttachRecord};

/// 입력을 기다리는 화면으로 시작한 세션인가. `compact` 는 [`is_compaction`] 이 따로 다루고,
/// 값이 없거나 모르는 값이면 판단하지 않는다.
fn session_start_waits_for_input(source: Option<&str>) -> bool {
    matches!(source, Some("startup" | "resume" | "clear" | "fork"))
}

/// 압축 뒤에 오는 SessionStart(`source: compact`)인가. 압축은 턴 도중에도, 턴이 끝나 쉬는 중에도
/// (Claude Code 의 idle compaction) 일어나며 같은 세션이 이어진다. 턴의 시작도 끝도 아니므로
/// 상태와 턴 경계 처리를 바꾸지 않는다.
pub(crate) fn is_compaction(event: &str, params: &Value) -> bool {
    event == "session-start" && params.get("source").and_then(Value::as_str) == Some("compact")
}

/// 압축의 SessionStart 에서 턴에 속한 호출을 뺀다. 상태 보고와 새 턴이 지우는 meta(직전 턴의 API 오류,
/// 백그라운드 작업 대기)를 남기지 않고, 세션 id·복원 명령 같은 세션 기록만 둔다. 압축은 같은 세션이 이어지므로
/// Monitor 기록도 지우지 않는다.
/// 쉬는 중의 압축이 idle 을 active 로 바꾸면 그 뒤 Stop 이 오지 않아 active 가 계속 남는다.
pub(crate) fn keep_turn_state_across_compaction(calls: &mut Vec<HostCall>) {
    calls.retain(|call| match call {
        HostCall::SetState { .. } => false,
        HostCall::MetaUnset { key, .. } => !matches!(
            *key,
            crate::hook::STOP_FAILURE_META_KEY
                | crate::hook::BACKGROUND_WAIT_META_KEY
                | crate::monitor_tasks::MONITOR_TASKS_META_KEY
        ),
        _ => true,
    });
}

/// surface meta 키 — 플러그인이 프롬프트를 실어 Claude 를 실행했다는 표시. 실행 기록이 쓰고 지우며,
/// 그 실행의 SessionStart 가 읽고 지운다. 그 세션은 시작하자마자 프롬프트를 처리하므로 idle 로 두지 않는다.
pub(crate) const LAUNCH_PROMPT_META_KEY: &str = "claude-launch-prompt";

/// 입력 대기로 시작한 세션은 첫 Stop 전에도 idle 로 기록한다. 그래야 기존 세션 agent task 처럼
/// idle 을 기다리는 소비자가 사용자 턴 없이 진행한다. 완료 훅(`claude-idle`)·화면 알림·
/// telemetry 는 턴이 끝난 것이 아니므로 만들지 않는다. 상태 값만 바꾼다.
/// 프롬프트를 실어 실행한 세션은 곧 그 턴이 시작되므로 active 로 둔다. idle 을 거치면
/// spawn 노드처럼 idle 을 완료로 보는 소비자가 프롬프트를 처리하기 전에 끝난다.
pub(crate) fn session_start_state<H: HostCallSink>(
    calls: &mut Vec<HostCall>,
    host: &H,
    surface_id: u32,
    source: Option<&str>,
) {
    if !session_start_waits_for_input(source) {
        return;
    }
    if crate::stop_pairing::meta_value(host, surface_id, LAUNCH_PROMPT_META_KEY).is_some() {
        calls.push(HostCall::MetaUnset {
            surface_id,
            key: LAUNCH_PROMPT_META_KEY,
        });
        return;
    }
    for call in calls.iter_mut() {
        if let HostCall::SetState { state, .. } = call
            && *state == "active"
        {
            *state = "idle";
        }
    }
}

/// session-start 가 복원할 프로필 계획.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct SessionStartProfile {
    /// 저장 기록에서 복구했을 때 메타데이터를 되돌릴 호출 목록.
    pub meta_calls: Vec<HostCall>,
    /// 이번 세션에 실제로 실을 프로필 파일 경로. `None` 이면 프로필 없이 진행한다.
    pub profile_file: Option<String>,
    /// 프로필을 확인한 뒤 다시 저장할 기록.
    pub restamp: Option<AttachRecord>,
}

/// surface 메타데이터를 먼저 보고 없으면 저장된 프로필 기록을 읽는다.
/// 등록 이름은 매번 다시 해석해 변경된 내용을 반영한다.
/// 실패하면 경고 후 프로필 없이 복원 명령을 남긴다. 프로필 문제로 세션 복원까지 막지 않기 위해서다.
pub(crate) fn plan_session_start_profile(
    session_id: &str,
    meta: &crate::reboot::AttachedProfile,
    data_dir: Option<&Path>,
    tr: &Translator,
) -> SessionStartProfile {
    let (attached, from_record) = match (&meta.names, &meta.path) {
        (Some(names), _) => (Some(AttachRecord::Names(names.clone())), false),
        (None, Some(path)) => (Some(AttachRecord::Path(path.clone())), false),
        (None, None) => (profile_attach::load(data_dir, session_id), true),
    };
    let Some(record) = attached else {
        return SessionStartProfile::default();
    };

    let resolved = match &record {
        AttachRecord::Names(names) => {
            match crate::profile::resolve_names(data_dir, names, tr) {
                Ok(path) => Some(path.to_string_lossy().into_owned()),
                Err(e) => {
                    // 부착 후 unregister 됐거나 파일이 깨진 경우.
                    tracing::warn!(
                        "claude hook session-start {session_id}: profile names {names:?} no longer resolve ({e:?}) — restoring without a profile"
                    );
                    None
                }
            }
        }
        AttachRecord::Path(path) => match crate::reboot::validate_profile_file(path, tr) {
            Ok(()) => Some(path.clone()),
            Err(e) => {
                tracing::warn!(
                    "claude hook session-start {session_id}: attached profile file {path:?} is unusable ({}) — restoring without a profile",
                    e.message
                );
                None
            }
        },
    };

    let Some(profile_file) = resolved else {
        // 실패 시 기존 메타데이터와 기록은 유지해 다음 session-start에서 다시 시도할 수 있게 한다.
        return SessionStartProfile::default();
    };

    let meta_calls = if from_record {
        vec![match &record {
            AttachRecord::Names(names) => HostCall::MetaSet {
                surface_id: 0,
                key: crate::reboot::PROFILE_NAMES_META_KEY,
                value: names.clone(),
            },
            AttachRecord::Path(path) => HostCall::MetaSet {
                surface_id: 0,
                key: crate::reboot::PROFILE_META_KEY,
                value: path.clone(),
            },
        }]
    } else {
        Vec::new()
    };

    SessionStartProfile {
        meta_calls,
        profile_file: Some(profile_file),
        restamp: Some(record),
    }
}

/// 복원 명령에 settings를 반영하고 기록에서 복구할 메타데이터 호출을 덧붙인다.
pub(crate) fn apply_session_start_profile(
    calls: &mut Vec<HostCall>,
    surface_id: u32,
    session_id: &str,
    plan: &SessionStartProfile,
) {
    if let Some(path) = &plan.profile_file {
        for call in calls.iter_mut() {
            if let HostCall::MetaSet { key, value, .. } = call
                && *key == RESTORE_COMMAND_META_KEY
            {
                *value = crate::reboot::resume_command_line(session_id, Some(path));
            }
        }
    }
    calls.extend(plan.meta_calls.iter().map(|call| match call {
        // 프로필 선택 단계에서 알 수 없던 대상 surface를 채운다.
        HostCall::MetaSet { key, value, .. } => HostCall::MetaSet {
            surface_id,
            key,
            value: value.clone(),
        },
        other => other.clone(),
    }));
}

/// 세션 시작·종료에서 프로필 복원 기록과 settings meta 를 맞추는 호출을 덧붙인다.
#[allow(clippy::too_many_arguments)] // reason: 훅 처리기가 가진 입력을 그대로 넘겨받는다.
pub(crate) fn session_lifecycle_calls<H: HostCallSink>(
    calls: &mut Vec<HostCall>,
    host: &H,
    event: &str,
    surface_id: u32,
    session: Option<&str>,
    params: &Value,
    data_dir: Option<&Path>,
    tr: &Translator,
) {
    // surface 메타데이터는 복원 시 유지되지 않으므로 복원 명령에도 프로필을 넣는다.
    if event == "session-start"
        && let Some(session_id) = session
    {
        match crate::session_options::stale_start(host, surface_id, session_id, tr) {
            Ok(false) => {}
            other => {
                // Also protect callers that plan lifecycle calls without the full hook handler.
                calls.clear();
                tracing::warn!(
                    "claude session-start ignored while launch ownership cannot be established: {other:?}"
                );
                return;
            }
        }
        let meta = crate::reboot::attached_profile_summary(host, surface_id);
        let plan = plan_session_start_profile(session_id, &meta, data_dir, tr);
        apply_session_start_profile(calls, surface_id, session_id, &plan);
        crate::session_options::session_start(calls, host, surface_id, data_dir, session_id, tr);

        calls.extend(crate::stop_pairing::settings_file_for_resumed_session(
            surface_id,
            params.get("source").and_then(Value::as_str),
            plan.profile_file.as_deref(),
        ));
        calls.push(HostCall::MetaSet {
            surface_id,
            key: crate::stop_pairing::SETTINGS_SESSION_META_KEY,
            value: session_id.to_string(),
        });
        // reboot 중 session-end가 남긴 종료 표시를 새 session-start 기록으로 갱신한다.
        if let Some(record) = &plan.restamp {
            profile_attach::store(data_dir, session_id, record);
        }
        // 종료 훅이 오지 않은 세션의 오래된 기록도 정리한다.
        profile_attach::sweep(data_dir);
    }
    if event == "session-end" {
        let reason = params.get("reason").and_then(Value::as_str);
        if !matches!(reason, Some("clear" | "resume"))
            && session.is_some()
            && crate::stop_pairing::settings_owner_session(host, surface_id).as_deref() == session
        {
            // Keep the session file for closed-tab restore, but do not attach these options to
            // an unrelated process later started manually in the same terminal.
            calls.push(HostCall::MetaUnset {
                surface_id,
                key: crate::session_options::META_KEY,
            });
        }
        calls.extend(crate::stop_pairing::settings_file_after_session_end(
            surface_id,
            params.get("reason").and_then(Value::as_str),
            session,
            crate::stop_pairing::settings_owner_session(host, surface_id).as_deref(),
        ));
    }
}
