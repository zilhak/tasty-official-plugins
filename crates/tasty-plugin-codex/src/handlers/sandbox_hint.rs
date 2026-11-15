//! 샌드박스 초기화 실패 안내의 근거. 화면과 출력 stream 은 Codex TUI 가 매 턴 지난 대화를 다시 그려 이번 턴을
//! 가리지 못하므로, `PostToolUse` 훅의 도구 결과(`tool_response`)에서 실패 줄을 보고 그 활동 순번을 surface meta
//! 에 남긴다. 완료 줄은 그 순번이 이번 무장의 문턱 이상일 때만 안내를 붙인다.
//!
//! 도구 결과는 판정에만 쓰고 저장하거나 다른 곳으로 보내지 않는다. meta 에는 순번 숫자만 쓴다.

use super::*;

/// surface meta 키 — 도구 결과에 샌드박스 초기화 실패 줄이 있던 마지막 활동 순번. 원격 mirror 에도 relay 한다.
/// 호스트는 키에 소문자·숫자·`.`·`_`·`-` 만 받는다.
pub(crate) const SANDBOX_FAIL_SEQ_META_KEY: &str = "codex-sandbox-fail-seq";

/// 실패 줄의 앞부분. bubblewrap 이 자기 오류를 이 접두어로 찍는다.
const BWRAP_PREFIX: &str = "bwrap:";

/// 도구 결과에 샌드박스 초기화 실패 줄이 있는가. 셸 도구의 결과는 stdout·stderr 를 합친 문자열이다. 문자열이
/// 아니면(셸이 아닌 도구) 보지 않는다. 어떤 줄이든 앞 공백을 뺀 뒤 `bwrap:` 로 시작하고 표식을 담아야 한다.
/// `grep` 결과처럼 표식을 내용으로 담은 줄(`path:line: …`)은 거른다. 같은 줄을 그대로 찍는 명령은 거르지 못한다.
pub(crate) fn tool_failed(tool_response: Option<&Value>) -> bool {
    tool_response.and_then(Value::as_str).is_some_and(|text| {
        text.lines().any(|line| {
            line.trim_start().starts_with(BWRAP_PREFIX) && line.contains(SANDBOX_FAILURE_MARKER)
        })
    })
}

/// 실패를 본 활동의 순번을 기록한다. 실패한 호출 수를 반환한다.
pub(crate) fn record<H: HostCall>(host: &H, surface_id: u32, seq: u64) -> usize {
    match host.call(
        "surface.meta.set",
        json!({ "surface_id": surface_id, "key": SANDBOX_FAIL_SEQ_META_KEY, "value": seq.to_string() }),
    ) {
        Ok(_) => 0,
        Err(e) => {
            tracing::warn!("codex hook: sandbox failure of s{surface_id} not recorded: {e}");
            1
        }
    }
}

/// 이번 무장 동안(`done_from` 이상, 지금 순번 이하)의 도구 결과에 실패가 있었는가. 기록이 없거나 읽지 못하면
/// 거짓이다. 지금 순번보다 큰 기록은 순번만 초기화된 경우의 옛 기록이라 쓰지 않는다.
pub(crate) fn failed_since<H: HostCall>(host: &H, target: u32, done_from: u64) -> bool {
    let Some(failed_at) = host
        .call(
            "surface.meta.get",
            json!({ "surface_id": target, "key": SANDBOX_FAIL_SEQ_META_KEY }),
        )
        .ok()
        .and_then(|v| v.get("value")?.as_str()?.parse::<u64>().ok())
    else {
        return false;
    };
    let seq = completion_gate::activity_seq(host, target, ACTIVITY_SEQ_META_KEY);
    done_from <= failed_at && failed_at <= seq
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_bwrap_line_with_the_marker_is_a_sandbox_failure() {
        let failed = |v: Value| tool_failed(Some(&v));
        assert!(failed(json!(
            "bwrap: loopback: Failed RTM_NEWADDR: Operation not permitted\n"
        )));
        assert!(failed(json!(
            "Warning: truncated output (12000000 bytes omitted)\n  bwrap: loopback: Failed RTM_NEWADDR: Operation not permitted\n"
        )));
        assert!(!failed(json!(
            "docs/x.md:3: bwrap: loopback: Failed RTM_NEWADDR\n"
        )));
        assert!(!failed(
            json!({ "text": "bwrap: loopback: Failed RTM_NEWADDR" })
        ));
        assert!(!failed(json!("hello\n")));
        assert!(!failed(json!("bwrap: some other failure\n")));
        assert!(!tool_failed(None));
    }
}
