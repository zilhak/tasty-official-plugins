//! 스캔 응답의 출처 정보(`text_origins`)와 연속성 키가 정지 시계와 오류 직후 기준에 주는 영향.
//! 응답 모양은 tasty `docs/reference/api.md`의 `read_since_scan_mark` 항목을 따른다.

use super::tests::ScanHost;
use super::*;

use serde_json::Value;

const SID: u32 = 1;
const STREAM: &str = "s1";

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

/// 출처 정보가 있는 응답. `ranges`는 `(start, end, origin)`, `gaps`는 `(at, reason)`이다.
fn with_origins(text: &str, ranges: &[(usize, usize, &str)], gaps: &[(usize, &str)]) -> Value {
    let ranges: Vec<Value> = ranges
        .iter()
        .map(|(start, end, origin)| json!({ "start": start, "end": end, "origin": origin }))
        .collect();
    let gaps: Vec<Value> = gaps
        .iter()
        .map(|(at, reason)| json!({ "at": at, "reason": reason }))
        .collect();
    json!({
        "text": text,
        "surface_id": SID,
        "text_origins": { "ranges": ranges, "gaps": gaps },
    })
}

/// 텍스트 전체가 한 출처인 응답.
fn all(text: &str, origin: &str) -> Value {
    if text.is_empty() {
        with_origins(text, &[], &[])
    } else {
        with_origins(text, &[(0, text.len(), origin)], &[])
    }
}

/// 출처 정보가 빠진 응답(옛 호스트 꼴).
fn plain(text: &str) -> Value {
    json!({ "text": text, "surface_id": SID })
}

/// 응답에 연속성 키를 단다. 원문 바이트 수는 `next_cursor - cursor`로 둔다.
fn at(mut reply: Value, stream: &str, cursor: u64, next_cursor: u64, skipped: u64) -> Value {
    reply["stream"] = json!(stream);
    reply["cursor"] = json!(cursor);
    reply["next_cursor"] = json!(next_cursor);
    reply["raw_bytes"] = json!(next_cursor - cursor);
    reply["retention_start"] = json!(0);
    reply["retention_end"] = json!(next_cursor);
    reply["skipped"] = json!(skipped);
    reply
}

/// 같은 stream 에서 손실 없이 이어지는 응답을 만든다. 원문 바이트 수는 텍스트 길이로 둔다.
struct Reader {
    cursor: u64,
}

impl Reader {
    fn new() -> Self {
        Self { cursor: 0 }
    }

    fn next(&mut self, reply: Value) -> Value {
        let len = reply["text"].as_str().map_or(0, str::len) as u64;
        let reply = at(reply, STREAM, self.cursor, self.cursor + len, 0);
        self.cursor += len;
        reply
    }

    /// 아무 출력도 없는 회차.
    fn idle(&mut self) -> Value {
        self.next(all("", "normal"))
    }
}

fn scan(s: &mut ErrorScanner, host: &ScanHost, reply: Value, now: Instant) {
    host.push_scan_reply(Some(reply));
    s.scan_one_at(host, SID, now);
}

/// 응답을 차례로 1초 간격으로 준 스캐너.
fn run(replies: impl IntoIterator<Item = Option<Value>>) -> ErrorScanner {
    let host = ScanHost::new("", "active");
    let mut s = ErrorScanner::new();
    let t0 = Instant::now();
    for (i, reply) in replies.into_iter().enumerate() {
        host.push_scan_reply(reply);
        s.scan_one_at(&host, SID, t0 + secs(i as u64));
    }
    s
}

/// 오류 직후 기준(30초)에 쓸 오류가 최근 출력에 있는가.
fn error_threshold(s: &ErrorScanner) -> bool {
    s.recent_output
        .get(&SID)
        .is_some_and(RecentOutput::has_error)
}

fn error_threshold_after(replies: impl IntoIterator<Item = Value>) -> bool {
    error_threshold(&run(replies.into_iter().map(Some)))
}

fn watch(s: &ErrorScanner) -> &OutputWatch {
    s.watch.get(&SID).expect("the scanner watches the surface")
}

/// 정지 시계·알림 여부·기준 선택·상태 확인.
fn observation(s: &ErrorScanner) -> (Instant, bool, bool, bool) {
    let w = watch(s);
    (w.last_change, w.stall_notified, w.saw_error, w.settled)
}

/// 일반 출력으로 관측을 시작한 스캐너. 시계는 `t0`, 오류 직후 기준은 꺼져 있다.
fn observed(host: &ScanHost, r: &mut Reader, t0: Instant) -> ErrorScanner {
    let mut s = ErrorScanner::new();
    scan(&mut s, host, r.next(all("start\n", "normal")), t0);
    assert_eq!(observation(&s), (t0, false, false, false));
    s
}

fn unset_count(host: &ScanHost) -> usize {
    let unset = format!("meta-unset:{STALL_NOTICE_META_KEY}");
    host.events().iter().filter(|e| **e == unset).count()
}

// ── ranges 섞임 ─────────────────────────────────────────────────────────

#[test]
fn normal_output_with_a_redrawn_error_wakes_the_clock_but_not_the_error_threshold() {
    let host = ScanHost::new("", "active");
    let mut r = Reader::new();
    let t0 = Instant::now();
    let mut s = observed(&host, &mut r, t0);
    let t1 = t0 + secs(5);
    let reply = with_origins(
        "ok\nAPI Error\n",
        &[(0, 3, "normal"), (3, 13, "redraw")],
        &[],
    );
    scan(&mut s, &host, r.next(reply), t1);
    assert_eq!(watch(&s).last_change, t1, "일반 출력이 시계를 깨운다");
    assert!(
        !watch(&s).saw_error,
        "재그리기의 오류 줄은 30초 기준을 켜지 않는다"
    );
    scan(&mut s, &host, r.idle(), t1 + STALL_QUIET);
    assert_eq!(host.stalled_count(), 0, "30초에 알리지 않는다");
    scan(&mut s, &host, r.idle(), t1 + NO_OUTPUT_NOTICE_DEFAULT);
    assert_eq!(host.stalled_count(), 1, "설정 기준에 알린다");
}

#[test]
fn a_redraw_only_reply_keeps_the_observation() {
    let host = ScanHost::new("", "active");
    let mut r = Reader::new();
    let t0 = Instant::now();
    let mut s = observed(&host, &mut r, t0);
    let before = observation(&s);
    scan(
        &mut s,
        &host,
        r.next(all("screen\nAPI Error: overloaded\n", "redraw")),
        t0 + secs(30),
    );
    assert_eq!(
        observation(&s),
        before,
        "재그리기만 온 회차는 시계·알림 여부·기준·상태 확인을 바꾸지 않는다"
    );
}

/// 앞선 일반 오류로 고른 30초 기준은 재그리기만 온 회차에도 그대로다.
#[test]
fn a_redraw_only_reply_keeps_an_active_error_threshold() {
    let host = ScanHost::new("", "active");
    let mut r = Reader::new();
    let t0 = Instant::now();
    let mut s = ErrorScanner::new();
    scan(&mut s, &host, r.next(all("API Error\n", "normal")), t0);
    assert!(watch(&s).saw_error);
    scan(
        &mut s,
        &host,
        r.next(all("redrawn\n", "redraw")),
        t0 + secs(1),
    );
    assert_eq!(watch(&s).last_change, t0);
    assert!(watch(&s).saw_error, "재그리기가 30초 기준을 끄지 않는다");
}

/// 재그리기가 앞 오류 문구의 단어 경계를 없애 이번 최근 출력만으로는 오류가 아니어도, 재그리기만 온 회차는 앞에서
/// 고른 30초 기준을 다시 고르지 않는다.
#[test]
fn a_redraw_only_reply_keeps_the_error_threshold_even_when_it_breaks_the_phrase() {
    let host = ScanHost::new("", "active");
    let mut r = Reader::new();
    let t0 = Instant::now();
    let mut s = ErrorScanner::new();
    scan(&mut s, &host, r.next(all("API Error", "normal")), t0);
    assert!(watch(&s).saw_error);
    scan(&mut s, &host, r.next(all("s", "redraw")), t0 + secs(1));
    assert_eq!(
        watch(&s).last_change,
        t0,
        "재그리기만 온 회차는 시계를 깨우지 않는다"
    );
    // 대조: 재그리기 `s`가 `Error` 뒤의 단어 경계를 없애 최근 출력에는 오류가 없다.
    assert!(!error_threshold(&s));
    assert!(watch(&s).saw_error, "30초 기준을 다시 고르지 않는다");
    scan(&mut s, &host, r.idle(), t0 + STALL_QUIET);
    assert_eq!(host.stalled_count(), 1, "30초에 알린다");
}

#[test]
fn redrawn_characters_around_an_error_phrase_keep_their_word_boundaries() {
    // 바깥 두 문자만 재그리기.
    assert!(!error_threshold_after([with_origins(
        "xAPI Errory",
        &[(0, 1, "redraw"), (1, 10, "normal"), (10, 11, "redraw")],
        &[],
    )]));
    // 앞 문자만 재그리기: 실제 문자열에서 `x`와 `A` 사이에 단어 경계가 없다.
    assert!(!error_threshold_after([with_origins(
        "xAPI Error",
        &[(0, 1, "redraw"), (1, 10, "normal")],
        &[],
    )]));
    // 뒤 문자만 재그리기: 실제 문자열에서 `r`와 `y` 사이에 단어 경계가 없다.
    assert!(!error_threshold_after([with_origins(
        "API Errory",
        &[(0, 9, "normal"), (9, 10, "redraw")],
        &[],
    )]));
    // 대조: 재그리기 문자가 단어 경계를 이루면 일반 부분의 문구를 센다.
    assert!(error_threshold_after([with_origins(
        "\nAPI Error\n",
        &[(0, 1, "redraw"), (1, 10, "normal"), (10, 11, "redraw")],
        &[],
    )]));
}

// ── mixed ─────────────────────────────────────────────────────────────

/// `ſ`(U+017F)는 원문 두 바이트라 일반·재그리기 바이트가 함께 들 수 있다. ASCII 문자는 `mixed`가 되지 않는다.
#[test]
fn a_mixed_character_wakes_the_clock_and_breaks_the_error_phrase() {
    let host = ScanHost::new("", "active");
    let mut r = Reader::new();
    let t0 = Instant::now();
    let mut s = observed(&host, &mut r, t0);
    let text = "Internal \u{17f}erver Error";
    assert_eq!(&text[9..11], "\u{17f}");
    let t1 = t0 + secs(5);
    let reply = with_origins(
        text,
        &[
            (0, 9, "normal"),
            (9, 11, "mixed"),
            (11, text.len(), "normal"),
        ],
        &[],
    );
    scan(&mut s, &host, r.next(reply), t1);
    assert_eq!(watch(&s).last_change, t1, "시계를 깨운다");
    assert!(!error_threshold(&s), "30초 기준을 켜지 않는다");
    assert!(
        host.events().iter().any(|e| e == "claude-error"),
        "창 전체를 보는 오류 알림은 발화한다"
    );
}

#[test]
fn a_reply_of_only_a_mixed_character_wakes_the_clock() {
    let host = ScanHost::new("", "active");
    let mut r = Reader::new();
    let t0 = Instant::now();
    let mut s = observed(&host, &mut r, t0);
    let t1 = t0 + secs(5);
    scan(
        &mut s,
        &host,
        r.next(with_origins("\u{17f}", &[(0, 2, "mixed")], &[])),
        t1,
    );
    assert_eq!(watch(&s).last_change, t1);
}

// ── gaps: redraw_removed ──────────────────────────────────────────────

#[test]
fn a_removed_redraw_escape_breaks_an_error_phrase() {
    assert!(!error_threshold_after([with_origins(
        "API Error",
        &[(0, 9, "normal")],
        &[(4, "redraw_removed")],
    )]));
    // 대조: 표시가 없으면 센다.
    assert!(error_threshold_after([all("API Error", "normal")]));
}

#[test]
fn a_removed_redraw_escape_between_two_replies_breaks_an_error_phrase() {
    let host = ScanHost::new("", "active");
    let mut r = Reader::new();
    let t0 = Instant::now();
    let mut s = ErrorScanner::new();
    scan(&mut s, &host, r.next(all("API ", "normal")), t0);
    let t1 = t0 + secs(1);
    scan(
        &mut s,
        &host,
        r.next(with_origins("", &[], &[(0, "redraw_removed")])),
        t1,
    );
    assert_eq!(
        watch(&s).last_change,
        t0,
        "표시만 온 회차는 시계를 깨우지 않는다"
    );
    scan(&mut s, &host, r.next(all("Error", "normal")), t1 + secs(1));
    assert!(!error_threshold(&s));
}

#[test]
fn a_removed_escape_at_the_start_of_a_phrase_keeps_the_real_word_boundary() {
    // 일반 `x` + 재그리기 `ESC[2J` + 일반 `API Error`를 한 번에 읽은 꼴.
    assert!(!error_threshold_after([with_origins(
        "xAPI Error",
        &[(0, 10, "normal")],
        &[(1, "redraw_removed")],
    )]));
    // 앞 문자가 개행이면 단어 경계가 있고, 표시가 매치 시작점에 있을 뿐이라 센다.
    assert!(error_threshold_after([with_origins(
        "\nAPI Error",
        &[(0, 10, "normal")],
        &[(1, "redraw_removed")],
    )]));
}

#[test]
fn a_removed_escape_at_the_end_of_a_phrase_keeps_the_real_word_boundary() {
    // 표시가 매치 `[0, 9)`의 끝점에 있다.
    assert!(error_threshold_after([with_origins(
        "API Error\n",
        &[(0, 10, "normal")],
        &[(9, "redraw_removed")],
    )]));
    assert!(!error_threshold_after([with_origins(
        "API Errorx",
        &[(0, 10, "normal")],
        &[(9, "redraw_removed")],
    )]));
}

// ── gaps: trimmed ─────────────────────────────────────────────────────

#[test]
fn a_trimmed_front_breaks_an_error_phrase() {
    let first = at(all("API ", "normal"), STREAM, 0, 4, 0);
    // 보존 한도로 1바이트를 잃고 이어 읽은 꼴. 연속성은 `skipped`로 보정돼 이어진다.
    let trimmed = at(
        with_origins("Error", &[(0, 5, "normal")], &[(0, "trimmed")]),
        STREAM,
        5,
        10,
        1,
    );
    assert!(!error_threshold_after([first.clone(), trimmed]));
    // 대조: 같은 위치에 표시가 없으면 이어진 출력이라 센다.
    let joined = at(all("Error", "normal"), STREAM, 5, 10, 1);
    assert!(error_threshold_after([first, joined]));
}

#[test]
fn two_marks_at_one_offset_are_a_valid_reply() {
    let host = ScanHost::new("", "active");
    let mut r = Reader::new();
    let t0 = Instant::now();
    let mut s = observed(&host, &mut r, t0);
    let reply = with_origins(
        "screen",
        &[(0, 6, "redraw")],
        &[(0, "trimmed"), (0, "redraw_removed")],
    );
    scan(&mut s, &host, r.next(reply), t0 + secs(5));
    assert_eq!(
        watch(&s).last_change,
        t0,
        "재그리기만 남은 응답은 시계를 깨우지 않는다"
    );
}

// ── 출처 정보 생략 ────────────────────────────────────────────────────

#[test]
fn a_reply_without_origins_counts_all_text_as_before() {
    for keys in [true, false] {
        let host = ScanHost::new("", "active");
        let mut r = Reader::new();
        let t0 = Instant::now();
        let mut s = observed(&host, &mut r, t0);
        let t1 = t0 + secs(5);
        let reply = if keys {
            r.next(plain("ok\nAPI Error\n"))
        } else {
            plain("ok\nAPI Error\n")
        };
        scan(&mut s, &host, reply, t1);
        assert_eq!(watch(&s).last_change, t1, "연속성 키 {keys}: 시계를 깨운다");
        assert!(watch(&s).saw_error, "연속성 키 {keys}: 30초 기준을 켠다");
    }
}

/// 호스트는 출처 항목(ranges + gaps) 합이 4,096을 넘을 때만 `text_origins`를 뺀다.
#[test]
fn origins_at_the_host_entry_limit_are_used_and_a_reply_past_it_counts_as_before() {
    let text = format!("{}\nAPI Error\n", "x".repeat(4095));
    let gaps: Vec<(usize, &str)> = (1..=4095).map(|at| (at, "redraw_removed")).collect();
    let within = with_origins(&text, &[(0, text.len(), "redraw")], &gaps);
    assert_eq!(
        within["text_origins"]["ranges"].as_array().unwrap().len()
            + within["text_origins"]["gaps"].as_array().unwrap().len(),
        4096
    );

    let host = ScanHost::new("", "active");
    let mut r = Reader::new();
    let t0 = Instant::now();
    let mut s = observed(&host, &mut r, t0);
    scan(&mut s, &host, r.next(within), t0 + secs(5));
    assert_eq!(
        observation(&s),
        (t0, false, false, false),
        "상한까지는 정상 응답이다"
    );

    let host = ScanHost::new("", "active");
    let mut r = Reader::new();
    let mut s = observed(&host, &mut r, t0);
    let t1 = t0 + secs(5);
    scan(&mut s, &host, r.next(plain(&text)), t1);
    assert_eq!(
        watch(&s).last_change,
        t1,
        "키가 빠지면 지금처럼 시계가 움직인다"
    );
    assert!(watch(&s).saw_error, "키가 빠지면 지금처럼 30초 기준을 켠다");
}

// ── 생략 전후의 연속성 ────────────────────────────────────────────────

#[test]
fn a_reply_without_origins_does_not_break_the_next_reply() {
    let api = at(all("API ", "normal"), STREAM, 0, 4, 0);
    // 앞 응답과 견주면 단절이지만 출처 정보가 없는 회차라 끊긴 자리를 두지 않고 값만 기록한다.
    assert!(error_threshold_after([
        api.clone(),
        at(plain("Er"), STREAM, 5, 7, 0),
        at(all("ror", "normal"), STREAM, 7, 10, 0),
    ]));
    assert!(error_threshold_after([
        api,
        at(plain("Er"), "s2", 0, 2, 0),
        at(all("ror", "normal"), "s2", 2, 5, 0),
    ]));
    // 옛 호스트 꼴은 지금처럼 이어 붙인다.
    assert!(error_threshold_after([plain("API "), plain("Error")]));
}

/// mirror 꼴: 모든 응답에서 출처가 빠지고 연속성 키는 있다. 둘째 회차는 정상 OSC로 채운 보존 원문 1 MiB를
/// `strip_ansi`가 지워 `Er`만 남은 꼴이다.
#[test]
fn replies_without_origins_join_across_a_trimmed_read() {
    assert!(error_threshold_after([
        at(plain("API "), STREAM, 0, 4, 0),
        at(plain("Er"), STREAM, 5, 1_048_581, 1),
        at(plain("ror"), STREAM, 1_048_581, 1_048_584, 0),
    ]));
}

#[test]
fn a_reply_without_keys_clears_the_recorded_position() {
    let host = ScanHost::new("", "active");
    let mut r = Reader::new();
    let t0 = Instant::now();
    let mut s = ErrorScanner::new();
    scan(&mut s, &host, r.next(all("ok\n", "normal")), t0);
    assert_eq!(
        s.recent_output[&SID].continuity,
        Some(ScanContinuity {
            stream: STREAM.into(),
            next_cursor: 3
        })
    );
    scan(&mut s, &host, plain("more\n"), t0 + secs(1));
    assert_eq!(s.recent_output[&SID].continuity, None);
}

/// 4 KiB 창의 맨 앞은 지금처럼 문자열의 시작으로 판정한다. 잘라 낸 `x`는 단어 경계 판정에 쓰지 않는다.
#[test]
fn the_front_of_the_recent_output_is_a_string_start_as_before() {
    let text = format!("xAPI Error{}", ".".repeat(4087));
    assert_eq!(text.len(), RECENT_ERROR_BYTES + 1);
    assert!(error_threshold_after([plain(&text)]), "옛 호스트 꼴");
    assert!(
        error_threshold_after([at(plain(&text), STREAM, 0, 4097, 0)]),
        "연속성 키만 있는 응답"
    );
    assert!(
        error_threshold_after([at(all(&text, "normal"), STREAM, 0, 4097, 0)]),
        "전부 일반인 응답"
    );
}

/// 4 KiB 절단점이 재그리기 자리와 겹쳐도 창의 맨 앞은 문자열의 시작이다. 잘린 앞 출력 바로 뒤의 재그리기 문자를
/// 맨 앞 문구의 왼쪽 경계로 쓰지 않는다.
#[test]
fn a_front_cut_at_a_redraw_break_is_a_string_start() {
    let tail = format!("API Error{}", ".".repeat(4087));
    assert_eq!(1 + tail.len(), RECENT_ERROR_BYTES + 1);
    let text = format!("qx{tail}");
    assert!(
        error_threshold_after([with_origins(
            &text,
            &[
                (0, 1, "normal"),
                (1, 2, "redraw"),
                (2, text.len(), "normal")
            ],
            &[],
        )]),
        "한 응답"
    );
    let mut r = Reader::new();
    assert!(
        error_threshold_after([
            r.next(with_origins(
                "qx",
                &[(0, 1, "normal"), (1, 2, "redraw")],
                &[]
            )),
            r.next(plain(&tail)),
        ]),
        "마지막 일반 부분이 출처 생략 응답으로 따로 옴"
    );
}

/// 4 KiB 절단 뒤에도 창 안에 남은 끊긴 자리는 그대로 문구를 끊는다.
#[test]
fn a_break_left_in_the_window_after_a_front_cut_still_breaks_an_error_phrase() {
    let text = format!("{}API zError", ".".repeat(4096));
    assert_eq!(text.len(), 4106);
    // 일반 부분은 재그리기 `z`를 뺀 4,105바이트라 앞 9바이트가 잘리고, `z` 자리는 창 안에 남는다.
    assert_eq!(text.len() - 1 - RECENT_ERROR_BYTES, 9);
    assert!(!error_threshold_after([with_origins(
        &text,
        &[
            (0, 4100, "normal"),
            (4100, 4101, "redraw"),
            (4101, 4106, "normal")
        ],
        &[],
    )]));
    // 대조: 같은 절단에서 끊긴 자리가 없으면 문구가 창 안에 남아 센다.
    let joined = format!("{}API Error", ".".repeat(4096));
    assert_eq!(joined.len() - RECENT_ERROR_BYTES, 9);
    assert!(error_threshold_after([all(&joined, "normal")]));
}

// ── stream 바뀜·응답 유실·호출 실패 ───────────────────────────────────

#[test]
fn normal_output_joins_across_replies_of_one_stream_only() {
    let api = at(all("API ", "normal"), STREAM, 0, 4, 0);
    assert!(error_threshold_after([
        api.clone(),
        at(all("Error", "normal"), STREAM, 4, 9, 0),
    ]));
    assert!(!error_threshold_after([
        api,
        at(all("Error", "normal"), "s2", 4, 9, 0),
    ]));
}

#[test]
fn a_lost_reply_breaks_an_error_phrase() {
    let api = at(all("API ", "normal"), STREAM, 0, 4, 0);
    assert!(!error_threshold_after([
        api.clone(),
        at(all("Error", "normal"), STREAM, 6, 11, 1),
    ]));
    // 원문을 소비한 앞 응답과 같은 `cursor`가 다시 오면 끊긴 것이다.
    assert!(!error_threshold_after([
        api.clone(),
        at(all("Error", "normal"), STREAM, 0, 5, 0),
    ]));
    // 원문을 소비하지 않은 응답(`raw_bytes == 0`) 뒤 같은 `cursor`는 이어진 것이다.
    assert!(error_threshold_after([
        api,
        at(all("", "normal"), STREAM, 4, 4, 0),
        at(all("Error", "normal"), STREAM, 4, 9, 0),
    ]));
}

#[test]
fn continuity_needs_the_same_stream_and_the_next_cursor_after_skipped_bytes() {
    let prev = ScanContinuity {
        stream: STREAM.into(),
        next_cursor: 4,
    };
    let read = |stream: &str, cursor: u64, skipped: Option<u64>| ReadPosition {
        stream: Some(stream.into()),
        cursor: Some(cursor),
        next_cursor: Some(cursor + 1),
        skipped,
    };
    assert!(continues(Some(&prev), &read(STREAM, 5, Some(1))));
    assert!(!continues(Some(&prev), &read(STREAM, 6, Some(1))));
    assert!(continues(Some(&prev), &read(STREAM, 4, Some(0))));
    assert!(!continues(Some(&prev), &read("s2", 5, Some(1))));
    assert!(!continues(Some(&prev), &read(STREAM, 4, None)));
    assert!(!continues(Some(&prev), &read(STREAM, 0, Some(1))));
    assert!(!continues(None, &read(STREAM, 4, Some(0))));
}

#[test]
fn a_failed_call_breaks_the_phrase_only_when_the_host_consumed_output() {
    let api = Some(at(all("API ", "normal"), STREAM, 0, 4, 0));
    let s = run([
        api.clone(),
        None,
        Some(at(all("Error", "normal"), STREAM, 4, 9, 0)),
    ]);
    assert!(error_threshold(&s), "실패한 호출이 원문을 소비하지 않았다");
    let s = run([
        api,
        None,
        Some(at(all("Error", "normal"), STREAM, 5, 10, 0)),
    ]);
    assert!(!error_threshold(&s), "실패한 호출이 원문을 소비했다");
}

// ── 부분 unknown ──────────────────────────────────────────────────────

#[test]
fn unknown_output_counts_as_before_range_by_range() {
    assert!(error_threshold_after([all("API Error\n", "unknown")]));

    let host = ScanHost::new("", "active");
    let mut r = Reader::new();
    let t0 = Instant::now();
    let mut s = observed(&host, &mut r, t0);
    let t1 = t0 + secs(5);
    let reply = with_origins(
        "ok\nAPI Error\n",
        &[(0, 3, "unknown"), (3, 13, "redraw")],
        &[],
    );
    scan(&mut s, &host, r.next(reply), t1);
    assert_eq!(watch(&s).last_change, t1);
    assert!(!watch(&s).saw_error);
}

// ── 구조·값 방어 ──────────────────────────────────────────────────────

#[test]
fn malformed_origins_are_ignored_without_a_panic() {
    let text = "API Error\n";
    // 대조: 올바른 응답에서는 재그리기라 세지 않는다.
    assert!(!error_threshold_after([all(text, "redraw")]));
    let malformed: [(&str, Value); 8] = [
        (
            "범위가 text를 넘음",
            with_origins(text, &[(0, 20, "redraw")], &[]),
        ),
        (
            "범위가 text 끝에 못 미침",
            with_origins(text, &[(0, 5, "redraw")], &[]),
        ),
        (
            "범위 사이 빈틈",
            with_origins(text, &[(0, 3, "redraw"), (5, 10, "redraw")], &[]),
        ),
        (
            "범위 겹침",
            with_origins(text, &[(0, 5, "redraw"), (3, 10, "redraw")], &[]),
        ),
        (
            "끊긴 자리 역순",
            with_origins(
                text,
                &[(0, 10, "redraw")],
                &[(5, "redraw_removed"), (2, "redraw_removed")],
            ),
        ),
        (
            "끊긴 자리가 text 밖",
            with_origins(text, &[(0, 10, "redraw")], &[(11, "redraw_removed")]),
        ),
        (
            "ranges 없음",
            json!({ "text": text, "text_origins": { "gaps": [] } }),
        ),
        ("모양이 다름", json!({ "text": text, "text_origins": [] })),
    ];
    for (what, reply) in malformed {
        assert!(error_threshold_after([reply]), "{what}: 지금 동작");
    }
    // 문자 경계가 아닌 오프셋. `→`는 단어 문자가 아니라 지금 동작에서 문구를 센다.
    let wide = "\u{2192}API Error";
    assert!(error_threshold_after([with_origins(
        wide,
        &[(0, 1, "redraw"), (1, wide.len(), "redraw")],
        &[],
    )]));
    assert!(error_threshold_after([with_origins(
        wide,
        &[(0, wide.len(), "redraw")],
        &[(1, "redraw_removed")],
    )]));
}

#[test]
fn unknown_values_are_read_conservatively() {
    // 모르는 출처는 unknown 이라 지금처럼 센다.
    assert!(error_threshold_after([all("API Error\n", "later")]));
    // 모르는 표시 사유는 끊긴 자리다.
    assert!(!error_threshold_after([with_origins(
        "API Error",
        &[(0, 9, "normal")],
        &[(4, "stream_renewed")],
    )]));
}

// ── 포화 창 ───────────────────────────────────────────────────────────

#[test]
fn a_full_window_changed_only_by_a_redraw_keeps_the_clock() {
    let host = ScanHost::new("", "active");
    let mut r = Reader::new();
    let t0 = Instant::now();
    let mut s = ErrorScanner::new();
    scan(
        &mut s,
        &host,
        r.next(all(&"a".repeat(SCAN_WINDOW_MAX), "normal")),
        t0,
    );
    let full = output_fingerprint(&s.window[&SID]);
    scan(
        &mut s,
        &host,
        r.next(with_origins(
            "ab",
            &[(0, 1, "normal"), (1, 2, "redraw")],
            &[],
        )),
        t0 + secs(1),
    );
    assert_ne!(
        output_fingerprint(&s.window[&SID]),
        full,
        "창 전체는 바뀐다"
    );
    assert_eq!(watch(&s).last_change, t0);
}

#[test]
fn a_full_window_kept_the_same_by_a_redraw_still_wakes_on_normal_output() {
    let host = ScanHost::new("", "active");
    let mut r = Reader::new();
    let t0 = Instant::now();
    let mut s = ErrorScanner::new();
    scan(
        &mut s,
        &host,
        r.next(all(&"ab".repeat(SCAN_WINDOW_MAX / 2), "normal")),
        t0,
    );
    let full = output_fingerprint(&s.window[&SID]);
    let t1 = t0 + secs(1);
    scan(
        &mut s,
        &host,
        r.next(with_origins(
            "ab",
            &[(0, 1, "normal"), (1, 2, "redraw")],
            &[],
        )),
        t1,
    );
    assert_eq!(output_fingerprint(&s.window[&SID]), full, "창 전체는 같다");
    assert_eq!(watch(&s).last_change, t1);
}

#[test]
fn a_kept_clock_carries_the_whole_window_to_the_next_comparison() {
    let host = ScanHost::new("", "active");
    let mut r = Reader::new();
    let t0 = Instant::now();
    let mut s = ErrorScanner::new();
    scan(
        &mut s,
        &host,
        r.next(all(&"a".repeat(SCAN_WINDOW_MAX), "normal")),
        t0,
    );
    let redraw = "b".repeat(SCAN_WINDOW_MAX);
    scan(&mut s, &host, r.next(all(&redraw, "redraw")), t0 + secs(1));
    assert_eq!(watch(&s).fingerprint, output_fingerprint(&redraw));
    assert_eq!(watch(&s).last_change, t0);
    // 셋째 회차의 비교 창은 둘째 회차의 전체 창과 같다.
    scan(&mut s, &host, r.next(all("b", "normal")), t0 + secs(2));
    assert_eq!(watch(&s).last_change, t0);
}

// ── 4 KiB 예산 ────────────────────────────────────────────────────────

#[test]
fn a_long_redraw_does_not_push_out_a_normal_error() {
    let host = ScanHost::new("", "active");
    let mut r = Reader::new();
    let t0 = Instant::now();
    let mut s = ErrorScanner::new();
    scan(&mut s, &host, r.next(all("API Error\n", "normal")), t0);
    let redraw = "~".repeat(RECENT_ERROR_BYTES + 1000);
    scan(&mut s, &host, r.next(all(&redraw, "redraw")), t0 + secs(1));
    let t2 = t0 + secs(2);
    scan(&mut s, &host, r.next(all("ok\n", "normal")), t2);
    assert_eq!(watch(&s).last_change, t2);
    assert!(watch(&s).saw_error, "앞의 일반 오류가 남아 30초 기준이다");
}

// ── 관측 유지 ─────────────────────────────────────────────────────────

#[test]
fn a_redraw_after_a_notice_keeps_the_notice() {
    let host = ScanHost::new("", "active");
    let mut r = Reader::new();
    let t0 = Instant::now();
    let mut s = observed(&host, &mut r, t0);
    let t1 = t0 + NO_OUTPUT_NOTICE_DEFAULT;
    scan(&mut s, &host, r.idle(), t1);
    assert_eq!(host.stalled_count(), 1);
    scan(
        &mut s,
        &host,
        r.next(all("screen\n", "redraw")),
        t1 + secs(1),
    );
    assert_eq!(unset_count(&host), 0, "알림 meta 를 지우지 않는다");
    assert!(watch(&s).stall_notified);
    scan(
        &mut s,
        &host,
        r.idle(),
        t1 + STALL_NOTIFY_COOLDOWN + NO_OUTPUT_NOTICE_DEFAULT,
    );
    assert_eq!(host.stalled_count(), 1, "같은 정지를 다시 알리지 않는다");
}

#[test]
fn a_redraw_after_a_restart_keeps_the_restored_notice() {
    let host = ScanHost::new("", "active");
    let mut r = Reader::new();
    let t0 = Instant::now();
    let mut s = ErrorScanner::new();
    s.restore_stall_notice(SID, unix_now_ms(), t0);
    scan(&mut s, &host, r.next(all("screen\n", "redraw")), t0);
    assert_eq!(unset_count(&host), 0);
    assert!(watch(&s).stall_notified);
    scan(&mut s, &host, r.idle(), t0 + secs(700));
    assert_eq!(host.stalled_count(), 0);
}

#[test]
fn a_redraw_first_in_a_new_turn_starts_a_watch_without_the_error_threshold() {
    let host = ScanHost::new("", "active");
    let mut r = Reader::new();
    let t0 = Instant::now();
    let mut s = observed(&host, &mut r, t0);
    s.reset_dedupe(SID);
    let t1 = t0 + secs(5);
    scan(
        &mut s,
        &host,
        r.next(all("\u{23bf} API Error: Connection error\n", "redraw")),
        t1,
    );
    assert_eq!(observation(&s), (t1, false, false, false));
}

#[test]
fn a_new_turn_and_disable_clear_the_position_and_the_breaks() {
    let host = ScanHost::new("", "active");
    let mut r = Reader::new();
    let t0 = Instant::now();
    let mut s = ErrorScanner::new();
    let reply = r.next(with_origins(
        "ok",
        &[(0, 2, "normal")],
        &[(1, "redraw_removed")],
    ));
    scan(&mut s, &host, reply.clone(), t0);
    let recent = &s.recent_output[&SID];
    assert!(recent.continuity.is_some() && !recent.breaks.is_empty());
    s.reset_dedupe(SID);
    assert!(!s.recent_output.contains_key(&SID), "새 턴");

    let mut s = ErrorScanner::new();
    scan(&mut s, &host, reply, t0);
    assert!(s.recent_output.contains_key(&SID));
    s.disable(SID);
    assert!(!s.recent_output.contains_key(&SID), "disable");
}

#[test]
fn a_redraw_does_not_rearm_the_state_check() {
    let host = ScanHost::new("", "idle");
    let mut r = Reader::new();
    let t0 = Instant::now();
    let mut s = observed(&host, &mut r, t0);
    let t1 = t0 + NO_OUTPUT_NOTICE_DEFAULT;
    scan(&mut s, &host, r.idle(), t1);
    assert_eq!(host.count("terminal.state"), 1);
    assert!(watch(&s).settled);
    scan(
        &mut s,
        &host,
        r.next(all("screen\n", "redraw")),
        t1 + secs(1),
    );
    scan(&mut s, &host, r.idle(), t1 + secs(2));
    assert_eq!(host.count("terminal.state"), 1, "상태를 다시 묻지 않는다");
}

#[test]
fn a_redraw_before_the_threshold_does_not_delay_the_notice() {
    let host = ScanHost::new("", "active");
    let mut r = Reader::new();
    let t0 = Instant::now();
    let mut s = observed(&host, &mut r, t0);
    let almost = t0 + NO_OUTPUT_NOTICE_DEFAULT - secs(1);
    scan(&mut s, &host, r.next(all("screen\n", "redraw")), almost);
    assert_eq!(host.stalled_count(), 0);
    scan(&mut s, &host, r.idle(), t0 + NO_OUTPUT_NOTICE_DEFAULT);
    assert_eq!(host.stalled_count(), 1, "남은 1초 뒤에 알린다");
    scan(&mut s, &host, r.idle(), t0 + NO_OUTPUT_NOTICE_DEFAULT * 2);
    assert_eq!(host.stalled_count(), 1);
}

/// 문구는 화면으로, 기준 시간은 정지 시계의 오류 여부로 정한다.
#[test]
fn a_redrawn_error_line_keeps_the_configured_threshold_and_screen_wording() {
    let host = ScanHost::new("", "active");
    let mut r = Reader::new();
    let t0 = Instant::now();
    let mut s = observed(&host, &mut r, t0);
    let line = "\u{23bf} API Error: Connection error\n";
    scan(&mut s, &host, r.next(all(line, "redraw")), t0 + secs(1));
    *host.screen.borrow_mut() = line.to_string();
    scan(&mut s, &host, r.idle(), t0 + STALL_QUIET + secs(1));
    assert_eq!(host.stalled_count(), 0, "30초에 알리지 않는다");
    scan(&mut s, &host, r.idle(), t0 + NO_OUTPUT_NOTICE_DEFAULT);
    assert_eq!(host.stalled_count(), 1, "설정 기준에 알린다");
    assert_eq!(
        host.stall_notice().expect("알림 기록")["kind"],
        STALL_KIND_ERROR,
        "화면의 오류 줄로 문구를 고른다"
    );
}

// ── 회귀: 전부 일반인 응답과 출처 정보가 없는 응답 ─────────────────────

#[test]
fn all_normal_replies_match_replies_without_origins() {
    let texts = [
        "\u{23bf} API Error: Connection error\n".to_string(),
        "normal output\n".repeat(RECENT_ERROR_BYTES / 14 + 1),
        "\u{276f} \n".to_string(),
        String::new(),
        "API Error\n".to_string(),
    ];
    let outcome = |shape: &dyn Fn(&str, &mut Reader) -> Value| {
        let host = ScanHost::new("", "active");
        let mut r = Reader::new();
        let mut s = ErrorScanner::new();
        let t0 = Instant::now();
        let mut seen = Vec::new();
        for (i, text) in texts.iter().enumerate() {
            let now = t0 + secs(10 * i as u64);
            scan(&mut s, &host, shape(text, &mut r), now);
            let w = watch(&s);
            seen.push((w.last_change.duration_since(t0), w.saw_error));
        }
        seen
    };
    let normal = outcome(&|t, r| r.next(all(t, "normal")));
    assert_eq!(normal, outcome(&|t, r| r.next(plain(t))));
    assert_eq!(normal, outcome(&|t, _| plain(t)));
}
