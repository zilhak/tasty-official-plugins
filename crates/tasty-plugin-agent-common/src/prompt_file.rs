//! 자식 CLI에 넘길 프롬프트를 임시 파일로 쓰고 오래된 파일을 정리한다.
//! 파일명에 플러그인별 접두어와 프로세스 PID를 넣어 다른 플러그인·인스턴스와 구분한다.
//! 같은 프로세스에서 같은 surface를 다시 쓰면 같은 경로를 사용한다.

use std::path::{Path, PathBuf};

/// 프롬프트 파일의 공통 확장자. 정리할 파일을 고를 때도 사용한다.
pub const SUFFIX: &str = ".txt";

/// 이 시간보다 오래된 파일은 다음 spawn의 정리 대상이다.
/// 파일을 이미 읽었는지는 확인하지 않으므로 아직 사용할 파일이 지워지지 않는다는 보장은 아니다.
pub const TTL: std::time::Duration = std::time::Duration::from_secs(600);

/// {prefix}{pid}-{surface_id}{SUFFIX} 형태의 파일 경로.
/// 접두어 뒤에 PID를 넣어 이전 버전의 PID 없는 파일도 같은 정리 패턴에 포함한다.
pub fn path_for(dir: &Path, prefix: &str, surface_id: u32) -> PathBuf {
    dir.join(format!(
        "{prefix}{}-{surface_id}{SUFFIX}",
        std::process::id()
    ))
}

/// 이전 파일을 지우고 내용을 쓴다. Unix에서는 새 파일에 0600 모드를 지정한다.
/// mode는 기존 파일을 열 때 권한을 바꾸지 않으므로 먼저 삭제를 시도한다.
pub fn write(path: &Path, content: &str) -> std::io::Result<()> {
    // Opening and writing below remain authoritative, including when the old file cannot be removed.
    if let Err(error) = std::fs::remove_file(path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::debug!(%error, "could not remove the previous prompt file before writing");
    }
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(content.as_bytes())
    }
    #[cfg(not(unix))]
    {
        // Windows 에는 mode 개념이 없다 — 임시 디렉터리의 ACL 을 따른다.
        std::fs::write(path, content)
    }
}

/// 접두어·확장자가 맞고 mtime이 TTL보다 오래된 파일을 지운다.
/// 프로세스 생존 여부는 확인하지 않으며 삭제 실패는 기동을 막지 않는다.
pub fn sweep_stale(dir: &Path, prefix: &str) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            tracing::debug!(
                "prompt tempfile sweep: read_dir({}) failed: {e}",
                dir.display()
            );
            return;
        }
    };
    let now = std::time::SystemTime::now();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with(prefix) || !name.ends_with(SUFFIX) {
            continue;
        }
        let is_stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .and_then(|modified| {
                now.duration_since(modified)
                    .map_err(|e| std::io::Error::other(e.to_string()))
            })
            .is_ok_and(|age| age >= TTL);
        if is_stale && let Err(e) = std::fs::remove_file(entry.path()) {
            tracing::debug!(
                "prompt tempfile sweep: remove({:?}) failed: {e}",
                entry.path()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 실제 플러그인과 겹치지 않는 시험용 접두어.
    const PREFIX: &str = "tasty-test-prompt-";

    fn age_back(path: &Path) {
        let old = std::time::SystemTime::now() - (TTL + std::time::Duration::from_secs(60));
        // Windows `SetFileTime` 은 핸들에 `FILE_WRITE_ATTRIBUTES` 를 요구한다 —
        // `File::open` 의 읽기 전용 핸들로는 `PermissionDenied(os error 5)` 가 난다.
        // POSIX `futimens` 는 읽기 전용 fd 로도 되므로 Linux·macOS 에선 안 드러난다.
        std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(old)
            .unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn write_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        // 고유 임시 디렉터리로 다른 시험의 파일과 격리한다.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prompt.txt");
        write(&path, "secret").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "got mode {mode:o}");
    }

    #[test]
    fn write_narrows_permissions_on_reuse() {
        // 이전 파일의 넓은 권한도 다시 쓸 때 0600으로 좁혀져야 한다.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prompt.txt");
        std::fs::write(&path, "old").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        write(&path, "new").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "got mode {mode:o}");
        }
    }

    /// 프로세스 PID가 파일명에 포함돼야 한다.
    #[test]
    fn the_path_carries_this_process_id() {
        let name = path_for(Path::new("/tmp"), PREFIX, 7)
            .file_name()
            .expect("file name")
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            name,
            format!("{PREFIX}{}-7{SUFFIX}", std::process::id()),
            "이름 규칙이 바뀌었다"
        );
    }

    /// 이전 버전의 PID 없는 파일명도 정리할 수 있어야 한다.
    #[test]
    fn sweep_still_catches_the_name_shape_that_had_no_pid() {
        let tmp = tempfile::tempdir().unwrap();
        let legacy = tmp.path().join(format!("{PREFIX}7{SUFFIX}"));
        std::fs::write(&legacy, "x").unwrap();
        age_back(&legacy);

        sweep_stale(tmp.path(), PREFIX);

        assert!(!legacy.exists(), "pid 없던 옛 이름이 안 지워졌다");
    }

    #[test]
    fn sweep_removes_files_past_ttl() {
        let tmp = tempfile::tempdir().unwrap();
        let stale = path_for(tmp.path(), PREFIX, 7);
        std::fs::write(&stale, "x").unwrap();
        age_back(&stale);

        sweep_stale(tmp.path(), PREFIX);

        assert!(!stale.exists(), "stale prompt file should have been swept");
    }

    #[test]
    fn sweep_keeps_files_within_ttl() {
        let tmp = tempfile::tempdir().unwrap();
        let fresh = path_for(tmp.path(), PREFIX, 7);
        std::fs::write(&fresh, "x").unwrap();

        sweep_stale(tmp.path(), PREFIX);

        assert!(fresh.exists(), "fresh prompt file should not be swept");
    }

    #[test]
    fn sweep_ignores_non_matching_names() {
        let tmp = tempfile::tempdir().unwrap();
        let unrelated = tmp.path().join("some-other-file.txt");
        std::fs::write(&unrelated, "x").unwrap();
        age_back(&unrelated);

        sweep_stale(tmp.path(), PREFIX);

        assert!(unrelated.exists(), "non-matching file must not be swept");
    }

    /// 다른 플러그인의 접두어를 가진 파일은 지우지 않는다.
    #[test]
    fn sweep_does_not_touch_another_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let mine = path_for(tmp.path(), PREFIX, 7);
        let theirs = path_for(tmp.path(), "tasty-other-prompt-", 7);
        std::fs::write(&mine, "x").unwrap();
        std::fs::write(&theirs, "x").unwrap();
        age_back(&mine);
        age_back(&theirs);

        sweep_stale(tmp.path(), PREFIX);

        assert!(!mine.exists(), "자기 prefix 의 낡은 파일은 지운다");
        assert!(theirs.exists(), "다른 prefix 의 파일은 낡았어도 남긴다");
    }
}
