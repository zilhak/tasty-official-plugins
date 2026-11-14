//! 요청 params 를 호스트 `terminal.*` 로 넘기기 전 다듬는 헬퍼.

use serde_json::{Map, Value};

/// 있는 키만 새 Map에 복사한다. 없는 키를 null로 만들면 값이 지정된 것으로 해석될 수 있다.
pub fn forward(params: &Value, keys: &[&str]) -> Map<String, Value> {
    let mut out = Map::new();
    for k in keys {
        if let Some(v) = params.get(*k) {
            out.insert((*k).to_string(), v.clone());
        }
    }
    out
}

/// u32 인자 오류. 사용자 메시지는 각 플러그인이 자기 번역으로 만든다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum U32FieldError {
    /// 키가 없거나 null이다.
    Missing,
    /// 값이 왔는데 32 비트 정수가 아니다.
    Malformed { raw: String },
}

/// u32 범위의 정수를 읽는다. 큰 값을 잘라 다른 surface id로 해석하지 않는다.
pub fn u32_field(params: &Value, key: &str) -> Result<u32, U32FieldError> {
    let Some(raw) = params.get(key).filter(|v| !v.is_null()) else {
        return Err(U32FieldError::Missing);
    };
    raw.as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| U32FieldError::Malformed {
            raw: raw.to_string(),
        })
}

/// 대상 surface 인자 오류. 사용자 메시지는 플러그인이 만든다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetSurfaceError {
    /// 값이 u32 정수가 아니다. 큰 값을 잘라서 쓰지 않는다.
    Malformed { key: &'static str, raw: String },
    /// surface와 surface_id가 서로 다른 대상을 가리킨다.
    Conflict { surface: u32, surface_id: u32 },
}

/// surface와 surface_id를 같은 대상 필드로 읽는다. 둘 다 없거나 null이면 None.
/// 둘을 함께 지정했다면 값이 같아야 한다. 어느 이름으로 지정해도
/// 유효하지 않은 대상을 무시하고 기본 대상에 적용해서는 안 된다.
pub fn target_surface(params: &Value) -> Result<Option<u32>, TargetSurfaceError> {
    let mut surface = None;
    let mut surface_id = None;
    for key in ["surface", "surface_id"] {
        // 공통 u32 판정 결과에 잘못된 키 이름을 붙인다.
        let v = match u32_field(params, key) {
            Ok(v) => v,
            Err(U32FieldError::Missing) => continue,
            Err(U32FieldError::Malformed { raw }) => {
                return Err(TargetSurfaceError::Malformed { key, raw });
            }
        };
        if key == "surface" {
            surface = Some(v);
        } else {
            surface_id = Some(v);
        }
    }
    match (surface, surface_id) {
        (Some(a), Some(b)) if a != b => Err(TargetSurfaceError::Conflict {
            surface: a,
            surface_id: b,
        }),
        (a, b) => Ok(a.or(b)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 어느 이름으로 지정해도 같은 대상이 나온다.
    #[test]
    fn either_name_names_the_same_target() {
        for p in [json!({ "surface": 7 }), json!({ "surface_id": 7 })] {
            assert_eq!(
                target_surface(&p),
                Ok(Some(7)),
                "{p} 에서 대상을 못 읽었다 — 호출자는 유일-parent 폴백을 받게 된다"
            );
        }
    }

    /// 두 필드가 없으면 호출자가 기본 대상 선택을 적용할 수 있도록 None을 반환한다.
    #[test]
    fn naming_no_target_yields_none() {
        assert_eq!(target_surface(&json!({ "child": 0 })), Ok(None));
    }

    /// 두 이름이 다른 값이면 고르지 않고 거절한다. 같은 값이면 CLI 가 보내는
    /// 정상 형태이므로 막지 않는다.
    #[test]
    fn two_names_disagreeing_is_refused_not_picked() {
        assert_eq!(
            target_surface(&json!({ "surface": 1, "surface_id": 2 })),
            Err(TargetSurfaceError::Conflict {
                surface: 1,
                surface_id: 2
            })
        );
        assert_eq!(
            target_surface(&json!({ "surface": 3, "surface_id": 3 })),
            Ok(Some(3))
        );
    }

    /// 32 비트를 넘는 값을 **자르지 않는다** — 잘린 id 는 실재할 수 있는 다른 surface 다.
    #[test]
    fn an_oversized_id_is_refused_not_truncated() {
        let err = target_surface(&json!({ "surface": 4_294_967_297u64 })).unwrap_err();
        assert!(
            matches!(err, TargetSurfaceError::Malformed { key: "surface", .. }),
            "잘라서 1 로 만들었다: {err:?}"
        );
    }

    /// 부재와 null은 같은 Missing으로 처리한다.
    #[test]
    fn an_absent_key_and_an_explicit_null_read_the_same() {
        assert_eq!(u32_field(&json!({}), "child"), Err(U32FieldError::Missing));
        assert_eq!(
            u32_field(&json!({ "child": null }), "child"),
            Err(U32FieldError::Missing)
        );
        assert_eq!(u32_field(&json!({ "child": 0 }), "child"), Ok(0));
        assert_eq!(
            u32_field(&json!({ "child": u32::MAX }), "child"),
            Ok(u32::MAX)
        );
    }

    /// 32 비트를 넘는 값은 **자르지 않고** 거절한다 — `4_294_967_297 as u32 == 1`.
    #[test]
    fn an_oversized_field_is_refused_not_truncated() {
        assert_eq!(
            u32_field(&json!({ "child": 4_294_967_297u64 }), "child"),
            Err(U32FieldError::Malformed {
                raw: "4294967297".to_string()
            })
        );
        assert!(matches!(
            u32_field(&json!({ "child": "conductor" }), "child"),
            Err(U32FieldError::Malformed { .. })
        ));
    }

    #[test]
    fn absent_keys_stay_absent_and_null_is_preserved() {
        let p = json!({ "a": 1, "b": null });
        let out = forward(&p, &["a", "b", "c"]);
        assert_eq!(out.get("a"), Some(&json!(1)));
        assert_eq!(out.get("b"), Some(&json!(null)), "명시된 null 은 보존한다");
        assert!(!out.contains_key("c"), "없는 키를 만들어내지 않는다");
    }
}
