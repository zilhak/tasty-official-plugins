//! Parse interactive native launches without reinterpreting prompts as shell code.

#[derive(Debug, Default, PartialEq)]
pub(crate) struct NativeArgs {
    pub tui: Vec<String>,
    pub policy: Vec<String>,
    pub restore: Vec<String>,
}

/// None leaves utility commands and explicit remote connections to the native CLI.
/// Unsupported interactive options fail visibly instead of silently losing their semantics.
pub(crate) fn parse(args: &[String]) -> Result<Option<NativeArgs>, String> {
    let commands = [
        "exec",
        "e",
        "review",
        "login",
        "logout",
        "mcp",
        "mcp-server",
        "app-server",
        "completion",
        "sandbox",
        "debug",
        "apply",
        "a",
        "cloud",
        "features",
        "help",
    ];
    let mut out = NativeArgs::default();
    let mut positional = false;
    let mut resume = false;
    let mut has_position = false;
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if positional {
            out.tui.push(arg.clone());
            i += 1;
            continue;
        }
        let (key, inline) = arg
            .split_once('=')
            .map_or((arg.as_str(), None), |(k, v)| (k, Some(v)));
        if matches!(
            key,
            "--help" | "-h" | "--version" | "-V" | "--remote" | "--remote-auth-token-env"
        ) || (!has_position && commands.contains(&key))
        {
            return Ok(None);
        }
        if !has_position && key == "resume" {
            has_position = true;
            resume = true;
            out.tui.push(arg.clone());
            i += 1;
            continue;
        }
        if key == "--" {
            positional = true;
            out.tui.push(arg.clone());
            i += 1;
            continue;
        }
        let policy = match key {
            "-a" | "--ask-for-approval" => Some("-a"),
            "-s" | "--sandbox" => Some("-s"),
            "-m" | "--model" => Some("-m"),
            _ => None,
        };
        if let Some(canonical) = policy {
            let value = if let Some(value) = inline {
                value.to_owned()
            } else {
                i += 1;
                args.get(i)
                    .ok_or_else(|| format!("missing value after {key}"))?
                    .clone()
            };
            if canonical == "-m" {
                out.tui.extend([canonical.to_owned(), value.clone()]);
            }
            out.policy.extend([canonical.to_owned(), value.clone()]);
            out.restore.extend([canonical.to_owned(), value]);
        } else if key == "--dangerously-bypass-approvals-and-sandbox" {
            out.policy.push(arg.clone());
            out.restore.push(arg.clone());
        } else if matches!(key, "--last" | "--all" | "--include-non-interactive") && resume {
            out.tui.push(arg.clone());
        } else if matches!(key, "--no-alt-screen" | "--search") {
            out.tui.push(arg.clone());
            out.restore.push(arg.clone());
        } else if matches!(key, "-C" | "--cd") {
            let value = if let Some(value) = inline {
                value.to_owned()
            } else {
                i += 1;
                args.get(i)
                    .ok_or_else(|| format!("missing value after {key}"))?
                    .clone()
            };
            let absolute = std::path::absolute(&value)
                .map_err(|e| e.to_string())?
                .to_string_lossy()
                .into_owned();
            out.tui.extend(["-C".into(), absolute.clone()]);
            out.restore.extend(["-C".into(), absolute]);
        } else if key.starts_with('-') || key == "fork" {
            return Err(format!(
                "Tasty cannot preserve the interactive option {key}"
            ));
        } else {
            has_position = true;
            out.tui.push(arg.clone());
        }
        i += 1;
    }
    for flag in [
        "-a",
        "-s",
        "-m",
        "--dangerously-bypass-approvals-and-sandbox",
    ] {
        if out.policy.iter().filter(|a| a.as_str() == flag).count() > 1 {
            return Err(format!("duplicate option {flag}"));
        }
    }
    if out
        .policy
        .iter()
        .any(|a| a == "--dangerously-bypass-approvals-and-sandbox")
        && out.policy.iter().any(|a| a == "-a" || a == "-s")
    {
        return Err("conflicting permission options".into());
    }
    super::command::validate_tui_args(&out.policy)?;
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|v| (*v).into()).collect()
    }
    #[test]
    fn picker_keeps_explicit_policy_without_a_guessed_thread() {
        let args = parse(&s(&[
            "--dangerously-bypass-approvals-and-sandbox",
            "resume",
            "--all",
        ]))
        .unwrap()
        .unwrap();
        assert_eq!(args.tui, s(&["resume", "--all"]));
        assert_eq!(
            args.policy,
            s(&["--dangerously-bypass-approvals-and-sandbox"])
        );
        assert_eq!(args.restore, args.policy);
    }
    #[test]
    fn prompts_are_not_options_and_are_not_replayed_on_restore() {
        let args = parse(&s(&["-m", "gpt-test", "--", "fix this", "--sandbox"]))
            .unwrap()
            .unwrap();
        assert_eq!(
            args.tui,
            s(&["-m", "gpt-test", "--", "fix this", "--sandbox"])
        );
        assert_eq!(args.restore, s(&["-m", "gpt-test"]));
        assert!(
            parse(&s(&["exec", "--sandbox", "read-only", "test"]))
                .unwrap()
                .is_none()
        );
        assert!(
            parse(&s(&["--remote=unix:///tmp/explicit", "resume"]))
                .unwrap()
                .is_none()
        );
        assert!(parse(&s(&["-p", "custom"])).is_err());
    }
}
