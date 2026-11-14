//! 대용량 확인과 파일 열기 팝업의 상태 및 egui-mesh 렌더링.
//! 문서 본문은 render.rs에서 HTML로 만든다.

use serde_json::{Value, json};
use tasty_plugin_sdk::{HostHandle, PopupSetContextCtx, Translator};
use tasty_type_appearance::theme::Theme;

#[cfg(any(unix, windows))]
use tasty_plugin_sdk::EguiMeshPopup;

use crate::{MarkdownPlugin, baked_icons, navigate, theme_from_wire};

/// 대용량 확인 팝업 인스턴스의 대상 정보(open_popup context 로 받아 보관). [열기] 시
/// 이 surface 의 문서 read 를 재개한다.
#[derive(Clone)]
pub(crate) struct LargeFileConfirm {
    pub(crate) surface_id: u32,
    /// 표시용 파일명(basename). 경로 전체 대신 파일명만 팝업에 노출.
    pub(crate) file_name: String,
    /// 크기 칩 라벨 (예: "3.2 MB").
    pub(crate) size_label: String,
}

/// 파일 열기 팝업의 입력 경로와 변환 대상. 대상 surface가 있으면
/// markdown.navigate로 바꾸고, 없으면 file_handler.dispatch로 새 탭을 연다.
#[derive(Default)]
pub(crate) struct FileOpenState {
    pub(crate) path_input: String,
    pub(crate) convert_surface_id: Option<u32>,
    /// **찾아보기…** 가 여는 host 파일 피커의 출발점 — 이 팝업을 띄운 surface 의 폴더.
    pub(crate) picker_start: PickerStart,
}

/// host 파일 피커를 어디서 출발시킬지. popup context 에서 한 번 읽어 둔다.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct PickerStart {
    /// 시작 폴더. 미러는 remote_cwd, 로컬은 observed_cwd를 사용한다.
    /// 새 surface의 작업 폴더 상속 여부(inherit_cwd)와는 관계없다.
    pub(crate) dir: Option<String>,
    /// 팝업을 띄운 로컬 surface — host 가 로컬/원격 판정을 이 surface 의 workspace 로 한다.
    pub(crate) origin_surface_id: Option<u32>,
    /// dir가 원격 호스트의 경로인지. 확정 경로를 조합할 규칙을 고른다.
    pub(crate) remote: bool,
}

impl PickerStart {
    /// 팝업 context에서 읽는다. 키가 없거나 null이면 None으로 둔다.
    pub(crate) fn from_context(context: &Value) -> Self {
        let is_mirror = context
            .get("mirror")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let key = if is_mirror {
            "remote_cwd"
        } else {
            "observed_cwd"
        };
        Self {
            dir: context.get(key).and_then(Value::as_str).map(str::to_string),
            origin_surface_id: context
                .get("origin_surface_id")
                .and_then(Value::as_u64)
                .map(|v| v as u32),
            remote: is_mirror,
        }
    }
}

/// 확정한 상대경로를 팝업을 띄운 surface의 셸 cwd 기준 절대경로로 만든다.
/// 절대경로, `~`로 시작하는 경로, cwd를 모르는 경우는 입력을 그대로 둔다.
/// 원격 경로는 로컬 OS 규칙이 아니라 원격 cwd의 모양으로 구분자를 고른다.
pub(crate) fn resolve_confirmed_path(input: &str, start: &PickerStart) -> String {
    let Some(dir) = start.dir.as_deref().filter(|d| !d.is_empty()) else {
        return input.to_string();
    };
    if input.starts_with('~') {
        return input.to_string();
    }
    if start.remote {
        return join_remote(dir, input);
    }
    let path = std::path::Path::new(input);
    if path.is_absolute() {
        return input.to_string();
    }
    std::path::Path::new(dir)
        .join(path)
        .to_string_lossy()
        .into_owned()
}

/// Windows 경로는 드라이브 문자나 `\\`로 시작한다. 그 밖은 Unix 경로로 본다.
fn is_windows_style(path: &str) -> bool {
    let b = path.as_bytes();
    path.starts_with("\\\\") || (b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':')
}

fn join_remote(dir: &str, input: &str) -> String {
    // 드라이브 상대경로(`C:a.md`)도 원격 OS가 해석하도록 그대로 둔다.
    if input.starts_with('/') || input.starts_with('\\') || is_windows_style(input) {
        return input.to_string();
    }
    if is_windows_style(dir) {
        format!("{}\\{input}", dir.trim_end_matches(['\\', '/']))
    } else {
        format!("{}/{input}", dir.trim_end_matches('/'))
    }
}

impl MarkdownPlugin {
    /// 확인 팝업을 그린다. 열기를 선택하면 문서를 읽고 팝업을 닫는다.
    /// 취소하면 팝업만 닫고 문서는 대기 상태로 둔다. 팝업 외곽과 Esc 처리는 호스트가 맡는다.
    #[cfg(any(unix, windows))]
    pub(crate) fn paint_confirm(&mut self, ctx: PopupSetContextCtx) {
        let iid = ctx.params.instance_id;
        let Some(theme) = ctx.params.theme.as_ref().map(theme_from_wire) else {
            tracing::debug!("markdown confirm popup {iid}: set_context without theme — skipping");
            return;
        };
        let Some(confirm) = self.confirm.get(&iid).cloned() else {
            return;
        };

        let mut chosen: Option<ConfirmChoice> = None;
        {
            // 다른 필드를 처리하기 전에 popups에 대한 차용을 끝낸다.
            let popups = &mut self.popups;
            let installed = &mut self.popup_fonts_installed;
            let tr = &self.tr;
            let popup = popups.entry(iid).or_insert_with(|| EguiMeshPopup::new(iid));
            if installed.insert(iid) {
                install_fonts(popup.context());
            }
            let result = popup.paint(&ctx.host, &ctx.params, |egui_ctx| {
                chosen = draw_confirm(egui_ctx, &theme, &confirm, tr);
            });
            if let Err(e) = result {
                tracing::warn!("markdown confirm popup {iid} paint failed: {e}");
            }
        }

        match chosen {
            Some(ConfirmChoice::Open) => {
                if let Some(doc) = self.docs.get_mut(&confirm.surface_id) {
                    doc.resume_load();
                }
                self.reload_webview(confirm.surface_id);
                close_popup(&ctx.host, iid);
            }
            Some(ConfirmChoice::Cancel) => close_popup(&ctx.host, iid),
            None => {}
        }
    }

    /// 파일 열기 팝업을 그린다. 찾아보기는 호스트 파일 피커에 요청하고
    /// 선택 결과는 이벤트로 받는다. 열기를 선택하면 파일 열기 또는 변환을 요청한다.
    #[cfg(any(unix, windows))]
    pub(crate) fn paint_file_open(&mut self, ctx: PopupSetContextCtx) {
        let iid = ctx.params.instance_id;
        let Some(theme) = ctx.params.theme.as_ref().map(theme_from_wire) else {
            tracing::debug!("markdown file-open popup {iid}: set_context without theme — skipping");
            return;
        };

        let mut action = FileOpenAction::None;
        {
            // 이 블록에서 popups 차용을 끝내고 선택한 동작을 처리한다.
            let popups = &mut self.popups;
            let installed = &mut self.popup_fonts_installed;
            let tr = &self.tr;
            let Some(st) = self.file_open.get_mut(&iid) else {
                return;
            };
            let popup = popups.entry(iid).or_insert_with(|| EguiMeshPopup::new(iid));
            if installed.insert(iid) {
                install_fonts(popup.context());
            }
            let result = popup.paint(&ctx.host, &ctx.params, |egui_ctx| {
                action = draw_file_open(egui_ctx, &theme, st, tr);
            });
            if let Err(e) = result {
                tracing::warn!("markdown file-open popup {iid} paint failed: {e}");
            }
        }

        match action {
            FileOpenAction::Browse => {
                // 파일 피커는 요청 ID를 먼저 반환하고 선택 결과를 file_picker.result로 보낸다.
                let start = self
                    .file_open
                    .get(&iid)
                    .map(|st| st.picker_start.clone())
                    .unwrap_or_default();
                if let Some(request_id) = trigger_file_picker(&ctx.host, iid, &start) {
                    self.pending_file_picker
                        .insert(request_id, crate::browse::PickerOwner::Popup(iid));
                }
            }
            FileOpenAction::Open => {
                let (path, convert_sid) = self
                    .file_open
                    .get(&iid)
                    .map(|s| {
                        let input = s.path_input.trim();
                        let path = if input.is_empty() {
                            String::new()
                        } else {
                            resolve_confirmed_path(input, &s.picker_start)
                        };
                        (path, s.convert_surface_id)
                    })
                    .unwrap_or_default();
                if !path.is_empty() {
                    match convert_sid {
                        // convert 대상 surface → 제자리 markdown 변환.
                        Some(sid) => navigate(&ctx.host, sid, &path, Some(iid)),
                        // 대상 없음 → 새 탭으로 연다.
                        None => open_markdown_file(&ctx.host, iid, &path),
                    }
                    close_popup(&ctx.host, iid);
                }
            }
            FileOpenAction::Cancel => close_popup(&ctx.host, iid),
            FileOpenAction::None => {}
        }
    }

    /// Unix와 Windows 이외의 대상에서는 팝업을 렌더하지 않는다.
    #[cfg(not(any(unix, windows)))]
    pub(crate) fn paint_confirm(&mut self, _ctx: PopupSetContextCtx) {}

    /// unix/windows 외에는 egui-mesh 채널이 비활성이라 파일열기 팝업 렌더는 no-op.
    #[cfg(not(any(unix, windows)))]
    pub(crate) fn paint_file_open(&mut self, _ctx: PopupSetContextCtx) {}
}

/// 경로에서 표시용 파일명(basename)을 파생한다. 파생 실패 시 경로 그대로.
pub(crate) fn basename(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

/// bytes → "3.2 MB" 형태. 10MB 이상은 소수점 없이(host size_confirm 미러).
pub(crate) fn format_size(bytes: u64) -> String {
    let mb = bytes as f64 / (1024.0 * 1024.0);
    if mb >= 10.0 {
        format!("{mb:.0} MB")
    } else {
        format!("{mb:.1} MB")
    }
}

/// host 에 팝업 인스턴스 닫기를 요청한다(셸 생명주기는 host 소유).
#[cfg(any(unix, windows))]
fn close_popup(host: &HostHandle, instance_id: u64) {
    if let Err(e) = host.call("popup.close", json!({ "instance_id": instance_id })) {
        tracing::warn!("markdown confirm popup close failed: {e}");
    }
}

/// 확인 팝업에서 사용자가 고른 결정.
#[cfg(any(unix, windows))]
#[derive(Clone, Copy, PartialEq, Eq)]
enum ConfirmChoice {
    /// 대용량이어도 읽어 렌더한다.
    Open,
    /// 열지 않는다(surface 대기 유지).
    Cancel,
}

/// large-file 확인 팝업 콘텐츠. 파일명 + 크기 태그 + 안내문 + [취소]/[열기]. 색·폰트·
/// 간격은 host 가 보낸 `Theme` 토큰에서만 가져온다. 셸(scrim/border/Esc)은 host 소유.
#[cfg(any(unix, windows))]
fn draw_confirm(
    ctx: &egui::Context,
    theme: &Theme,
    confirm: &LargeFileConfirm,
    tr: &Translator,
) -> Option<ConfirmChoice> {
    use tasty_ui_widgets::{Button, ButtonVariant, TagVariant, margin_all, vspace};

    let frame = egui::Frame::new()
        .fill(theme.bg_panel().to_egui())
        .inner_margin(margin_all(theme.spacing_md));
    let mut choice = None;
    egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
        ui.spacing_mut().item_spacing.y = theme.spacing_sm.value();

        // 제목.
        ui.label(
            egui::RichText::new(tr.t("markdown.large_file.title"))
                .size(theme.font_size_body.value())
                .strong()
                .color(theme.text_primary().to_egui()),
        );

        // 파일명 (mono, muted).
        ui.add(
            egui::Label::new(
                egui::RichText::new(&confirm.file_name)
                    .size(theme.font_size_caption.value())
                    .family(egui::FontFamily::Monospace)
                    .color(theme.text_muted().to_egui()),
            )
            .truncate(),
        );

        // 경고 태그(크기) + 안내문.
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = theme.spacing_sm.value();
            tasty_ui_widgets::tag_mono(ui, theme, &confirm.size_label, TagVariant::Warning, false);
            ui.label(
                egui::RichText::new(tr.t("markdown.large_file.body"))
                    .size(theme.font_size_caption.value())
                    .color(theme.text_secondary().to_egui()),
            );
        });

        vspace(ui, theme.spacing_xs);

        // 푸터: 취소(ghost) / 열기(primary), 우측 정렬.
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if Button::new(tr.t("markdown.large_file.open"))
                    .variant(ButtonVariant::Primary)
                    .show(ui, theme)
                    .clicked()
                {
                    choice = Some(ConfirmChoice::Open);
                }
                if Button::new(tr.t("markdown.large_file.cancel"))
                    .variant(ButtonVariant::Ghost)
                    .show(ui, theme)
                    .clicked()
                {
                    choice = Some(ConfirmChoice::Cancel);
                }
            });
        });
    });
    choice
}

/// 파일열기 팝업에서 사용자가 취한 동작.
#[cfg(any(unix, windows))]
enum FileOpenAction {
    /// 아무것도 안 함.
    None,
    /// 호스트 파일 피커를 연다.
    Browse,
    /// 입력 경로로 파일을 연다.
    Open,
    /// 열지 않고 닫는다.
    Cancel,
}

/// 호스트가 이 플러그인에 보내는 파일 피커 결과 이벤트.
/// src/app/dispatch/file_picker.rs의 FILE_PICKER_RESULT_EVENT와 같은 값이다.
pub(crate) const FILE_PICKER_RESULT_EVENT: &str = "file_picker.result";

/// 파일 피커 결과에서 사용하는 요청 ID, 경로 목록과 취소 여부.
#[derive(serde::Deserialize)]
pub(crate) struct FilePickerResultWire {
    pub(crate) request_id: u64,
    #[serde(default)]
    pub(crate) paths: Vec<String>,
    #[serde(default)]
    pub(crate) cancelled: bool,
}

/// 호스트 파일 피커를 열도록 요청한다. 반환값은 요청 ID이며 선택 경로는
/// file_picker.result 이벤트로 받는다. owner_popup_instance는 부모 팝업을,
/// start는 시작 폴더와 surface를 지정한다.
/// docs/dev-guide/popup-implementation.md#플러그인이-호스트-팝업-결과를-기다릴-때.
#[cfg(any(unix, windows))]
fn trigger_file_picker(
    host: &HostHandle,
    owner_popup_instance: u64,
    start: &PickerStart,
) -> Option<u64> {
    match host.call(
        "file_picker.trigger",
        file_picker_trigger_params(owner_popup_instance, start),
    ) {
        Ok(v) => v.get("request_id").and_then(Value::as_u64),
        Err(e) => {
            tracing::warn!("markdown file-open browse (file_picker.trigger) failed: {e}");
            None
        }
    }
}

/// `file_picker.trigger` 파라미터. 호출(`HostHandle`)과 떼어 두어 단위 테스트가 wire 모양을 본다.
#[cfg(any(unix, windows))]
pub(crate) fn file_picker_trigger_params(owner_popup_instance: u64, start: &PickerStart) -> Value {
    json!({
        "filters": ["md", "markdown"],
        // 파일 피커를 이 팝업의 자식으로 열도록 부모 인스턴스를 전달한다.
        "owner_popup_instance": owner_popup_instance,
        "start_dir": start.dir,
        "origin_surface_id": start.origin_surface_id,
    })
}

/// 입력/선택한 markdown 파일을 host `file_handler.dispatch` 로 연다(origin 없이 → focused
/// pane 의 새 탭). markdown 감지·surface 생성은 host detector 가 담당한다.
#[cfg(any(unix, windows))]
fn open_markdown_file(host: &HostHandle, owner_popup_instance: u64, path: &str) {
    if let Err(e) = host.call(
        "file_handler.dispatch",
        file_open_dispatch_params(owner_popup_instance, path),
    ) {
        tracing::warn!("markdown file-open dispatch failed: {e}");
    }
}

/// 파일 열기 요청. 호스트는 owner_popup_instance의 소유자와 사용자 입력을
/// 확인해 사용자 요청으로 처리한다. 이 정보가 없으면 에이전트 요청으로 처리한다.
/// docs/features/file-handler/index.md#origin-소유권과-비동기-완료.
#[cfg(any(unix, windows))]
pub(crate) fn file_open_dispatch_params(owner_popup_instance: u64, path: &str) -> Value {
    json!({
        "path": path,
        "depth": "deep",
        "owner_popup_instance": owner_popup_instance,
    })
}

/// 파일열기 팝업 콘텐츠. 경로 입력 필드 + [browse] + [취소]/[열기]. 색·폰트·간격은 host 가
/// 보낸 `Theme` 토큰에서만 가져온다. 키보드/텍스트 입력은 host 가 popup raw_input 으로
/// forward 한다. 셸(scrim/border/Esc/outside-click)은 host 소유.
#[cfg(any(unix, windows))]
fn draw_file_open(
    ctx: &egui::Context,
    theme: &Theme,
    st: &mut FileOpenState,
    tr: &Translator,
) -> FileOpenAction {
    use tasty_ui_widgets::{Button, ButtonVariant, ControlSize, margin_all, vspace};

    let frame = egui::Frame::new()
        .fill(theme.bg_panel().to_egui())
        .inner_margin(margin_all(theme.spacing_md));
    let mut action = FileOpenAction::None;
    egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
        ui.spacing_mut().item_spacing.y = theme.spacing_sm.value();

        // Esc → 닫기(host 셸도 처리하나 forward 된 입력에서 즉시 반응).
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            action = FileOpenAction::Cancel;
        }

        // 제목.
        ui.label(
            egui::RichText::new(tr.t("markdown.file_open.title"))
                .size(theme.font_size_body.value())
                .strong()
                .color(theme.text_primary().to_egui()),
        );

        // 경로 라벨.
        ui.label(
            egui::RichText::new(tr.t("markdown.file_open.path_label"))
                .size(theme.font_size_caption.value())
                .color(theme.text_secondary().to_egui()),
        );

        // 찾아보기 버튼을 오른쪽에 먼저 놓고 남은 너비를 경로 입력에 쓴다.
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = theme.spacing_sm.value();
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let browse_clicked = Button::new(tr.t("markdown.file_open.browse"))
                    .variant(ButtonVariant::Secondary)
                    .size(ControlSize::Sm)
                    .leading_icon(&|ui, rect, c| {
                        tasty_plugin_sdk::baked_icon::draw(
                            ui.painter(),
                            baked_icons::FOLDER,
                            rect.center(),
                            rect.width(),
                            c,
                        );
                    })
                    .show(ui, theme)
                    .clicked();

                let field_w = ui.available_width();
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut st.path_input)
                        .desired_width(field_w)
                        .hint_text(tasty_egui_theme::hint_text(
                            theme,
                            tr.t("markdown.file_open.path_label"),
                        ))
                        .font(egui::FontId::new(
                            theme.font_size_body.value(),
                            egui::FontFamily::Monospace,
                        )),
                );
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    action = FileOpenAction::Open;
                }
                if browse_clicked {
                    action = FileOpenAction::Browse;
                }
            });
        });

        vspace(ui, theme.spacing_xs);

        // 푸터: 취소(ghost) / 열기(primary), 우측 정렬.
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if Button::new(tr.t("markdown.file_open.open"))
                    .variant(ButtonVariant::Primary)
                    .show(ui, theme)
                    .clicked()
                {
                    action = FileOpenAction::Open;
                }
                if Button::new(tr.t("markdown.file_open.cancel"))
                    .variant(ButtonVariant::Ghost)
                    .show(ui, theme)
                    .clicked()
                {
                    action = FileOpenAction::Cancel;
                }
            });
        });
    });
    action
}

/// 팝업의 egui Context에 시스템 CJK 폰트를 찾을 수 있을 때 설치한다.
/// 본문의 글꼴은 WebView가 처리한다.
#[cfg(any(unix, windows))]
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    tasty_egui_theme::install_system_cjk_fallback(&mut fonts);
    // 언어팩 폰트도 공용 검증 함수로 확인한 뒤 fallback에 추가한다.
    // TASTY_LOCALE_FONT 경로는 호스트가 전달한다.
    if let Some(path) = std::env::var_os("TASTY_LOCALE_FONT").filter(|v| !v.is_empty()) {
        let path = std::path::PathBuf::from(path);
        let face = tasty_egui_theme::locale_font_face_from_env();
        if let Err(e) = tasty_egui_theme::install_locale_font_fallback(&mut fonts, &path, face) {
            tracing::warn!(
                "locale font at {} could not be installed: {e}",
                path.display()
            );
        }
    }
    ctx.set_fonts(fonts);
}
