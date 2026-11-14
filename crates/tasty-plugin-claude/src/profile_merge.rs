//! Claude 프로필 여러 개를 하나의 --settings 파일로 병합한다.
//!
//! - 객체는 키별로 재귀 병합한다.
//! - 배열은 JSON 값이 완전히 같은 항목만 중복 제거한다.
//! - 다른 값·유형이 충돌하면 경고 후 뒤 프로필의 값을 사용한다.
//!   permissions.defaultMode의 충돌은 권한 변경을 막기 위해 거부한다.
//!
//! 병합 후 deny와 완전히 같은 항목을 allow에서 제거한다.

use serde_json::Value;
use tasty_plugin_sdk::i18n::Translator;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeError {
    /// 프로필 파일의 최상위가 JSON object 가 아님.
    NotAnObject(String),
    /// 보안 민감 스칼라 키(`permissions.defaultMode`)가 서로 다른 값으로 충돌.
    ScalarConflict {
        path: String,
        existing: Value,
        incoming: Value,
    },
}

impl MergeError {
    pub(crate) fn translate(&self, tr: &Translator) -> String {
        match self {
            Self::NotAnObject(label) => tr.t_fmt("claude.profile_merge.not_an_object", label),
            Self::ScalarConflict {
                path,
                existing,
                incoming,
            } => tr.t_args(
                "claude.profile_merge.scalar_conflict",
                &[path, &existing.to_string(), &incoming.to_string()],
            ),
        }
    }
}

/// 충돌을 경고만으로 넘기지 않고 거부할 경로.
const HARD_REJECT_SCALAR_PATHS: &[&str] = &["$.permissions.defaultMode"];

/// 입력 순서대로 병합하고 경고를 함께 반환한다. 입력이 없으면 빈 객체를 반환한다.
pub(crate) fn merge_contents(
    contents: &[(String, Value)],
) -> Result<(Value, Vec<String>), MergeError> {
    let mut warnings = Vec::new();
    let mut acc = Value::Object(serde_json::Map::new());
    for (label, v) in contents {
        if !v.is_object() {
            return Err(MergeError::NotAnObject(label.clone()));
        }
        merge_value(&mut acc, v, "$", &mut warnings)?;
    }
    enforce_deny_beats_allow(&mut acc);
    Ok((acc, warnings))
}

fn merge_value(
    base: &mut Value,
    incoming: &Value,
    path: &str,
    warnings: &mut Vec<String>,
) -> Result<(), MergeError> {
    match (base, incoming) {
        (Value::Object(b), Value::Object(i)) => {
            for (k, v) in i {
                let child_path = format!("{path}.{k}");
                match b.get_mut(k) {
                    Some(existing) => merge_value(existing, v, &child_path, warnings)?,
                    None => {
                        b.insert(k.clone(), v.clone());
                    }
                }
            }
            Ok(())
        }
        (Value::Array(b), Value::Array(i)) => {
            for item in i {
                if !b.contains(item) {
                    b.push(item.clone());
                }
            }
            Ok(())
        }
        (b, i) => {
            if b == i {
                return Ok(());
            }
            if HARD_REJECT_SCALAR_PATHS.contains(&path) {
                return Err(MergeError::ScalarConflict {
                    path: path.to_string(),
                    existing: b.clone(),
                    incoming: i.clone(),
                });
            }
            warnings.push(format!(
                "scalar key '{path}' conflict: {b} -> {i} (last-wins, later profile overrides earlier)"
            ));
            *b = i.clone();
            Ok(())
        }
    }
}

/// 배열 병합을 마친 뒤 deny와 같은 항목을 allow에서 제거한다.
fn enforce_deny_beats_allow(root: &mut Value) {
    let Some(perms) = root.get_mut("permissions").and_then(|p| p.as_object_mut()) else {
        return;
    };
    let deny: Vec<Value> = perms
        .get("deny")
        .and_then(|d| d.as_array())
        .cloned()
        .unwrap_or_default();
    if deny.is_empty() {
        return;
    }
    if let Some(allow) = perms.get_mut("allow").and_then(|a| a.as_array_mut()) {
        allow.retain(|item| !deny.contains(item));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn labeled(v: Value) -> (String, Value) {
        ("p".into(), v)
    }

    #[test]
    fn empty_input_yields_empty_object() {
        let (merged, warnings) = merge_contents(&[]).unwrap();
        assert_eq!(merged, json!({}));
        assert!(warnings.is_empty());
    }

    #[test]
    fn hook_arrays_concat_across_profiles() {
        let a = json!({"hooks": {"Stop": [{"type":"command","command":"a"}]}});
        let b = json!({"hooks": {"Stop": [{"type":"command","command":"b"}]}});
        let (merged, _) = merge_contents(&[("a".into(), a), ("b".into(), b)]).unwrap();
        let stop = merged["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 2);
    }

    #[test]
    fn duplicate_hook_entries_are_not_duplicated() {
        let a = json!({"hooks": {"Stop": [{"type":"command","command":"same"}]}});
        let b = json!({"hooks": {"Stop": [{"type":"command","command":"same"}]}});
        let (merged, _) = merge_contents(&[("a".into(), a), ("b".into(), b)]).unwrap();
        assert_eq!(merged["hooks"]["Stop"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn env_maps_merge_key_wise() {
        let a = json!({"env": {"A": "1"}});
        let b = json!({"env": {"B": "2"}});
        let (merged, warnings) = merge_contents(&[("a".into(), a), ("b".into(), b)]).unwrap();
        assert_eq!(merged["env"]["A"], "1");
        assert_eq!(merged["env"]["B"], "2");
        assert!(warnings.is_empty());
    }

    #[test]
    fn env_key_conflict_warns_and_last_wins() {
        let a = json!({"env": {"A": "1"}});
        let b = json!({"env": {"A": "2"}});
        let (merged, warnings) = merge_contents(&[("a".into(), a), ("b".into(), b)]).unwrap();
        assert_eq!(merged["env"]["A"], "2");
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn permission_lists_union() {
        let a = json!({"permissions": {"allow": ["Read"]}});
        let b = json!({"permissions": {"allow": ["Write"]}});
        let (merged, _) = merge_contents(&[("a".into(), a), ("b".into(), b)]).unwrap();
        let mut allow: Vec<String> = merged["permissions"]["allow"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        allow.sort();
        assert_eq!(allow, vec!["Read".to_string(), "Write".to_string()]);
    }

    /// deny에 있는 권한이 병합 후 allow에 남지 않아야 한다.
    #[test]
    fn deny_beats_allow_even_when_allow_profile_applied_later() {
        let deny_profile = json!({"permissions": {"deny": ["Bash"]}});
        let allow_profile = json!({"permissions": {"allow": ["Bash"]}});
        let (merged, _) = merge_contents(&[
            ("deny".into(), deny_profile),
            ("allow".into(), allow_profile),
        ])
        .unwrap();
        let allow = merged["permissions"]["allow"].as_array().unwrap();
        assert!(
            !allow.iter().any(|v| v == "Bash"),
            "Bash must not survive in allow when a deny profile denies it: {merged}"
        );
        let deny = merged["permissions"]["deny"].as_array().unwrap();
        assert!(deny.iter().any(|v| v == "Bash"));
    }

    /// allow를 통째로 비우는 오류도 잡도록 deny에 없는 항목을 함께 넣는다.
    #[test]
    fn deny_beats_allow_regardless_of_profile_order() {
        let deny_profile = json!({"permissions": {"deny": ["Bash"]}});
        let allow_profile = json!({"permissions": {"allow": ["Bash", "Read"]}});
        let (merged, _) = merge_contents(&[
            ("allow".into(), allow_profile),
            ("deny".into(), deny_profile),
        ])
        .unwrap();
        let allow = merged["permissions"]["allow"].as_array().unwrap();
        assert!(!allow.iter().any(|v| v == "Bash"));
        // deny 와 무관한 항목은 allow 에 남는다.
        assert!(allow.iter().any(|v| v == "Read"));
    }

    #[test]
    fn default_mode_conflict_is_rejected() {
        let a = json!({"permissions": {"defaultMode": "default"}});
        let b = json!({"permissions": {"defaultMode": "acceptEdits"}});
        let err = merge_contents(&[("a".into(), a), ("b".into(), b)]).unwrap_err();
        assert!(matches!(err, MergeError::ScalarConflict { .. }));
    }

    #[test]
    fn default_mode_same_value_is_not_a_conflict() {
        let a = json!({"permissions": {"defaultMode": "default"}});
        let b = json!({"permissions": {"defaultMode": "default"}});
        let (merged, warnings) = merge_contents(&[("a".into(), a), ("b".into(), b)]).unwrap();
        assert_eq!(merged["permissions"]["defaultMode"], "default");
        assert!(warnings.is_empty());
    }

    #[test]
    fn other_scalar_conflict_warns_and_last_wins() {
        let a = json!({"theme": "dark"});
        let b = json!({"theme": "light"});
        let (merged, warnings) = merge_contents(&[("a".into(), a), ("b".into(), b)]).unwrap();
        assert_eq!(merged["theme"], "light");
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn non_object_top_level_is_rejected() {
        let err = merge_contents(&[labeled(json!(["not", "an", "object"]))]).unwrap_err();
        assert!(matches!(err, MergeError::NotAnObject(_)));
    }

    #[test]
    fn single_profile_passthrough() {
        let a = json!({"env": {"A": "1"}, "permissions": {"allow": ["Read"]}});
        let (merged, warnings) = merge_contents(&[("a".into(), a.clone())]).unwrap();
        assert_eq!(merged, a);
        assert!(warnings.is_empty());
    }
}
