use super::*;
use registry::{HelperPhase, HelperState};
use std::sync::Mutex;
const A: &str = "01a12f74-dab9-7a00-8000-0123456789ab";
const B: &str = "01a12f74-dab9-7a00-8000-0123456789ac";
#[derive(Default)]
struct Host(Mutex<Vec<Value>>);
impl tasty_plugin_agent_common::host_call::HostCall for Host {
    fn call(&self, method: &str, params: Value) -> Result<Value, tasty_plugin_sdk::PluginError> {
        assert_eq!(method, "surface.meta.set");
        self.0.lock().unwrap().push(params);
        Ok(json!({}))
    }
}
fn ready(root: &Path, surface: u32) -> registry::Session {
    let mut session =
        registry::open_session(root, surface, None, vec!["-a".into(), "never".into()]).unwrap();
    session.native_args = Some(vec!["-a".into(), "never".into()]);
    registry::write_session(root, &session).unwrap();
    crate::store::write_json(
        &registry::helper_file(&registry::session_dir(root, &session.generation)),
        &HelperState {
            phase: HelperPhase::Ready,
            url: Some("unix:///owned-test.sock".into()),
            detail: None,
            resumed: None,
            updated_at_ms: 0,
        },
    )
    .unwrap();
    session
}
#[test]
fn selection_without_a_model_turn_records_an_exact_restore_and_switches_the_binding() {
    let root = tempfile::tempdir().unwrap();
    let session = ready(root.path(), 7);
    let host = Host::default();
    let tr = Translator::default();
    for thread in [A, B] {
        handle_bind_with(
            &host,
            root.path(),
            &json!({"surface":7,"gen":session.generation,"session":thread,"model":"gpt-test"}),
            &tr,
            &|_, _| Ok(true),
        )
        .unwrap();
    }
    assert!(registry::thread_session(root.path(), A).unwrap().is_none());
    assert_eq!(
        registry::thread_session(root.path(), B)
            .unwrap()
            .unwrap()
            .surface,
        7
    );
    let calls = host.0.lock().unwrap();
    assert_eq!(calls.len(), 4);
    assert_eq!(calls[2]["value"], B);
    let line = calls[3]["value"].as_str().unwrap();
    assert!(line.contains(&format!("terminal resume '{B}'")), "{line}");
    assert!(line.ends_with("'-a' 'never' '-m' 'gpt-test'"));
}
#[test]
fn previous_generation_and_another_surface_cannot_overwrite_restore_metadata() {
    let root = tempfile::tempdir().unwrap();
    let old = ready(root.path(), 7);
    let current = ready(root.path(), 7);
    let host = Host::default();
    let tr = Translator::default();
    for (surface, generation) in [
        (7, old.generation.as_str()),
        (8, current.generation.as_str()),
    ] {
        assert!(
            handle_bind_with(
                &host,
                root.path(),
                &json!({"surface":surface,"gen":generation,"session":A}),
                &tr,
                &|_, _| Ok(true),
            )
            .is_err()
        );
    }
    assert!(host.0.lock().unwrap().is_empty());
}
