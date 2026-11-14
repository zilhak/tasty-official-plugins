//! 주소 띠·목차·문서 안 링크와 스크롤 복원을 다루는 신뢰된 인라인 스크립트(디자인 결정 Option 1: HTML/CSS 와
//! 최소한의 신뢰된 JS). 문서 전체 정리(sanitize)
//! 뒤에 플러그인이 붙인다.

use super::{ADDR_WAITING_AFTER_MS, NAV_FRAGMENT_MARKER};

/// 플러그인이 본문 정리 후 추가하는 이동 스크립트.
/// 주소 입력과 새로고침은 #tasty-nav 마커로 호스트에 알린다. 주소 이동에는 nonce 를 붙여 같은 경로를
/// 다시 보내도 hash 가 바뀌게 한다. 원격 문서의 Esc 는 입력을 지금 원격 경로로 되돌린다.
/// 오류 행은 다음 입력이나 Esc 에서 지우고 플러그인에 알려 보관한 실패도 버리게 한다.
/// Esc 는 로컬·원격 모두 편집 중인 입력을 지금 경로로 되돌리고 이동하지 않는다.
/// `address_keys`(단축키 설정 `explorer_focus_address`의 판정 값 JSON)는 주소 입력에 포커스를 주고 값 전체를
/// 고른다. 읽기 전용 주소도 고를 수 있으며 Enter 는 이동하지 않는다.
/// 원격 이동을 보낸 뒤 [`ADDR_WAITING_AFTER_MS`] 안에 결과로 문서가 다시 실리지 않으면 숨겨 둔 대기 행을
/// 보인다. 페이지 안에서만 바꾸므로 입력 포커스와 IME 조합을 건드리지 않는다. 대기 행도 다음 입력·Esc
/// 에서 지운다.
/// 문서 안 링크는 기본 이동을 취소하고 해당 ID로 스크롤한다. 같은 링크를 다시
/// 클릭해도 동작하며, ID는 원문과 percent-decoding한 값을 차례로 찾는다.
///
/// 파일 경로별 스크롤 위치를 sessionStorage에 저장하고 복원을 시도한다.
/// load_html 이후 저장소가 유지되는지는 세 WebView 백엔드 모두에서 검증하지 않았다.
/// 저장소를 사용할 수 없으면 복원하지 않는다.
pub(super) fn nav_script(
    file_path: &str,
    bar_h: f32,
    bar_h_row: f32,
    address_keys: &str,
) -> String {
    format!(
        r#"(function(){{
var bar=document.getElementById('tasty-addr-bar');
var field=document.getElementById('tasty-addr-field');
var i=document.getElementById('tasty-addr-input');
var g=document.getElementById('tasty-addr-go');
var shown=document.querySelector('.tasty-addr-display bdi');
var waitRow=document.getElementById('tasty-addr-waiting');
var remote=!!bar&&bar.getAttribute('data-kind')==='remote';
var current={file_path_json};
var wt=0;
function nonce(){{return String(Date.now());}}
function sync(){{if(shown&&i)shown.textContent=i.value;}}
function waiting(){{
wt=0;
var row=document.getElementById('tasty-addr-error');
if(row)row.parentNode.removeChild(row);
if(field){{field.classList.remove('tasty-addr-invalid');field.classList.add('tasty-addr-pending');}}
if(i)i.removeAttribute('aria-invalid');
waitRow.hidden=false;
document.documentElement.style.setProperty('--md-addr-bar-h','{bar_h_row}px');
}}
function wait(ms){{if(wt)clearTimeout(wt);wt=setTimeout(waiting,ms);}}
function dismiss(){{
var timed=!!wt;
if(wt){{clearTimeout(wt);wt=0;}}
var row=document.getElementById('tasty-addr-error');
var open=!!waitRow&&!waitRow.hidden;
if(!timed&&!row&&!open&&!(field&&(field.classList.contains('tasty-addr-invalid')||field.classList.contains('tasty-addr-pending'))))return;
if(row)row.parentNode.removeChild(row);
if(waitRow)waitRow.hidden=true;
if(field){{field.classList.remove('tasty-addr-invalid');field.classList.remove('tasty-addr-pending');}}
if(i)i.removeAttribute('aria-invalid');
document.documentElement.style.setProperty('--md-addr-bar-h','{bar_h}px');
location.hash='tasty-nav:addr-dismiss:'+nonce();
}}
function go(){{
if(!i||i.readOnly)return;
var v=i.value.trim();
if(!v)return;
location.hash='tasty-nav:addr:'+nonce()+':'+encodeURIComponent(v);
if(remote&&waitRow)wait({wait_ms});
}}
var left=bar?parseInt(bar.getAttribute('data-wait-ms')||'',10):NaN;
if(left>0&&waitRow)wait(left);
if(g)g.addEventListener('click',go);
var f=document.getElementById('tasty-browse');
if(f)f.addEventListener('click',function(){{location.hash='tasty-nav:browse:'+nonce();}});
var r=document.getElementById('tasty-refresh');
if(r)r.addEventListener('click',function(){{location.hash='tasty-nav:refresh:'+nonce();}});
var o=document.querySelector('.tasty-state-action');
if(o)o.addEventListener('click',function(){{location.hash='tasty-nav:refresh:'+nonce();}});
if(i){{
i.addEventListener('input',function(){{sync();dismiss();}});
i.addEventListener('keydown',function(e){{
if(e.isComposing||e.keyCode===229)return;
if(e.key==='Enter'){{go();return;}}
if(e.key==='Escape'&&!i.readOnly){{e.preventDefault();if(i.value!==current){{i.value=current;sync();}}dismiss();i.blur();}}
}});
var ADDR_KEYS={address_keys};
document.addEventListener('keydown',function(e){{
if(e.isComposing||e.keyCode===229)return;
var hit=ADDR_KEYS.some(function(k){{return e.ctrlKey===k.ctrl&&e.altKey===k.alt&&e.metaKey===k.meta&&e.shiftKey===k.shift&&e.code===k.code;}});
if(!hit)return;
e.preventDefault();
i.focus();
i.select();
}},true);
}}
var toc=document.getElementById('tasty-toc');
var tocToggle=document.getElementById('tasty-toc-toggle');
if(toc&&tocToggle){{
tocToggle.addEventListener('click',function(){{
var collapsed=toc.classList.toggle('tasty-toc-collapsed');
tocToggle.setAttribute('aria-expanded',collapsed?'false':'true');
}});
}}
document.addEventListener('click',function(e){{
var t=e.target;
if(!t||!t.closest)return;
var a=t.closest('a[href]');
if(!a)return;
var href=a.getAttribute('href')||'';
if(href.charAt(0)!=='#')return;
if(href.indexOf('#'+{marker_json})===0)return;
e.preventDefault();
var id=href.slice(1);
if(!id){{window.scrollTo(0,0);return;}}
var el=document.getElementById(id);
if(!el){{try{{el=document.getElementById(decodeURIComponent(id));}}catch(err){{}}}}
if(!el)return;
el.scrollIntoView({{block:'start'}});
}});
var scrollKey='tasty-md-scroll:'+{file_path_json};
try{{
var saved=sessionStorage.getItem(scrollKey);
if(saved)window.scrollTo(0,parseInt(saved,10)||0);
}}catch(e){{}}
var saveTimer=null;
window.addEventListener('scroll',function(){{
if(saveTimer)clearTimeout(saveTimer);
saveTimer=setTimeout(function(){{
try{{sessionStorage.setItem(scrollKey,String(window.scrollY));}}catch(e){{}}
}},150);
}});
}})();"#,
        marker_json = serde_json::to_string(NAV_FRAGMENT_MARKER)
            .unwrap_or_else(|_| format!("\"{NAV_FRAGMENT_MARKER}\"")),
        file_path_json = serde_json::to_string(file_path).unwrap_or_else(|_| "\"\"".to_string()),
        bar_h = bar_h,
        bar_h_row = bar_h_row,
        wait_ms = ADDR_WAITING_AFTER_MS,
        address_keys = address_keys,
    )
}
