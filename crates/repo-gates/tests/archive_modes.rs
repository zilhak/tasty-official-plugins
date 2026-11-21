//! The file modes of the plugin archives (docs/ci.md, "Packaging"): stage gives folders and
//! the executable 755 and every other file 644, pack writes the archives under umask 022,
//! and pack checks every entry against those modes. The checks run the functions of
//! scripts/lib/archive-modes.sh with a fake tar, scripts/package-plugin.sh pack on hand-made
//! stages with the real tar, and the stage step of a copy of the packaging with a stub
//! scripts/check.sh in place of cargo.

#![cfg(unix)]

mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const ID: &str = "com.tasty.claude";
const EXE: &str = "tasty-plugin-claude";
const LIB: &str = "scripts/lib/archive-modes.sh";

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn set_mode(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .unwrap_or_else(|e| panic!("chmod {}: {e}", path.display()));
}

fn mode_of(path: &Path) -> u32 {
    fs::metadata(path).expect("metadata").permissions().mode() & 0o7777
}

fn write(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("mkdir");
    }
    fs::write(path, body).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}

/// The real tar, found before a wrapper goes in front of it on PATH.
fn real_tar() -> String {
    let out = Command::new("bash")
        .args(["-c", "command -v tar"])
        .output()
        .expect("run bash");
    let path = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    assert!(out.status.success() && !path.is_empty(), "no tar on PATH");
    path
}

fn path_with(bin: &Path) -> String {
    format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

// ── T1: verify_archive_modes with a fake tar ─────────────────────────────────

/// A verbose listing line as GNU tar prints it.
fn gnu(mode: &str, name: &str) -> String {
    format!("{mode} user/group 0 2026-11-22 04:36 {name}")
}

/// A verbose listing line as bsdtar prints it.
fn bsd(mode: &str, name: &str) -> String {
    format!("{mode}  0 user group 0 Nov 22 04:36 {name}")
}

type Line = fn(&str, &str) -> String;

/// The entries of a good archive: names and the modes they need.
fn good(exe: &str) -> Vec<(String, &'static str)> {
    vec![
        (format!("{ID}/"), "drwxr-xr-x"),
        (format!("{ID}/{exe}"), "-rwxr-xr-x"),
        (format!("{ID}/tasty-plugin.toml"), "-rw-r--r--"),
        (format!("{ID}/lang/"), "drwxr-xr-x"),
        (format!("{ID}/lang/a b.toml"), "-rw-r--r--"),
        (format!("{ID}/LICENSES/"), "drwxr-xr-x"),
        (
            format!("{ID}/LICENSES/Ubuntu-Ubuntu-font-1.0.txt"),
            "-rw-r--r--",
        ),
    ]
}

/// Runs verify_archive_modes inside an if, as a caller with set -e off would, on <names>
/// and the verbose <lines> that a fake tar prints before it exits with <rc>. Returns
/// whether it passed and its stderr.
fn verify_with(names: &[String], lines: &[String], rc: i32, exe: &str) -> (bool, String) {
    let tmp = tempfile::tempdir().expect("temp dir");
    let bin = tmp.path().join("bin");
    support::write_executable(
        &bin.join("tar"),
        "#!/usr/bin/env bash\ncat \"$FAKE_LONG\"\nexit \"$FAKE_RC\"\n",
    )
    .expect("write fake tar");
    let long = tmp.path().join("long.txt");
    write(&long, &format!("{}\n", lines.join("\n")));
    let out = Command::new("bash")
        .current_dir(support::repo_root())
        .args([
            "-c",
            ". \"$1\"; if verify_archive_modes x.tar.gz \"$NAMES\" \"$2\" \"$3\" x.tar.gz; then echo PASS; else echo FAIL; fi",
            "_",
            LIB,
            ID,
            exe,
        ])
        .env("PATH", path_with(&bin))
        .env("NAMES", names.join("\n"))
        .env("FAKE_LONG", &long)
        .env("FAKE_RC", rc.to_string())
        .output()
        .expect("run bash");
    let verdict = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    assert!(
        verdict == "PASS" || verdict == "FAIL",
        "{verdict:?} {}",
        stderr(&out)
    );
    (verdict == "PASS", stderr(&out))
}

/// Checks the entries with names and lines made by <line>.
fn verify(entries: &[(String, String)], line: Line, exe: &str) -> (bool, String) {
    let names: Vec<String> = entries.iter().map(|(n, _)| n.clone()).collect();
    let lines: Vec<String> = entries.iter().map(|(n, m)| line(m, n)).collect();
    verify_with(&names, &lines, 0, exe)
}

fn owned(entries: Vec<(String, &str)>) -> Vec<(String, String)> {
    entries
        .into_iter()
        .map(|(n, m)| (n, m.to_owned()))
        .collect()
}

/// <entries> with the mode of <name> replaced.
fn with_mode(mut entries: Vec<(String, String)>, name: &str, mode: &str) -> Vec<(String, String)> {
    let e = entries
        .iter_mut()
        .find(|(n, _)| n == name)
        .unwrap_or_else(|| panic!("no entry {name}"));
    e.1 = mode.to_owned();
    entries
}

#[test]
fn good_listings_pass_in_both_tar_formats() {
    for line in [gnu as Line, bsd] {
        let (ok, err) = verify(&owned(good(EXE)), line, EXE);
        assert!(ok, "{err}");
        let mut windows = owned(good("tasty-plugin-claude.exe"));
        windows.push((format!("{ID}/tasty_plugin_claude.pdb"), "-rw-r--r--".into()));
        let (ok, err) = verify(&windows, line, "tasty-plugin-claude.exe");
        assert!(ok, "{err}");
    }
}

#[test]
fn a_wrong_mode_is_a_mode_error() {
    let lic = format!("{ID}/LICENSES/Ubuntu-Ubuntu-font-1.0.txt");
    let cases = [
        (lic.clone(), "-rwxr-xr-x", "-rw-r--r--"),
        (
            format!("{ID}/tasty-plugin.toml"),
            "-rw-rw-r--",
            "-rw-r--r--",
        ),
        (format!("{ID}/lang/"), "drwxrwxr-x", "drwxr-xr-x"),
        (format!("{ID}/{EXE}"), "-rw-r--r--", "-rwxr-xr-x"),
    ];
    for line in [gnu as Line, bsd] {
        for (name, got, want) in &cases {
            let entries = with_mode(owned(good(EXE)), name, got);
            let (ok, err) = verify(&entries, line, EXE);
            assert!(!ok, "{name} {got} passed");
            assert!(
                err.contains(&format!("{name} has mode {got}; expected {want}")),
                "{err}"
            );
        }
    }
}

#[test]
fn a_missing_entry_command_fails() {
    let entries: Vec<(String, String)> = owned(good(EXE))
        .into_iter()
        .filter(|(n, _)| n != &format!("{ID}/{EXE}"))
        .collect();
    let (ok, err) = verify(&entries, gnu, EXE);
    assert!(!ok);
    assert!(
        err.contains(&format!(
            "{ID}/{EXE} (the entry command) is not in the archive"
        )),
        "{err}"
    );

    // On Windows the entry command is the .exe; a file without the suffix is not it.
    let entries = with_mode(owned(good(EXE)), &format!("{ID}/{EXE}"), "-rw-r--r--");
    let (ok, err) = verify(&entries, gnu, "tasty-plugin-claude.exe");
    assert!(!ok);
    assert!(
        err.contains(&format!(
            "{ID}/tasty-plugin-claude.exe (the entry command) is not in the archive"
        )),
        "{err}"
    );
}

#[test]
fn links_are_kind_or_link_errors_not_mode_errors() {
    let link = format!("{ID}/link");
    let cases = [
        // A symbolic link and a GNU hard link carry their kind letter.
        (
            gnu("lrwxrwxrwx", &format!("{link} -> LICENSE")),
            "is not a regular file or folder",
        ),
        (
            gnu("hrw-r--r--", &format!("{link} link to {ID}/LICENSE")),
            "is not a regular file or folder",
        ),
        // bsdtar shows a hard link whose header holds a file type as a plain file. Its target
        // ends with " " and the name, so only the link mark gives it away.
        (
            bsd(
                "-rw-r--r--",
                &format!("{link} link to {ID}/lang/alias {link}"),
            ),
            "does not end with its name (link)",
        ),
    ];
    for (line, want) in cases {
        let mut names: Vec<String> = good(EXE).into_iter().map(|(n, _)| n).collect();
        let mut lines: Vec<String> = good(EXE).into_iter().map(|(n, m)| gnu(m, &n)).collect();
        names.push(link.clone());
        lines.push(line.clone());
        let (ok, err) = verify_with(&names, &lines, 0, EXE);
        assert!(!ok, "{line} passed");
        assert!(err.contains(&format!("{link} {want}")), "{err}");
        assert!(!err.contains("has mode"), "{err}");
    }
}

#[test]
fn a_name_that_ends_like_the_executable_is_not_the_executable() {
    let alias = format!("{ID}/lang/alias {ID}/{EXE}");
    let entries: Vec<(String, String)> = owned(good(EXE))
        .into_iter()
        .filter(|(n, _)| n != &format!("{ID}/{EXE}"))
        .chain([(alias.clone(), "-rwxr-xr-x".to_owned())])
        .collect();
    for line in [gnu as Line, bsd] {
        let (ok, err) = verify(&entries, line, EXE);
        assert!(!ok);
        assert!(
            err.contains(&format!("{alias} has mode -rwxr-xr-x")),
            "{err}"
        );
        assert!(
            err.contains("(the entry command) is not in the archive"),
            "{err}"
        );
    }
}

#[test]
fn a_listing_with_another_line_count_fails() {
    let names: Vec<String> = good(EXE).into_iter().map(|(n, _)| n).collect();
    let mut lines: Vec<String> = good(EXE).into_iter().map(|(n, m)| gnu(m, &n)).collect();
    lines.pop();
    let (ok, err) = verify_with(&names, &lines, 0, EXE);
    assert!(!ok);
    assert!(
        err.contains("cannot read the archive listing (7 names, 6 long lines)"),
        "{err}"
    );
}

/// The fake tar prints the whole good listing and then fails, so only the exit status
/// check can tell. The exit code is 7, so a message that always says 1 is caught too.
#[test]
fn a_failed_verbose_listing_fails_with_its_exit_code() {
    let names: Vec<String> = good(EXE).into_iter().map(|(n, _)| n).collect();
    let lines: Vec<String> = good(EXE).into_iter().map(|(n, m)| gnu(m, &n)).collect();
    let (ok, err) = verify_with(&names, &lines, 7, EXE);
    assert!(!ok);
    assert!(err.contains("x.tar.gz: tar -tvzf failed (exit 7)"), "{err}");
}

// ── T2: pack on hand-made stages with the real tar ───────────────────────────

/// A stage for one plugin under <out>/<platform>/stage/<id>/ with the given manifest and
/// files, all at the modes the rule gives.
struct Stage {
    out: tempfile::TempDir,
    platform: &'static str,
}

impl Stage {
    fn new(platform: &'static str, manifest: &str, exe: &str, extra: &[(&str, u32)]) -> Self {
        let out = tempfile::tempdir().expect("temp dir");
        let s = Stage { out, platform };
        let dir = s.dir();
        let root = support::repo_root();
        write(&dir.join("tasty-plugin.toml"), manifest);
        for f in ["LICENSE", "THIRD_PARTY_LICENSES.md"] {
            fs::copy(root.join(f), dir.join(f)).expect("copy notice");
        }
        fs::create_dir_all(dir.join("LICENSES")).expect("mkdir");
        for e in fs::read_dir(root.join("LICENSES")).expect("read LICENSES") {
            let e = e.expect("entry");
            fs::copy(e.path(), dir.join("LICENSES").join(e.file_name())).expect("copy license");
        }
        write(&dir.join("lang/en.toml"), "[x]\n");
        write(&dir.join("lang/a b.toml"), "[x]\n");
        write(&dir.join(exe), "stub");
        for (rel, _) in extra {
            write(&dir.join(rel), "stub");
        }
        s.set_rule_modes(exe);
        for (rel, mode) in extra {
            set_mode(&dir.join(rel), *mode);
        }
        s
    }

    fn dir(&self) -> PathBuf {
        self.out.path().join(self.platform).join("stage").join(ID)
    }

    fn set_rule_modes(&self, exe: &str) {
        fn walk(p: &Path) {
            if p.is_dir() {
                set_mode(p, 0o755);
                for e in fs::read_dir(p).expect("read dir") {
                    walk(&e.expect("entry").path());
                }
            } else {
                set_mode(p, 0o644);
            }
        }
        walk(&self.dir());
        set_mode(&self.dir().join(exe), 0o755);
    }

    /// Runs pack, with <bin> first on PATH when given, under <umask>.
    fn pack(&self, bin: Option<&Path>, umask: &str) -> Output {
        let mut cmd = Command::new("bash");
        cmd.current_dir(support::repo_root()).args([
            "-c",
            "umask \"$1\" && exec bash scripts/package-plugin.sh pack --platform \"$2\" --out \"$3\"",
            "_",
            umask,
            self.platform,
            &self.out.path().to_string_lossy(),
        ]);
        if let Some(bin) = bin {
            cmd.env("PATH", path_with(bin));
        }
        cmd.output().expect("run pack")
    }
}

fn claude_manifest() -> String {
    fs::read_to_string(support::repo_file(
        "crates/tasty-plugin-claude/tasty-plugin.toml",
    ))
    .expect("read manifest")
}

/// The claude manifest with another table right before [entry] that has its own command,
/// which a reader that does not tell the tables apart would take.
fn manifest_with_a_command_before_entry() -> String {
    let m = claude_manifest();
    assert!(
        m.contains("\n[entry]\n"),
        "the manifest has no [entry] table"
    );
    m.replacen(
        "\n[entry]\n",
        "\n[meta]\ncommand = \"unrelated-command\"\n\n[entry]\n",
        1,
    )
}

#[test]
fn pack_passes_a_good_linux_stage_and_fails_a_wrong_mode() {
    let s = Stage::new("linux-x64", &claude_manifest(), EXE, &[]);
    let out = s.pack(None, "022");
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("archive umask 0022"),
        "{}",
        stderr(&out)
    );

    let s = Stage::new("linux-x64", &claude_manifest(), EXE, &[]);
    set_mode(&s.dir().join("LICENSES/Ubuntu-Ubuntu-font-1.0.txt"), 0o755);
    let out = s.pack(None, "022");
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("LICENSES/Ubuntu-Ubuntu-font-1.0.txt has mode -rwxr-xr-x"),
        "{}",
        stderr(&out)
    );

    let s = Stage::new("linux-x64", &claude_manifest(), EXE, &[]);
    fs::remove_file(s.dir().join(EXE)).expect("remove exe");
    let out = s.pack(None, "022");
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains(&format!(
            "{ID}/{EXE} (the entry command) is not in the archive"
        )),
        "{}",
        stderr(&out)
    );
}

/// Under a caller umask of 027, tar -czf must still run under 022. The member modes come
/// from the stage on Unix either way, so only the umask that tar sees tells.
#[test]
fn pack_writes_archives_under_umask_022() {
    let s = Stage::new("linux-x64", &claude_manifest(), EXE, &[]);
    let bin = s.out.path().join("bin");
    let log = s.out.path().join("umask.log");
    support::write_executable(
        &bin.join("tar"),
        format!(
            "#!/usr/bin/env bash\nfor a in \"$@\"; do [ \"$a\" = -czf ] && umask >> '{}'; done\nexec '{}' \"$@\"\n",
            log.display(),
            real_tar()
        ),
    )
    .expect("write tar wrapper");
    let out = s.pack(Some(&bin), "027");
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("caller umask 0027"),
        "{}",
        stderr(&out)
    );
    assert_eq!(
        fs::read_to_string(&log).expect("read umask log").trim(),
        "0022"
    );
}

/// tar -tzf prints the whole good list and then fails: pack stops with exit 1 and names
/// the command and its exit code, 7.
#[test]
fn pack_stops_when_the_name_listing_fails() {
    let s = Stage::new("linux-x64", &claude_manifest(), EXE, &[]);
    let bin = s.out.path().join("bin");
    support::write_executable(
        &bin.join("tar"),
        format!(
            "#!/usr/bin/env bash\nif [ \"$1\" = -tzf ]; then '{tar}' \"$@\"; exit 7; fi\nexec '{tar}' \"$@\"\n",
            tar = real_tar()
        ),
    )
    .expect("write tar wrapper");
    let out = s.pack(Some(&bin), "022");
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("-linux-x64.tar.gz: tar -tzf failed (exit 7)"),
        "{}",
        stderr(&out)
    );
}

/// A windows-* archive needs <command>.exe, where <command> is the one in [entry], not
/// in another table.
#[test]
fn pack_reads_the_entry_command_and_adds_exe_on_windows() {
    let manifest = manifest_with_a_command_before_entry();
    let s = Stage::new(
        "windows-x64",
        &manifest,
        "tasty-plugin-claude.exe",
        &[("tasty_plugin_claude.pdb", 0o644)],
    );
    let out = s.pack(None, "022");
    assert!(out.status.success(), "{}", stderr(&out));

    let s = Stage::new(
        "windows-x64",
        &manifest,
        EXE,
        &[("tasty_plugin_claude.pdb", 0o644)],
    );
    set_mode(&s.dir().join(EXE), 0o644);
    let out = s.pack(None, "022");
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains(&format!(
            "{ID}/tasty-plugin-claude.exe (the entry command) is not in the archive"
        )),
        "{}",
        stderr(&out)
    );
}

// ── T3: normalize_stage_modes ────────────────────────────────────────────────

#[test]
fn normalize_gives_folders_and_the_executable_755_and_files_644() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join(ID);
    let files = [
        ("tasty-plugin.toml", 0o600),
        ("lang/en.toml", 0o664),
        ("LICENSES/Ubuntu-Ubuntu-font-1.0.txt", 0o4755),
        (EXE, 0o644),
    ];
    for (rel, mode) in files {
        write(&dir.join(rel), "x");
        set_mode(&dir.join(rel), mode);
    }
    set_mode(&dir.join("lang"), 0o2775);
    set_mode(&dir.join("LICENSES"), 0o775);
    let out = Command::new("bash")
        .current_dir(support::repo_root())
        .args([
            "-c",
            ". \"$1\" && normalize_stage_modes \"$2\" \"$3\"",
            "_",
            LIB,
            &dir.to_string_lossy(),
            EXE,
        ])
        .output()
        .expect("run bash");
    assert!(out.status.success(), "{}", stderr(&out));
    for (rel, want) in [
        ("", 0o755),
        ("lang", 0o755),
        ("LICENSES", 0o755),
        (EXE, 0o755),
        ("tasty-plugin.toml", 0o644),
        ("lang/en.toml", 0o644),
        ("LICENSES/Ubuntu-Ubuntu-font-1.0.txt", 0o644),
    ] {
        assert_eq!(mode_of(&dir.join(rel)), want, "{rel:?}");
    }
}

// ── T4: the stage path of the packaging, with a stub cargo ──────────────────

/// A stand-in for scripts/check.sh: `build` writes an executable and, with a message
/// format, prints the compiler-artifact message cargo would; `list-bins` names the crate.
const STUB_CHECK_SH: &str = r#"#!/usr/bin/env bash
set -euo pipefail
case "$1" in
  build)
    exe="$PWD/target/stub/tasty-plugin-claude"
    mkdir -p "${exe%/*}"
    printf 'stub' > "$exe"
    chmod 755 "$exe"
    for a in "$@"; do
      if [[ "$a" == --message-format=* ]]; then
        printf '{"reason":"compiler-artifact","target":{"name":"tasty-plugin-claude"},"executable":"%s"}\n' "$exe"
      fi
    done
    ;;
  list-bins) echo tasty-plugin-claude ;;
  *) exit 2 ;;
esac
"#;

/// A copy of the packaging scripts, the notice set, and the claude crate's manifest and
/// lang/ with modes a umask 002 checkout and the old index give (LICENSE 664, the Ubuntu
/// text 755), and the stub check.sh. <edit> changes the copy of package-plugin.sh.
fn stub_repo(edit: impl Fn(String) -> String) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("temp dir");
    let r = tmp.path();
    let root = support::repo_root();
    let pkg = fs::read_to_string(root.join("scripts/package-plugin.sh")).expect("read script");
    support::write_executable(&r.join("scripts/package-plugin.sh"), edit(pkg)).expect("write");
    support::write_executable(&r.join("scripts/check.sh"), STUB_CHECK_SH).expect("write");
    for lib in ["notice-set.sh", "plugin-build.sh", "archive-modes.sh"] {
        let rel = format!("scripts/lib/{lib}");
        write(
            &r.join(&rel),
            &fs::read_to_string(root.join(&rel)).expect("read lib"),
        );
    }
    for f in ["LICENSE", "THIRD_PARTY_LICENSES.md"] {
        write(
            &r.join(f),
            &fs::read_to_string(root.join(f)).expect("read notice"),
        );
        set_mode(&r.join(f), 0o664);
    }
    for e in fs::read_dir(root.join("LICENSES")).expect("read LICENSES") {
        let e = e.expect("entry");
        let to = r.join("LICENSES").join(e.file_name());
        write(&to, &fs::read_to_string(e.path()).expect("read license"));
        set_mode(&to, 0o664);
    }
    set_mode(&r.join("LICENSES/Ubuntu-Ubuntu-font-1.0.txt"), 0o755);
    let crate_dir = root.join("crates/tasty-plugin-claude");
    let to = r.join("crates/tasty-plugin-claude");
    write(
        &to.join("tasty-plugin.toml"),
        &fs::read_to_string(crate_dir.join("tasty-plugin.toml")).expect("read manifest"),
    );
    for e in fs::read_dir(crate_dir.join("lang")).expect("read lang") {
        let e = e.expect("entry");
        let f = to.join("lang").join(e.file_name());
        write(&f, &fs::read_to_string(e.path()).expect("read lang file"));
        set_mode(&f, 0o664);
    }
    set_mode(&to.join("lang"), 0o775);
    tmp
}

fn package_all(repo: &Path, out: &Path) -> Output {
    Command::new("bash")
        .current_dir(repo)
        .args([
            "-c",
            "umask 002 && exec bash scripts/package-plugin.sh all --all --platform linux-x64 --target x86_64-unknown-linux-gnu --out \"$1\"",
            "_",
            &out.to_string_lossy(),
        ])
        .output()
        .expect("run package-plugin.sh")
}

#[test]
fn stage_normalizes_the_modes_and_pack_fails_without_it() {
    let repo = stub_repo(|s| s);
    let out = repo.path().join("out");
    let run = package_all(repo.path(), &out);
    assert!(run.status.success(), "{}", stderr(&run));
    let stage = out.join("linux-x64/stage").join(ID);
    for (rel, want) in [
        ("", 0o755),
        ("lang", 0o755),
        ("LICENSES", 0o755),
        (EXE, 0o755),
        ("LICENSE", 0o644),
        ("tasty-plugin.toml", 0o644),
        ("lang/en.toml", 0o644),
        ("LICENSES/Ubuntu-Ubuntu-font-1.0.txt", 0o644),
    ] {
        assert_eq!(mode_of(&stage.join(rel)), want, "{rel:?}");
    }

    // Without the call, the modes of the sources and the umask reach the archive.
    let call = "    normalize_stage_modes \"$STAGE_DIR/$id\" \"$(basename \"$exe\")\" ||\n      die \"cannot set the file modes of $id\"\n";
    let repo = stub_repo(|s| {
        assert!(s.contains(call), "the stage call moved");
        s.replace(call, "")
    });
    let out = repo.path().join("out");
    let run = package_all(repo.path(), &out);
    assert!(!run.status.success(), "pack passed without the stage call");
    assert!(
        stderr(&run).contains("LICENSES/Ubuntu-Ubuntu-font-1.0.txt has mode -rwxr-xr-x"),
        "{}",
        stderr(&run)
    );
}
