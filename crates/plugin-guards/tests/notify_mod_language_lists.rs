//! Compares the language lists written by hand in the Claude mod with the built-in
//! languages. The mod's TypeScript cannot call the Rust translator, and a language
//! missing there falls back to English. The check only sees that a text exists, not
//! whether its translation is right.

use std::collections::BTreeSet;

const MOD_DIR: &str = "crates/tasty-plugin-claude/src/notify_mod/claude";

fn read(rel: &str) -> String {
    let path = plugin_guards::repo_root().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn builtin() -> BTreeSet<String> {
    tasty_i18n::BUILTIN_CODES
        .iter()
        .map(std::string::ToString::to_string)
        .collect()
}

/// Keys of the object literal that `opening` starts. Only one key per line is read; any
/// other form fails instead of being skipped.
fn object_keys(source: &str, opening: &str) -> BTreeSet<String> {
    let start = source
        .find(opening)
        .unwrap_or_else(|| panic!("`{opening}` not found"));
    let body = &source[start + opening.len()..];
    let end = body
        .find("};")
        .unwrap_or_else(|| panic!("end of `{opening}` not found"));
    body[..end]
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| {
            let (key, _) = line
                .split_once(':')
                .unwrap_or_else(|| panic!("cannot read a line in `{opening}`: {line}"));
            key.trim_matches(['\'', '"']).to_string()
        })
        .collect()
}

/// Languages whose text the heading test expects. `unknown` is the input that checks the
/// fallback to English.
fn expected_heading_codes(test_source: &str) -> BTreeSet<String> {
    const CALL: &str = "expect(heading('";
    test_source
        .match_indices(CALL)
        .filter_map(|(at, _)| {
            let rest = &test_source[at + CALL.len()..];
            let (code, after) = rest.split_once("')")?;
            after.starts_with(").toContain(").then(|| code.to_string())
        })
        .collect()
}

fn assert_same(file: &str, what: &str, found: &BTreeSet<String>) {
    let want = builtin();
    assert_eq!(
        found,
        &want,
        "the {what} list in {file} differs from BUILTIN_CODES. A missing language shows English without notice.\nmissing: {:?}\nextra: {:?}",
        want.difference(found).collect::<Vec<_>>(),
        found.difference(&want).collect::<Vec<_>>()
    );
}

#[test]
fn notify_heading_covers_every_builtin_language() {
    let file = format!("{MOD_DIR}/hooks/messages.ts");
    let keys = object_keys(&read(&file), "const messages = {");
    assert_same(&file, "messages", &keys);
}

#[test]
fn notify_gate_reason_covers_every_builtin_language() {
    let file = format!("{MOD_DIR}/gate/hooks/register.ts");
    let keys = object_keys(&read(&file), "const duplicates: Record<string, string> = {");
    assert_same(&file, "duplicates", &keys);
}

#[test]
fn notify_heading_test_expects_every_builtin_language() {
    let file = format!("{MOD_DIR}/tests/notify.test.ts");
    assert_same(
        &file,
        "expected heading",
        &expected_heading_codes(&read(&file)),
    );
}

#[test]
fn the_readers_take_the_named_object_and_the_expected_calls_only() {
    let source = "const other = {\n  xx: 'no',\n};\nconst messages = {\n  en: 'a: b',\n  'zh-Hans': \"c\",\n};\nconst after = {\n  yy: 'no',\n};\n";
    assert_eq!(
        object_keys(source, "const messages = {"),
        BTreeSet::from(["en".to_string(), "zh-Hans".to_string()])
    );
    let test = "expect(heading('en')).toContain('x');\nexpect(heading('unknown')).toBe(heading('en'));\nexpect(heading('zh-Hans')).toContain('y');\n";
    assert_eq!(
        expected_heading_codes(test),
        BTreeSet::from(["en".to_string(), "zh-Hans".to_string()])
    );
}

#[test]
#[should_panic(expected = "cannot read a line")]
fn a_line_without_a_key_fails_instead_of_being_skipped() {
    object_keys("const m = {\n  ...base\n};\n", "const m = {");
}
