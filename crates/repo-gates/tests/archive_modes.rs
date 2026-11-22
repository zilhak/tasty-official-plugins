//! The file modes and owners of the plugin archives (docs/ci.md, "Packaging"): stage gives
//! folders and the executable 755 and every other file 644, pack writes USTAR archives under
//! umask 022 with uid 0, gid 0, and no names, and pack checks every entry against those
//! rules. The checks run the functions of scripts/lib/archive-modes.sh with a fake tar and
//! on hand-made USTAR bytes, scripts/package-plugin.sh pack on hand-made stages with the
//! real tar, and the stage step of a copy of the packaging with a stub scripts/check.sh in
//! place of cargo. Names and ids in the test data are made up.

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

/// A folder entry without a trailing slash can stand at the path of the entry command.
/// Both tars list it as `drwxr-xr-x … <id>/<command>`, so the name alone does not tell.
#[test]
fn a_folder_at_the_entry_command_path_is_not_the_entry_command() {
    let exe = format!("{ID}/{EXE}");
    let entries: Vec<(String, String)> = owned(good(EXE))
        .into_iter()
        .map(|(n, m)| {
            if n == exe {
                (n, "drwxr-xr-x".to_owned())
            } else {
                (n, m)
            }
        })
        .collect();
    for line in [gnu as Line, bsd] {
        let (ok, err) = verify(&entries, line, EXE);
        assert!(!ok, "a folder at {exe} passed");
        assert!(
            err.contains(&format!("{exe} is a folder, not the entry command")),
            "{err}"
        );
        assert!(
            err.contains("(the entry command) is not in the archive"),
            "{err}"
        );
    }
}

/// One USTAR header block for a folder entry named <name>, mode 755, with no trailing
/// slash added.
fn ustar_folder(name: &str) -> [u8; 512] {
    let mut h = [0u8; 512];
    let put = |h: &mut [u8; 512], at: usize, s: &[u8]| h[at..at + s.len()].copy_from_slice(s);
    assert!(name.len() < 100, "name too long for a plain USTAR header");
    put(&mut h, 0, name.as_bytes());
    put(&mut h, 100, b"0000755\0");
    put(&mut h, 108, b"0000000\0");
    put(&mut h, 116, b"0000000\0");
    put(&mut h, 124, b"00000000000\0");
    put(&mut h, 136, b"00000000000\0");
    put(&mut h, 148, b"        ");
    h[156] = b'5';
    put(&mut h, 257, b"ustar\0");
    put(&mut h, 263, b"00");
    let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
    put(&mut h, 148, format!("{sum:06o}\0 ").as_bytes());
    h
}

/// The same with a real archive and the real tar: <id>/ and a folder <id>/<command>.
#[test]
fn a_real_archive_with_a_folder_at_the_entry_command_path_fails() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let tar_path = tmp.path().join("x.tar");
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&ustar_folder(&format!("{ID}/")));
    bytes.extend_from_slice(&ustar_folder(&format!("{ID}/{EXE}")));
    bytes.extend_from_slice(&[0u8; 1024]);
    fs::write(&tar_path, bytes).expect("write tar");
    let gz = Command::new("gzip")
        .args(["-n", "-f"])
        .arg(&tar_path)
        .status()
        .expect("run gzip");
    assert!(gz.success());
    let archive = tmp.path().join("x.tar.gz");
    let out = Command::new("bash")
        .current_dir(support::repo_root())
        .args([
            "-c",
            ". \"$1\"; names=$(LC_ALL=C tar -tzf \"$2\") || exit 3; \
             if verify_archive_modes \"$2\" \"$names\" \"$3\" \"$4\" x.tar.gz; then echo PASS; else echo FAIL; fi",
            "_",
            LIB,
            &archive.to_string_lossy(),
            ID,
            EXE,
        ])
        .output()
        .expect("run bash");
    let verdict = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    assert_eq!(verdict, "FAIL", "{}", stderr(&out));
    assert!(
        stderr(&out).contains(&format!("{ID}/{EXE} is a folder, not the entry command")),
        "{}",
        stderr(&out)
    );
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
        self.pack_with(bin, umask, &[])
    }

    /// The same with the extra environment <envs>.
    fn pack_with(&self, bin: Option<&Path>, umask: &str, envs: &[(&str, &str)]) -> Output {
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
        cmd.envs(envs.iter().copied());
        cmd.output().expect("run pack")
    }

    fn dist(&self) -> PathBuf {
        self.out.path().join(self.platform).join("dist")
    }

    /// The one archive pack wrote.
    fn archive(&self) -> PathBuf {
        let mut found: Vec<PathBuf> = fs::read_dir(self.dist())
            .expect("read dist")
            .map(|e| e.expect("entry").path())
            .filter(|p| p.to_string_lossy().ends_with(".tar.gz"))
            .collect();
        assert_eq!(found.len(), 1, "{found:?}");
        found.remove(0)
    }

    /// The number of staged entries, the plugin folder included.
    fn entries(&self) -> usize {
        fn walk(p: &Path) -> usize {
            1 + if p.is_dir() {
                fs::read_dir(p)
                    .expect("read dir")
                    .map(|e| walk(&e.expect("entry").path()))
                    .sum()
            } else {
                0
            }
        }
        walk(&self.dir())
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

// ── T1-h: verify_archive_owners on hand-made USTAR bytes ─────────────────────

fn put(h: &mut [u8; 512], at: usize, s: &[u8]) {
    h[at..at + s.len()].copy_from_slice(s);
}

/// Writes the checksum of <h> as GNU tar does.
fn seal(h: &mut [u8; 512]) {
    put(h, 148, b"        ");
    let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
    put(h, 148, format!("{sum:06o}\0 ").as_bytes());
}

/// A sealed USTAR header for <path> with type <kind> and size <size>: uid and gid 0, no
/// names, and numbers as GNU tar writes them. <edit> changes it before it is sealed.
fn header(path: &[u8], kind: u8, size: usize, edit: impl Fn(&mut [u8; 512])) -> [u8; 512] {
    let mut h = [0u8; 512];
    assert!(path.len() <= 100, "path too long for the name field");
    put(&mut h, 0, path);
    put(
        &mut h,
        100,
        if kind == b'5' {
            b"0000755\0"
        } else {
            b"0000644\0"
        },
    );
    put(&mut h, 108, b"0000000\0");
    put(&mut h, 116, b"0000000\0");
    put(&mut h, 124, format!("{size:011o}\0").as_bytes());
    put(&mut h, 136, b"15000000000\0");
    h[156] = kind;
    put(&mut h, 257, b"ustar\0");
    put(&mut h, 263, b"00");
    edit(&mut h);
    seal(&mut h);
    h
}

/// <h> followed by <data> padded to whole blocks.
fn with_data(h: [u8; 512], data: &[u8]) -> Vec<u8> {
    let mut v = h.to_vec();
    v.extend_from_slice(data);
    v.resize(512 + data.len().div_ceil(512) * 512, 0);
    v
}

fn file_edit(path: &str, data: &[u8], edit: impl Fn(&mut [u8; 512])) -> Vec<u8> {
    with_data(header(path.as_bytes(), b'0', data.len(), edit), data)
}

fn file(path: &str, data: &[u8]) -> Vec<u8> {
    file_edit(path, data, |_| {})
}

fn folder(path: &str) -> Vec<u8> {
    with_data(header(path.as_bytes(), b'5', 0, |_| {}), &[])
}

/// The headers of a good archive; the third one is the manifest.
fn good_headers() -> Vec<Vec<u8>> {
    vec![
        folder(&format!("{ID}/")),
        file_edit(&format!("{ID}/{EXE}"), b"stub", |h| {
            put(h, 100, b"0000755\0")
        }),
        file(&format!("{ID}/tasty-plugin.toml"), b"id = 1\n"),
        folder(&format!("{ID}/lang/")),
        file(&format!("{ID}/lang/en.toml"), b"[x]\n"),
    ]
}

/// <headers> with the manifest header (entry 3) replaced by <h>.
fn with_manifest(h: Vec<u8>) -> Vec<Vec<u8>> {
    let mut v = good_headers();
    v[2] = h;
    v
}

/// The manifest header with <edit> applied.
fn manifest_edit(edit: impl Fn(&mut [u8; 512])) -> Vec<Vec<u8>> {
    with_manifest(file_edit(
        &format!("{ID}/tasty-plugin.toml"),
        b"id = 1\n",
        edit,
    ))
}

/// The tar bytes of <headers> with the end marker, two zero blocks.
fn tar_of(headers: &[Vec<u8>]) -> Vec<u8> {
    let mut v: Vec<u8> = headers.concat();
    v.extend_from_slice(&[0u8; 1024]);
    v
}

/// <bytes> compressed by gzip -n into <dir>/x.tar.gz.
fn gz(dir: &Path, bytes: &[u8]) -> PathBuf {
    let tar_path = dir.join("x.tar");
    fs::write(&tar_path, bytes).expect("write tar");
    let st = Command::new("gzip")
        .args(["-n", "-f"])
        .arg(&tar_path)
        .status()
        .expect("run gzip");
    assert!(st.success());
    dir.join("x.tar.gz")
}

/// A list of <n> names: the guard only counts them, so for one check alone the cases give
/// it as many as there are raw headers.
fn names(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("{ID}/entry{i}")).collect()
}

/// Runs verify_archive_owners inside an if on <archive>, with <names> or, when None, the
/// names the real tar lists. Returns whether it passed and its stderr.
fn owners_of(archive: &Path, names: Option<&[String]>) -> (bool, String) {
    let mut cmd = Command::new("bash");
    cmd.current_dir(support::repo_root()).args([
        "-c",
        ". \"$1\"; if [ -n \"${USE_TAR-}\" ]; then NAMES=$(LC_ALL=C tar -tzf \"$2\") || exit 3; fi; \
         if verify_archive_owners \"$2\" \"$NAMES\" x.tar.gz; then echo PASS; else echo FAIL; fi",
        "_",
        LIB,
        &archive.to_string_lossy(),
    ]);
    match names {
        Some(n) => cmd.env("NAMES", n.join("\n")),
        None => cmd.env("USE_TAR", "1"),
    };
    let out = cmd.output().expect("run bash");
    let verdict = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    assert!(
        verdict == "PASS" || verdict == "FAIL",
        "{verdict:?} {}",
        stderr(&out)
    );
    (verdict == "PASS", stderr(&out))
}

/// Checks the tar bytes <bytes> with a list as long as <count>, or the real tar's list.
fn owners(bytes: &[u8], count: Option<usize>) -> (bool, String) {
    let tmp = tempfile::tempdir().expect("temp dir");
    let archive = gz(tmp.path(), bytes);
    match count {
        Some(n) => owners_of(&archive, Some(&names(n))),
        None => owners_of(&archive, None),
    }
}

/// <headers> must fail, with <want> in the error, given a list of one name per header.
fn rejects(case: &str, headers: &[Vec<u8>], want: &str) {
    let (ok, err) = owners(&tar_of(headers), Some(headers.len()));
    assert!(!ok, "{case} passed");
    assert!(err.contains(want), "{case}: {err}");
}

/// h1, h1a, h1n: good archives pass with the real tar's list, with GNU or Apple numbers
/// and with a name that tar lists escaped.
#[test]
fn good_ustar_archives_pass_the_owner_check() {
    let (ok, err) = owners(&tar_of(&good_headers()), None);
    assert!(ok, "h1: {err}");

    let apple: Vec<Vec<u8>> = good_headers()
        .into_iter()
        .map(|mut v| {
            let mut h: [u8; 512] = v[..512].try_into().expect("header");
            let size = std::str::from_utf8(&h[124..135])
                .ok()
                .and_then(|s| u64::from_str_radix(s, 8).ok())
                .expect("size");
            put(
                &mut h,
                100,
                if h[156] == b'5' {
                    b"000755 \0"
                } else {
                    b"000644 \0"
                },
            );
            put(&mut h, 108, b"000000 \0");
            put(&mut h, 116, b"000000 \0");
            put(&mut h, 124, format!("{size:011o} ").as_bytes());
            put(&mut h, 136, b"15000000000 ");
            seal(&mut h);
            v[..512].copy_from_slice(&h);
            v
        })
        .collect();
    let (ok, err) = owners(&tar_of(&apple), None);
    assert!(ok, "h1a: {err}");

    let mut odd = good_headers();
    odd.push(file(&format!("{ID}/lang/a\u{e9}.toml"), b"[x]\n"));
    let (ok, err) = owners(&tar_of(&odd), None);
    assert!(ok, "h1n: {err}");
}

/// h2u, h2g, h2n, h2m change one owner field each; h3 and h5 store names that read as
/// numbers, and h5 hides other ids behind them in a listing.
#[test]
fn each_owner_field_fails_on_its_own() {
    let cases: [(&str, Vec<Vec<u8>>, &str); 6] = [
        (
            "h2u",
            manifest_edit(|h| put(h, 108, b"0000027\0")),
            "entry 3 has uid 23, gid 0, no user name, no group name",
        ),
        (
            "h2g",
            manifest_edit(|h| put(h, 116, b"0000052\0")),
            "entry 3 has uid 0, gid 42, no user name, no group name",
        ),
        (
            "h2n",
            manifest_edit(|h| put(h, 265, b"user")),
            "entry 3 has uid 0, gid 0, a user name (4 bytes), no group name",
        ),
        (
            "h2m",
            manifest_edit(|h| put(h, 297, b"group")),
            "entry 3 has uid 0, gid 0, no user name, a group name (5 bytes)",
        ),
        (
            "h3",
            manifest_edit(|h| {
                put(h, 265, b"0");
                put(h, 297, b"0")
            }),
            "entry 3 has uid 0, gid 0, a user name (1 bytes), a group name (1 bytes)",
        ),
        (
            "h5",
            manifest_edit(|h| {
                put(h, 108, b"0000027\0");
                put(h, 116, b"0000052\0");
                put(h, 265, b"0");
                put(h, 297, b"0")
            }),
            "entry 3 has uid 23, gid 42, a user name (1 bytes), a group name (1 bytes)",
        ),
    ];
    for (case, headers, want) in &cases {
        rejects(case, headers, want);
    }
}

/// h6x, h6g, h6l, h6t: other header types, each a good USTAR header otherwise, with one name
/// per raw header so that the count does not decide. h6m, h6mg, h6v: other magic or version.
#[test]
fn only_ustar_file_and_folder_headers_pass() {
    let pax =
        |kind: u8, data: &[u8]| with_data(header(b"PaxHeader/x", kind, data.len(), |_| {}), data);
    let ext = |h: Vec<u8>| {
        let mut v = good_headers();
        v.insert(2, h);
        v
    };
    rejects(
        "h6x",
        &ext(pax(b'x', b"21 comment=synthetic\n")),
        "entry 3 has type flag byte 120",
    );
    rejects(
        "h6g",
        &ext(pax(b'g', b"21 comment=synthetic\n")),
        "entry 3 has type flag byte 103",
    );
    rejects(
        "h6l",
        &ext(with_data(
            header(b"././@LongLink", b'L', 22, |_| {}),
            b"com.tasty.claude/long\0",
        )),
        "entry 3 has type flag byte 76",
    );
    for (kind, byte) in [(b'2', "50"), (b'1', "49")] {
        rejects(
            "h6t",
            &ext(with_data(
                header(format!("{ID}/link").as_bytes(), kind, 0, |h| {
                    put(h, 157, b"LICENSE")
                }),
                &[],
            )),
            &format!("entry 3 has type flag byte {byte}"),
        );
    }
    rejects(
        "h6m",
        &manifest_edit(|h| put(h, 257, b"ustar ")),
        "entry 3 is not a USTAR header (magic)",
    );
    rejects(
        "h6mg",
        &manifest_edit(|h| put(h, 257, b"ustar  \0")),
        "entry 3 is not a USTAR header (magic)",
    );
    rejects(
        "h6v",
        &manifest_edit(|h| put(h, 263, b"01")),
        "entry 3 is not a USTAR header (version)",
    );
}

/// h6a, h6ad, h6ad2: AppleDouble entries, judged by the last path part that is not empty.
#[test]
fn appledouble_entries_fail() {
    rejects(
        "h6a",
        &with_manifest(file(&format!("{ID}/._tasty-plugin.toml"), b"x")),
        "entry 3 is an AppleDouble entry",
    );
    rejects(
        "h6ad",
        &with_manifest(folder(&format!("{ID}/._lang/"))),
        "entry 3 is an AppleDouble entry",
    );
    rejects(
        "h6ad2",
        &with_manifest(folder(&format!("{ID}/._lang//"))),
        "entry 3 is an AppleDouble entry",
    );
}

/// h7s: a file whose stored path ends with a slash, which Apple's reader takes as a folder
/// without data, so the two readers would walk different headers. h7z: a folder with data.
#[test]
fn a_file_path_with_a_trailing_slash_and_a_folder_with_data_fail() {
    rejects(
        "h7s",
        &with_manifest(file(&format!("{ID}/lang/"), b"data")),
        "entry 3 is a file whose stored path ends with a slash",
    );
    rejects(
        "h7z",
        &with_manifest(with_data(
            header(format!("{ID}/conf/").as_bytes(), b'5', 4, |_| {}),
            b"data",
        )),
        "entry 3 is a folder with size 4",
    );
}

/// h8c: one checksum digit changed to another octal digit. h8b: a base-256 zero. h8f1: a
/// zero after the end of a zero field. h8f2: no digit at all.
#[test]
fn numeric_fields_must_be_plain_octal() {
    let mut bad = good_headers();
    let h = &mut bad[2];
    h[153] = if h[153] == b'7' { b'6' } else { h[153] + 1 };
    rejects("h8c", &bad, "entry 3 has a wrong header checksum");
    rejects(
        "h8b",
        &manifest_edit(|h| put(h, 108, &[0x80, 0, 0, 0, 0, 0, 0, 0])),
        "entry 3 has an unreadable uid field",
    );
    rejects(
        "h8f1",
        &manifest_edit(|h| put(h, 108, b"0\0 0\0\0\0\0")),
        "entry 3 has an unreadable uid field",
    );
    rejects(
        "h8f2",
        &manifest_edit(|h| put(h, 108, b"       \0")),
        "entry 3 has an unreadable uid field",
    );
}

/// h8n: one name more or less than there are headers.
#[test]
fn the_header_count_must_match_the_listing() {
    let bytes = tar_of(&good_headers());
    for n in [4, 6] {
        let (ok, err) = owners(&bytes, Some(n));
        assert!(!ok, "{n} names passed");
        assert!(
            err.contains(&format!(
                "the archive has 5 headers but tar -tzf listed {n} names"
            )),
            "{err}"
        );
    }
}

/// h11: a header after a single zero block, which tar does not list. h12: data after the end
/// marker. h13e: no end marker. h13s: a second zero block of 171 bytes.
#[test]
fn the_archive_must_end_with_two_zero_blocks_and_zero_bytes() {
    let good = good_headers();
    let mut b = good.concat();
    b.extend_from_slice(&[0u8; 512]);
    b.extend_from_slice(&file_edit(&format!("{ID}/hidden"), b"x", |h| {
        put(h, 265, b"user")
    }));
    b.extend_from_slice(&[0u8; 1024]);
    let (ok, err) = owners(&b, Some(good.len()));
    assert!(!ok, "h11 passed");
    assert!(
        err.contains("a header follows a single zero block"),
        "h11: {err}"
    );

    let mut b = tar_of(&good);
    let mut tail = [0u8; 512];
    tail[100] = 1;
    b.extend_from_slice(&tail);
    let (ok, err) = owners(&b, Some(good.len()));
    assert!(!ok, "h12 passed");
    assert!(
        err.contains("nonzero bytes after its two zero blocks"),
        "h12: {err}"
    );

    let (ok, err) = owners(&good.concat(), Some(good.len()));
    assert!(!ok, "h13e passed");
    assert!(err.contains("ends without two zero blocks"), "h13e: {err}");

    let mut b = good.concat();
    b.extend_from_slice(&[0u8; 512 + 171]);
    let (ok, err) = owners(&b, Some(good.len()));
    assert!(!ok, "h13s passed");
    assert!(err.contains("has 171 bytes, not 512"), "h13s: {err}");
}

/// h13g: gzip -dc fails on a stream without its 8-byte trailer although it writes the whole
/// tar, so only the gzip status tells.
#[test]
fn a_failed_gzip_fails_even_when_the_tar_is_whole() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let good = good_headers();
    let archive = gz(tmp.path(), &tar_of(&good));
    let bytes = fs::read(&archive).expect("read archive");
    fs::write(&archive, &bytes[..bytes.len() - 8]).expect("cut archive");
    let (ok, err) = owners_of(&archive, Some(&names(good.len())));
    assert!(!ok, "h13g passed");
    assert!(
        err.contains("x.tar.gz: gzip -dc failed (exit "),
        "h13g: {err}"
    );
    assert!(!err.contains("entry "), "h13g: {err}");
}

/// h9: a stored user or group name never reaches the error, as release logs are public.
#[test]
fn errors_never_print_a_stored_name() {
    for (at, name) in [
        (265, b"synthuser".as_slice()),
        (297, b"synthgroup".as_slice()),
    ] {
        let (ok, err) = owners(
            &tar_of(&manifest_edit(|h| put(h, at, name))),
            Some(good_headers().len()),
        );
        assert!(!ok);
        assert!(err.contains("entry 3 has uid 0, gid 0"), "{err}");
        assert!(!err.contains(std::str::from_utf8(name).unwrap()), "{err}");
    }
}

/// h10: an archive made the way pack made it before, with the tar's default format and
/// no owner options, fails. Which check stops it first depends on the tar.
#[test]
fn an_archive_from_the_old_pack_call_fails() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let stage = tmp.path().join("stage");
    write(&stage.join(ID).join("tasty-plugin.toml"), "id = 1\n");
    let archive = tmp.path().join("old.tar.gz");
    let st = Command::new("tar")
        .arg("-czf")
        .arg(&archive)
        .arg("-C")
        .arg(&stage)
        .arg(ID)
        .env_remove("TAR_OPTIONS")
        .status()
        .expect("run tar");
    assert!(st.success());
    let (ok, err) = owners_of(&archive, None);
    assert!(!ok, "the old archive passed: {err}");
}

// ── T2 owners: pack writes USTAR with owner 0/0 and no names ─────────────────

/// The value of an octal field as the plan reads it: leading blanks, one or more octal
/// digits, then blanks or NULs only.
fn octal(f: &[u8]) -> Option<u64> {
    if f.first().is_some_and(|&b| b >= 0x80) {
        return None;
    }
    let s = f.iter().skip_while(|&&b| b == b' ');
    let digits: Vec<u8> = s
        .clone()
        .take_while(|&&b| (b'0'..=b'7').contains(&b))
        .copied()
        .collect();
    if digits.is_empty() || !s.skip(digits.len()).all(|&b| b == b' ' || b == 0) {
        return None;
    }
    u64::from_str_radix(std::str::from_utf8(&digits).ok()?, 8).ok()
}

/// Reads the gzip'd archive <archive> on its own, apart from the shell function, and
/// returns its stored paths after checking every header: USTAR, a right checksum, type 0
/// or 5, uid and gid 0, NUL names, no AppleDouble entry, no file path with a trailing
/// slash, and an end of two zero blocks with zero bytes after them.
fn read_ustar(archive: &Path) -> Vec<String> {
    let out = Command::new("gzip")
        .arg("-dc")
        .arg(archive)
        .output()
        .expect("run gzip");
    assert!(out.status.success(), "gzip -dc {}", archive.display());
    let t = out.stdout;
    let mut off = 0;
    let mut paths = Vec::new();
    loop {
        assert!(
            off + 512 <= t.len(),
            "no end marker in {}",
            archive.display()
        );
        let h = &t[off..off + 512];
        if h.iter().all(|&b| b == 0) {
            assert!(off + 1024 <= t.len(), "one zero block only");
            assert!(t[off + 512..].iter().all(|&b| b == 0), "data after the end");
            return paths;
        }
        let sum: u64 = h
            .iter()
            .enumerate()
            .map(|(i, &b)| {
                if (148..156).contains(&i) {
                    32
                } else {
                    u64::from(b)
                }
            })
            .sum();
        assert_eq!(octal(&h[148..156]), Some(sum), "checksum at {off}");
        assert_eq!(&h[257..263], b"ustar\0", "magic at {off}");
        assert_eq!(&h[263..265], b"00", "version at {off}");
        let cut = |f: &[u8]| {
            f.iter()
                .take_while(|&&b| b != 0)
                .copied()
                .collect::<Vec<u8>>()
        };
        let mut path = cut(&h[345..500]);
        if !path.is_empty() {
            path.push(b'/');
        }
        path.extend(cut(&h[..100]));
        let path = String::from_utf8_lossy(&path).into_owned();
        let kind = h[156];
        assert!(kind == b'0' || kind == b'5', "type {kind} of {path}");
        assert_eq!(octal(&h[108..116]), Some(0), "uid of {path}");
        assert_eq!(octal(&h[116..124]), Some(0), "gid of {path}");
        assert!(h[265..329].iter().all(|&b| b == 0), "names of {path}");
        assert!(
            kind != b'0' || !path.ends_with('/'),
            "file {path} ends with /"
        );
        let last = path.split('/').rfind(|p| !p.is_empty()).unwrap_or("");
        assert!(!last.starts_with("._"), "AppleDouble {path}");
        let size = octal(&h[124..136]).expect("size") as usize;
        off += 512 + size.div_ceil(512) * 512;
        paths.push(path);
    }
}

/// T2-a: the real tar writes headers the independent reader accepts, one per staged entry,
/// and dist holds the archive and the two lists only.
#[test]
fn pack_writes_ustar_headers_with_owner_and_group_0_and_no_names() {
    let s = Stage::new("linux-x64", &claude_manifest(), EXE, &[]);
    let out = s.pack(None, "022");
    assert!(out.status.success(), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(
        !err.contains("wrong owner or group") && !err.contains(" has uid "),
        "{err}"
    );
    let paths = read_ustar(&s.archive());
    assert_eq!(paths.len(), s.entries(), "{paths:?}");
    let mut dist: Vec<String> = fs::read_dir(s.dist())
        .expect("read dist")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    dist.sort();
    let mut want = vec![
        "SHA256SUMS-linux-x64.txt".to_owned(),
        format!("{ID}-{}-linux-x64.tar.gz", manifest_version()),
        "plugins-linux-x64.tsv".to_owned(),
    ];
    want.sort();
    assert_eq!(dist, want);
}

fn manifest_version() -> String {
    claude_manifest()
        .lines()
        .find_map(|l| l.strip_prefix("version = \""))
        .and_then(|v| v.strip_suffix('"'))
        .expect("manifest version")
        .to_owned()
}

/// A tar wrapper that answers --version with $FAKE_VERSION, logs a -czf call with its
/// arguments and the two variables pack sets, and exits 1 there; other calls go to tar.
fn version_wrapper(bin: &Path, log: &Path) {
    support::write_executable(
        &bin.join("tar"),
        format!(
            "#!/usr/bin/env bash\n\
             if [ \"$1\" = --version ]; then echo \"$FAKE_VERSION\"; exit 0; fi\n\
             for a in \"$@\"; do if [ \"$a\" = -czf ]; then\n\
               printf '%s\\n' \"$*\" >> '{log}'\n\
               printf 'TAR_OPTIONS=[%s] COPYFILE_DISABLE=[%s]\\n' \"${{TAR_OPTIONS-unset}}\" \"${{COPYFILE_DISABLE-unset}}\" >> '{log}'\n\
               exit 1\n\
             fi; done\n\
             exec '{tar}' \"$@\"\n",
            log = log.display(),
            tar = real_tar()
        ),
    )
    .expect("write tar wrapper");
}

/// T2-b: each tar gets its options with TAR_OPTIONS empty and COPYFILE_DISABLE=1, and an
/// unknown tar stops pack before any archive is written.
#[test]
fn pack_gives_each_tar_its_format_and_owner_options() {
    let cases = [
        (
            "tar (GNU tar) 1.35",
            "--format=ustar --owner=0 --group=0 --numeric-owner -czf ",
        ),
        (
            "bsdtar 3.5.3 - libarchive 3.7.4 zlib/1.2.12 liblzma/5.4.3 bz2lib/1.0.8",
            "--format ustar --uid 0 --gid 0 --numeric-owner --no-mac-metadata -czf ",
        ),
    ];
    for (version, opts) in cases {
        let s = Stage::new("linux-x64", &claude_manifest(), EXE, &[]);
        let bin = s.out.path().join("bin");
        let log = s.out.path().join("tar.log");
        version_wrapper(&bin, &log);
        let out = s.pack_with(
            Some(&bin),
            "022",
            &[
                ("FAKE_VERSION", version),
                ("TAR_OPTIONS", "--synthetic-option"),
            ],
        );
        assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
        assert!(
            stderr(&out).contains("tar -czf failed (exit 1)"),
            "{}",
            stderr(&out)
        );
        let logged = fs::read_to_string(&log).expect("read tar log");
        let lines: Vec<&str> = logged.lines().collect();
        assert!(lines[0].starts_with(opts), "{version}: {logged}");
        assert_eq!(lines[1], "TAR_OPTIONS=[] COPYFILE_DISABLE=[1]", "{version}");
    }

    let s = Stage::new("linux-x64", &claude_manifest(), EXE, &[]);
    let bin = s.out.path().join("bin");
    let log = s.out.path().join("tar.log");
    version_wrapper(&bin, &log);
    let out = s.pack_with(Some(&bin), "022", &[("FAKE_VERSION", "tar (busybox) 1.36")]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("unsupported tar: tar (busybox) 1.36"),
        "{}",
        stderr(&out)
    );
    assert!(!log.exists(), "tar -czf ran");
}

/// A tar wrapper that runs <script> (bash, with the arguments in "$@") before the real tar
/// on a -czf call and passes every other call through.
fn czf_wrapper(bin: &Path, script: &str) {
    support::write_executable(
        &bin.join("tar"),
        format!(
            "#!/usr/bin/env bash\n\
             czf=0; for a in \"$@\"; do [ \"$a\" = -czf ] && czf=1; done\n\
             if [ $czf = 1 ]; then {script}\nfi\n\
             exec '{tar}' \"$@\"\n",
            tar = real_tar()
        ),
    )
    .expect("write tar wrapper");
}

/// T2-c: with the format kept and --numeric-owner dropped, both tars store the names of id
/// 0, and the owner check stops pack with its own error and pack's summary.
#[test]
fn pack_fails_when_the_archive_stores_owner_names() {
    let s = Stage::new("linux-x64", &claude_manifest(), EXE, &[]);
    let bin = s.out.path().join("bin");
    czf_wrapper(
        &bin,
        "args=(); for a in \"$@\"; do [ \"$a\" = --numeric-owner ] || args+=(\"$a\"); done; set -- \"${args[@]}\"",
    );
    let out = s.pack(Some(&bin), "022");
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("has uid 0, gid 0, a user name ("), "{err}");
    assert!(
        err.contains("has entries with a wrong owner or group"),
        "{err}"
    );
}

/// The tools on PATH, linked into <bin> except <without>.
fn path_without(bin: &Path, without: &str) {
    fs::create_dir_all(bin).expect("mkdir");
    for dir in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let name = e.file_name();
            let to = bin.join(&name);
            if name == without || to.symlink_metadata().is_ok() {
                continue;
            }
            let _ = std::os::unix::fs::symlink(e.path(), &to);
        }
    }
}

/// T2-d: without od, or without gzip, pack stops before it writes an archive.
#[test]
fn pack_needs_gzip_and_od_each() {
    for tool in ["od", "gzip"] {
        let s = Stage::new("linux-x64", &claude_manifest(), EXE, &[]);
        let bin = s.out.path().join("bin");
        path_without(&bin, tool);
        let out = Command::new("bash")
            .current_dir(support::repo_root())
            .args([
                "scripts/package-plugin.sh",
                "pack",
                "--platform",
                "linux-x64",
                "--out",
                &s.out.path().to_string_lossy(),
            ])
            .env("PATH", &bin)
            .output()
            .expect("run pack");
        assert_eq!(out.status.code(), Some(1), "{tool}: {}", stderr(&out));
        assert!(
            stderr(&out).contains("pack needs gzip and od"),
            "{tool}: {}",
            stderr(&out)
        );
        assert!(
            !s.dist()
                .join(format!("{ID}-{}-linux-x64.tar.gz", manifest_version()))
                .exists()
        );
    }
}

/// T2-e: a tar that leaves an entry out and still exits 0, as bsdtar does with a path USTAR
/// cannot hold, fails the count against the stage.
#[test]
fn pack_fails_when_the_archive_misses_a_staged_entry() {
    let s = Stage::new("linux-x64", &claude_manifest(), EXE, &[]);
    let bin = s.out.path().join("bin");
    czf_wrapper(&bin, "set -- --exclude '*/lang/en.toml' \"$@\"");
    let out = s.pack(Some(&bin), "022");
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(
        stderr(&out).contains(&format!(
            "holds {} entries; the stage has {}",
            s.entries() - 1,
            s.entries()
        )),
        "{}",
        stderr(&out)
    );
}

/// T2-f: on macOS a staged file with an extended attribute gives no AppleDouble entry.
#[cfg(target_os = "macos")]
#[test]
fn pack_writes_no_appledouble_entry_on_macos() {
    let s = Stage::new("linux-x64", &claude_manifest(), EXE, &[]);
    let st = Command::new("xattr")
        .args(["-w", "com.example.synthetic", "1"])
        .arg(s.dir().join("tasty-plugin.toml"))
        .status()
        .expect("run xattr");
    assert!(st.success());
    let out = s.pack(None, "022");
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(read_ustar(&s.archive()).len(), s.entries());
}

fn is_gnu_tar() -> bool {
    let out = Command::new("tar")
        .arg("--version")
        .output()
        .expect("run tar");
    String::from_utf8_lossy(&out.stdout).contains("(GNU tar)")
}

fn id_of(flag: &str) -> String {
    let out = Command::new("id").arg(flag).output().expect("run id");
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// T2-g (GNU tar): an owner and group map in TAR_OPTIONS overrides --owner and --group on
/// the command line, as the control archive shows, and does not reach pack's archive.
#[test]
fn pack_ignores_owner_maps_in_tar_options() {
    if !is_gnu_tar() {
        return;
    }
    let s = Stage::new("linux-x64", &claude_manifest(), EXE, &[]);
    let maps = s.out.path().join("maps");
    write(
        &maps.join("owner"),
        &format!("+{} synthetic:23\n", id_of("-u")),
    );
    write(
        &maps.join("group"),
        &format!("+{} synthetic:42\n", id_of("-g")),
    );
    let opts = format!(
        "--owner-map={} --group-map={}",
        maps.join("owner").display(),
        maps.join("group").display()
    );

    let control = s.out.path().join("control.tar.gz");
    let st = Command::new("tar")
        .args([
            "--format=ustar",
            "--owner=0",
            "--group=0",
            "--numeric-owner",
            "-czf",
        ])
        .arg(&control)
        .arg("-C")
        .arg(s.dir())
        .arg("tasty-plugin.toml")
        .env("TAR_OPTIONS", &opts)
        .status()
        .expect("run tar");
    assert!(st.success());
    let raw = Command::new("gzip")
        .arg("-dc")
        .arg(&control)
        .output()
        .expect("run gzip")
        .stdout;
    assert_eq!(octal(&raw[108..116]), Some(23), "the map did not apply");
    assert_eq!(octal(&raw[116..124]), Some(42), "the map did not apply");

    let out = s.pack_with(None, "022", &[("TAR_OPTIONS", opts.as_str())]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(read_ustar(&s.archive()).len(), s.entries());
}

/// T2-h: a failed tar -czf stops pack with its exit code.
#[test]
fn pack_stops_when_tar_czf_fails() {
    let s = Stage::new("linux-x64", &claude_manifest(), EXE, &[]);
    let bin = s.out.path().join("bin");
    czf_wrapper(&bin, "exit 2");
    let out = s.pack(Some(&bin), "022");
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("-linux-x64.tar.gz: tar -czf failed (exit 2)"),
        "{}",
        stderr(&out)
    );
}
