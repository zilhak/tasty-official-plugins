//! 무출력 알림 선언(`claude.quiet`)의 기록과 보류·묶음.
//!
//! 에이전트가 surface 전체나 백그라운드 작업 하나에 선언을 건다. 양수 선언은 그 기간에 생긴 무출력 알림(warning)을
//! 보내지 않고 쌓았다가 선언이 끝날 때 한 번에 묶어 보낸다. `-1` 선언은 쌓지 않고 버리며 끝나도 아무것도 보내지 않는다.
//! 선언은 시간 만료(양수만)·작업 종료(작업 선언만)·`--clear`·error 알림으로 끝난다.
//!
//! 선언과 쌓인 항목은 surface meta [`QUIET_META_KEY`] 에도 남겨 플러그인이 다시 시작돼도 이어 간다. 끝난 선언의 묶음은
//! 부모 훅이 가져갈 때까지 [`QUIET_RELEASE_META_KEY`] 에 남긴다. 부모마다 한 번씩 가져가도록 받은 부모를 기록한다.

use std::collections::HashMap;

use serde_json::{Value, json};
use tasty_plugin_agent_common::host_call::HostCall;

use super::{ErrorScanner, unix_now_ms};

/// 선언과 쌓인 항목을 남기는 surface meta.
pub(crate) const QUIET_META_KEY: &str = "claude-quiet";

/// 끝난 선언의 묶음을 부모가 가져갈 때까지 남기는 surface meta.
pub(crate) const QUIET_RELEASE_META_KEY: &str = "claude-quiet-release";

/// 묶음이 생겼음을 부모 훅에 알리는 이벤트. 매니페스트의 hook_events 와 이름이 같아야 한다.
pub(crate) const QUIET_RELEASED_EVENT: &str = "claude-quiet-released";

/// 추적이 먼저 끝난 surface(kill 등)의 묶음을 error 완료 줄이 가져갈 수 있게 두는 시간.
const ORPHAN_KEPT_MS: u64 = 60_000;

/// 묶음을 두는 시간과, 받을 부모가 모두 가져간 묶음을 남기는 개수. 다 가져가지 않은 묶음은 시간이 지나야 지운다.
const RELEASE_KEPT_MS: u64 = 60 * 60 * 1000;
const RELEASES_KEPT: usize = 16;

/// 선언 하나가 따로 적는 항목(서로 다른 대상) 수. 넘는 알림은 [`Overflow`] 로 센다.
const HELD_KEYS_MAX: usize = 64;

/// 항목 이름에 남기는 최대 글자 수. 묶음 줄에도 이 길이로 적는다.
pub(crate) const LABEL_CHARS: usize = 80;

/// 양수 `secs` 의 끝 시각. 너무 큰 값은 감기지 않고 가장 먼 시각에 머문다(상한 없는 양수 선언).
fn until_of(secs: i64, now_ms: u64) -> Option<u64> {
    let secs = u64::try_from(secs).ok().filter(|s| *s > 0)?;
    Some(now_ms.saturating_add(secs.saturating_mul(1000)))
}

/// 줄바꿈을 공백으로 바꾸고 앞부분만 남긴다. 한 줄이 알림 하나라는 로그 계약을 지킨다.
pub(crate) fn one_line(text: &str) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect();
    let head: String = flat.chars().take(LABEL_CHARS).collect();
    if head.chars().count() < flat.chars().count() {
        format!("{head}…")
    } else {
        head
    }
}

/// 쌓인 무출력 알림의 종류. 묶음 문구에 번역하지 않는 표기로 적는다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HeldKind {
    /// 기다리는 작업의 출력 파일이 조용하다.
    Task,
    /// 백그라운드 대기 중 화면 출력이 조용하다(출력 파일을 못 찾음).
    Wait,
    /// 대기가 아닌 턴 진행 중 오류 없이 조용하다.
    Turn,
}

impl HeldKind {
    pub(crate) fn token(self) -> &'static str {
        match self {
            Self::Task => "task quiet",
            Self::Wait => "wait quiet",
            Self::Turn => "turn quiet",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        [Self::Task, Self::Wait, Self::Turn]
            .into_iter()
            .find(|k| k.token() == s)
    }
}

/// 쌓인 항목 하나. 같은 대상(작업 id, 대기의 작업 집합, 턴)의 반복은 한 항목으로 합친다.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Held {
    pub key: String,
    pub kind: HeldKind,
    pub label: String,
    pub count: u32,
    /// 처음·마지막으로 쌓은 시각(Unix ms).
    pub first_ms: u64,
    pub last_ms: u64,
    /// 쌓을 때 본 조용한 시간 중 가장 긴 값.
    pub quiet_ms: u64,
}

impl Held {
    fn to_json(&self) -> Value {
        json!({
            "key": self.key,
            "kind": self.kind.token(),
            "label": self.label,
            "count": self.count,
            "first_ms": self.first_ms,
            "last_ms": self.last_ms,
            "quiet_ms": self.quiet_ms,
        })
    }

    fn from_json(v: &Value) -> Option<Self> {
        let num = |k: &str| v.get(k).and_then(Value::as_u64);
        Some(Self {
            key: v.get("key")?.as_str()?.to_string(),
            kind: HeldKind::parse(v.get("kind")?.as_str()?)?,
            label: v.get("label")?.as_str()?.to_string(),
            count: num("count")?.try_into().ok()?,
            first_ms: num("first_ms")?,
            last_ms: num("last_ms")?,
            quiet_ms: num("quiet_ms")?,
        })
    }
}

/// 항목 수 상한을 넘어 따로 적지 못한 알림의 요약.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub(crate) struct Overflow {
    pub count: u32,
    pub first_ms: u64,
    pub last_ms: u64,
}

impl Overflow {
    fn add(&mut self, count: u32, first_ms: u64, last_ms: u64) {
        self.first_ms = if self.count == 0 {
            first_ms
        } else {
            self.first_ms.min(first_ms)
        };
        self.last_ms = self.last_ms.max(last_ms);
        self.count = self.count.saturating_add(count);
    }

    fn to_json(self) -> Value {
        json!({ "count": self.count, "first_ms": self.first_ms, "last_ms": self.last_ms })
    }

    fn from_json(v: Option<&Value>) -> Option<Self> {
        let v = v?;
        let num = |k: &str| v.get(k).and_then(Value::as_u64);
        Some(Self {
            count: num("count")?.try_into().ok()?,
            first_ms: num("first_ms")?,
            last_ms: num("last_ms")?,
        })
    }
}

/// 선언 하나. `task` 가 없으면 surface 전체다. `secs` 는 양수(보류)나 `-1`(버림)이다.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Decl {
    pub task: Option<String>,
    pub secs: i64,
    pub set_at_ms: u64,
    /// 끝나는 시각(Unix ms). `-1` 은 없다.
    pub until_ms: Option<u64>,
    pub held: Vec<Held>,
    /// 항목 수 상한을 넘은 알림.
    pub overflow: Option<Overflow>,
}

impl Decl {
    /// 보류하는 선언인가. `-1` 은 버린다.
    pub(crate) fn holds(&self) -> bool {
        self.secs > 0
    }

    fn active_at(&self, now_ms: u64) -> bool {
        self.until_ms.is_none_or(|u| now_ms < u)
    }

    fn hold(&mut self, item: Held) {
        hold_item(&mut self.held, &mut self.overflow, item);
    }

    fn to_json(&self) -> Value {
        json!({
            "task": self.task,
            "mode": if self.holds() { "hold" } else { "discard" },
            "secs": self.secs,
            "set_at_ms": self.set_at_ms,
            "until_ms": self.until_ms,
            "held": self.held.iter().map(Held::to_json).collect::<Vec<_>>(),
            "overflow": self.overflow.map(Overflow::to_json),
        })
    }

    fn from_json(v: &Value) -> Option<Self> {
        let (held, overflow) = held_from_json(v.get("held"), v.get("overflow"));
        Some(Self {
            task: v.get("task").and_then(Value::as_str).map(String::from),
            secs: v.get("secs")?.as_i64()?,
            set_at_ms: v.get("set_at_ms")?.as_u64()?,
            until_ms: v.get("until_ms").and_then(Value::as_u64),
            held,
            overflow,
        })
    }
}

/// 같은 대상이면 합치고, 항목 수 상한을 넘으면 [`Overflow`] 로 센다. 이름은 한 줄로 잘라 둔다.
fn hold_item(held: &mut Vec<Held>, overflow: &mut Option<Overflow>, mut item: Held) {
    if let Some(h) = held.iter_mut().find(|h| h.key == item.key) {
        h.count = h.count.saturating_add(item.count);
        h.last_ms = h.last_ms.max(item.last_ms);
        h.first_ms = h.first_ms.min(item.first_ms);
        h.quiet_ms = h.quiet_ms.max(item.quiet_ms);
    } else if held.len() >= HELD_KEYS_MAX {
        overflow
            .get_or_insert_with(Overflow::default)
            .add(item.count, item.first_ms, item.last_ms);
    } else {
        item.label = one_line(&item.label);
        held.push(item);
    }
}

/// meta 의 항목 배열을 읽는다. meta 는 밖에서 바뀔 수 있어 쌓을 때와 같은 이름 길이·항목 수 상한을 다시 건다.
fn held_from_json(
    items: Option<&Value>,
    overflow: Option<&Value>,
) -> (Vec<Held>, Option<Overflow>) {
    let mut held = Vec::new();
    let mut overflow = Overflow::from_json(overflow);
    for item in items
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Held::from_json)
    {
        hold_item(&mut held, &mut overflow, item);
    }
    (held, overflow)
}

/// 선언이 끝난 이유. 묶음 문구에 번역하지 않는 표기로 적는다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EndReason {
    Expired,
    TaskEnded,
    Cleared,
    /// 같은 대상의 양수 선언을 `-1` 로 바꿨다.
    Replaced,
    /// error 알림이 나갔다. 묶음은 그 error 줄에 붙는다.
    Error,
    /// 완료 줄 없이 추적이 끝났다. error 완료 줄이 잠시 안에 오면 그 줄에 붙고, 아니면 버린다.
    SurfaceGone,
}

impl EndReason {
    pub(crate) fn token(self) -> &'static str {
        match self {
            Self::Expired => "expired",
            Self::TaskEnded => "task ended",
            Self::Cleared => "cleared",
            Self::Replaced => "replaced",
            Self::Error => "error",
            Self::SurfaceGone => "surface gone",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        [
            Self::Expired,
            Self::TaskEnded,
            Self::Cleared,
            Self::Replaced,
            Self::Error,
            Self::SurfaceGone,
        ]
        .into_iter()
        .find(|r| r.token() == s)
    }
}

/// 끝난 양수 선언의 묶음. 쌓인 항목이 있을 때만 만든다.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Release {
    pub id: String,
    pub reason: EndReason,
    pub task: Option<String>,
    pub secs: i64,
    /// 선언한 시각과 끝난 시각(Unix ms).
    pub set_at_ms: u64,
    pub at_ms: u64,
    pub items: Vec<Held>,
    pub overflow: Option<Overflow>,
    /// 이 묶음을 받은 부모 surface.
    pub delivered_to: Vec<u32>,
    /// 묶음이 생길 때 이 surface 에 묶음 훅을 건 부모. 모두 받으면 전달이 끝난 것이다. 비었으면 한 부모가 받으면 끝난다.
    pub recipients: Vec<u32>,
}

impl Release {
    fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "reason": self.reason.token(),
            "task": self.task,
            "secs": self.secs,
            "set_at_ms": self.set_at_ms,
            "at_ms": self.at_ms,
            "items": self.items.iter().map(Held::to_json).collect::<Vec<_>>(),
            "overflow": self.overflow.map(Overflow::to_json),
            "delivered_to": self.delivered_to,
            "recipients": self.recipients,
        })
    }

    fn from_json(v: &Value) -> Option<Self> {
        v.get("items")?.as_array()?;
        let (items, overflow) = held_from_json(v.get("items"), v.get("overflow"));
        Some(Self {
            id: v.get("id")?.as_str()?.to_string(),
            reason: EndReason::parse(v.get("reason")?.as_str()?)?,
            task: v.get("task").and_then(Value::as_str).map(String::from),
            secs: v.get("secs")?.as_i64()?,
            set_at_ms: v.get("set_at_ms")?.as_u64()?,
            at_ms: v.get("at_ms")?.as_u64()?,
            items,
            overflow,
            delivered_to: surfaces(v.get("delivered_to")),
            recipients: surfaces(v.get("recipients")),
        })
    }

    /// error 완료 줄에만 붙는 묶음인가. error 로 끝난 선언과 완료 줄 없이 사라진 surface 의 묶음이다.
    /// 일반 묶음 처리자가 먼저 가져가 `[warning]` 으로 쓰면 error 줄에 붙지 않게 되므로 막는다.
    fn error_only(&self) -> bool {
        matches!(self.reason, EndReason::Error | EndReason::SurfaceGone)
    }

    /// 받을 부모가 모두 가져갔는가. 받을 부모를 모르면 한 부모라도 가져갔는가.
    fn taken_by_all(&self) -> bool {
        !self.delivered_to.is_empty()
            && self
                .recipients
                .iter()
                .all(|c| self.delivered_to.contains(c))
    }

    /// 쌓인 알림 수(따로 적지 못한 것 포함).
    pub(crate) fn held_count(&self) -> u32 {
        self.items
            .iter()
            .map(|i| i.count)
            .fold(self.overflow.map_or(0, |o| o.count), u32::saturating_add)
    }
}

/// 보류 판정에 넘기는 무출력 알림.
#[derive(Debug)]
pub(crate) enum QuietNotice {
    /// 출력 파일이 조용한 작업 (id, 이름).
    Tasks(Vec<(String, String)>),
    /// 출력 파일을 못 찾은 대기. 기다리는 작업 id 와 알림에 쓸 이름.
    Wait { ids: Vec<String>, label: String },
    /// 대기가 아닌 턴의 오류 없는 정지.
    Turn,
}

/// 보류 판정 뒤에 부모에게 바로 보낼 것.
#[derive(Debug, PartialEq)]
pub(crate) enum Send {
    /// 선언에 걸리지 않았다. 지금처럼 보낸다.
    All,
    /// 선언에 걸리지 않은 작업의 이름만 보낸다.
    Labels(Vec<String>),
    /// 모두 보류하거나 버렸다.
    Nothing,
}

/// 한 surface 의 선언과 아직 다 전달하지 못한 묶음.
#[derive(Debug, Default)]
pub(crate) struct QuietState {
    pub decls: Vec<Decl>,
    pub releases: Vec<Release>,
}

impl QuietState {
    /// 같은 대상의 선언 위치.
    fn position(&self, task: Option<&str>) -> Option<usize> {
        self.decls.iter().position(|d| d.task.as_deref() == task)
    }

    /// 선언을 넣거나 바꾼다. 양수→양수는 쌓인 것을 두고 끝 시각만 바꾼다. 양수→`-1` 은 앞 선언을 끝내 묶음을 돌려준다.
    /// `-1`→양수는 새로 쌓는다.
    pub(crate) fn declare(&mut self, task: Option<String>, secs: i64, now_ms: u64) -> Vec<Release> {
        let until_ms = until_of(secs, now_ms);
        if let Some(i) = self.position(task.as_deref())
            && self.decls[i].holds()
            && secs > 0
        {
            // 양수→양수: 쌓인 것과 선언 시각을 두고, 끝 시각은 늦출 때만 바꾼다.
            let d = &mut self.decls[i];
            if until_ms > d.until_ms {
                d.until_ms = until_ms;
                d.secs = secs;
            }
            return Vec::new();
        }
        let mut released = Vec::new();
        if let Some(i) = self.position(task.as_deref()) {
            let old = self.decls.remove(i);
            released.extend(release_of(old, EndReason::Replaced, now_ms));
        }
        self.decls.push(Decl {
            task,
            secs,
            set_at_ms: now_ms,
            until_ms,
            held: Vec::new(),
            overflow: None,
        });
        released
    }

    /// 대상 선언의 끝 시각. 재선언이 끝 시각을 늦추지 않았을 수 있어 응답은 이 값을 쓴다.
    pub(crate) fn until_for(&self, task: Option<&str>) -> Option<u64> {
        self.position(task).and_then(|i| self.decls[i].until_ms)
    }

    /// 조건에 맞는 선언을 끝내고 묶음을 돌려준다.
    pub(crate) fn end(
        &mut self,
        mut pick: impl FnMut(&Decl) -> bool,
        reason: EndReason,
        now_ms: u64,
    ) -> (usize, Vec<Release>) {
        let (ended, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.decls)
            .into_iter()
            .partition(|d| pick(d));
        self.decls = kept;
        let count = ended.len();
        let released = ended
            .into_iter()
            .filter_map(|d| release_of(d, reason, now_ms))
            .collect();
        (count, released)
    }

    /// 무출력 알림을 선언에 맞춰 보류하거나 버린다. 파일 기준 알림은 작업 선언이 surface 전체 선언보다 먼저고,
    /// 화면 기준 대기 알림은 surface 전체 선언이 작업 선언보다 먼저다.
    pub(crate) fn dispose(&mut self, notice: QuietNotice, quiet_ms: u64, now_ms: u64) -> Send {
        let task_decl = |decls: &[Decl], id: &str| {
            decls
                .iter()
                .position(|d| d.task.as_deref() == Some(id) && d.active_at(now_ms))
        };
        let surface_decl = self
            .decls
            .iter()
            .position(|d| d.task.is_none() && d.active_at(now_ms));
        let item = |key: String, kind, label: String| Held {
            key,
            kind,
            label,
            count: 1,
            first_ms: now_ms,
            last_ms: now_ms,
            quiet_ms,
        };
        match notice {
            QuietNotice::Tasks(tasks) => {
                let mut send = Vec::new();
                let mut covered = 0;
                for (id, label) in tasks {
                    match task_decl(&self.decls, &id).or(surface_decl) {
                        Some(i) => {
                            covered += 1;
                            self.hold_into(i, item(id, HeldKind::Task, label));
                        }
                        None => send.push(label),
                    }
                }
                match (covered, send.is_empty()) {
                    (0, _) => Send::All,
                    (_, true) => Send::Nothing,
                    (_, false) => Send::Labels(send),
                }
            }
            QuietNotice::Wait { ids, label } => {
                let targets: Option<Vec<usize>> = match surface_decl {
                    Some(i) => Some(vec![i]),
                    None if ids.is_empty() => None,
                    None => ids.iter().map(|id| task_decl(&self.decls, id)).collect(),
                };
                let Some(targets) = targets else {
                    return Send::All;
                };
                // 알림 하나는 한 선언에만 쌓는다. 양수 선언 중 끝 시각이 가장 늦은 것, 모두 `-1` 이면 버린다.
                let target = targets
                    .iter()
                    .copied()
                    .filter(|&i| self.decls[i].holds())
                    .max_by_key(|&i| self.decls[i].until_ms)
                    .unwrap_or(targets[0]);
                // 같은 작업 집합이면 Stop 이 알려 준 순서와 관계없이 한 항목이다.
                let mut set: Vec<&str> = ids.iter().map(String::as_str).collect();
                set.sort_unstable();
                set.dedup();
                let key = format!("wait:{}", set.join(","));
                self.hold_into(target, item(key, HeldKind::Wait, label));
                Send::Nothing
            }
            QuietNotice::Turn => match surface_decl {
                Some(i) => {
                    self.hold_into(i, item("turn".into(), HeldKind::Turn, "turn".into()));
                    Send::Nothing
                }
                None => Send::All,
            },
        }
    }

    /// 양수 선언이면 쌓고 `-1` 이면 버린다.
    fn hold_into(&mut self, index: usize, item: Held) {
        let decl = &mut self.decls[index];
        if decl.holds() {
            decl.hold(item);
        } else {
            tracing::info!(
                "claude quiet: discarded a {} notice for {} under a -1 declaration",
                item.kind.token(),
                item.key
            );
        }
    }

    /// 부모 `caller` 가 아직 받지 않은 묶음을 꺼내고 받은 것으로 기록한다. error 완료 줄만 받는 묶음은 `with_error` 일 때만 꺼낸다.
    pub(crate) fn take_for(&mut self, caller: u32, with_error: bool) -> Vec<Release> {
        let mut taken = Vec::new();
        for r in &mut self.releases {
            if r.delivered_to.contains(&caller) || (r.error_only() && !with_error) {
                continue;
            }
            r.delivered_to.push(caller);
            taken.push(r.clone());
        }
        taken
    }

    /// 오래된 묶음을 지운다. 추적이 끝난 surface 의 묶음은 error 완료 줄을 잠시만 기다린다.
    /// 개수 상한은 받을 부모가 모두 가져간 묶음에만 적용한다. 다 가져가지 않은 묶음은 보관 시간이 지나야 지운다.
    /// 추적이 끝난 surface 의 묶음은 개수 상한에서 빼고 보관 시간으로만 지운다. 추적이 끝날 때 그 surface 의 선언을
    /// 모두 비우므로 다시 선언하지 않으면 더 생기지 않고(같은 id 를 다시 추적해 선언하면 다시 생길 수 있다),
    /// 생긴 뒤 60초만 두므로 상한 없이도 쌓이지 않는다.
    pub(crate) fn prune(&mut self, now_ms: u64) {
        self.releases.retain(|r| {
            let kept = if r.reason == EndReason::SurfaceGone {
                ORPHAN_KEPT_MS
            } else {
                RELEASE_KEPT_MS
            };
            let keep = now_ms.saturating_sub(r.at_ms) < kept;
            if !keep && !r.taken_by_all() {
                tracing::info!(
                    "claude quiet: dropped {} held notice(s) not taken by every parent ({}, taken by {:?} of {:?})",
                    r.held_count(),
                    r.reason.token(),
                    r.delivered_to,
                    r.recipients
                );
            }
            keep
        });
        let mut over = self.releases.len().saturating_sub(RELEASES_KEPT);
        self.releases.retain(|r| {
            if over > 0 && r.reason != EndReason::SurfaceGone && r.taken_by_all() {
                over -= 1;
                false
            } else {
                true
            }
        });
    }

    fn is_empty(&self) -> bool {
        self.decls.is_empty() && self.releases.is_empty()
    }
}

/// 끝난 선언의 묶음. 양수 선언에 쌓인 것이 있을 때만 만든다.
fn release_of(decl: Decl, reason: EndReason, now_ms: u64) -> Option<Release> {
    if !decl.holds() || (decl.held.is_empty() && decl.overflow.is_none()) {
        return None;
    }
    Some(Release {
        id: uuid::Uuid::new_v4().simple().to_string(),
        reason,
        task: decl.task,
        secs: decl.secs,
        set_at_ms: decl.set_at_ms,
        at_ms: now_ms,
        items: decl.held,
        overflow: decl.overflow,
        delivered_to: Vec::new(),
        recipients: Vec::new(),
    })
}

/// 묶음마다 받을 부모를 적는다.
fn with_recipients(
    released: Vec<Release>,
    recipients: &[u32],
) -> impl Iterator<Item = Release> + '_ {
    released.into_iter().map(|r| Release {
        recipients: recipients.to_vec(),
        ..r
    })
}

/// surface id 배열. 숫자가 아닌 값은 건너뛴다.
fn surfaces(v: Option<&Value>) -> Vec<u32> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|n| n.as_u64().and_then(|n| u32::try_from(n).ok()))
                .collect()
        })
        .unwrap_or_default()
}

/// 이 surface 에 묶음 훅([`QUIET_RELEASED_EVENT`])을 건 부모. 완료·error 훅과 함께 등록하므로 error 줄을 받을 부모와 같다.
fn release_recipients<H: HostCall>(host: &H, surface_id: u32) -> Vec<u32> {
    let hooks = match host.call("hook.list", json!({ "surface_id": surface_id })) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("claude quiet s{surface_id}: listing hooks failed: {e}");
            return Vec::new();
        }
    };
    let mut callers: Vec<u32> = hooks
        .as_array()
        .map_or(&[][..], Vec::as_slice)
        .iter()
        .filter(|h| h.get("event").and_then(Value::as_str) == Some(QUIET_RELEASED_EVENT))
        .filter_map(|h| h.get("command").and_then(Value::as_str))
        .filter_map(|c| {
            let caller = c
                .strip_prefix(crate::quiet_notice::NOTIFY_QUIET_RELEASE_PREFIX)?
                .split_whitespace()
                .next()?
                .parse()
                .ok()?;
            (c == crate::quiet_notice::notify_quiet_release_command(caller, surface_id))
                .then_some(caller)
        })
        .collect();
    callers.sort_unstable();
    callers.dedup();
    callers
}

/// 선언 meta 값을 읽는다.
fn parse_decls(raw: &str) -> Vec<Decl> {
    serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|v| {
            v.get("decls")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Decl::from_json).collect())
        })
        .unwrap_or_default()
}

fn parse_releases(raw: &str) -> Vec<Release> {
    serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|v| {
            v.get("releases")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Release::from_json).collect())
        })
        .unwrap_or_default()
}

/// 선언 결과. `claude.quiet` 의 응답이 된다.
pub(crate) struct Declared {
    pub mode: &'static str,
    pub until_ms: Option<u64>,
}

impl ErrorScanner {
    /// 이 surface 의 선언 상태. 처음 보면 meta 에서 되살린다(플러그인 재시작).
    pub(crate) fn quiet_state<H: HostCall>(
        &mut self,
        host: &H,
        surface_id: u32,
    ) -> &mut QuietState {
        self.quiet.entry(surface_id).or_insert_with(|| {
            let read = |key| crate::stop_pairing::meta_value(host, surface_id, key);
            QuietState {
                decls: read(QUIET_META_KEY)
                    .as_deref()
                    .map(parse_decls)
                    .unwrap_or_default(),
                releases: read(QUIET_RELEASE_META_KEY)
                    .as_deref()
                    .map(parse_releases)
                    .unwrap_or_default(),
            }
        })
    }

    /// 메모리의 선언과 묶음을 meta 에 쓴다. 비었으면 지운다.
    fn save_quiet<H: HostCall>(&self, host: &H, surface_id: u32) {
        let state = self.quiet.get(&surface_id);
        let decls = state.map(|s| &s.decls).filter(|d| !d.is_empty());
        let releases = state.map(|s| &s.releases).filter(|r| !r.is_empty());
        let pairs = [
            (
                QUIET_META_KEY,
                decls.map(|d| json!({ "decls": d.iter().map(Decl::to_json).collect::<Vec<_>>() })),
            ),
            (
                QUIET_RELEASE_META_KEY,
                releases.map(
                    |r| json!({ "releases": r.iter().map(Release::to_json).collect::<Vec<_>>() }),
                ),
            ),
        ];
        for (key, value) in pairs {
            let result = match value {
                Some(v) => host.call(
                    "surface.meta.set",
                    json!({ "surface_id": surface_id, "key": key, "value": v.to_string() }),
                ),
                None => host.call(
                    "surface.meta.unset",
                    json!({ "surface_id": surface_id, "key": key }),
                ),
            };
            if let Err(e) = result {
                tracing::warn!("claude quiet s{surface_id}: writing {key} failed: {e}");
            }
        }
    }

    /// 끝난 선언의 묶음을 남긴다. `notify` 면 부모 훅을 부른다(error 로 끝난 묶음은 error 줄이 가져간다).
    fn keep_releases<H: HostCall>(
        &mut self,
        host: &H,
        surface_id: u32,
        released: Vec<Release>,
        notify: bool,
    ) -> bool {
        let any = !released.is_empty();
        let recipients = if any {
            release_recipients(host, surface_id)
        } else {
            Vec::new()
        };
        let state = self.quiet_state(host, surface_id);
        state
            .releases
            .extend(with_recipients(released, &recipients));
        state.prune(unix_now_ms());
        self.save_quiet(host, surface_id);
        if any
            && notify
            && let Err(e) = host.call(
                "surface.fire_hook",
                json!({ "surface_id": surface_id, "event": QUIET_RELEASED_EVENT }),
            )
        {
            tracing::warn!("claude quiet s{surface_id}: firing {QUIET_RELEASED_EVENT} failed: {e}");
        }
        any
    }

    /// 선언을 넣거나 바꾼다. 인자 검사는 호출자가 한다.
    pub(crate) fn declare_quiet<H: HostCall>(
        &mut self,
        host: &H,
        surface_id: u32,
        task: Option<String>,
        secs: i64,
    ) -> Declared {
        let now_ms = unix_now_ms();
        let state = self.quiet_state(host, surface_id);
        let released = state.declare(task.clone(), secs, now_ms);
        let until_ms = state.until_for(task.as_deref());
        self.keep_releases(host, surface_id, released, true);
        Declared {
            mode: if secs > 0 { "hold" } else { "discard" },
            until_ms,
        }
    }

    /// `--clear`. 작업을 주면 그 선언만, 없으면 그 surface 의 모든 선언을 끝낸다. (끝낸 수, 묶음을 보냈는지).
    pub(crate) fn clear_quiet<H: HostCall>(
        &mut self,
        host: &H,
        surface_id: u32,
        task: Option<&str>,
    ) -> (usize, bool) {
        let (count, released) = self.quiet_state(host, surface_id).end(
            |d| task.is_none() || d.task.as_deref() == task,
            EndReason::Cleared,
            unix_now_ms(),
        );
        (count, self.keep_releases(host, surface_id, released, true))
    }

    /// 끝 시각이 지난 양수 선언을 끝낸다. 폴링이 추적 중인 surface 마다 부른다.
    pub(crate) fn release_expired<H: HostCall>(&mut self, host: &H, surface_id: u32, now_ms: u64) {
        let state = self.quiet_state(host, surface_id);
        if !state.decls.iter().any(|d| !d.active_at(now_ms)) {
            return;
        }
        let (_, released) = state.end(|d| !d.active_at(now_ms), EndReason::Expired, now_ms);
        self.keep_releases(host, surface_id, released, true);
    }

    /// 작업이 끝났으면 그 작업 선언을 끝낸다.
    pub(crate) fn end_quiet_tasks<H: HostCall>(
        &mut self,
        host: &H,
        surface_id: u32,
        mut ended: impl FnMut(&str) -> bool,
    ) {
        let state = self.quiet_state(host, surface_id);
        if !state
            .decls
            .iter()
            .any(|d| d.task.as_deref().is_some_and(&mut ended))
        {
            return;
        }
        let (count, released) = state.end(
            |d| d.task.as_deref().is_some_and(&mut ended),
            EndReason::TaskEnded,
            unix_now_ms(),
        );
        tracing::info!(
            "claude quiet s{surface_id}: {count} task declaration(s) ended with their task"
        );
        self.keep_releases(host, surface_id, released, true);
    }

    /// error 알림이 나간다. 그 surface 의 선언을 모두 끝내고 묶음은 error 줄이 가져가게 둔다.
    pub(crate) fn end_quiet_on_error<H: HostCall>(&mut self, host: &H, surface_id: u32) {
        let state = self.quiet_state(host, surface_id);
        if state.decls.is_empty() {
            return;
        }
        let (_, released) = state.end(|_| true, EndReason::Error, unix_now_ms());
        self.keep_releases(host, surface_id, released, false);
    }

    /// 부모 `caller` 가 아직 받지 않은 묶음을 꺼낸다.
    pub(crate) fn take_quiet_releases<H: HostCall>(
        &mut self,
        host: &H,
        surface_id: u32,
        caller: u32,
        with_error: bool,
    ) -> Vec<Release> {
        let state = self.quiet_state(host, surface_id);
        state.prune(unix_now_ms());
        let taken = state.take_for(caller, with_error);
        if !taken.is_empty() {
            self.save_quiet(host, surface_id);
        }
        taken
    }

    /// 완료 줄 없이 추적이 끝났다. 쌓인 것은 error 완료 줄이 잠시 안에 오면 그 줄에 붙고, 아니면 버린다.
    /// 받을 부모는 다른 경로처럼 기록하되(surface 가 없어 조회가 안 되면 빈 집합) 개수 상한으로는 지우지 않고 잠시 둔 뒤에만 지운다.
    /// surface 가 이미 없을 수 있어 meta 는 지우기만 시도한다.
    pub(crate) fn drop_quiet<H: HostCall>(&mut self, host: &H, surface_id: u32) {
        let Some(state) = self.quiet.get_mut(&surface_id) else {
            return;
        };
        let (count, released) = state.end(|_| true, EndReason::SurfaceGone, unix_now_ms());
        if count > 0 {
            tracing::info!(
                "claude quiet s{surface_id}: {count} declaration(s) ended with the surface"
            );
        }
        if !released.is_empty() {
            let recipients = release_recipients(host, surface_id);
            state
                .releases
                .extend(with_recipients(released, &recipients));
        }
        if count > 0 {
            for key in [QUIET_META_KEY, QUIET_RELEASE_META_KEY] {
                if let Err(e) = host.call(
                    "surface.meta.unset",
                    json!({ "surface_id": surface_id, "key": key }),
                ) {
                    tracing::debug!("claude quiet s{surface_id}: clearing {key} failed: {e}");
                }
            }
        }
    }

    /// 모든 surface 의 오래된 묶음을 지운다. 추적하지 않는 surface 의 빈 상태도 버린다.
    pub(crate) fn prune_quiet(&mut self, now_ms: u64) {
        for state in self.quiet.values_mut() {
            state.prune(now_ms);
        }
        let enabled = &self.enabled;
        self.quiet
            .retain(|sid, state| enabled.contains_key(sid) || !state.is_empty());
    }

    /// 무출력 알림을 선언에 맞춰 보류하거나 버린다. 쌓았으면 meta 에 남긴다.
    pub(crate) fn dispose_quiet<H: HostCall>(
        &mut self,
        host: &H,
        surface_id: u32,
        notice: QuietNotice,
        quiet_ms: u64,
    ) -> Send {
        let state = self.quiet_state(host, surface_id);
        if state.decls.is_empty() {
            return Send::All;
        }
        let send = state.dispose(notice, quiet_ms, unix_now_ms());
        if send != Send::All {
            self.save_quiet(host, surface_id);
        }
        send
    }
}

/// 여러 surface 의 선언을 묶는 표. [`ErrorScanner`] 의 필드다.
pub(crate) type QuietTable = HashMap<u32, QuietState>;

#[cfg(test)]
#[path = "quiet_tests.rs"]
mod tests;
