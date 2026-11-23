//! scripts/dev-bundle.sh lets one run at a time replace the plugins in its output folder.
//! The lock is the folder DIR/.dev-bundle.lock with the file owner in it: mkdir makes the
//! folder and bash noclobber makes the owner file, which decides the holder. These tests run
//! a copy of the script in a synthetic repository with a stub cargo, so a run goes as far as
//! the lock and then fails with "cargo reported no executable", which here means it took the
//! lock. Nothing is built. One run uses a stub cargo that writes an executable, so it goes to
//! the end and exits 0.
//!
//! A run is held after the lock by a grep in front of PATH: the script first calls grep after
//! taking the lock. A run is held inside its release by an rmdir in front of PATH: the
//! release calls rmdir last. A mkdir in front of PATH plays a run that lost a race to create
//! the lock folder, as uutils mkdir before 0.11.0 does: it finds the folder made by the other
//! run and exits 0 all the same.
//!
//! The runs use the default profile, so the script expands an empty array on its way to the
//! lock; one run passes --profile release. The bash that runs the script can be chosen, and its
//! version required, through the environment: see `dev_bundle_bash`.

#![cfg(unix)]

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Words of the message for a lock that is held or left.
const REFUSED: &str = "another dev-bundle.sh is writing";
/// The failure the stub cargo leads to after the lock: the run took the lock.
const PASSED: &str = "cargo reported no executable for tasty-plugin-x";
/// Words the cleanup adds when it cannot remove the lock.
const RELEASE_FAILED: &str = "remove the lock";
const CLEANUP_FAILED: &str = "cleanup could not";

const STUB_CHECK_SH: &str =
    "#!/usr/bin/env bash\ncase \"$1\" in list-bins) echo tasty-plugin-x ;; *) exit 2 ;; esac\n";
const STUB_CARGO: &str = "#!/bin/sh\nexit 0\n";
/// A cargo that builds; @EXE@ is the executable it writes. See `use_cargo_that_builds`.
const BUILDING_CARGO: &str = r#"#!/bin/sh
exe='@EXE@'
mkdir -p "${exe%/*}"
printf 'stub' > "$exe"
chmod 755 "$exe"
for a in "$@"; do
  case "$a" in
    --message-format=*)
      printf '{"reason":"compiler-artifact","target":{"name":"tasty-plugin-x"},"executable":"%s"}\n' "$exe"
      ;;
  esac
done
"#;

/// Names the bash that runs dev-bundle.sh, as an absolute path.
const BASH_VAR: &str = "REPO_GATES_DEV_BUNDLE_BASH";
/// Asks for the major.minor version of that bash, for example 3.2.
const BASH_VERSION_VAR: &str = "REPO_GATES_DEV_BUNDLE_BASH_VERSION";

fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// The bash that runs dev-bundle.sh: the one `BASH_VAR` names, or else the bash on PATH. When
/// `BASH_VERSION_VAR` is set, the tests fail unless `BASH_VAR` names a bash of that version.
fn dev_bundle_bash() -> &'static Path {
    static BASH: OnceLock<PathBuf> = OnceLock::new();
    BASH.get_or_init(|| {
        let want = non_empty_env(BASH_VERSION_VAR);
        let Some(bash) = non_empty_env(BASH_VAR).map(PathBuf::from) else {
            if let Some(want) = want {
                panic!("{BASH_VERSION_VAR}={want} asks for a version, but {BASH_VAR} is not set");
            }
            return PathBuf::from("bash");
        };
        assert!(
            bash.is_absolute(),
            "{BASH_VAR} must be an absolute path: {}",
            bash.display()
        );
        if let Some(want) = want {
            let out = Command::new(&bash)
                .arg("-c")
                .arg(concat!(
                    r#"printf '%s.%s %s' "${BASH_VERSINFO[0]}" "${BASH_VERSINFO[1]}""#,
                    r#" "$BASH_VERSION""#
                ))
                .stdin(Stdio::null())
                .output()
                .unwrap_or_else(|e| panic!("run {}: {e}", bash.display()));
            let text = String::from_utf8_lossy(&out.stdout).into_owned();
            let (got, full) = text.split_once(' ').unwrap_or((text.as_str(), ""));
            assert!(
                out.status.success() && got == want,
                "{} is bash {full}, not {want}",
                bash.display()
            );
        }
        bash
    })
}

/// The first `name` on PATH.
fn real_tool(name: &str) -> PathBuf {
    std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .map(|d| Path::new(d).join(name))
        .find(|p| p.is_file())
        .unwrap_or_else(|| panic!("{name} on PATH"))
}

/// What is at `p`, in a form two snapshots can be compared by.
fn describe(p: &Path) -> String {
    let Ok(meta) = fs::symlink_metadata(p) else {
        return "missing".to_owned();
    };
    if meta.file_type().is_symlink() {
        format!("link to {}", fs::read_link(p).expect("read link").display())
    } else if meta.is_dir() {
        let mut names: Vec<String> = fs::read_dir(p)
            .expect("read dir")
            .map(|e| {
                let e = e.expect("entry");
                let name = e.file_name().to_string_lossy().into_owned();
                format!("{name}: {}", describe(&e.path()))
            })
            .collect();
        names.sort();
        format!("folder [{}]", names.join(", "))
    } else {
        format!("file {:?}", fs::read_to_string(p).expect("read file"))
    }
}

fn wait_for(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// A repository with copies of dev-bundle.sh and the library it sources, a stub check.sh
/// that lists the crate tasty-plugin-x, its manifest, a stub cargo in front of PATH, and an
/// output folder that already holds x/keep.
struct Fixture {
    tmp: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().expect("temp dir");
        let t = tmp.path();
        for rel in ["scripts/dev-bundle.sh", "scripts/lib/plugin-build.sh"] {
            let body = fs::read(support::repo_file(rel)).expect("read script");
            support::write_executable(&t.join("repo").join(rel), body).expect("copy script");
        }
        support::write_executable(&t.join("repo/scripts/check.sh"), STUB_CHECK_SH)
            .expect("stub check.sh");
        let manifest = t.join("repo/crates/tasty-plugin-x/tasty-plugin.toml");
        fs::create_dir_all(manifest.parent().expect("parent")).expect("crate dir");
        fs::write(&manifest, "id = \"x\"\n").expect("manifest");
        support::write_executable(&t.join("bin/cargo"), STUB_CARGO).expect("stub cargo");
        support::write_executable(
            &t.join("bin/grep"),
            format!(
                "#!/bin/sh\nif [ -n \"$HOLD_DIR\" ]; then\n  : > \"$HOLD_DIR/held\"\n  while [ ! -e \"$HOLD_DIR/go\" ]; do sleep 0.05; done\nfi\nexec \"{}\" \"$@\"\n",
                real_tool("grep").display()
            ),
        )
        .expect("holding grep");
        support::write_executable(
            &t.join("bin/rmdir"),
            format!(
                "#!/bin/sh\nif [ -n \"$RMDIR_HOLD\" ]; then\n  : > \"$RMDIR_HOLD/in\"\n  while [ ! -e \"$RMDIR_HOLD/go\" ]; do sleep 0.05; done\nfi\nexec \"{}\" \"$@\"\n",
                real_tool("rmdir").display()
            ),
        )
        .expect("holding rmdir");
        fs::create_dir_all(t.join("out/x")).expect("out dir");
        fs::write(t.join("out/x/keep"), "kept\n").expect("keep");
        Fixture { tmp }
    }

    /// Puts the mkdir of a run that lost the race in front of PATH. A mkdir of one folder
    /// without options makes the folder as the other run would have, and with
    /// STUB_MKDIR_MODE=owned also that run's owner file, then exits 0. With
    /// STUB_MKDIR_MODE=lost it makes nothing and exits 0: the other run made the folder
    /// earlier, and this run reaches its owner file only now.
    fn use_mkdir_that_lost_the_race(&self) {
        let real = real_tool("mkdir");
        support::write_executable(
            &self.tmp.path().join("bin/mkdir"),
            format!(
                "#!/bin/sh\nif [ \"$#\" -eq 1 ] && [ \"${{1#-}}\" = \"$1\" ] && [ \"$STUB_MKDIR_MODE\" = lost ]; then\n  exit 0\nfi\nif [ \"$#\" -eq 1 ] && [ \"${{1#-}}\" = \"$1\" ] && [ ! -e \"$1\" ] && [ ! -h \"$1\" ]; then\n  \"{real}\" \"$1\" || exit 1\n  if [ \"$STUB_MKDIR_MODE\" = owned ]; then printf 'other run\\n' > \"$1/owner\"; fi\n  exit 0\nfi\nexec \"{real}\" \"$@\"\n",
                real = real.display()
            ),
        )
        .expect("stub mkdir");
    }

    /// Puts a cargo in front of PATH that builds: it writes an executable for tasty-plugin-x
    /// and, with a message format, prints the compiler-artifact message cargo would. A run
    /// then goes to the end.
    fn use_cargo_that_builds(&self) {
        let exe = self.tmp.path().join("target/stub/tasty-plugin-x");
        support::write_executable(
            &self.tmp.path().join("bin/cargo"),
            BUILDING_CARGO.replace("@EXE@", &exe.display().to_string()),
        )
        .expect("building cargo");
    }

    fn out(&self) -> PathBuf {
        self.tmp.path().join("out")
    }

    fn lock(&self) -> PathBuf {
        self.out().join(".dev-bundle.lock")
    }

    fn command(&self) -> Command {
        let t = self.tmp.path();
        let mut cmd = Command::new(dev_bundle_bash());
        cmd.arg(t.join("repo/scripts/dev-bundle.sh"))
            .arg("--out")
            .arg(self.out())
            .current_dir(t)
            .env_remove("CARGO_TARGET_DIR")
            .env_remove("HOLD_DIR")
            .env_remove("STUB_MKDIR_MODE")
            .env_remove("RMDIR_HOLD")
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    t.join("bin").display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .stdin(Stdio::null());
        cmd
    }

    fn run(&self) -> Output {
        self.command().output().expect("run dev-bundle.sh")
    }

    fn run_with_mkdir_mode(&self, mode: &str) -> Output {
        self.command()
            .env("STUB_MKDIR_MODE", mode)
            .output()
            .expect("run dev-bundle.sh")
    }

    /// Starts a run and returns once it holds the lock. It stays after the lock until the
    /// returned value is released or dropped.
    fn hold(&self) -> Held {
        let dir = self.tmp.path().join("hold");
        fs::create_dir_all(&dir).expect("hold dir");
        let child = self
            .command()
            .env("HOLD_DIR", &dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start dev-bundle.sh");
        let held = Held {
            child: Some(child),
            go: dir.join("go"),
        };
        wait_for(&dir.join("held"));
        held
    }

    /// Starts a run and returns once its release has moved the lock and reached rmdir. It
    /// stays there until the returned value is released or dropped.
    fn hold_in_release(&self) -> Held {
        let dir = self.tmp.path().join("release-hold");
        fs::create_dir_all(&dir).expect("hold dir");
        let child = self
            .command()
            .env("RMDIR_HOLD", &dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start dev-bundle.sh");
        let held = Held {
            child: Some(child),
            go: dir.join("go"),
        };
        wait_for(&dir.join("in"));
        held
    }

    /// The names in DIR: after a run ends, only the plugin folder may be there, not the lock
    /// or a folder it was moved to.
    fn out_names(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.out())
            .expect("read out")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn assert_out_untouched(&self) {
        assert_eq!(
            describe(&self.out().join("x")),
            "folder [keep: file \"kept\\n\"]"
        );
    }
}

/// A run held after the lock. Dropping it lets the run go on, so a failed test does not
/// leave it waiting.
struct Held {
    child: Option<Child>,
    go: PathBuf,
}

impl Held {
    fn release(mut self) -> Output {
        fs::write(&self.go, "").expect("go");
        self.child
            .take()
            .expect("child")
            .wait_with_output()
            .expect("wait")
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = fs::write(&self.go, "");
            let _ = child.wait();
        }
    }
}

fn assert_refused(out: &Output) {
    let err = stderr(out);
    assert!(!out.status.success(), "{err}");
    assert!(err.contains(REFUSED), "{err}");
    assert!(err.contains("cannot create the lock"), "{err}");
    assert!(!err.contains(PASSED), "the run went past the lock: {err}");
    assert!(!err.contains(CLEANUP_FAILED), "{err}");
}

fn assert_passed_and_released(f: &Fixture, out: &Output) {
    let err = stderr(out);
    assert!(!out.status.success(), "{err}");
    assert!(err.contains(PASSED), "the run did not take the lock: {err}");
    assert!(!err.contains(CLEANUP_FAILED), "{err}");
    assert_eq!(f.out_names(), ["x"], "{err}");
}

// ── A lock that is there ────────────────────────────────────────────────────

/// Each kind of entry a run can find at the lock path: one left or held by this version, by
/// an earlier version (an empty folder), or something else.
fn place(f: &Fixture, kind: &str) {
    let lock = f.lock();
    match kind {
        "file" => fs::write(&lock, "a file\n").expect("file"),
        "earlier folder" => fs::create_dir(&lock).expect("folder"),
        "link" => std::os::unix::fs::symlink("/dev/null", &lock).expect("link"),
        "held folder" => {
            fs::create_dir(&lock).expect("folder");
            fs::write(lock.join("owner"), "holder\n").expect("owner");
        }
        _ => unreachable!("{kind}"),
    }
}

const KINDS: [&str; 4] = ["file", "earlier folder", "link", "held folder"];

#[test]
fn a_run_refuses_whatever_is_at_the_lock_path_and_leaves_it() {
    for kind in KINDS {
        let f = Fixture::new();
        place(&f, kind);
        let before = describe(&f.lock());
        let out = f.run();
        assert_refused(&out);
        assert_eq!(describe(&f.lock()), before, "{kind}");
        f.assert_out_untouched();
    }
}

#[test]
fn a_run_refused_by_an_earlier_version_lock_leaves_nothing_behind() {
    let f = Fixture::new();
    place(&f, "earlier folder");
    assert_refused(&f.run());
    // The earlier version removes its lock with rmdir, which works only on an empty folder.
    fs::remove_dir(f.lock()).expect("the earlier version's rmdir");
    assert_passed_and_released(&f, &f.run());
}

// ── A mkdir that lost the race ──────────────────────────────────────────────

#[test]
fn a_run_whose_mkdir_lost_the_race_does_not_take_a_lock_the_other_run_owns() {
    let f = Fixture::new();
    f.use_mkdir_that_lost_the_race();
    let out = f.run_with_mkdir_mode("owned");
    assert_refused(&out);
    assert_eq!(
        describe(&f.lock()),
        "folder [owner: file \"other run\\n\"]",
        "{}",
        stderr(&out)
    );
    f.assert_out_untouched();
}

#[test]
fn a_run_whose_mkdir_lost_the_race_takes_a_folder_nobody_owns_yet() {
    let f = Fixture::new();
    f.use_mkdir_that_lost_the_race();
    let out = f.run_with_mkdir_mode("folder");
    assert_passed_and_released(&f, &out);
}

// ── Two runs ────────────────────────────────────────────────────────────────

#[test]
fn a_second_run_is_refused_while_the_first_holds_the_lock() {
    let f = Fixture::new();
    let first = f.hold();
    let owner = describe(&f.lock());
    assert_eq!(owner, "folder [owner: file \"\"]");
    let second = f.run();
    assert_refused(&second);
    assert_eq!(describe(&f.lock()), owner);
    let first = first.release();
    assert_passed_and_released(&f, &first);
    f.assert_out_untouched();
}

// ── Release ─────────────────────────────────────────────────────────────────

#[test]
fn a_run_removes_its_lock_when_it_ends() {
    let f = Fixture::new();
    let out = f.run();
    assert_passed_and_released(&f, &out);
    f.assert_out_untouched();
}

#[test]
fn a_release_profile_run_removes_its_lock_when_it_ends() {
    let f = Fixture::new();
    let out = f
        .command()
        .args(["--profile", "release"])
        .output()
        .expect("run dev-bundle.sh");
    assert_passed_and_released(&f, &out);
    f.assert_out_untouched();
}

#[test]
fn a_run_that_bundles_the_plugins_exits_0_and_removes_its_lock() {
    let f = Fixture::new();
    f.use_cargo_that_builds();
    let out = f.run();
    let err = stderr(&out);
    assert!(out.status.success(), "{err}");
    assert!(err.contains("bundled 1 plugins into"), "{err}");
    assert_eq!(f.out_names(), ["x"], "{err}");
    assert_eq!(
        describe(&f.out().join("x")),
        "folder [tasty-plugin-x: file \"stub\", tasty-plugin.toml: file \"id = \\\"x\\\"\\n\"]",
        "{err}"
    );
}

#[test]
fn a_run_whose_owner_file_vanished_reports_the_lock() {
    let f = Fixture::new();
    let held = f.hold();
    fs::remove_file(f.lock().join("owner")).expect("remove owner");
    let out = held.release();
    let err = stderr(&out);
    assert!(!out.status.success(), "{err}");
    assert!(err.contains(PASSED), "{err}");
    assert!(err.contains(CLEANUP_FAILED), "{err}");
    assert!(err.contains(RELEASE_FAILED), "{err}");
}

#[test]
fn a_run_about_to_claim_the_folder_being_released_fails_and_leaves_nothing() {
    let f = Fixture::new();
    f.use_mkdir_that_lost_the_race();
    let first = f.hold_in_release();
    // The second run's mkdir lost the race to the first run long ago; it reaches its owner
    // file while the first run releases the lock.
    let second = f.run_with_mkdir_mode("lost");
    assert_refused(&second);
    let first = first.release();
    assert_passed_and_released(&f, &first);
    f.assert_out_untouched();
}

#[test]
fn a_release_does_not_remove_a_lock_folder_made_after_it() {
    let f = Fixture::new();
    let first = f.hold_in_release();
    // Plays the runs that come next: one that took over whatever is at the lock path and
    // released it, then one that has just made a new lock folder and not yet its owner file.
    let lock = f.lock();
    if lock.exists() {
        fs::remove_dir_all(&lock).expect("the next run's release");
    }
    fs::create_dir(&lock).expect("a new lock folder");
    let first = first.release();
    let err = stderr(&first);
    assert!(err.contains(PASSED), "{err}");
    assert_eq!(describe(&lock), "folder []", "{err}");
}
