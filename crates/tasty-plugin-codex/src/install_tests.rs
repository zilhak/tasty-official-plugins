use crate::install::{merge_install, remove_install};

#[test]
fn mixed_and_separate_user_handlers_survive_install_reinstall_and_uninstall() {
    let original: toml::Value = r#"
model = "sentinel-model"
[hooks.state.sentinel]
trusted_hash = "sha256:sentinel"
[[hooks.Stop]]
matcher = "owned-matcher"
custom_group_metadata = "keep"
[[hooks.Stop.hooks]]
type = "command"
command = "echo user-sentinel"
[[hooks.Stop.hooks]]
type = "command"
command = "tasty codex hook stop --surface 1"
[[hooks.Stop]]
matcher = "separate-matcher"
[[hooks.Stop.hooks]]
type = "command"
command = "echo separate-sentinel"
"#
    .parse()
    .unwrap();
    let once = merge_install(original.clone());
    let twice = merge_install(once.clone());
    assert_eq!(once, twice);
    for value in [
        once.clone(),
        remove_install(original.clone()),
        remove_install(twice),
    ] {
        assert_eq!(value["model"], original["model"]);
        assert_eq!(value["hooks"]["state"], original["hooks"]["state"]);
        let groups = value["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(groups[0]["matcher"].as_str(), Some("owned-matcher"));
        assert_eq!(groups[0]["custom_group_metadata"].as_str(), Some("keep"));
        assert_eq!(
            groups[0]["hooks"][0]["command"].as_str(),
            Some("echo user-sentinel")
        );
        assert_eq!(
            groups[1]["hooks"][0]["command"].as_str(),
            Some("echo separate-sentinel")
        );
    }
    assert!(
        !toml::to_string(&remove_install(once))
            .unwrap()
            .contains("tasty codex hook")
    );
}
