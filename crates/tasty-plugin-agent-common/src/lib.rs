#![forbid(unsafe_code)]

//! Claude와 Codex 플러그인의 공용 헬퍼: 프롬프트 파일, 자식 조회, 요청 인자, 완료 알림 훅,
//! agent task 턴 보고.
//!
//! CLI별 종료 대기 시간·키 입력·이벤트 목록은 각 플러그인에서 정한다.
//! 오류 판정은 공유하지만 메시지와 번역 키는 각 플러그인이 제공한다.
//! 잘못된 surface 인자는 어느 키가 잘못됐는지까지 호출자에게 전달해야 한다.
//!
//! 현재 공개 응답 형식도 유지한다. Claude의 children은 추가 조회·변환한 배열이고,
//! kill은 호스트 응답에 killed를 더한다(killed_surface_id가 있을 때만 true, 원격 전달이면 false).
//! Codex는 두 경우 모두 호스트 응답을 그대로 반환한다.
//! Claude의 kill은 별도로 error_scan도 해제한다. 이 동작은 Codex에는 없다.
//! 계약과 각 플러그인의 응답 시험은 docs/dev-guide/paired-agent-handlers.md 참고.
//!
//! 이 크레이트는 매니페스트가 없는 공유 라이브러리다. 다만 변경이 번들 산출물에
//! 영향을 주면 의존하는 두 플러그인의 버전도 갱신해야 한다
//! (docs/dev-guide/release.md#플러그인-버전-비교).

pub mod children;
pub mod host_call;
pub mod params;
pub mod prompt_file;
pub mod reboot;
pub mod remote_attach;
pub mod remote_owner;
pub mod remote_results;
pub mod task_turn;
