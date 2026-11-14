//! JSONL 파일을 이어 읽는다. 외부 프로세스의 삭제·절단·교체도 처리한다.
//!
//!
//! | 상태 | 판정 | 대응 |
//! |------|------|------|
//! | 아직 생성 안 됨(세션 시작 직후 race) | `metadata` 가 `NotFound` | [`TailPoll::Missing`], offset 초기화 후 다음 tick 재시도 |
//! | 읽는 중 삭제 | 위와 동일 | 위와 동일 — 재생성되면 처음부터 다시 읽는다 |
//! | 중간 truncate | `len < offset` | 0 부터 재동기화 |
//! | rotate / 파일 교체 | inode(Unix) · file index(Windows) 변화 | 0 부터 재동기화 |
//! | 개행 없이 끝난 부분 라인 | 버퍼 잔여 | 다음 read 로 완성될 때까지 보류 — **완성 시 정확히 1 회** 방출 |
//!
//! 재동기화하면 같은 기록을 다시 읽을 수 있다. 상위 계층은 기억하고 있는 uuid만 중복 제거한다.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// 한 tick 에 읽는 최대 바이트. 큰 백로그를 한 번에 다 삼켜 메모리·지연이 튀지 않게
/// 나눠 읽는다. 남은 분량은 다음 tick 이 이어 읽는다.
const MAX_READ_PER_TICK: usize = 1 << 22; // 4 MiB

/// 한 파일에 대한 tail 진행 상태.
#[derive(Debug, Default)]
pub struct TailState {
    /// 다음에 읽기 시작할 바이트 위치.
    offset: u64,
    /// 개행으로 끝나지 않은 잔여 바이트. UTF-8 문자가 read 경계에서 쪼개질 수 있어
    /// `String` 이 아니라 바이트로 들고 있다가 라인이 완성될 때 디코드한다.
    partial: Vec<u8>,
    /// 파일 동일성 지문. 값이 바뀌면 같은 경로의 **다른 파일**이다.
    identity: Option<u64>,
}

/// 한 tick 의 tail 결과.
#[derive(Debug, PartialEq, Eq)]
pub enum TailPoll {
    /// 파일이 아직(또는 더 이상) 없다.
    Missing,
    /// 완성된 라인들 + 이번 tick 에 재동기화가 일어났는지.
    Lines { lines: Vec<String>, resynced: bool },
}

impl TailState {
    /// 파일 끝에서 시작하는 상태 — 등록 시점의 백로그를 건너뛴다.
    pub fn at_end(path: &Path) -> Self {
        let meta = std::fs::metadata(path).ok();
        Self {
            offset: meta.as_ref().map(std::fs::Metadata::len).unwrap_or(0),
            partial: Vec::new(),
            identity: meta.as_ref().and_then(|m| file_identity(path, m)),
        }
    }

    /// 저장된 offset에서 재개한다. 이전 파일 지문은 저장하지 않아 현재 경로에서 새로 읽는다.
    /// 재시작 중 더 긴 다른 파일로 바뀌면 길이와 지문만으로는 교체를 감지하지 못해
    /// 앞부분을 놓칠 수 있다. 세션 id가 바뀐 경우는 pump의 호스트 조회로 따로 확인한다.
    pub fn resume_at(path: &Path, offset: u64) -> Self {
        let meta = std::fs::metadata(path).ok();
        Self {
            offset,
            partial: Vec::new(),
            identity: meta.as_ref().and_then(|m| file_identity(path, m)),
        }
    }

    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// 다음 read 를 파일 처음부터 다시 하도록 되돌린다. 부분 라인 잔여도 버린다
    /// (그 바이트는 이제 다른 파일 내용이거나 잘려나간 내용이다).
    fn resync(&mut self) {
        self.offset = 0;
        self.partial.clear();
    }

    /// 파일을 현재 offset 부터 읽어 완성된 라인들을 돌려준다.
    ///
    /// I/O 오류(권한 등)는 `Err`. 파일 부재는 오류가 아니라 [`TailPoll::Missing`] 이다
    /// — transcript 는 세션이 시작돼야 생기고, 삭제·재생성도 정상 흐름이기 때문.
    pub fn poll(&mut self, path: &Path) -> std::io::Result<TailPoll> {
        let meta = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // 재생성되면 처음부터 읽도록 상태를 비운다.
                self.resync();
                self.identity = None;
                return Ok(TailPoll::Missing);
            }
            Err(e) => return Err(e),
        };
        let resynced = self.reconcile(path, &meta);
        let mut file = File::open(path)?;
        file.seek(SeekFrom::Start(self.offset))?;
        let want = (meta.len().saturating_sub(self.offset)).min(MAX_READ_PER_TICK as u64) as usize;
        let mut buf = vec![0u8; want];
        let read = read_fully(&mut file, &mut buf)?;
        buf.truncate(read);
        self.offset += read as u64;
        Ok(TailPoll::Lines {
            lines: self.split_lines(&buf),
            resynced,
        })
    }

    /// 파일 교체/절단을 판정해 필요하면 재동기화한다. 재동기화했으면 `true`.
    fn reconcile(&mut self, path: &Path, meta: &std::fs::Metadata) -> bool {
        let identity = file_identity(path, meta);
        let replaced = match (self.identity, identity) {
            (Some(prev), Some(now)) => prev != now,
            // 지문을 얻을 수 없는 플랫폼/파일시스템에서는 길이 비교에만 의존한다.
            _ => false,
        };
        self.identity = identity.or(self.identity);
        if replaced || meta.len() < self.offset {
            self.resync();
            self.identity = identity;
            return true;
        }
        false
    }

    /// 잔여 바이트에 이번 청크를 이어 붙여 완성된 라인만 잘라낸다. 개행 없이 끝난
    /// 꼬리는 다시 잔여로 남는다 — 다음 청크가 개행을 가져올 때 정확히 1 회 방출된다.
    fn split_lines(&mut self, chunk: &[u8]) -> Vec<String> {
        self.partial.extend_from_slice(chunk);
        let mut lines = Vec::new();
        let mut start = 0usize;
        while let Some(rel) = self.partial[start..].iter().position(|b| *b == b'\n') {
            let end = start + rel;
            let line = String::from_utf8_lossy(&self.partial[start..end])
                .trim_end_matches('\r')
                .to_string();
            if !line.trim().is_empty() {
                lines.push(line);
            }
            start = end + 1;
        }
        self.partial.drain(..start);
        lines
    }
}

/// 버퍼가 찰 때까지(또는 EOF 까지) 읽는다. `Read::read` 는 짧게 돌려줄 수 있어
/// 한 번의 호출로 요청량을 채웠다고 가정하면 offset 이 어긋난다.
fn read_fully(file: &mut File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut total = 0usize;
    while total < buf.len() {
        match file.read(&mut buf[total..]) {
            Ok(0) => break,
            Ok(n) => total += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(total)
}

/// 파일 식별자. Unix는 inode, Windows는 file index를 사용한다.
/// 식별자를 얻지 못하면 길이로만 교체·절단을 판단한다.
#[cfg(unix)]
fn file_identity(_path: &Path, meta: &std::fs::Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    Some(meta.ino())
}

/// Windows 의 file index 는 `Metadata` 가 아니라 **열린 핸들**에서만 나온다.
/// std 의 `MetadataExt::file_index()` 는 unstable feature `windows_by_handle` 이라 stable
/// 툴체인에서 쓸 수 없으므로 Win32 `GetFileInformationByHandle` 을 직접 호출한다.
/// 실패(핸들 열기 실패·API 실패)는 `None` — 길이 비교 판정으로 degrade 한다.
#[cfg(windows)]
#[allow(unsafe_code)] // 이 함수만 Win32 FFI 를 쓴다 — 나머지 crate 는 unsafe 금지 유지.
fn file_identity(path: &Path, _meta: &std::fs::Metadata) -> Option<u64> {
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };

    let file = File::open(path).ok()?;
    // SAFETY: `BY_HANDLE_FILE_INFORMATION` 은 정수/`FILETIME` 만으로 이루어진 POD 라
    // 전 비트 0 이 유효한 값이다. 아래 호출이 성공한 경우에만 내용을 읽는다.
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: `file` 이 살아 있는 동안 그 raw handle 을 넘기고, 출력 버퍼는 위에서
    // 초기화한 유효한 `BY_HANDLE_FILE_INFORMATION` 하나다 — Win32 계약을 만족한다.
    let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) };
    if ok == 0 {
        return None;
    }
    Some((u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow))
}

#[cfg(not(any(unix, windows)))]
fn file_identity(_path: &Path, _meta: &std::fs::Metadata) -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn append(path: &Path, text: &str) {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("open for append");
        f.write_all(text.as_bytes()).expect("append");
    }

    fn lines_of(poll: TailPoll) -> Vec<String> {
        match poll {
            TailPoll::Lines { lines, .. } => lines,
            TailPoll::Missing => panic!("expected lines, got Missing"),
        }
    }

    #[test]
    fn missing_file_is_not_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("absent.jsonl");
        let mut state = TailState::at_end(&path);
        assert_eq!(state.poll(&path).expect("poll"), TailPoll::Missing);
    }

    #[test]
    fn each_append_yields_exactly_one_line_once() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("t.jsonl");
        append(&path, "{\"a\":1}\n");
        // 등록 시점에 이미 있던 백로그는 건너뛴다.
        let mut state = TailState::at_end(&path);
        assert!(lines_of(state.poll(&path).expect("poll")).is_empty());

        for i in 0..3 {
            append(&path, &format!("{{\"n\":{i}}}\n"));
            let lines = lines_of(state.poll(&path).expect("poll"));
            assert_eq!(lines, [format!("{{\"n\":{i}}}")]);
            // 새로 쓴 것이 없으면 다시 방출하지 않는다.
            assert!(lines_of(state.poll(&path).expect("poll")).is_empty());
        }
    }

    #[test]
    fn partial_line_is_held_until_the_newline_arrives_then_emitted_once() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("t.jsonl");
        std::fs::write(&path, b"").expect("create");
        let mut state = TailState::at_end(&path);

        append(&path, "{\"partial\":");
        assert!(
            lines_of(state.poll(&path).expect("poll")).is_empty(),
            "an unterminated chunk must not be emitted"
        );
        append(&path, "true}\n");
        assert_eq!(
            lines_of(state.poll(&path).expect("poll")),
            ["{\"partial\":true}"]
        );
        assert!(
            lines_of(state.poll(&path).expect("poll")).is_empty(),
            "the completed line must not be emitted a second time"
        );
    }

    #[test]
    fn multibyte_char_split_across_reads_is_reassembled() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("t.jsonl");
        std::fs::write(&path, b"").expect("create");
        let mut state = TailState::at_end(&path);

        // "한" = E1..., 3 바이트. 앞 2 바이트만 먼저 도착시킨다.
        let bytes = "{\"t\":\"한\"}\n".as_bytes().to_vec();
        let split = bytes
            .iter()
            .position(|b| *b == 0xed)
            .expect("multibyte lead byte")
            + 2;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open");
        f.write_all(&bytes[..split]).expect("write head");
        assert!(lines_of(state.poll(&path).expect("poll")).is_empty());
        f.write_all(&bytes[split..]).expect("write tail");
        assert_eq!(
            lines_of(state.poll(&path).expect("poll")),
            ["{\"t\":\"한\"}"]
        );
    }

    #[test]
    fn truncation_resyncs_from_zero() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("t.jsonl");
        append(&path, "{\"a\":1}\n{\"b\":2}\n");
        let mut state = TailState::at_end(&path);
        assert!(lines_of(state.poll(&path).expect("poll")).is_empty());

        std::fs::write(&path, b"{\"c\":3}\n").expect("truncate-rewrite");
        let poll = state.poll(&path).expect("poll");
        match poll {
            TailPoll::Lines { lines, resynced } => {
                assert!(resynced, "shrinking file must be reported as a resync");
                assert_eq!(lines, ["{\"c\":3}"]);
            }
            TailPoll::Missing => panic!("file exists"),
        }
    }

    #[test]
    fn file_replacement_resyncs_even_when_the_new_file_is_longer() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("t.jsonl");
        append(&path, "{\"a\":1}\n");
        let mut state = TailState::at_end(&path);
        assert!(lines_of(state.poll(&path).expect("poll")).is_empty());

        // 같은 경로에 새 파일을 rename 으로 갈아끼운다 — 길이는 더 길다.
        let replacement = dir.path().join("new.jsonl");
        std::fs::write(&replacement, b"{\"x\":1}\n{\"y\":2}\n").expect("write replacement");
        std::fs::rename(&replacement, &path).expect("rename over");

        let poll = state.poll(&path).expect("poll");
        match poll {
            TailPoll::Lines { lines, resynced } => {
                // 지문을 얻을 수 있는 플랫폼에서는 길이가 늘었어도 교체를 잡아낸다.
                assert!(resynced, "replaced file must be detected");
                assert_eq!(lines, ["{\"x\":1}", "{\"y\":2}"]);
            }
            TailPoll::Missing => panic!("file exists"),
        }
    }

    #[test]
    fn deleted_then_recreated_file_is_read_from_the_start() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("t.jsonl");
        append(&path, "{\"a\":1}\n");
        let mut state = TailState::at_end(&path);
        assert!(lines_of(state.poll(&path).expect("poll")).is_empty());

        std::fs::remove_file(&path).expect("delete");
        assert_eq!(state.poll(&path).expect("poll"), TailPoll::Missing);

        append(&path, "{\"fresh\":1}\n");
        assert_eq!(
            lines_of(state.poll(&path).expect("poll")),
            ["{\"fresh\":1}"]
        );
    }

    #[test]
    fn resume_at_replays_from_the_persisted_offset() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("t.jsonl");
        append(&path, "{\"a\":1}\n{\"b\":2}\n");
        let mut state = TailState::resume_at(&path, 8);
        assert_eq!(lines_of(state.poll(&path).expect("poll")), ["{\"b\":2}"]);
        assert_eq!(state.offset(), 16);
    }

    #[test]
    fn blank_lines_are_dropped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("t.jsonl");
        std::fs::write(&path, b"").expect("create");
        let mut state = TailState::at_end(&path);
        append(&path, "\n\n{\"a\":1}\n\n");
        assert_eq!(lines_of(state.poll(&path).expect("poll")), ["{\"a\":1}"]);
    }
}
