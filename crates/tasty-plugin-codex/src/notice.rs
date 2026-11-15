//! 부모 완료 로그에 남기는 Codex child 알림의 종류와 타입. 줄마다 타입 표지를 붙여 reader 가 언어와 무관하게 거른다.
//! 새 종류를 더하면 [`NoticeKind::notice_type`] 의 `match` 가 컴파일을 막아 타입을 정하게 한다.

use tasty_utils::notify::NoticeType;

/// 완료 로그에 쓰는 Codex 알림 종류.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NoticeKind {
    /// 일을 시작한 뒤 턴이 끝났다(idle).
    StateChanged,
    /// 도구 승인을 기다린다(needs_input). 부모가 답해야 하므로 warning 이다.
    NeedsInput,
    /// child 프로세스가 끝났다. 부모가 kill 로 끝낸 경우도 구분하지 않는다.
    Exited,
    /// 원격 attach child 의 턴 결과를 저장했다.
    RemoteResult,
    /// 완료 줄에 샌드박스 초기화 실패 안내가 붙었다.
    SandboxFailure,
}

impl NoticeKind {
    /// 오류가 섞인 종류는 무조건 `Error` 다. 승인 대기는 warning 이다.
    pub(crate) fn notice_type(self) -> NoticeType {
        match self {
            Self::StateChanged | Self::RemoteResult => NoticeType::Info,
            Self::NeedsInput => NoticeType::Warning,
            Self::Exited | Self::SandboxFailure => NoticeType::Error,
        }
    }

    /// 이 종류의 문구 키. 문구 키와 종류의 짝 시험이 쓴다.
    #[cfg(test)]
    fn message_key(self) -> &'static str {
        match self {
            Self::StateChanged | Self::Exited => "codex.notify.done_message",
            Self::NeedsInput => "codex.notify.needs_input_message",
            Self::RemoteResult => "codex.notify.remote_result_message",
            Self::SandboxFailure => "codex.notify.sandbox_hint",
        }
    }

    #[cfg(test)]
    const ALL: [Self; 5] = [
        Self::StateChanged,
        Self::NeedsInput,
        Self::Exited,
        Self::RemoteResult,
        Self::SandboxFailure,
    ];

    /// 완료 줄의 종류. 샌드박스 안내가 붙었거나 대상이 끝났으면 오류다.
    pub(crate) fn of_done_line(target_state: Option<&str>, sandbox_hint: bool) -> Self {
        if sandbox_hint {
            Self::SandboxFailure
        } else if target_state == Some("exited") {
            Self::Exited
        } else {
            Self::StateChanged
        }
    }
}

/// 부모 완료 로그에 종류의 타입 표지를 붙여 한 줄을 쓴다.
#[cfg(not(test))]
pub(crate) fn append(caller_surface: u32, kind: NoticeKind, line: &str) -> std::io::Result<()> {
    tasty_utils::notify::append_notice(caller_surface, kind.notice_type(), line)
}

/// 시험은 실제 완료 로그(`TASTY_PARENT_HOME`)에 쓰지 않고 이 스레드에 모은다. 표지는 실제 쓰기와 같게 붙인다.
#[cfg(test)]
pub(crate) fn append(caller_surface: u32, kind: NoticeKind, line: &str) -> std::io::Result<()> {
    let line = tasty_utils::notify::notice_line(kind.notice_type(), line);
    test_log::LINES.with(|l| l.borrow_mut().push((caller_surface, line)));
    Ok(())
}

#[cfg(test)]
pub(crate) mod test_log {
    use std::cell::RefCell;

    thread_local! {
        pub(super) static LINES: RefCell<Vec<(u32, String)>> = const { RefCell::new(Vec::new()) };
    }

    /// 이 스레드에서 쓴 줄을 꺼낸다.
    pub(crate) fn take() -> Vec<(u32, String)> {
        LINES.with(|l| std::mem::take(&mut *l.borrow_mut()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `[codex.notify]` 의 `_message` 키는 모두 어떤 종류의 문구다. 종류 없이 문구만 더하면 이 시험이 실패한다.
    #[test]
    fn every_notify_message_key_belongs_to_a_notice_kind() {
        let lang = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("lang/en.toml");
        let table: toml::Table = std::fs::read_to_string(lang)
            .expect("en.toml")
            .parse()
            .expect("en.toml parses");
        let notify = table["codex"]["notify"].as_table().expect("[codex.notify]");
        let keys: Vec<String> = NoticeKind::ALL
            .iter()
            .map(|k| k.message_key().to_string())
            .collect();
        let mut messages = 0;
        for name in notify.keys().filter(|k| k.ends_with("_message")) {
            messages += 1;
            let key = format!("codex.notify.{name}");
            assert!(keys.contains(&key), "{key} has no NoticeKind");
        }
        assert!(messages >= 2, "{messages} message keys read");
        for key in &keys {
            let name = key.trim_start_matches("codex.notify.");
            assert!(notify.contains_key(name), "{key} is not in en.toml");
        }
    }

    #[test]
    fn a_done_line_is_an_error_when_the_child_exited_or_the_sandbox_failed() {
        let ty = |state, hint| NoticeKind::of_done_line(state, hint).notice_type();
        assert_eq!(ty(Some("idle"), false), NoticeType::Info);
        assert_eq!(ty(Some("needs_input"), false), NoticeType::Info);
        assert_eq!(ty(None, false), NoticeType::Info);
        assert_eq!(ty(Some("exited"), false), NoticeType::Error);
        assert_eq!(ty(Some("idle"), true), NoticeType::Error);
        assert_eq!(NoticeKind::RemoteResult.notice_type(), NoticeType::Info);
        assert_eq!(NoticeKind::NeedsInput.notice_type(), NoticeType::Warning);
    }
}
