use super::*;

/// The path-list form `configure` writes: canonical paths without the Windows verbatim prefix.
fn entry(dir: &Path) -> String {
    tasty_utils::path::strip_verbatim_prefix(dir.to_str().unwrap())
}

#[test]
fn merges_idempotently_and_uninstalls_only_own_path_on_both_platforms() {
    for (sep, own, other) in [
        (':', "/home/u/config/tasty-managed/notify-mod", "/other"),
        (
            ';',
            r"C:\Users\u\config\tasty-managed\notify-mod",
            r"D:\other",
        ),
    ] {
        let mut root = json!({"env":{"CLAUDE_CODE_PLUGIN_DIRS":other,"KEEP":"value"},"hooks":{"Stop":[{"command":"user"}]}});
        assert!(configure(&mut root, Path::new(own), sep, true).unwrap());
        let installed = root.clone();
        assert!(!configure(&mut root, Path::new(own), sep, true).unwrap());
        assert_eq!(root, installed);
        assert_eq!(root["env"][ENV], format!("{other}{sep}{own}"));
        assert!(configure(&mut root, Path::new(own), sep, false).unwrap());
        assert_eq!(root["env"][ENV], other);
        assert_eq!(root["env"]["KEEP"], "value");
        assert_eq!(root["hooks"]["Stop"][0]["command"], "user");
    }
}

#[test]
fn duplicate_own_entries_are_normalized_and_empty_key_is_removed() {
    let mut root = json!({"env":{ENV:"/own:/other:/own"}});
    configure(&mut root, Path::new("/own"), ':', true).unwrap();
    assert_eq!(root["env"][ENV], "/other:/own");
    configure(&mut root, Path::new("/own"), ':', false).unwrap();
    assert_eq!(root["env"][ENV], "/other");
    let mut root = json!({});
    configure(&mut root, Path::new("/own"), ':', true).unwrap();
    configure(&mut root, Path::new("/own"), ':', false).unwrap();
    assert!(root.get("env").is_none());
}

#[test]
fn invalid_user_settings_are_not_coerced_or_overwritten() {
    for mut root in [json!({"env":false}), json!({"env":{ENV:["user"]}})] {
        let original = root.clone();
        assert!(configure(&mut root, Path::new("/own"), ':', true).is_err());
        assert_eq!(root, original);
    }
    assert!(configure(&mut json!({}), Path::new("/has:separator"), ':', true).is_err());
}

#[test]
fn installed_assets_are_embedded_and_second_install_does_not_write() {
    let temp = tempfile::tempdir().unwrap();
    let settings = temp.path().join("settings.json");
    let mut root = json!({});
    assert!(install(&mut root, &settings).unwrap());
    let mods = directories(&settings).unwrap();
    assert_eq!(mods.len(), 2);
    let times: Vec<_> = mods
        .iter()
        .flat_map(|(dir, assets)| assets.iter().map(move |asset| (dir, asset)))
        .map(|(dir, (name, content))| {
            let path = dir.join(name);
            assert_eq!(std::fs::read_to_string(&path).unwrap(), *content);
            std::fs::metadata(path).unwrap().modified().unwrap()
        })
        .collect();
    assert!(!install(&mut root, &settings).unwrap());
    let after: Vec<_> = mods
        .iter()
        .flat_map(|(dir, assets)| assets.iter().map(move |(name, _)| dir.join(name)))
        .map(|path| std::fs::metadata(path).unwrap().modified().unwrap())
        .collect();
    assert_eq!(after, times);
    let dir = &mods[0].0;
    std::fs::write(dir.join("keep.txt"), "user").unwrap();
    assert!(uninstall(&mut root, &settings).unwrap());
    assert_eq!(
        std::fs::read_to_string(dir.join("keep.txt")).unwrap(),
        "user"
    );
    assert!(!mods[1].0.exists());
    assert!(!uninstall(&mut root, &settings).unwrap());
    for (dir, assets) in &mods {
        for (name, _) in *assets {
            assert!(!dir.join(name).exists());
        }
    }
}

#[test]
fn gate_is_a_sibling_plugin_with_its_own_path_entry() {
    let temp = tempfile::tempdir().unwrap();
    let settings = temp.path().join("settings.json");
    let mut root = json!({"env":{ENV:"/other"}});
    install(&mut root, &settings).unwrap();
    let notify = directory(&settings).unwrap();
    let gate = notify.parent().unwrap().join("notify-gate");
    let sep = separator();
    assert_eq!(
        root["env"][ENV],
        format!("/other{sep}{}{sep}{}", entry(&notify), entry(&gate))
    );
    assert!(!gate.starts_with(&notify));
    assert!(
        std::fs::read_to_string(gate.join(".claude-plugin/plugin.json"))
            .unwrap()
            .contains("\"tasty-notify-gate\"")
    );
    assert_eq!(
        std::fs::read_to_string(gate.join(".tasty-owner")).unwrap(),
        MARKER
    );
    assert!(uninstall(&mut root, &settings).unwrap());
    assert_eq!(root, json!({"env":{ENV:"/other"}}));
    assert!(!gate.exists() && !notify.exists());
}

#[test]
fn upgrade_from_notify_only_install_adds_the_gate_and_keeps_order() {
    let temp = tempfile::tempdir().unwrap();
    let settings = temp.path().join("settings.json");
    let notify = directory(&settings).unwrap();
    std::fs::create_dir_all(&notify).unwrap();
    std::fs::write(notify.join(".tasty-owner"), MARKER).unwrap();
    for (name, content) in ASSETS {
        let path = notify.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    let sep = separator();
    let mut root = json!({"env":{ENV:format!("{}{sep}/after", entry(&notify))}});
    assert!(install(&mut root, &settings).unwrap());
    let gate = notify.parent().unwrap().join("notify-gate");
    assert_eq!(
        root["env"][ENV],
        format!("{}{sep}/after{sep}{}", entry(&notify), entry(&gate))
    );
    for (name, content) in GATE_ASSETS {
        assert_eq!(std::fs::read_to_string(gate.join(name)).unwrap(), *content);
    }
    let installed = root.clone();
    assert!(!install(&mut root, &settings).unwrap());
    assert_eq!(root, installed);
}

#[test]
fn unowned_gate_directory_refuses_before_any_change() {
    let temp = tempfile::tempdir().unwrap();
    let settings = temp.path().join("settings.json");
    let gate = directory(&settings)
        .unwrap()
        .parent()
        .unwrap()
        .join("notify-gate");
    std::fs::create_dir_all(&gate).unwrap();
    let mut root = json!({});
    assert!(install(&mut root, &settings).is_err());
    assert_eq!(root, json!({}));
    assert!(!directory(&settings).unwrap().exists());
    assert!(uninstall(&mut root, &settings).is_err());
    assert!(gate.exists());
}

#[cfg(unix)]
#[test]
fn install_and_uninstall_refuse_linked_assets_without_touching_target() {
    let temp = tempfile::tempdir().unwrap();
    let settings = temp.path().join("settings.json");
    let mut root = json!({});
    install(&mut root, &settings).unwrap();
    let target = temp.path().join("user-file");
    std::fs::write(&target, "preserve").unwrap();
    let asset = directory(&settings).unwrap().join("hooks/register.ts");
    std::fs::remove_file(&asset).unwrap();
    std::os::unix::fs::symlink(&target, asset).unwrap();
    assert!(install(&mut root, &settings).is_err());
    assert!(uninstall(&mut root, &settings).is_err());
    assert_eq!(std::fs::read_to_string(target).unwrap(), "preserve");
}

#[test]
fn unowned_existing_directory_is_never_overwritten_or_removed() {
    let temp = tempfile::tempdir().unwrap();
    let settings = temp.path().join("settings.json");
    let dir = directory(&settings).unwrap();
    std::fs::create_dir_all(dir.join("hooks")).unwrap();
    std::fs::write(dir.join("hooks/register.ts"), "user").unwrap();
    let mut root = json!({});
    assert!(install(&mut root, &settings).is_err());
    assert!(uninstall(&mut root, &settings).is_err());
    assert_eq!(
        std::fs::read_to_string(dir.join("hooks/register.ts")).unwrap(),
        "user"
    );
    assert_eq!(root, json!({}));
}

#[cfg(unix)]
#[test]
fn settings_parent_alias_has_one_canonical_identity_and_preserves_user_values() {
    let temp = tempfile::tempdir().unwrap();
    let canonical = temp.path().canonicalize().unwrap();
    let alias = canonical.join("alias");
    let real = canonical.join("real");
    std::fs::create_dir(&real).unwrap();
    std::os::unix::fs::symlink(&real, &alias).unwrap();
    let settings = alias.join("settings.json");
    let mut root = json!({"env":{"KEEP":"user"},"hooks":{"Stop":[{"command":"user"}]}});
    let original = root.clone();
    assert!(install(&mut root, &settings).unwrap());
    assert_eq!(
        directory(&settings).unwrap(),
        directory(&real.join("settings.json")).unwrap()
    );
    assert!(!install(&mut root, &real.join("settings.json")).unwrap());
    assert!(uninstall(&mut root, &real.join("settings.json")).unwrap());
    assert_eq!(root, original);
}

#[cfg(unix)]
#[test]
fn aliased_parent_does_not_authorize_a_linked_managed_subtree() {
    let temp = tempfile::tempdir().unwrap();
    let canonical = temp.path().canonicalize().unwrap();
    let real = canonical.join("real");
    let outside = canonical.join("outside");
    std::fs::create_dir(&real).unwrap();
    std::fs::create_dir(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, real.join("tasty-managed")).unwrap();
    let alias = canonical.join("alias");
    std::os::unix::fs::symlink(&real, &alias).unwrap();
    let mut root = json!({});
    assert!(install(&mut root, &alias.join("settings.json")).is_err());
    assert!(uninstall(&mut root, &alias.join("settings.json")).is_err());
    assert_eq!(std::fs::read_dir(outside).unwrap().count(), 0);
}
