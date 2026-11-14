//! 주소창 폴더 버튼: 문서 폴더에서 host 파일 피커를 열고, 고른 경로로 같은 surface 를 바꾼다.
//! 피커는 경로만 돌려주며 파일을 열거나 클립보드에 쓰지 않는다. 이동은 이 플러그인이
//! `markdown.navigate` 로 하고, 피커 요청 번호를 실어 host 가 사용자 요청으로 판정하게 한다
//! (docs/surfaces/markdown/index.md 의 주소창 절).

use serde_json::{Value, json};
use tasty_plugin_sdk::PluginError;

use crate::popup::FilePickerResultWire;
use crate::{MarkdownPlugin, MdDoc};

/// 파일 열기 팝업의 찾아보기와 같은 필터다.
pub(crate) const FILTERS: [&str; 2] = ["md", "markdown"];

/// host 피커가 이미 열려 있을 때 `file_picker.trigger` 가 돌려주는 오류 코드.
const PICKER_BUSY: i32 = -32000;

/// 피커 요청을 낸 쪽. 결과는 낸 쪽이 아직 있고 그 쪽의 마지막 요청일 때만 쓴다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PickerOwner {
    /// 파일 열기 팝업의 찾아보기(팝업 instance_id). 결과는 경로 입력칸을 채운다.
    Popup(u64),
    /// 주소창 폴더 버튼(surface_id). 결과로 같은 surface 를 바꾼다.
    Surface(u32),
}

/// 피커가 시작할 문서 폴더. 로컬은 파일의 부모 폴더, mirror 는 원격 경로의 부모다.
/// 원격 경로는 원격 OS 의 모양이라 로컬 `Path` 로 나누지 않는다.
pub(crate) fn start_dir(doc: &MdDoc) -> Option<String> {
    let path = doc.file_path.as_deref()?;
    if doc.remote.is_some() {
        return remote_parent(path);
    }
    std::path::Path::new(path)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_string_lossy().into_owned())
}

/// 원격 경로의 부모 폴더. 드라이브 문자나 `\\` 로 시작하면 `\`, 그 밖은 `/` 로 나눈다.
/// 부모가 루트면 루트(`/`, `C:\`)를 돌려주고, 구분자가 없으면 None 이다.
pub(crate) fn remote_parent(path: &str) -> Option<String> {
    let bytes = path.as_bytes();
    let windows = path.starts_with("\\\\")
        || (bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic());
    let sep = if windows { '\\' } else { '/' };
    let trimmed = path.trim_end_matches(sep);
    let i = trimmed.rfind(sep)?;
    let root_len = if windows && bytes.get(1) == Some(&b':') {
        3
    } else {
        1
    };
    if i < root_len {
        return path.get(..root_len).map(str::to_string);
    }
    Some(trimmed[..i].to_string())
}

/// `file_picker.trigger` 파라미터. 팝업 밖에서 열므로 `owner_popup_instance` 는 없다.
/// 버튼 클릭이 낸 navigation URL 을 돌려주면 host 가 사용자 조작을 확인하고 피커를 사용자 요청으로
/// 연다. 확인하지 못하면 포커스 없는 피커가 되고 키를 받는 이 페이지가 피커를 덮는다.
pub(crate) fn trigger_params(sid: u32, start_dir: Option<String>, nav_url: &str) -> Value {
    json!({
        "filters": FILTERS,
        "start_dir": start_dir,
        "origin_surface_id": sid,
        "user_navigation_url": nav_url,
    })
}

/// 피커 요청의 응답에서 request_id 를 꺼낸다. 피커가 이미 열려 있으면 두 번째 피커를 열지 않고
/// 지금 열린 피커를 쓰게 둔다. 다른 실패도 기록만 남긴다.
pub(crate) fn request_id_of(sid: u32, resp: Result<Value, PluginError>) -> Option<u64> {
    match resp {
        Ok(v) => v.get("request_id").and_then(Value::as_u64),
        Err(PluginError::HostCall {
            code: Some(PICKER_BUSY),
            ..
        }) => {
            tracing::warn!(
                "markdown surface {sid}: a file picker is already open — the folder button waits for it"
            );
            None
        }
        Err(e) => {
            tracing::warn!("markdown surface {sid}: folder button picker failed: {e}");
            None
        }
    }
}

/// 고른 경로로 같은 surface 를 바꾸는 `markdown.navigate` 파라미터. 경로는 피커가 준 그대로다.
pub(crate) fn navigate_params(sid: u32, path: &str, request_id: u64) -> Value {
    json!({
        "surface_id": sid,
        "path": path,
        "picker_request_id": request_id,
    })
}

impl MarkdownPlugin {
    /// 고른 경로로 같은 surface 를 바꾼다. mirror 문서면 `run.result` 를 기다려 성공 시 원격이 바꾼
    /// 문서의 원문과 경로를 다시 받는다. 실패는 host 가 사용자 toast 로 알린다.
    pub(crate) fn navigate_picked(&mut self, sid: u32, path: &str, request_id: u64) {
        let Some(host) = self.host.clone() else {
            return;
        };
        let resp = host.call("markdown.navigate", navigate_params(sid, path, request_id));
        let run = match resp {
            Ok(v) => v.get("run").and_then(Value::as_u64),
            Err(e) => {
                tracing::warn!("markdown surface {sid}: picked file navigate failed: {e}");
                return;
            }
        };
        let Some(remote) = self.docs.get_mut(&sid).and_then(|d| d.remote.as_mut()) else {
            return;
        };
        let (ticket, replaced_waiting) = remote.replace_with_picked();
        if let Some(run) = run {
            crate::addr_nav::spawn_addr_watch(host, sid, ticket, path.to_string(), run, true);
        }
        // 피커 경유 실패는 toast 로만 알리고 다시 그리지 않으므로, 대신한 직접 이동의 대기 행은 여기서 끝낸다.
        if replaced_waiting {
            self.reload_webview(sid);
        }
    }

    /// 주소창 폴더 버튼의 피커 요청을 기록한다. 같은 surface 의 이전 요청 결과는 버린다.
    pub(crate) fn track_surface_picker(&mut self, sid: u32, request_id: u64) {
        self.pending_file_picker
            .retain(|_, owner| *owner != PickerOwner::Surface(sid));
        self.pending_file_picker
            .insert(request_id, PickerOwner::Surface(sid));
    }

    /// 피커 결과를 낸 쪽에 맞춘다. 팝업 요청이면 경로 입력칸을 채우고 None 이다. 주소창 요청이면
    /// 그 surface 가 아직 있을 때 이동할 (surface, 경로, request_id) 를 돌려준다. 취소·모르는 요청·
    /// 버린 요청은 아무것도 하지 않는다.
    pub(crate) fn take_picker_result(
        &mut self,
        reply: FilePickerResultWire,
    ) -> Option<(u32, String, u64)> {
        let owner = self.pending_file_picker.remove(&reply.request_id)?;
        if reply.cancelled {
            return None;
        }
        let path = reply.paths.into_iter().next()?;
        match owner {
            PickerOwner::Popup(iid) => {
                if let Some(st) = self.file_open.get_mut(&iid) {
                    st.path_input = path;
                }
                None
            }
            PickerOwner::Surface(sid) => {
                self.docs
                    .contains_key(&sid)
                    .then_some((sid, path, reply.request_id))
            }
        }
    }
}
