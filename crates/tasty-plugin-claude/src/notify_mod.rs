//! Instance-independent Claude mod assets and their settings path-list entry.
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const MARKER: &str = "tasty-notify-managed-v1\n";
const ENV: &str = "CLAUDE_CODE_PLUGIN_DIRS";
const ASSETS: &[(&str, &str)] = &[
    (
        ".claude-plugin/plugin.json",
        include_str!("notify_mod/claude/.claude-plugin/plugin.json"),
    ),
    (
        "hooks/hooks.json",
        include_str!("notify_mod/claude/hooks/hooks.json"),
    ),
    (
        "hooks/register.ts",
        include_str!("notify_mod/claude/hooks/register.ts"),
    ),
    (
        "hooks/reader.ts",
        include_str!("notify_mod/claude/hooks/reader.ts"),
    ),
    (
        "hooks/messages.ts",
        include_str!("notify_mod/claude/hooks/messages.ts"),
    ),
    (
        "types/index.d.ts",
        include_str!("notify_mod/claude/types/index.d.ts"),
    ),
];
// A separate plugin: tasty-notify's own hooks do not see its timer submissions.
// Its source sits inside the mod folder only so the engine tests can load it.
const GATE_ASSETS: &[(&str, &str)] = &[
    (
        ".claude-plugin/plugin.json",
        include_str!("notify_mod/claude/gate/.claude-plugin/plugin.json"),
    ),
    (
        "hooks/hooks.json",
        include_str!("notify_mod/claude/gate/hooks/hooks.json"),
    ),
    (
        "hooks/register.ts",
        include_str!("notify_mod/claude/gate/hooks/register.ts"),
    ),
    (
        "types/index.d.ts",
        include_str!("notify_mod/claude/gate/types/index.d.ts"),
    ),
];
/// Sibling managed plugin directories, each its own path-list entry, in install order.
type Assets = &'static [(&'static str, &'static str)];
const MODS: &[(&str, Assets)] = &[("notify-mod", ASSETS), ("notify-gate", GATE_ASSETS)];

#[cfg(test)]
pub(crate) fn directory(settings: &Path) -> Result<PathBuf> {
    Ok(managed_root(settings)?.join(MODS[0].0))
}

fn directories(settings: &Path) -> Result<Vec<(PathBuf, Assets)>> {
    let root = managed_root(settings)?;
    Ok(MODS
        .iter()
        .map(|(name, assets)| (root.join(name), *assets))
        .collect())
}

fn managed_root(settings: &Path) -> Result<PathBuf> {
    let mut anchor = settings.parent().context("notify_mod.settings_parent")?;
    let mut missing = Vec::new();
    let mut canonical = loop {
        match std::fs::canonicalize(anchor) {
            Ok(path) => break path,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                missing.push(anchor.file_name().context("notify_mod.settings_parent")?);
                anchor = anchor.parent().context("notify_mod.settings_parent")?;
            }
            Err(e) => return Err(e.into()),
        }
    };
    for component in missing.into_iter().rev() {
        canonical.push(component);
    }
    Ok(canonical.join("tasty-managed"))
}

fn separator() -> char {
    if cfg!(windows) { ';' } else { ':' }
}

/// Validate before touching files. Never coerce a user's non-string value.
pub(crate) fn configure(root: &mut Value, dir: &Path, sep: char, install: bool) -> Result<bool> {
    let path =
        tasty_utils::path::strip_verbatim_prefix(dir.to_str().context("notify_mod.non_utf8_path")?);
    let path = path.as_str();
    if path.contains(sep) {
        bail!("notify_mod.path_separator");
    }
    let obj = root
        .as_object_mut()
        .context("notify_mod.settings_root_type")?;
    if !install && !obj.contains_key("env") {
        return Ok(false);
    }
    let env = obj
        .entry("env")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .context("notify_mod.settings_env_type")?;
    let old = match env.get(ENV) {
        None => "",
        Some(value) => value.as_str().context("notify_mod.plugin_dirs_type")?,
    };
    // Retain other entries verbatim and in order, including empty path-list fields.
    let mut parts: Vec<&str> = if old.is_empty() {
        vec![]
    } else {
        old.split(sep).collect()
    };
    let before = parts.clone();
    // An entry present once keeps its place, so sibling entries do not swap on reinstall.
    let present_once = parts.iter().filter(|part| **part == path).count() == 1;
    if !(install && present_once) {
        parts.retain(|part| *part != path);
        if install {
            parts.push(path);
        }
    }
    if before == parts {
        return Ok(false);
    }
    if parts.is_empty() {
        env.remove(ENV);
    } else {
        env.insert(ENV.into(), json!(parts.join(&sep.to_string())));
    }
    if env.is_empty() {
        obj.remove("env");
    }
    Ok(true)
}

/// Refuse links below the canonical settings-parent boundary. Unknown files are never removed.
fn check_path(path: &Path, boundary: &Path) -> Result<()> {
    for ancestor in path
        .ancestors()
        .take_while(|ancestor| *ancestor != boundary)
    {
        match std::fs::symlink_metadata(ancestor) {
            Ok(meta) if meta.file_type().is_symlink() => {
                bail!("notify_mod.symlink: {}", ancestor.display())
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

fn check_owner(dir: &Path) -> Result<()> {
    check_path(
        &dir.join(".tasty-owner"),
        dir.parent()
            .and_then(Path::parent)
            .context("notify_mod.settings_parent")?,
    )?;
    if dir.exists() {
        let marker = std::fs::read_to_string(dir.join(".tasty-owner"))
            .context("notify_mod.owner_missing")?;
        if marker != MARKER {
            bail!("notify_mod.owner_mismatch");
        }
    }
    Ok(())
}

fn preflight(settings: &Path, dir: &Path, assets: &[(&str, &str)]) -> Result<()> {
    if std::fs::symlink_metadata(settings).is_ok_and(|meta| meta.file_type().is_symlink()) {
        bail!("notify_mod.symlink: {}", settings.display());
    }
    for (name, _) in assets {
        check_path(
            &dir.join(name),
            dir.parent()
                .and_then(Path::parent)
                .context("notify_mod.settings_parent")?,
        )?;
    }
    Ok(())
}

pub(crate) fn install(root: &mut Value, settings: &Path) -> Result<bool> {
    let mods = directories(settings)?;
    let mut changed = false;
    for (dir, _) in &mods {
        check_owner(dir)?;
    }
    for (dir, _) in &mods {
        changed |= configure(root, dir, separator(), true)?;
    }
    // Preflight every target so a known refusal does not leave half an asset set.
    for (dir, assets) in &mods {
        preflight(settings, dir, assets)?;
    }
    for (dir, assets) in &mods {
        if !dir.exists() {
            std::fs::create_dir_all(dir)?;
            std::fs::write(dir.join(".tasty-owner"), MARKER)?;
            changed = true;
        }
        for (name, content) in *assets {
            let path = dir.join(name);
            match std::fs::read(&path) {
                Ok(bytes) if bytes == content.as_bytes() => continue,
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            std::fs::create_dir_all(path.parent().context("notify_mod.asset_parent")?)?;
            std::fs::write(path, content)?;
            changed = true;
        }
    }
    Ok(changed)
}

pub(crate) fn uninstall(root: &mut Value, settings: &Path) -> Result<bool> {
    let mods = directories(settings)?;
    let mut changed = false;
    for (dir, _) in &mods {
        check_owner(dir)?;
    }
    for (dir, _) in &mods {
        changed |= configure(root, dir, separator(), false)?;
    }
    for (dir, assets) in &mods {
        preflight(settings, dir, assets)?;
    }
    for (dir, assets) in &mods {
        for (name, _) in *assets {
            match std::fs::remove_file(dir.join(name)) {
                Ok(()) => changed = true,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        // Claude may generate types here. Do not recursively delete those or user files.
        for name in [".claude-plugin", "hooks", "types"] {
            match std::fs::remove_dir(dir.join(name)) {
                Ok(()) => {}
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
                    ) => {}
                Err(e) => return Err(e.into()),
            }
        }
        if dir.exists()
            && std::fs::read_dir(dir)?
                .all(|entry| entry.is_ok_and(|entry| entry.file_name() == ".tasty-owner"))
        {
            std::fs::remove_file(dir.join(".tasty-owner"))?;
            std::fs::remove_dir(dir)?;
        }
    }
    Ok(changed)
}

#[cfg(test)]
mod tests;
