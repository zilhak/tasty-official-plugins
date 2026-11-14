//! 대화 기록의 JSONL을 text·thinking·tool_use·turn_end 이벤트로 바꾼다.
//! thinking은 소비자가 표시·보관 여부를 따로 정할 수 있도록 text와 구분한다.
//! 정상 종료뿐 아니라 API 오류와 사용자 취소도 턴 종료로 처리한다.
//! 세션 소멸·추적 해제는 기록 파서 밖의 tail/registry에서 알린다.
//! 사용자 프롬프트·도구 결과·첨부·모드 변경은 중계하지 않는다.

use serde_json::{Value, json};

/// 어떤 종류의 스트림 이벤트인가. 소비자(FE)가 kind 로 분기한다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    /// 에이전트 응답 텍스트 블록.
    Text,
    /// 에이전트 사고 블록. 표시/보존 정책이 `Text` 와 달라 분리한다.
    Thinking,
    /// 에이전트의 툴 호출.
    ToolUse,
    /// 턴 종료. `reason` 이 어떤 경로로 끝났는지 알려준다.
    TurnEnd,
}

impl EventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::Text => "text",
            EventKind::Thinking => "thinking",
            EventKind::ToolUse => "tool_use",
            EventKind::TurnEnd => "turn_end",
        }
    }
}

/// turn_end.reason의 접두어. 외부 stop_reason은 stop:, 내부 종료 사유는 stream:으로 구분한다.
pub const STOP_REASON_PREFIX: &str = "stop:";

/// 턴이 API 오류로 끝났다 (`isApiErrorMessage: true`).
pub const REASON_API_ERROR: &str = "stream:api_error";
/// 턴이 사용자 취소(Esc)로 끝났다.
pub const REASON_CANCELLED: &str = "stream:cancelled";
/// tail 대상 surface 또는 세션이 사라져 턴이 끝났다 — 파일이 아니라 tail 루프가 만든다.
pub const REASON_SESSION_ENDED: &str = "stream:session_ended";
/// 사용자가 watch 를 해제해 스트림이 끝났다 — 파일이 아니라 unwatch 핸들러가 만든다.
pub const REASON_UNWATCHED: &str = "stream:unwatched";
/// 같은 surface 를 다시 watch 해 이전 등록이 교체됐다 — 이전 등록의 턴을 닫는다.
pub const REASON_REWATCHED: &str = "stream:rewatched";
/// 오래 활동이 없는 턴을 정리했다. turn_start 뒤 실제 요청이 실패한 경우 등에도 적용한다.
pub const REASON_TURN_TIMEOUT: &str = "stream:turn_timeout";

/// 사용자 취소 시 transcript 에 남는 마커. `[Request interrupted by user]` 와
/// `[Request interrupted by user for tool use]` 두 형태가 관측되므로 접두로 본다.
const INTERRUPT_MARKER: &str = "[Request interrupted by user";

/// `stop_reason` 이 이 값이면 턴이 끝난 게 아니라 **툴 호출로 이어진다**.
const STOP_REASON_TOOL_USE: &str = "tool_use";

/// 한 레코드에서 뽑아낸 이벤트 하나.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamEvent {
    pub kind: EventKind,
    /// 레코드 `uuid`. 중복 제거 기준이자 소비자의 재조립 힌트.
    pub record_uuid: Option<String>,
    /// 레코드 `timestamp` (ISO-8601 문자열 그대로).
    pub timestamp: Option<String>,
    /// `text` / `thinking` 본문.
    pub text: Option<String>,
    /// `tool_use` 의 툴 이름.
    pub tool_name: Option<String>,
    /// `tool_use` 의 입력 JSON.
    pub tool_input: Option<Value>,
    /// `turn_end` 의 종료 사유.
    pub reason: Option<String>,
}

impl StreamEvent {
    fn new(kind: EventKind) -> Self {
        Self {
            kind,
            record_uuid: None,
            timestamp: None,
            text: None,
            tool_name: None,
            tool_input: None,
            reason: None,
        }
    }

    /// 턴 종료 이벤트. tail 루프가 파일 밖 사유(세션 소멸/unwatch)로도 만든다.
    pub fn turn_end(reason: &str) -> Self {
        let mut ev = Self::new(EventKind::TurnEnd);
        ev.reason = Some(reason.to_string());
        ev
    }

    /// IPC 응답용 JSON. `None` 필드는 아예 싣지 않는다 — 소비자가 존재 여부로 분기한다.
    pub fn to_json(&self) -> Value {
        let mut obj = json!({ "kind": self.kind.as_str() });
        let map = obj.as_object_mut().expect("json! object literal");
        if let Some(v) = &self.record_uuid {
            map.insert("record_uuid".into(), Value::from(v.clone()));
        }
        if let Some(v) = &self.timestamp {
            map.insert("timestamp".into(), Value::from(v.clone()));
        }
        if let Some(v) = &self.text {
            map.insert("text".into(), Value::from(v.clone()));
        }
        if let Some(v) = &self.tool_name {
            map.insert("tool_name".into(), Value::from(v.clone()));
        }
        if let Some(v) = &self.tool_input {
            map.insert("tool_input".into(), v.clone());
        }
        if let Some(v) = &self.reason {
            map.insert("reason".into(), Value::from(v.clone()));
        }
        obj
    }
}

/// JSONL 한 줄의 파싱 결과.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedRecord {
    /// 레코드 `uuid`. 중복 제거 키 — 없는 레코드 타입도 있어 `Option`.
    pub uuid: Option<String>,
    /// 이 레코드가 만든 이벤트들. 중계 대상이 아닌 레코드는 빈 vec.
    pub events: Vec<StreamEvent>,
}

/// JSONL 한 줄을 파싱한다. 올바른 JSON이 아니면 Err로 반환한다.
pub(crate) fn parse_line(line: &str) -> Result<ParsedRecord, serde_json::Error> {
    let value: Value = serde_json::from_str(line)?;
    let uuid = str_field(&value, "uuid");
    let timestamp = str_field(&value, "timestamp");
    let events = match value.get("type").and_then(Value::as_str) {
        Some("assistant") => assistant_events(&value),
        Some("user") => user_events(&value),
        _ => Vec::new(),
    };
    let events = events
        .into_iter()
        .map(|mut ev| {
            ev.record_uuid.clone_from(&uuid);
            ev.timestamp.clone_from(&timestamp);
            ev
        })
        .collect();
    Ok(ParsedRecord { uuid, events })
}

fn str_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

/// `assistant` 레코드 → 콘텐츠 블록 이벤트들 + (있으면) 턴 종료 이벤트.
fn assistant_events(value: &Value) -> Vec<StreamEvent> {
    let mut out = Vec::new();
    if let Some(blocks) = value.pointer("/message/content").and_then(Value::as_array) {
        out.extend(blocks.iter().filter_map(content_block_event));
    }
    if let Some(reason) = assistant_turn_end_reason(value) {
        out.push(StreamEvent::turn_end(&reason));
    }
    out
}

fn content_block_event(block: &Value) -> Option<StreamEvent> {
    match block.get("type").and_then(Value::as_str)? {
        "text" => {
            let mut ev = StreamEvent::new(EventKind::Text);
            ev.text = str_field(block, "text");
            Some(ev)
        }
        "thinking" => {
            let mut ev = StreamEvent::new(EventKind::Thinking);
            // 사고 블록의 본문 키는 `text` 가 아니라 `thinking` 이다.
            ev.text = str_field(block, "thinking");
            Some(ev)
        }
        "tool_use" => {
            let mut ev = StreamEvent::new(EventKind::ToolUse);
            ev.tool_name = str_field(block, "name");
            ev.tool_input = block.get("input").cloned();
            Some(ev)
        }
        _ => None,
    }
}

/// `assistant` 레코드가 턴을 끝냈는지, 끝냈다면 어떤 사유인지.
///
/// - API 오류 응답은 `stop_reason` 이 `stop_sequence` 같은 평범한 값으로 오므로
///   `isApiErrorMessage` 를 먼저 본다 — 그러지 않으면 오류 종료가 정상 완료로 보고된다.
/// - `tool_use` 는 턴 종료가 아니다(툴 결과를 받아 계속된다).
fn assistant_turn_end_reason(value: &Value) -> Option<String> {
    if value
        .get("isApiErrorMessage")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Some(REASON_API_ERROR.to_string());
    }
    let stop = value
        .pointer("/message/stop_reason")
        .and_then(Value::as_str)?;
    if stop == STOP_REASON_TOOL_USE {
        return None;
    }
    Some(format!("{STOP_REASON_PREFIX}{stop}"))
}

/// `user` 레코드는 취소 마커만 본다 — 사용자 프롬프트/툴 결과 본문은 이 파이프라인의
/// 중계 대상이 아니다.
fn user_events(value: &Value) -> Vec<StreamEvent> {
    if user_record_is_interrupt(value) {
        vec![StreamEvent::turn_end(REASON_CANCELLED)]
    } else {
        Vec::new()
    }
}

fn user_record_is_interrupt(value: &Value) -> bool {
    let Some(content) = value.pointer("/message/content") else {
        return false;
    };
    match content {
        Value::String(s) => s.trim_start().starts_with(INTERRUPT_MARKER),
        Value::Array(blocks) => blocks.iter().any(|b| {
            b.get("text")
                .and_then(Value::as_str)
                .is_some_and(|t| t.trim_start().starts_with(INTERRUPT_MARKER))
        }),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(rec: &ParsedRecord) -> Vec<&'static str> {
        rec.events.iter().map(|e| e.kind.as_str()).collect()
    }

    #[test]
    fn splits_text_thinking_and_tool_use_into_distinct_kinds() {
        let line = r#"{"type":"assistant","uuid":"u1","timestamp":"t1","message":{"stop_reason":"tool_use","content":[
            {"type":"thinking","thinking":"pondering","signature":"sig"},
            {"type":"text","text":"hello"},
            {"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}}"#;
        let rec = parse_line(line).expect("valid json");
        assert_eq!(kinds(&rec), ["thinking", "text", "tool_use"]);
        assert_eq!(rec.uuid.as_deref(), Some("u1"));
        assert_eq!(rec.events[0].text.as_deref(), Some("pondering"));
        assert_eq!(rec.events[1].text.as_deref(), Some("hello"));
        assert_eq!(rec.events[2].tool_name.as_deref(), Some("Bash"));
        assert_eq!(
            rec.events[2].tool_input,
            Some(json!({ "command": "ls" })),
            "tool input is relayed verbatim"
        );
        // 모든 이벤트가 레코드 메타(uuid/timestamp)를 물려받는다.
        assert!(rec.events.iter().all(
            |e| e.record_uuid.as_deref() == Some("u1") && e.timestamp.as_deref() == Some("t1")
        ));
    }

    #[test]
    fn tool_use_stop_reason_is_not_a_turn_end() {
        let line = r#"{"type":"assistant","uuid":"u1","message":{"stop_reason":"tool_use","content":[{"type":"text","text":"x"}]}}"#;
        let rec = parse_line(line).expect("valid json");
        assert_eq!(kinds(&rec), ["text"]);
    }

    #[test]
    fn end_turn_emits_turn_end_with_stop_reason() {
        let line = r#"{"type":"assistant","uuid":"u1","message":{"stop_reason":"end_turn","content":[{"type":"text","text":"done"}]}}"#;
        let rec = parse_line(line).expect("valid json");
        assert_eq!(kinds(&rec), ["text", "turn_end"]);
        assert_eq!(rec.events[1].reason.as_deref(), Some("stop:end_turn"));
    }

    #[test]
    fn max_tokens_stop_reason_also_ends_the_turn() {
        let line = r#"{"type":"assistant","uuid":"u1","message":{"stop_reason":"max_tokens","content":[]}}"#;
        let rec = parse_line(line).expect("valid json");
        assert_eq!(rec.events[0].reason.as_deref(), Some("stop:max_tokens"));
    }

    #[test]
    fn api_error_record_reports_error_not_its_stop_reason() {
        let line = r#"{"type":"assistant","uuid":"u1","isApiErrorMessage":true,"message":{"stop_reason":"stop_sequence","content":[{"type":"text","text":"API Error: 521"}]}}"#;
        let rec = parse_line(line).expect("valid json");
        assert_eq!(kinds(&rec), ["text", "turn_end"]);
        assert_eq!(rec.events[1].reason.as_deref(), Some(REASON_API_ERROR));
    }

    #[test]
    fn user_interrupt_marker_ends_the_turn_as_cancelled() {
        let line = r#"{"type":"user","uuid":"u2","message":{"content":[{"type":"text","text":"[Request interrupted by user]"}]}}"#;
        let rec = parse_line(line).expect("valid json");
        assert_eq!(kinds(&rec), ["turn_end"]);
        assert_eq!(rec.events[0].reason.as_deref(), Some(REASON_CANCELLED));
    }

    #[test]
    fn user_interrupt_for_tool_use_variant_also_matches() {
        let line = r#"{"type":"user","uuid":"u2","message":{"content":[{"type":"text","text":"[Request interrupted by user for tool use]"}]}}"#;
        let rec = parse_line(line).expect("valid json");
        assert_eq!(rec.events[0].reason.as_deref(), Some(REASON_CANCELLED));
    }

    #[test]
    fn ordinary_user_record_relays_nothing() {
        let line = r#"{"type":"user","uuid":"u2","message":{"content":[{"type":"tool_result","content":"ok"}]}}"#;
        let rec = parse_line(line).expect("valid json");
        assert!(rec.events.is_empty());
    }

    #[test]
    fn unrelated_record_types_are_ignored_but_still_parse() {
        for line in [
            r#"{"type":"system","uuid":"u3","subtype":"turn_duration"}"#,
            r#"{"type":"ai-title","aiTitle":"x"}"#,
            r#"{"type":"mode","mode":"default"}"#,
        ] {
            let rec = parse_line(line).expect("valid json");
            assert!(rec.events.is_empty(), "unexpected events for {line}");
        }
    }

    #[test]
    fn malformed_line_is_an_error() {
        assert!(parse_line("{not json").is_err());
    }

    #[test]
    fn reserved_reasons_and_stop_reasons_live_in_separate_namespaces() {
        for reserved in [
            REASON_API_ERROR,
            REASON_CANCELLED,
            REASON_SESSION_ENDED,
            REASON_UNWATCHED,
            REASON_REWATCHED,
            REASON_TURN_TIMEOUT,
        ] {
            assert!(
                reserved.starts_with("stream:"),
                "reserved reason '{reserved}' must carry the stream namespace"
            );
            assert!(
                !reserved.starts_with(STOP_REASON_PREFIX),
                "reserved reason '{reserved}' must not look like an external stop_reason"
            );
        }
        // 외부 stop_reason 이 우리 예약 이름과 같은 문자열을 쓰더라도 접두로 갈린다.
        let line = r#"{"type":"assistant","uuid":"u1","message":{"stop_reason":"session_ended","content":[]}}"#;
        let rec = parse_line(line).expect("valid json");
        assert_eq!(rec.events[0].reason.as_deref(), Some("stop:session_ended"));
        assert_ne!(rec.events[0].reason.as_deref(), Some(REASON_SESSION_ENDED));
    }

    #[test]
    fn to_json_omits_absent_fields() {
        let ev = StreamEvent::turn_end(REASON_SESSION_ENDED);
        let value = ev.to_json();
        assert_eq!(value["kind"], "turn_end");
        assert_eq!(value["reason"], REASON_SESSION_ENDED);
        assert!(value.get("text").is_none());
        assert!(value.get("record_uuid").is_none());
    }
}
