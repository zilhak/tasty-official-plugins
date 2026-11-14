//! 부모 완료 로그에 남기는 Claude child 알림의 종류와 타입. 줄마다 타입 표지를 붙여 reader 가 언어와 무관하게 거른다.
//! 새 종류를 더하면 [`NoticeKind::notice_type`] 의 `match` 가 컴파일을 막아 타입을 정하게 한다.

use tasty_utils::notify::NoticeType;

/// 완료 로그에 쓰는 Claude 알림 종류.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NoticeKind {
    /// idle·needs_input 상태 변경.
    StateChanged,
    /// child 프로세스가 끝났다. 부모가 kill 로 끝낸 경우도 구분하지 않는다.
    Exited,
    /// API 오류로 끝난 턴의 완료 줄(오류 힌트가 붙음).
    StopFailure,
    /// API 오류로 턴이 끝났지만 백그라운드 작업이 남아 계속 기다린다.
    StopFailureBackground,
    /// 원격 attach child 의 턴 결과를 저장했다.
    RemoteResult,
    /// 오류 뒤 출력이 멈췄다.
    Stalled,
    /// 턴 진행 중 오류 없이 출력이 오래 없다.
    StalledNoError,
    /// 백그라운드 작업을 기다리며 화면 출력이 오래 없다.
    StalledBackgroundWait,
    /// 기다리는 백그라운드 작업의 출력 파일이 오래 조용하다.
    StalledBackgroundTaskQuiet,
    /// 무출력 선언이 끝나 그동안 쌓인 무출력 알림을 묶어 보낸다.
    QuietReleased,
}

impl NoticeKind {
    /// 상태 변경·결과 저장은 info, 출력이 없다는 알림은 warning, 오류가 섞이면 무조건 error 다.
    pub(crate) fn notice_type(self) -> NoticeType {
        match self {
            Self::StateChanged | Self::RemoteResult => NoticeType::Info,
            Self::StalledNoError
            | Self::StalledBackgroundWait
            | Self::StalledBackgroundTaskQuiet
            | Self::QuietReleased => NoticeType::Warning,
            Self::Exited | Self::StopFailure | Self::StopFailureBackground | Self::Stalled => {
                NoticeType::Error
            }
        }
    }

    /// 이 종류의 줄을 시작하는 문구 키. 문구 키와 종류의 짝 시험이 쓴다.
    #[cfg(test)]
    fn message_key(self) -> &'static str {
        match self {
            Self::StateChanged | Self::Exited | Self::StopFailure => "claude.notify.done_message",
            Self::StopFailureBackground => "claude.notify.stop_failure_background_message",
            Self::RemoteResult => "claude.notify.remote_result_message",
            Self::Stalled => "claude.notify.stalled_message",
            Self::StalledNoError => "claude.notify.stalled_no_error_message",
            Self::StalledBackgroundWait => "claude.notify.stalled_background_wait_message",
            Self::StalledBackgroundTaskQuiet => {
                "claude.notify.stalled_background_task_quiet_message"
            }
            Self::QuietReleased => "claude.notify.quiet_released_message",
        }
    }

    #[cfg(test)]
    pub(crate) const ALL: [Self; 10] = [
        Self::StateChanged,
        Self::Exited,
        Self::StopFailure,
        Self::StopFailureBackground,
        Self::RemoteResult,
        Self::Stalled,
        Self::StalledNoError,
        Self::StalledBackgroundWait,
        Self::StalledBackgroundTaskQuiet,
        Self::QuietReleased,
    ];
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

    /// `[claude.notify]` 의 `_message` 키는 모두 어떤 종류의 문구다. 종류 없이 문구만 더하면 이 시험이 실패한다.
    #[test]
    fn every_notify_message_key_belongs_to_a_notice_kind() {
        let lang = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("lang/en.toml");
        let text = std::fs::read_to_string(lang).expect("en.toml");
        // `[claude.notify]` 표의 키 이름. 다음 표 머리까지 `name = ...` 줄을 읽는다.
        let names: Vec<&str> = text
            .lines()
            .skip_while(|l| l.trim() != "[claude.notify]")
            .skip(1)
            .take_while(|l| !l.trim_start().starts_with('['))
            .filter_map(|l| l.split_once('=').map(|(k, _)| k.trim()))
            .filter(|k| !k.is_empty() && !k.starts_with('#'))
            .collect();
        let keys: Vec<&str> = NoticeKind::ALL.iter().map(|k| k.message_key()).collect();
        let mut messages = 0;
        for name in names.iter().filter(|k| k.ends_with("_message")) {
            messages += 1;
            let key = format!("claude.notify.{name}");
            assert!(keys.contains(&key.as_str()), "{key} has no NoticeKind");
        }
        assert!(messages >= 7, "{messages} message keys read");
        for key in keys {
            let name = key.trim_start_matches("claude.notify.");
            assert!(names.contains(&name), "{key} is not in en.toml");
        }
    }

    /// 대응표: 무출력 3종과 그 묶음만 warning, 오류가 섞인 넷은 error, 나머지 둘은 info 다.
    #[test]
    fn each_notice_kind_has_the_type_of_the_table() {
        use NoticeKind::*;
        let of = |t| {
            NoticeKind::ALL
                .iter()
                .filter(|k| k.notice_type() == t)
                .copied()
                .collect::<Vec<_>>()
        };
        assert_eq!(of(NoticeType::Info), vec![StateChanged, RemoteResult]);
        assert_eq!(
            of(NoticeType::Warning),
            vec![
                StalledNoError,
                StalledBackgroundWait,
                StalledBackgroundTaskQuiet,
                QuietReleased
            ]
        );
        assert_eq!(
            of(NoticeType::Error),
            vec![Exited, StopFailure, StopFailureBackground, Stalled]
        );
    }
}
