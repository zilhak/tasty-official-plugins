//! CLI가 Codex 호출자에게 붙이는 예약 문맥(`params._tasty_context`).
//!
//! CLI는 호출 환경의 `CODEX_THREAD_ID`를 검증해 넣는다. 이 값은 같은 사용자의 프로세스가
//! 알려 준 것이며 인증된 신원이 아니다. 수신 대상과 Codex 실행 환경을 고르는 데 쓴다.

use std::path::PathBuf;

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CallerContext {
    pub thread: String,
    pub codex_home: Option<PathBuf>,
    pub execution: Option<crate::execution::CodexExecution>,
    /// CLI가 TASTY_SURFACE_ID로 채운 인자 이름. 사용자가 지정한 값과 구분한다.
    pub implicit: Vec<String>,
}

impl CallerContext {
    /// 사용자가 직접 지정한 인자인지. CLI가 채운 값은 수신 대상을 정하는 근거가 아니다.
    pub(crate) fn is_explicit(&self, params: &Value, key: &str) -> bool {
        params.get(key).is_some_and(|v| !v.is_null()) && !self.implicit.iter().any(|k| k == key)
    }
}

/// Codex 호출 문맥을 읽는다. 문맥이 없거나 다른 provider면 None이고, 형식이 틀리면 Err다.
pub(crate) fn codex_caller(params: &Value) -> Result<Option<CallerContext>, String> {
    let Some(ctx) = params.get("_tasty_context").filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    if ctx.get("provider").and_then(Value::as_str) != Some("codex") {
        return Ok(None);
    }
    let thread = ctx
        .get("thread_id")
        .and_then(Value::as_str)
        .filter(|t| crate::ids::is_uuid(t))
        .ok_or("_tasty_context.thread_id must be a UUID")?
        .to_ascii_lowercase();
    let codex_home = match ctx.get("codex_home") {
        None | Some(Value::Null) => None,
        Some(v) => Some(
            v.as_str()
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .ok_or("_tasty_context.codex_home must be an absolute path")?,
        ),
    };
    let execution = ctx
        .get("execution")
        .filter(|v| !v.is_null())
        .map(|v| {
            serde_json::from_value::<crate::execution::CodexExecution>(v.clone())
                .map_err(|e| format!("_tasty_context.execution: {e}"))
        })
        .transpose()?;
    if let Some(execution) = &execution {
        execution
            .validate()
            .map_err(|e| format!("_tasty_context.execution: {e}"))?;
    }
    let implicit = ctx
        .get("implicit")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    Ok(Some(CallerContext {
        thread,
        codex_home,
        execution,
        implicit,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn malformed_execution_cannot_be_used_as_a_relative_plugin_command() {
        let root = tempfile::tempdir().unwrap();
        for execution in [
            json!({"executable":"codex", "search_path":root.path()}),
            json!({"executable":root.path().join("codex"), "search_path":"relative"}),
            json!({"executable":root.path().join("codex")}),
        ] {
            assert!(
                codex_caller(&json!({"_tasty_context":{
                    "provider":"codex", "thread_id":"01a12f74-dab9-7a00-8000-0123456789ab",
                    "execution":execution
                }}))
                .is_err()
            );
        }
    }

    #[test]
    fn only_a_codex_context_with_a_uuid_is_a_codex_caller() {
        assert_eq!(codex_caller(&json!({})), Ok(None));
        assert_eq!(
            codex_caller(&json!({"_tasty_context": {"provider": "claude", "thread_id": "x"}})),
            Ok(None)
        );
        assert!(
            codex_caller(&json!({"_tasty_context": {"provider": "codex", "thread_id": "x"}}))
                .is_err()
        );
        assert!(
            codex_caller(&json!({"_tasty_context": {"provider": "codex",
                "thread_id": "01a12f74-dab9-7a00-8000-0123456789ab", "codex_home": "rel"}}))
            .is_err()
        );
        let c = codex_caller(&json!({"_tasty_context": {"provider": "codex",
            "thread_id": "01A12F74-DAB9-7A00-8000-0123456789AB", "implicit": ["surface"]}}))
        .unwrap()
        .unwrap();
        assert_eq!(c.thread, "01a12f74-dab9-7a00-8000-0123456789ab");
        assert!(!c.is_explicit(&json!({"surface": 3}), "surface"));
        assert!(c.is_explicit(&json!({"workspace": "w"}), "workspace"));
    }
}
