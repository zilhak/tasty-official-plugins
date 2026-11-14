#![forbid(unsafe_code)]

//! 호스트의 네이티브 WebView에 HTML·URL을 표시한다.
//! 생성·복원 때 URL을 전달하고 스냅샷에 남긴다. html.open은 기존 화면의 URL을 갱신한다.
//! 두 경로 모두 로컬 파일 경로를 file URI로 바꿔 전달한다.

use serde_json::{Value, json};
use tasty_plugin_sdk::{
    BusHandle, HostHandle, IpcMethodCtx, IpcMethodError, Plugin, SurfaceCreateCtx,
    SurfaceRestoreCtx, SurfaceResult,
};

const PLUGIN_ID: &str = "com.tasty.html";
const PLUGIN_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Default)]
struct HtmlPlugin {
    /// create_surface에서도 URL을 전달할 수 있도록 보관한다.
    host: Option<HostHandle>,
    mirrors: std::collections::HashSet<u32>,
    /// create_surface·restore_surface를 처리한 로컬 surface.
    local: std::collections::HashSet<u32>,
    /// 생성 처리보다 html.open이 먼저 온 surface. 호스트는 tab.create 응답 뒤에 생성 요청을
    /// 플러그인에 넘기므로 바로 뒤의 html.open이 먼저 처리될 수 있다. 이때 생성 URL을 다시
    /// 보내면 더 새로운 요청을 덮으므로 생성 쪽 webview.set_url을 생략한다.
    opened_before_create: std::collections::HashSet<u32>,
}

impl Plugin for HtmlPlugin {
    fn id(&self) -> &str {
        PLUGIN_ID
    }

    fn version(&self) -> &str {
        PLUGIN_VERSION
    }

    fn on_start(&mut self, host: HostHandle, _bus: BusHandle) {
        self.host = Some(host);
    }

    fn create_surface(&mut self, ctx: SurfaceCreateCtx) -> SurfaceResult {
        // SDK가 전달한 전체 요청에서 중첩된 생성 파라미터를 읽는다.
        if let Some(result) = self.remote_surface(ctx.surface_id, &ctx.params) {
            return result;
        }
        let url = surface_param_url(&ctx.params);
        self.open_url_surface(ctx.surface_id, url)
    }

    // 레이아웃 복원은 create_surface와 별도로 스냅샷의 URL을 다시 전달해야 한다.
    fn restore_surface(&mut self, ctx: SurfaceRestoreCtx) -> SurfaceResult {
        if let Some(result) = self.remote_surface(ctx.surface_id, &ctx.data) {
            return result;
        }
        let url = ctx
            .data
            .get("url")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        self.open_url_surface(ctx.surface_id, url)
    }

    fn destroy_surface(&mut self, surface_id: u32) {
        self.mirrors.remove(&surface_id);
        self.local.remove(&surface_id);
        self.opened_before_create.remove(&surface_id);
    }

    fn handle_ipc_method(&mut self, ctx: IpcMethodCtx) -> Result<Value, IpcMethodError> {
        match ctx.method.as_str() {
            "html.open"
                if ctx.params["surface"]
                    .as_u64()
                    .and_then(|id| u32::try_from(id).ok())
                    .is_some_and(|id| self.mirrors.contains(&id)) =>
            {
                Err(IpcMethodError::invalid_params(
                    "remote HTML is read-only; reload the mirrored entry",
                ))
            }
            "html.open" => {
                let reply = html_open(&ctx)?;
                if let Some(surface) = reply["surface_id"]
                    .as_u64()
                    .and_then(|id| u32::try_from(id).ok())
                    .filter(|id| !self.local.contains(id))
                {
                    self.opened_before_create.insert(surface);
                }
                Ok(reply)
            }
            other => Err(IpcMethodError::not_found(other)),
        }
    }
}

impl HtmlPlugin {
    fn remote_surface(&mut self, surface: u32, envelope: &Value) -> Option<SurfaceResult> {
        let params = envelope.get("params").unwrap_or(envelope);
        let remote = params
            .get("remote")
            .filter(|remote| remote["html_entry"] == true)?
            .clone();
        self.mirrors.insert(surface);
        let display_name = params
            .get("display_name")
            .and_then(Value::as_str)
            .unwrap_or("HTML")
            .to_owned();
        Some(SurfaceResult {
            display_name: Some(display_name.clone()),
            snapshot: Some(json!({"display_name":display_name,"remote":remote})),
        })
    }
    /// URL을 호스트에 전달하고 스냅샷에 담는다. 빈 문자열은 URL 없음으로 처리한다.
    fn open_url_surface(&mut self, surface_id: u32, url: Option<String>) -> SurfaceResult {
        let url = url.filter(|u| !u.is_empty());
        let superseded = self.claim_local_surface(surface_id);
        if let Some(u) = url.as_deref().filter(|_| !superseded) {
            self.send_initial_url(surface_id, u);
        }
        SurfaceResult {
            display_name: Some(url.clone().unwrap_or_else(|| "HTML".to_string())),
            snapshot: url.map(|u| json!({ "url": u })),
        }
    }

    /// 생성·복원을 처리한 surface로 기록하고, html.open이 먼저 왔으면 참을 돌려준다.
    fn claim_local_surface(&mut self, surface_id: u32) -> bool {
        self.local.insert(surface_id);
        let superseded = self.opened_before_create.remove(&surface_id);
        if superseded {
            tracing::debug!(
                "open_url_surface s{surface_id}: html.open already set a newer URL; keeping it"
            );
        }
        superseded
    }

    /// 생성·복원 URL을 webview.set_url로 보낸다. 실패는 로그만 남긴다.
    fn send_initial_url(&self, surface_id: u32, u: &str) {
        let Some(host) = &self.host else {
            tracing::warn!(
                "open_url_surface s{surface_id}: no host handle (on_start not called yet?) — cannot load {u}"
            );
            return;
        };
        let file_url = local_path_to_file_uri(u);
        if let Err(e) = host.call(
            "webview.set_url",
            json!({
                "surface_id": surface_id,
                "url": file_url,
            }),
        ) {
            tracing::warn!("open_url_surface s{surface_id}: webview.set_url failed: {e}");
        }
    }
}

/// 생성 요청의 params.url을 읽고 없으면 최상위 url을 사용한다.
fn surface_param_url(envelope: &Value) -> Option<String> {
    envelope
        .get("params")
        .and_then(|p| p.get("url"))
        .or_else(|| envelope.get("url"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// http/https/file URL은 그대로 두고, 나머지는 로컬 경로로 보고 file URI로 바꾼다.
fn local_path_to_file_uri(raw: &str) -> String {
    if raw.starts_with("http://") || raw.starts_with("https://") || raw.starts_with("file://") {
        return raw.to_string();
    }
    let normalized = raw.replace('\\', "/");
    let uri = if let Some(unc) = normalized.strip_prefix("//") {
        // UNC 경로: \\server\share\path → file://server/share/path
        format!("file://{unc}")
    } else if let Some(stripped) = normalized.strip_prefix('/') {
        // POSIX 절대경로: /home/user/a.html → file:///home/user/a.html
        format!("file:///{stripped}")
    } else {
        // Windows 드라이브 경로: C:/Users/a.html → file:///C:/Users/a.html
        format!("file:///{normalized}")
    };
    percent_encode_uri(&uri)
}

/// 영숫자와 -_.~/: 이외의 바이트를 %XX로 바꾼다.
fn percent_encode_uri(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' | b':' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// 절대 로컬 경로인지(POSIX `/…`, UNC `\\…`, Windows 드라이브 `C:\…`·`C:/…`).
fn is_absolute_local(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    raw.starts_with('/')
        || raw.starts_with("\\\\")
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'/' | b'\\'))
}

/// html.open의 url 값을 webview.set_url에 넘길 URL로 바꾼다. 상대 경로는 기준 디렉터리를 알 수 없어
/// 거절한다. CLI는 `path_kind = "url_or_file"`로 호출 위치 기준 절대 경로를 보낸다.
fn open_target(raw: &str) -> Result<String, IpcMethodError> {
    let is_url =
        raw.starts_with("http://") || raw.starts_with("https://") || raw.starts_with("file://");
    if !is_url && !is_absolute_local(raw) {
        return Err(IpcMethodError::invalid_params(&format!(
            "relative path '{raw}' has no base directory — pass an absolute path or a URL, or use `tasty html open`, which resolves it against the current directory"
        )));
    }
    Ok(local_path_to_file_uri(raw))
}

/// `html.open(url, surface)` — 로컬 경로를 file URI로 바꿔 host 의 webview.set_url 로 전달.
fn html_open(ctx: &IpcMethodCtx) -> Result<Value, IpcMethodError> {
    let raw = ctx
        .params
        .get("url")
        .and_then(|v| v.as_str())
        .ok_or_else(|| IpcMethodError::invalid_params("missing 'url'"))?;
    let url = open_target(raw)?;
    let surface_id = ctx
        .params
        .get("surface")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| {
            IpcMethodError::invalid_params("missing 'surface' — specify --surface <id>")
        })?;

    ctx.host
        .call(
            "webview.set_url",
            json!({
                "surface_id": surface_id,
                "url": url,
            }),
        )
        .map_err(|e| IpcMethodError::invalid_params(&format!("webview.set_url failed: {e}")))?;

    Ok(json!({ "ok": true, "surface_id": surface_id }))
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    tasty_plugin_sdk::run(HtmlPlugin::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_create_and_restore_keep_provenance_without_a_local_url() {
        let mut plugin = HtmlPlugin::default();
        let remote = json!({"html_entry":true,"source":{"kind":"file","path":"/server/entry.html","url":"file:///server/entry.html"}});
        let result = plugin.create_surface(SurfaceCreateCtx {
            surface_id: 7,
            kind: "html".into(),
            cwd: Some("/client/sentinel".into()),
            params: json!({"params":{"remote":remote,"display_name":"entry"}}),
        });
        let snapshot = result.snapshot.unwrap();
        assert_eq!(snapshot["remote"], remote);
        assert!(snapshot.get("url").is_none());
        assert!(plugin.mirrors.contains(&7));
        let restored = plugin.restore_surface(SurfaceRestoreCtx {
            surface_id: 8,
            kind: "html".into(),
            data: snapshot,
        });
        assert!(restored.snapshot.unwrap().get("url").is_none());
        assert!(plugin.mirrors.contains(&8));
        plugin.destroy_surface(7);
        assert!(!plugin.mirrors.contains(&7));
        assert!(plugin.mirrors.contains(&8));
    }

    #[test]
    fn surface_param_url_reads_nested_and_flat() {
        assert_eq!(
            surface_param_url(&json!({ "params": { "url": "/a/b.html" } })).as_deref(),
            Some("/a/b.html")
        );
        assert_eq!(
            surface_param_url(&json!({ "url": "/c/d.html" })).as_deref(),
            Some("/c/d.html")
        );
        assert_eq!(surface_param_url(&json!({ "params": {} })), None);
    }

    #[test]
    fn local_html_path_gets_file_scheme() {
        assert_eq!(
            local_path_to_file_uri("/home/user/project/index.html"),
            "file:///home/user/project/index.html"
        );
    }

    #[test]
    fn already_schemed_url_passthrough() {
        assert_eq!(
            local_path_to_file_uri("https://example.com"),
            "https://example.com"
        );
        assert_eq!(
            local_path_to_file_uri("file:///already/schemed.html"),
            "file:///already/schemed.html"
        );
    }

    #[test]
    fn windows_drive_path_gets_file_scheme() {
        assert_eq!(
            local_path_to_file_uri("C:\\Users\\a\\index.html"),
            "file:///C:/Users/a/index.html"
        );
    }

    #[test]
    fn unc_path_gets_file_scheme_with_host() {
        assert_eq!(
            local_path_to_file_uri("\\\\server\\share\\index.html"),
            "file://server/share/index.html"
        );
    }

    #[test]
    fn special_chars_are_percent_encoded() {
        assert_eq!(
            local_path_to_file_uri("/home/user/my file #1 100%.html"),
            "file:///home/user/my%20file%20%231%20100%25.html"
        );
    }

    #[test]
    fn open_target_turns_an_absolute_path_into_a_file_uri() {
        assert_eq!(
            open_target("/home/user/dist/index.html").unwrap(),
            "file:///home/user/dist/index.html"
        );
        assert_eq!(
            open_target("C:\\site\\index.html").unwrap(),
            "file:///C:/site/index.html"
        );
        assert_eq!(
            open_target("https://example.com/a").unwrap(),
            "https://example.com/a"
        );
    }

    #[test]
    fn open_target_rejects_a_relative_path() {
        // IPC 로 직접 온 상대 경로는 기준 디렉터리가 없다. 드라이브 경로로 잘못 바꾸지 않는다.
        assert!(open_target("./dist/index.html").is_err());
        assert!(open_target("dist/index.html").is_err());
    }

    #[test]
    fn create_surface_carries_url_in_snapshot() {
        let mut p = HtmlPlugin::default();
        let res = p.create_surface(SurfaceCreateCtx {
            surface_id: 1,
            kind: "html".into(),
            cwd: None,
            params: json!({ "surface_id": 1, "kind": "html", "params": { "url": "/a/b.html" } }),
        });
        assert_eq!(res.snapshot, Some(json!({ "url": "/a/b.html" })));
    }

    #[test]
    fn restore_surface_reopens_from_snapshot_and_re_carries_it() {
        // 복원 뒤에도 같은 URL을 스냅샷에 남겨야 한다.
        let mut p = HtmlPlugin::default();
        let res = p.restore_surface(SurfaceRestoreCtx {
            surface_id: 2,
            kind: "html".into(),
            data: json!({ "url": "/a/b.html" }),
        });
        assert_eq!(res.snapshot, Some(json!({ "url": "/a/b.html" })));
    }

    #[test]
    fn create_without_url_yields_no_snapshot() {
        let mut p = HtmlPlugin::default();
        let res = p.create_surface(SurfaceCreateCtx {
            surface_id: 3,
            kind: "html".into(),
            cwd: None,
            params: json!({ "surface_id": 3, "kind": "html", "params": {} }),
        });
        assert_eq!(res.snapshot, None);
    }
}
