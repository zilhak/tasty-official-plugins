# CI and release workflows

This repository builds and tests the official Tasty plugins from their sources in
`crates/` of this repository. Only the tasty crates they depend on (the plugin SDK,
protocol, manifest, and shared libraries) come from the tasty commit pinned in
`Cargo.toml`. The workflows call `scripts/check.sh` and `scripts/package-plugin.sh`, so
the steps that run in CI also run locally. The tasty sha is not repeated in any
workflow.

## Workflows

| File | Trigger | What it does |
|---|---|---|
| `.github/workflows/ci.yml` | push to `develop`, `release`, or `main`, pull request to one of them, manual | `check` job: fmt, clippy, test on Linux, then the WebKitGTK tests. `test` job: clippy, unit tests, and the `plugin-guards` source checks on Windows x64 and macOS arm64, and on macOS also the `repo-gates` integration tests. `lint` job: shellcheck on every shell script and actionlint on every workflow. `audit` job: runs `cargo audit` on `Cargo.lock`. `build` job: builds the plugin crates for five platforms. On Windows it also checks that the binaries link the C runtime statically |
| `.github/workflows/release.yml` | push of a `v*` tag; manual (`workflow_dispatch`) | On a tag, releases the plugins on a GitHub Release of this repository (see [Releases](#releases)). A manual run builds the same release files as workflow artifacts and creates no Release |
| `.github/workflows/plugin-version-check.yml` | push to `develop`, `release`, or `main`, pull request to one of them, for changes outside Markdown and `docs/`; manual | Checks that each plugin whose build output changed raised its version (see [Plugin version check](#plugin-version-check)), then runs the `repo-gates` tests |

The workflows use `permissions: contents: read` by default. In `release.yml`, only the
`create-release` and `publish` jobs get `contents: write`. In `ci.yml`, a new run waits
for the older run on the same ref instead of cancelling it, as in tasty: on the
self-hosted runners a run can take longer than the time between pushes, and a waiting run
is replaced by the newer push. Pull request runs cancel the older run. A run never waits
for a run on another ref. Every job has `timeout-minutes`.

The same jobs run on `develop`, `release`, and `main`. A commit fast-forwarded from
`develop` to `release` and `main` is checked again on each branch. A manual run without
`--ref` uses the default branch, `develop`. The branch rules are in
[branching.md](branching.md).

## Platforms and runners

Every job runs on a self-hosted runner registered for this repository. The labels follow
tasty's runners, and the runners live on the same machines as tasty's, in
separate runner folders. Checkouts and the runner tool cache are per runner folder, so
they are not shared with tasty.

| Platform label | Target | Runner labels |
|---|---|---|
| `windows-x64` | `x86_64-pc-windows-msvc` | `[self-hosted, Windows, X64]` |
| `windows-arm64` | `aarch64-pc-windows-msvc` | `[win-arm64]`, an x64 Windows host that cross-builds, as in tasty |
| `macos-arm64` | `aarch64-apple-darwin` | `[self-hosted, macOS, ARM64]` |
| `linux-x64` | `x86_64-unknown-linux-gnu` | `[self-hosted, Linux, X64]` |
| `linux-arm64` | `aarch64-unknown-linux-gnu` | `[self-hosted, Linux, ARM64]` |

The `check`, `lint`, and `audit` jobs run on `[self-hosted, Linux, X64]`, and the `test`
job on the Windows x64 and macOS runners.

These are the same five platforms that tasty releases. Linux ARM64 is included because
tasty ships a Linux ARM64 package, and the full-package lock file needs a plugin hash
for every platform.

The `build` and `package` jobs pass `--target` explicitly, even on a native runner.

A runner runs one job at a time, so jobs with the same labels wait for each other,
including jobs from `ci.yml` and `release.yml`. The runners can run at the same time as
tasty's runners on the same machine. GitHub concurrency groups do not reach across
repositories, so the workflows here add no group for that: a group in this repository
would only order runs of this repository, which the runner queue already does.

### What the runners need

Jobs do not change machine-wide state: they install no system packages, no toolchains
or targets, and nothing into `~/.cargo/bin`. The jobs check for the toolchain, the
build target, and the Linux WebKitGTK files, and fail with a message that points here
when one is missing.

Both workflows set `RUSTUP_AUTO_INSTALL=0`. rustup 1.28.1 and later install a missing
toolchain named in `rust-toolchain.toml` by default, which would change the runner's
`RUSTUP_HOME`, shared with other runners of the same user. With the variable set, a job
on a runner without the toolchain fails at "Show toolchain" instead. The workflows do not
change the persistent rustup setting (`rustup set auto-install`).

Each runner needs:

| Runner | Needs |
|---|---|
| All | Git, and rustup with the toolchain in `rust-toolchain.toml` (Rust 1.96.0 with `rustfmt` and `clippy`). For `release.yml`, `tar`, `awk`, and `sha256sum` or `shasum` (`scripts/package-plugin.sh`; Git for Windows provides them on Windows). On Linux the jobs add `$HOME/.cargo/bin` to `PATH`, as tasty does |
| Windows (both) | Git for Windows, whose bash runs `scripts/check.sh` (bash need not be on `PATH`: the jobs find `bin\bash.exe` from the location of `git` and put that folder first on `PATH`, ahead of the WSL bash in System32), PowerShell 7 (`pwsh`), and Visual Studio Build Tools with the MSVC x64 tools, `vswhere.exe`, and `dumpbin.exe` |
| `[win-arm64]` | In addition, the MSVC ARM64 build tools (C dependencies are compiled for ARM64) and the `aarch64-pc-windows-msvc` Rust target installed for the toolchain above |
| `[self-hosted, Linux, X64]` | For `plugin-version-check.yml`, `rustfmt` from the toolchain and access to GitHub to install `strip-cfg-test` and to fetch tasty commits that the Cargo git cache does not have. `pkg-config` and the WebKitGTK 4.1 development files (`libwebkit2gtk-4.1-dev` on Debian and Ubuntu, which brings the GTK 3, JavaScriptCore, and libsoup 3 headers) for the `check` job, `xvfb-run` (`xvfb`) for the WebKitGTK tests, and a C compiler. The `audit` job installs `cargo-audit` into the runner tool cache. The `lint` job needs no installed tools: it downloads shellcheck and actionlint into the runner tool cache, which needs access to GitHub, `curl`, and `tar` with `xz`. The `release-meta` job of `release.yml` needs `jq` or Python 3 (`scripts/release-meta.sh`). The `create-release` and `publish` jobs need the GitHub CLI (`gh`) and `sha256sum` |
| `[self-hosted, Linux, ARM64]` | A C compiler |
| `[self-hosted, macOS, ARM64]` | The Xcode command line tools, and access to GitHub to install `strip-cfg-test` into the runner tool cache for the `repo-gates` tests |

## Tests by platform

| Platform | Job | What runs |
|---|---|---|
| `linux-x64` | `check` on `[self-hosted, Linux, X64]` | `scripts/check.sh test`: every test of every workspace crate that is not `#[ignore]` (the eight `#[ignore]` tests of `tasty-plugin-codex` and the two WebKitGTK tests do not run here) |
| `linux-x64` | `check`, step "test (linux, webview layout)" | The two `#[ignore]` WebKitGTK tests of `tasty-plugin-markdown` under `xvfb-run`, with the same commands as tasty's `crossplatform-check.yml`. As in tasty, a failure is reported but does not fail the job yet |
| `windows-x64` | `test` on `[self-hosted, Windows, X64]` | `scripts/check.sh lint`, then `scripts/check.sh test --lib --bins --no-fail-fast`, then `cargo test --locked -p plugin-guards` |
| `macos-arm64` | `test` on `[self-hosted, macOS, ARM64]` | `scripts/check.sh lint`, then `scripts/check.sh test --lib --bins --no-fail-fast`, then `cargo test --locked -p plugin-guards`, then `cargo test --locked -p repo-gates` with `strip-cfg-test` from `scripts/install-judge.sh` |
| `windows-arm64`, `linux-arm64` | `build` | Build only. The plugin code branches on the operating system, not the CPU, so the x64 test runs cover the same code paths |

The `--lib --bins` scope is the one tasty uses for its Windows and macOS unit tests.

The `repo-gates` integration tests run the plugin version check, its fixup, and the git
hooks on fixture repositories. On macOS they run those shell scripts with the BSD tools
(`sed`, `awk`, `comm`, `mktemp`) and the bash on `PATH`, which Linux does not cover.
Before them the job runs `cargo fetch --locked`, because the fixtures resolve with `cargo
--offline`, and installs `strip-cfg-test` as `plugin-version-check.yml` does. They do not
run on Windows.

Clippy (`scripts/check.sh lint`) runs natively on `linux-x64` (`check` job),
`windows-x64`, and `macos-arm64` (`test` job). It does not run for `windows-arm64` or
`linux-arm64`.

The `check` job checks that the WebKitGTK 4.1 development files are installed on the
runner. The `tasty-plugin-markdown` Linux dev-dependencies (`webkit2gtk`, `gtk`,
`javascriptcore-rs`) link against them. The `build` job needs no extra packages,
because it builds no dev-dependencies.

## Actions and cache

- Actions are pinned to a commit sha, with the version in a comment
  (`actions/checkout` v7.0.1, `actions/upload-artifact` v7.0.2,
  `actions/download-artifact` v8.0.2). Update the sha and the comment together.
- The workflows use no build cache action, as in tasty. `actions/checkout` cleans the
  runner's working directory with `git clean -ffdx`, which also removes `target/`, so
  every job builds from scratch. Downloaded crates stay in the runner user's Cargo
  home. `Swatinem/rust-cache` is not used because its save step cleans up the registry
  in that Cargo home, which tasty's runners on the same machine may share.

## Static checks

The `lint` job in `ci.yml` runs `scripts/install-lint-tools.sh`, which downloads
shellcheck and actionlint from their GitHub release pages into the runner tool cache
(`$RUNNER_TOOL_CACHE/lint-tools/shellcheck-<version>-actionlint-<version>-<arch>`), and
then `scripts/lint-scripts.sh`. The versions and the SHA-256 sums of the release archives
are pinned in `install-lint-tools.sh`: an archive with another sum fails the job, and an
install that is already there and reports the pinned versions is reused. To update a tool,
change its version and sums together. actionlint publishes its sums in the release's
`checksums.txt`; shellcheck publishes none, so take the sum of the downloaded archive.

Jobs that share a tool cache can install at the same time without a lock. Each run
downloads, checks, and unpacks into a folder of its own next to the install
(`.shellcheck-<version>-actionlint-<version>-<arch>.<random>`) and publishes it by
creating the install name as a symlink to that folder with `ln -sn`, which fails when the
name exists. A run that loses uses the install that won. The script prints the folder the
link points at, not the link, so a job keeps its tools when another run replaces the link
later. When the link points at a folder whose tools fail the version check, or the name is
a plain directory from an earlier version of the script, the run renames it aside to
`.broken.<name>.<pid>.<attempt>` and creates the link again. No run deletes a folder that
another run may use: once a run has tried `ln`, it keeps its own folder, because the link
may have been made and another run may have printed that folder. When the download or its
sum check fails, the run removes its own folder and the install name is not created.
Replaced folders, the `.broken.*` entries, the folders of runs that lost the race (each
run says so in its log), and the folder of a run that was killed (`.shellcheck-*`) may
remain; remove them by hand when no job is running.

`lint-scripts.sh` finds the scripts instead of listing them: tracked files that end in
`.sh`, the hooks in `.githooks`, and tracked files whose first line is a bash or sh
shebang. It prints each file, runs `shellcheck -x` on them, and runs actionlint on
`.github/workflows/*.yml`, which also runs shellcheck on the `run` blocks. Run it locally
with `PATH="$(bash scripts/install-lint-tools.sh):$PATH" bash scripts/lint-scripts.sh`;
outside CI the tools go to `~/.cache/tasty-official-plugins/lint-tools`.

## Source checks on the plugins

The `plugin-guards` crate holds source checks that tasty ran on the plugin crates before
they moved here. They are integration tests (`crates/plugin-guards/tests/`). They run in
the `check` job with `scripts/check.sh test` on Linux, and in the `test` job on Windows
and macOS in a step of their own, because the `--lib --bins` run leaves them out. Run them
with `cargo test --locked -p plugin-guards`. They read the sources as text and use the
source readers of `tasty-doc-guards` from the pinned tasty commit, a dev-dependency of
this crate and of `plugin-pin-check` only, so it never enters a plugin build.

- `notify_mod_language_lists`: the language lists in the Claude mod's TypeScript (the
  notify heading, the gate reason, and the heading test) match
  `tasty_i18n::BUILTIN_CODES`.
- `plugin_locale_literals`: the shipped code of the plugins with a manifest holds no
  string literal with Hangul, kana, or CJK ideographs. Test-only code, comment lines, and
  diagnostic calls are left out.
- `read_only_handle_mtime`: no Rust source under `crates/` sets a modification time
  through a handle from `File::open`, which fails on Windows, and the number of mtime
  changes per file matches the list in the test.
- `plugin_locale_font_call_convention`: the four egui plugins read `TASTY_LOCALE_FONT`,
  call `install_locale_font_fallback`, and pass the face number from
  `locale_font_face_from_env`, and no file under `crates/` repeats the system CJK font
  names that tasty keeps in one place.
- `webkit_named_tests`: each markdown WebKitGTK test file holds one `#[ignore]` test,
  and an automatic workflow job runs it by its full name with
  `-p tasty-plugin-markdown`, `--ignored`, and `--exact`, so a renamed test cannot pass
  with `0 passed`.
- `lang_parity`: each plugin's `lang/<code>.toml` has the keys and placeholder slots of
  `en.toml`, and a value equal to English is allowlisted. Keys still waiting for a
  translation are listed in the test. The literal keys passed to `t`, `t_args`, `t_count`,
  `t_count_template`, `t_fmt`, `t_fmt2`, `t_fmt_fit`, and `t_replace` in the shipped
  plugin code exist in a plugin catalog, and each catalog key is used in Rust or TOML or
  listed as an orphan.
- `no_hardcoded_ui_strings`: reads the Rust sources under `src` and `crates/*/src`, tool
  crates included, with test files, test items, `debug` folders, build scripts, and
  comment lines left out. A literal passed to an egui widget fails with two letters in a
  row or CJK, one passed to `hint_text` only with a sentence or CJK, and one passed to a
  native menu call, an `unwrap_or` fallback, or a print macro with a sentence, a
  capitalized word, or CJK. The allowed tokens (such as key names, language names, units,
  product names) are listed in the test and apply to the widget, menu, fallback, and
  print rules only: a `hint_text` literal is judged without them. One pending literal is
  listed in the test too.
- `one_concept_one_word`: the plugin translation files use the canonical spellings of
  Pane and Window, and each Korean word for window that names a popup is registered with
  its phrase, count, and evidence.
- `notice_set`: `LICENSES/` holds at least one license text, and `THIRD_PARTY_LICENSES.md`
  links every file in `LICENSES/` and no file that is missing there. `LICENSES/` holds
  files only; a subfolder or link there fails this check and `stage`.

The checks that walk the sources fail when they find fewer files or plugins than a floor
measured in this repository, so an empty walk does not pass.

## Advisory check

The `audit` job in `ci.yml` installs `cargo-audit` at the version in `CARGO_AUDIT_VERSION`
into the runner tool cache (`$RUNNER_TOOL_CACHE/cargo-audit/<version>`) when it is not
there yet, and runs `scripts/check.sh audit`. To update the tool, change that value.

`cargo audit` reads `Cargo.lock` directly, so it also checks the crates.io dependencies
that only the plugin crates use. It matches advisories only against crates.io packages.
The `tiny_http` and `epaint` copies and the tasty crates come from git and are not
matched.

Only a vulnerability fails the job. Unsound, unmaintained, and yanked results appear as
warnings in the job log.

The job fetches the advisory database on every run, and the database is not pinned. A new
advisory can therefore fail the job on a commit that changed nothing related. This is
intended, and `check` and `build` report their own results. Do not ignore a
vulnerability. This repository keeps its own `Cargo.lock`, so a crates.io crate is
updated here with `cargo update -p <crate>`, with a patch bump of each plugin whose
dependency closure holds it ([Plugin version check](#plugin-version-check)). A crate of
the egui family and a tasty crate are fixed in tasty and arrive with a pin move. If the
log shows that fetching the database failed, rerun the job.

## Plugin version check

`plugin-version-check.yml` runs `scripts/check-plugin-version-bump.sh --range` on the two
commits of the event: the pull request base and head, or the push before and after
(`HEAD^` and `HEAD` for a manual run). It runs for every change except Markdown outside
`src`, `lang`, and `assets`, and `docs/`. A newer pull request run cancels the older one
on the same ref. Push runs and manual runs are neither cancelled nor replaced: each run
has a concurrency group of its own (its run id), because each one checks only its own
before..after range and a cancelled or replaced run would leave that range unchecked.

- `cargo fetch --locked` comes first, so `cargo tree --offline` works on a runner without
  a warm cache.
- `scripts/install-judge.sh` installs `strip-cfg-test` from the pinned tasty commit into
  `$RUNNER_TOOL_CACHE/strip-cfg-test/<first 8 characters of the tasty sha>`, never into
  `~/.cargo/bin`. It leaves test-only Rust code out of the comparison. The step fails if
  the install gives no runnable file.
- The first push of a ref has nothing to compare and is skipped with a message. A range
  start that is missing from the clone (after a force push) fails the job.
- Exit codes: 0 pass or nothing to judge, 1 a plugin needs a version bump, 2 undecidable.
  Only 0 passes.
- Besides the plugin directories and their path dependencies, the check judges the root
  build inputs. A tasty pin move or a changed `[workspace.dependencies]` entry needs a
  bump of each plugin whose dependency closure holds a changed crate. An update of a
  crates.io crate in `Cargo.lock` needs one too, unless the plugin is already above its
  version at the tip of `origin/main` (the local `main` only when there is no
  `origin/main`). `main` moves only for releases and hotfixes, so its tip is the published
  state, and tags are not read. A local check uses the `origin/main` the clone last
  fetched: if it was not fetched after a release, the baseline is older and a second
  update can pass locally; the CI checkout fetches it on every run. `[profile.*]`,
  `.cargo/config.toml`, and `rust-toolchain.toml` pass with a note; `[workspace]` and
  unknown sections are undecidable, and so is a changed `[workspace.dependencies]` line
  that cannot be read or that no package in `Cargo.lock` answers to. A root `Cargo.toml`
  with a multi-line string (`"""` or `'''`) or with unbalanced brackets is undecidable as
  a whole.
- A plugin's dependency closure is its normal and build dependencies for all targets
  (`cargo tree --target all`), so a crate that only Windows or macOS links counts on the
  Linux runner too. Every judgement that reads a closure (a change in a path dependency
  crate, a pin move, a root dependency change, a `Cargo.lock` change) reads the crates of
  every target offline, so they must be downloaded first. A build fetches only the host's
  crates; `scripts/dev-setup.sh` runs `cargo fetch --locked` once, and after a dependency
  change a local check may need it again.

## Static C runtime on Windows

`.cargo/config.toml` adds `-C target-feature=+crt-static` for both Windows MSVC targets,
so the plugins run without the VC++ Redistributable. On Windows, the CI build job
collects the executables from cargo's `compiler-artifact` messages and runs
`dumpbin /dependents` on each one. The job fails if any executable imports
`vcruntime*.dll`, `msvcp*.dll`, or `api-ms-win-crt-*.dll`.

| Platform | Result |
|---|---|
| `windows-x64` | No VC++ runtime DLL imports in the 9 plugin executables (CI run 42402231647) |
| `windows-arm64` | No VC++ runtime DLL imports in the 9 plugin executables (CI run 42402231647) |

To check by hand, open the "Inspect DLL imports with dumpbin (Windows)" step in the CI
log. It lists the DLLs each plugin imports and ends with
`static CRT: <n> executables import no VC++ runtime DLLs`.

## Packaging

`scripts/package-plugin.sh` follows tasty's `stage_plugins`. It runs in this fixed
order:

1. `stage`: build with `scripts/check.sh build --profile dist --target <triple>`, then
   copy each plugin into `<out>/<platform>/stage/<id>/` with its executable,
   `tasty-plugin.toml`, `lang/`, and the notice set of the repository: `LICENSE`,
   `THIRD_PARTY_LICENSES.md`, and every file in `LICENSES/`, kept under `LICENSES/`. A
   crate with an `assets/NOTICE.md` also gets it as `NOTICE.md`; now that is the markdown
   plugin. `<id>` is the manifest `id`, for example `com.tasty.claude`.
2. `pack`: one `<id>-<version>-<platform>.tar.gz` per plugin. `<version>` is the
   manifest `version`.
3. `pack` then writes `SHA256SUMS-<platform>.txt` (the `sha256sum` format) and
   `plugins-<platform>.tsv` with the columns `id`, `version`, `platform`, `file`,
   `sha256`.

The packages are not signed. `pack` fails if the stage holds a `.sig` file or if an
archive contains one. It also fails if an archive lacks a file of the notice set, or the
`NOTICE.md` of a crate that has one.

The script finds each executable through the `executable` field of cargo's
`compiler-artifact` messages. It copies the manifest and `lang/` from `crates/<crate>/`
in this repository.

Example:

```
scripts/package-plugin.sh all --all --platform linux-x64 --target x86_64-unknown-linux-gnu --out target/package
```

Each archive holds one folder, `<id>/`. For example,
`com.tasty.claude-0.3.1-linux-arm64.tar.gz` lists:

```
com.tasty.claude/
com.tasty.claude/tasty-plugin-claude
com.tasty.claude/tasty-plugin.toml
com.tasty.claude/lang/
com.tasty.claude/lang/en.toml
com.tasty.claude/lang/ko.toml
com.tasty.claude/lang/zh-Hans.toml
com.tasty.claude/lang/ja.toml
com.tasty.claude/LICENSE
com.tasty.claude/THIRD_PARTY_LICENSES.md
com.tasty.claude/LICENSES/
com.tasty.claude/LICENSES/Hack-MIT-Bitstream-Vera.txt
com.tasty.claude/LICENSES/emoji-icon-font-MIT.txt
com.tasty.claude/LICENSES/Ubuntu-Ubuntu-font-1.0.txt
com.tasty.claude/LICENSES/NotoEmoji-OFL-1.1.txt
com.tasty.claude/LICENSES/highlightjs-BSD-3-Clause.txt
com.tasty.claude/LICENSES/mermaid-MIT.txt
com.tasty.claude/LICENSES/KaTeX-MIT.txt
```

The markdown archive also lists `com.tasty.markdown/NOTICE.md`.

The archives are tar.gz on every platform, including Windows, so one format and one
hash list serve all platforms. The script needs bash, tar, awk, and `sha256sum` or
`shasum`, and reads cargo's JSON messages with awk, so it needs neither jq nor Python.
On Windows it runs under Git Bash, where `python3` can be the Microsoft Store app
execution alias rather than a Python.

### Plugins in the packages

The release workflow packages all eight plugin crates with `--all`, including
`tasty-plugin-agent-stream`, whose manifest says `bundle = false`. The `bundle` value of
each plugin is recorded in `release-meta.toml`, so tasty decides what to bundle. The
agent-stream archives are published on the Release with the others and have rows in
`plugins-<platform>.tsv`, and `release-meta.toml` records `bundle = false` for them, so
tasty's general package does not bundle agent-stream by default.
`tasty-plugin-mesh-demo` stays in tasty as an SDK example and is not built here.

## Releases

The repository version is in `VERSION` (`MAJOR.MINOR.PATCH`). A release is the push of
the tag `v` + `VERSION` on a `main` commit. The tag goes on the commit that bumps
`VERSION`: `create-release` uses the body of the tag commit as the Release notes. The
release steps are in [branching.md](branching.md).

### Jobs

| Job | Runner | Permissions | What it does |
|---|---|---|---|
| `create-release` | `[self-hosted, Linux, X64]` | `contents: write` | Tag runs only. Fails unless the tag commit is an ancestor of `origin/main` and the tag equals `v` + `VERSION`. Fails if a published Release with the tag exists. Reuses a draft Release only if its notes record the same commit (`Commit: <sha>`), and fails otherwise. Without a Release, creates a draft whose notes give the commit and the tasty rev |
| `release-meta` | `[self-hosted, Linux, X64]` | `contents: read` | Writes `release-meta.toml` with `scripts/release-meta.sh` and uploads it as the artifact `release-meta` |
| `package` | the five platform runners | `contents: read` | Builds and packages the eight plugins and uploads the artifact `plugins-<platform>` |
| `publish` | `[self-hosted, Linux, X64]` | `contents: write` | Tag runs only. Downloads the artifacts, runs `sha256sum -c` for each platform, checks that each platform has 8 archives, 8 hash lines, and 8 TSV rows, that `release-meta.toml` names this tag, commit, and the 8 plugins, and that there are 51 files. Uploads them to the draft, then publishes the Release |

On a tag run, `release-meta` and `package` start only after `create-release` succeeds.

### Release files

| File | Count |
|---|---|
| `<id>-<version>-<platform>.tar.gz` | 8 plugins × 5 platforms = 40 |
| `SHA256SUMS-<platform>.txt` | 5 |
| `plugins-<platform>.tsv` | 5 |
| `release-meta.toml` | 1 |

The files are attached to the Release of the tag in this repository and are
downloaded from
`https://github.com/zilhak/tasty-official-plugins/releases/download/<tag>/<file>`.
There is no merged hash list or TSV across platforms.

### Failures and reruns

If `create-release` fails, no Release is created. If a later job fails or is cancelled,
the Release stays a draft. In both cases nothing is published.
A rerun of the workflow for the same tag reuses the draft. `publish` does not overwrite
an asset: an asset already on the draft is skipped if its SHA-256 equals the new file,
and the job fails if it differs. The job also fails if the draft has an asset that is
not one of the 51 files. In that case, delete the draft (or the asset) by hand and rerun.

### release-meta.toml

`scripts/release-meta.sh` writes the release metadata. tasty reads it with the
`plugins-<platform>.tsv` files to build its official plugin lock file.

```toml
# Release metadata of zilhak/tasty-official-plugins. Written by scripts/release-meta.sh.
schema = 1

[release]
repository = "zilhak/tasty-official-plugins"
tag = "v0.1.0"
commit = "<40-hex commit of this repository>"
tasty_rev = "e277fce41d9a2db8bf3c14fda8f56dd6415bfcbc"
host_api_version = "1"
protocol_version = "0.2.16"
manifest_version = 1
epaint_version = "0.31.1"
epaint_patch_tree = "37251c43b1d13d12fb6d4c81f3589ace67a0ac12"

[[plugin]]
id = "com.tasty.agent-stream"
version = "0.2.0"
bundle = false

[[plugin]]
id = "com.tasty.claude"
version = "0.3.0"
bundle = true

# ... one [[plugin]] entry per plugin, eight in all
```

| Key | Source |
|---|---|
| `tag` | The release tag. A manual run and a local run without `--tag` write `unreleased` |
| `commit` | The commit of this repository the packages were built from |
| `tasty_rev` | The tasty rev pinned in the root `Cargo.toml` |
| `host_api_version`, `manifest_version` | `HOST_API_VERSION` and `MANIFEST_VERSION` in tasty's `crates/tasty-plugin-manifest/src/types.rs` at `tasty_rev` |
| `protocol_version` | The version of the `tasty-plugin-protocol` package at `tasty_rev` |
| `epaint_version` | The `epaint` version in `Cargo.lock` |
| `epaint_patch_tree` | The git tree id of tasty's `vendor/epaint` at `tasty_rev` |
| `[[plugin]]` | `id`, `version`, and `bundle` from each plugin's `tasty-plugin.toml`. `bundle` is `true` unless the manifest says otherwise |

The tasty values are read from Cargo's git checkout of the tasty crates, after the script
checks that the checkout is at `tasty_rev`. The script reads cargo metadata with `jq`, or
with Python 3 when `jq` is missing. It skips a `python3` or `python` that does not run,
such as the Microsoft Store alias on Windows. To write the file locally:

```
scripts/release-meta.sh --tag unreleased --out target/release-meta.toml
```

### Not signed

The packages, manifests, and binaries of this repository are not signed, and there is
no macOS codesign or notarization and no Windows Authenticode step. The release needs
no signing secrets. tasty signs the manifests of the plugins it bundles when it packages
them.

## Checks after the first push

These checks ran before the plugin sources moved here, on the earlier manual-only
release workflow, which had placeholder signing steps and packaged seven plugins, and on
the GitHub-hosted Linux runners that `ci.yml` used then. They are kept as a record. The
release now runs on a `v*` tag push and publishes unsigned packages on a Release of this
repository (see [Releases](#releases)); `v0.1.0` was the first.

| Check | How | Result |
|---|---|---|
| CI passes on `main` | `gh run list --workflow ci.yml --commit <sha>`, then `gh run view <id> --json jobs`: the `check` job and five `build` jobs succeed | Passed: check and all five build jobs (CI run 42402231647, commit 86ed61d) |
| Static CRT on Windows | "Inspect DLL imports with dumpbin (Windows)" step in the two Windows build jobs | Passed on both Windows targets (CI run 42402231647) |
| Release skeleton | `gh workflow run release.yml --ref main`, then `gh run download <id>`: each `plugins-<platform>` artifact holds the archives, `SHA256SUMS-<platform>.txt`, and `plugins-<platform>.tsv`. The signing step logs contain `unsigned:` | Passed: five artifacts, each with 7 archives, the SHA-256 list, and the TSV; `sha256sum -c` passed; every job logged `unsigned:` (release run 42402274255) |
| No release created | `gh release list` is empty | Passed: no releases |
| Extra system packages | Whether the Linux runners need packages beyond the image defaults. A local Linux ARM64 build needed none | None needed on the GitHub-hosted Linux runners of that time (CI run 42402231647). Since the plugin sources moved here, the plugin tests need WebKitGTK 4.1 and `xvfb` on the `check` runner (see [What the runners need](#what-the-runners-need)); the `build` jobs still need none |
