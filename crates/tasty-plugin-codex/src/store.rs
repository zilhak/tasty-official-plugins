//! 플러그인 데이터 파일의 원자적 저장과 배타 생성.
//!
//! 상태 갱신은 임시 파일 작성 → `sync_all` → `rename` → 디렉터리 fsync 순서다. 중복 처리를
//! 막는 claim은 내용을 다 쓴 임시 파일을 최종 경로에 hard link로 붙인다. 링크는 대상이 있으면
//! 실패하므로 배타 생성이고, 붙는 순간 내용이 완성돼 있어 중단돼도 빈 claim이 남지 않는다.

use std::io::Write;
use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;

/// 읽을 파일의 최대 크기. 본문 상한(1MiB)에 JSON 이스케이프 여유를 더했다.
const MAX_READ: u64 = 8 * 1024 * 1024;

/// 다른 사용자가 읽지 못하는 디렉터리를 만든다. 이미 있으면 권한만 다시 맞춘다.
pub(crate) fn private_dir(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// 없던 디렉터리만 만든다. 이미 있으면 다른 실행의 자원일 수 있어 실패로 돌려준다.
pub(crate) fn private_dir_new(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().mode(0o700).create(path)
    }
    #[cfg(not(unix))]
    {
        std::fs::DirBuilder::new().create(path)
    }
}

/// 디렉터리 항목 변경(rename·create)을 디스크에 반영한다. Windows는 디렉터리 핸들을
/// 열어 동기화하는 표준 API가 없어 생략한다.
fn sync_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::fs::File::open(dir)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _unused = dir;
        Ok(())
    }
}

/// 값을 JSON으로 원자적으로 쓴다.
pub(crate) fn write_json<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(std::io::Error::other)?;
    let (dir, name) = split(path)?;
    private_dir(dir)?;
    let tmp = synced_temp(dir, &name, &bytes)?;
    let renamed = std::fs::rename(&tmp, path);
    if renamed.is_err() {
        remove_temp(&tmp);
    }
    renamed?;
    sync_dir(dir)
}

/// JSON 파일을 읽는다. 파일이 없으면 None이다.
pub(crate) fn read_json<T: DeserializeOwned>(path: &Path) -> std::io::Result<Option<T>> {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    if meta.len() > MAX_READ {
        return Err(std::io::Error::other("record too large"));
    }
    let bytes = std::fs::read(path)?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(std::io::Error::other)
}

/// 같은 디렉터리에 내용을 다 쓰고 동기화한 임시 파일을 만든다.
fn synced_temp(dir: &Path, name: &str, content: &[u8]) -> std::io::Result<std::path::PathBuf> {
    let suffix = crate::ids::random_hex(8)?;
    let tmp = dir.join(format!(".{name}.{suffix}.tmp"));
    let written = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        file.write_all(content)?;
        file.sync_all()
    })();
    match written {
        Ok(()) => Ok(tmp),
        Err(e) => {
            remove_temp(&tmp);
            Err(e)
        }
    }
}

fn remove_temp(tmp: &Path) {
    if let Err(e) = remove_file(tmp) {
        tracing::warn!("codex store: temporary file cleanup failed: {e}");
    }
}

fn split(path: &Path) -> std::io::Result<(&Path, String)> {
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("path without a parent directory"))?;
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("path without a file name"))?
        .to_string_lossy()
        .into_owned();
    Ok((dir, name))
}

/// 최종 경로를 배타적으로 만든다. 이미 있으면 Ok(false)다. 내용을 다 쓴 임시 파일을 링크로
/// 붙이므로 중단돼도 최종 경로에는 완성된 내용만 보인다.
pub(crate) fn create_claim(path: &Path, content: &str) -> std::io::Result<bool> {
    let (dir, name) = split(path)?;
    private_dir(dir)?;
    let tmp = synced_temp(dir, &name, content.as_bytes())?;
    let linked = std::fs::hard_link(&tmp, path);
    remove_temp(&tmp);
    match linked {
        Ok(()) => {
            sync_dir(dir)?;
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(e),
    }
}

/// 내용을 원자적으로 바꾼다. 손상된 claim을 고칠 때 쓴다.
pub(crate) fn replace_text(path: &Path, content: &str) -> std::io::Result<()> {
    let (dir, name) = split(path)?;
    private_dir(dir)?;
    let tmp = synced_temp(dir, &name, content.as_bytes())?;
    let renamed = std::fs::rename(&tmp, path);
    if renamed.is_err() {
        remove_temp(&tmp);
    }
    renamed?;
    sync_dir(dir)
}

/// 작은 텍스트 파일을 읽는다. 없으면 None이다.
pub(crate) fn read_text(path: &Path) -> std::io::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// 파일을 지운다. 없으면 성공으로 본다.
pub(crate) fn remove_file(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// 현재 시각(ms).
pub(crate) fn now_ms() -> u64 {
    #[cfg(test)]
    {
        if let Some(now) = test_clock::frozen() {
            return now;
        }
    }
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 시험 스레드의 시각을 시험이 정한다. 재시도 시각 판정이 fsync·스케줄링으로 늘어나는 실제 경과 시간에
/// 좌우되지 않게 한다. 다른 스레드의 시각은 바꾸지 않으므로 판정이 시험 스레드에서 도는 시험에만 쓴다.
#[cfg(test)]
pub(crate) mod test_clock {
    use std::cell::Cell;
    use std::marker::PhantomData;
    use std::time::Duration;

    thread_local! {
        static FROZEN: Cell<Option<u64>> = const { Cell::new(None) };
    }

    pub(super) fn frozen() -> Option<u64> {
        FROZEN.get()
    }

    /// 살아 있는 동안 이 스레드의 시각을 멈춘다. `--test-threads=1` 은 시험들이 한 스레드를 함께 쓰므로
    /// 패닉 경로까지 Drop 에서 되돌린다.
    pub(crate) struct Frozen(PhantomData<*const ()>);

    pub(crate) fn freeze() -> Frozen {
        assert!(FROZEN.get().is_none(), "the test clock is already frozen");
        FROZEN.set(Some(super::now_ms()));
        Frozen(PhantomData)
    }

    impl Frozen {
        pub(crate) fn advance(&self, by: Duration) {
            FROZEN.set(FROZEN.get().map(|now| now + by.as_millis() as u64));
        }
    }

    impl Drop for Frozen {
        fn drop(&mut self) {
            FROZEN.set(None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_claim_is_created_once_and_keeps_the_first_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("claims").join("k");
        assert!(create_claim(&path, "first").unwrap());
        assert!(!create_claim(&path, "second").unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first");
    }

    /// 붙인 claim은 처음부터 완성된 내용이고 임시 파일을 남기지 않는다.
    #[test]
    fn a_claim_never_appears_without_its_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("claims").join("k");
        assert!(create_claim(&path, "0123").unwrap());
        assert_eq!(read_text(&path).unwrap().as_deref(), Some("0123"));
        let names: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 1, "{names:?}");
        replace_text(&path, "4567").unwrap();
        assert_eq!(read_text(&path).unwrap().as_deref(), Some("4567"));
        assert_eq!(read_text(&dir.path().join("none")).unwrap(), None);
    }

    #[test]
    fn json_writes_replace_atomically_and_leave_no_temporary_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a").join("v.json");
        write_json(&path, &serde_json::json!({"n": 1})).unwrap();
        write_json(&path, &serde_json::json!({"n": 2})).unwrap();
        let v: serde_json::Value = read_json(&path).unwrap().unwrap();
        assert_eq!(v["n"], 2);
        let names: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 1, "{names:?}");
        assert!(
            read_json::<serde_json::Value>(&dir.path().join("none.json"))
                .unwrap()
                .is_none()
        );
    }

    #[cfg(unix)]
    #[test]
    fn private_directories_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("p");
        private_dir_new(&p).unwrap();
        assert_eq!(
            std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert!(
            private_dir_new(&p).is_err(),
            "이미 있는 디렉터리는 실패한다"
        );
    }
}
