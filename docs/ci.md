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
| `.github/workflows/ci.yml` | push to `develop`, `release`, or `main`, pull request to one of them, manual | `check` job: fmt, clippy, test on Linux, then the WebKitGTK tests. `test` job: clippy, unit tests, and the `plugin-guards` source checks on Windows x64 and macOS arm64, and on macOS also the `repo-gates` integration tests. `lint` job: shellcheck on every shell script and actionlint on every workflow. `audit` job: runs `cargo audit` on `Cargo.lock`. `licenses` job: checks the dependency licenses against `deny.toml` with cargo-deny. `build` job: builds the plugin crates for five platforms. On Windows it also checks that the binaries link the C runtime statically |
| `.github/workflows/release.yml` | push of a `v*` tag; manual (`workflow_dispatch`) | On a tag, publishes the plugins as a pre-release on a GitHub Release of this repository, never as Latest (see [Releases](#releases)). A manual run builds the same release files as workflow artifacts and creates no Release |
| `.github/workflows/promote.yml` | manual (`workflow_dispatch`) from `main` | Marks a published pre-release as Latest after a published Tasty release bundles it (see [Promotion](#promotion)) |
| `.github/workflows/plugin-version-check.yml` | push to `develop`, `release`, or `main`, pull request to one of them, for changes outside Markdown and `docs/` (the notice files `THIRD_PARTY_LICENSES.md` and `LICENSES/` count); manual | Checks that each plugin whose build output changed raised its version (see [Plugin version check](#plugin-version-check)), then runs the `repo-gates` tests |

The workflows use `permissions: contents: read` by default. In `release.yml`, only the
`create-release` and `publish` jobs get `contents: write`; in `promote.yml`, only the
`promote` job. In `ci.yml`, a new run waits
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

The `test` job runs on the Windows x64 and macOS runners. The Linux jobs that build
nothing for a platform ask for `[self-hosted, Linux]`, which both Linux runners carry, so
the x64 or the ARM64 runner takes each one, whichever is free:

| Workflow | Jobs on `[self-hosted, Linux]` |
|---|---|
| `ci.yml` | `check`, `lint`, `audit`, `licenses` |
| `plugin-version-check.yml` | `check` |
| `release.yml` | `create-release`, `release-meta`, `publish` |
| `promote.yml` | `promote` |

Only the `build` and `package` matrix entries are tied to a platform's runner. A job on
the shared label gives the same result on either runner:

- `check`: the plugin code branches on the operating system, not the CPU (no crate
  tests `target_arch`), so both runners run the same Linux code paths. `target/` is in
  the runner's own checkout and is removed by `actions/checkout` on every run.
- `lint` and `licenses`: `scripts/install-lint-tools.sh` and
  `scripts/install-cargo-deny.sh` pin the x86_64 and the aarch64 release with its own
  SHA-256 sum, and the tool cache folder names the architecture or triple. `deny.toml`
  sets no `targets`, so cargo-deny reads every platform's dependencies on either host.
- `audit`: `cargo audit` reads only `Cargo.lock`. `cargo-audit` is built from source into
  the runner's own tool cache.
- Plugin version check: `cargo tree --target all` and `cargo metadata --no-deps` do not
  depend on the host, and the judge (`strip-cfg-test`, built from source into the
  runner's own tool cache) compares source text.
- `release-meta`: `cargo metadata` without `--filter-platform`, the manifests, and the
  tasty checkout at the pinned rev; nothing depends on the host. `create-release` and
  `publish` run only `git`, `gh`, and `sha256sum`.

Each runner folder has its own tool cache and checkout, so nothing built for one
architecture is read by the other runner. The runners share no cache action (see
[Actions and cache](#actions-and-cache)); each machine's Cargo home keeps only downloaded
sources and the git checkouts of dependencies.

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
| All | Git, and rustup with the toolchain in `rust-toolchain.toml` (Rust 1.96.0 with `rustfmt` and `clippy`). For `release.yml`, GNU tar or bsdtar, `gzip`, `od`, `awk`, and `sha256sum` or `shasum` (`scripts/package-plugin.sh`; Git for Windows provides them on Windows, but MinGit lacks `od`). On Linux the jobs add `$HOME/.cargo/bin` to `PATH`, as tasty does |
| Windows (both) | Git for Windows, whose bash runs `scripts/check.sh` (bash need not be on `PATH`: the jobs find `bin\bash.exe` from the location of `git` and put that folder first on `PATH`, ahead of the WSL bash in System32), PowerShell 7 (`pwsh`), and Visual Studio Build Tools with the MSVC x64 tools, `vswhere.exe`, and `dumpbin.exe` |
| `[win-arm64]` | In addition, the MSVC ARM64 build tools (C dependencies are compiled for ARM64) and the `aarch64-pc-windows-msvc` Rust target installed for the toolchain above |
| `[self-hosted, Linux, X64]` and `[self-hosted, Linux, ARM64]` | Both Linux runners take the jobs on `[self-hosted, Linux]`, so both need everything here. For `plugin-version-check.yml`, `rustfmt` from the toolchain and access to GitHub to install `strip-cfg-test` and to fetch tasty commits that the Cargo git cache does not have. `pkg-config` and the WebKitGTK 4.1 development files (`libwebkit2gtk-4.1-dev` on Debian and Ubuntu, which brings the GTK 3, JavaScriptCore, and libsoup 3 headers) for the `check` job, `xvfb-run` (`xvfb`) for the WebKitGTK tests, and a C compiler. The `audit` job installs `cargo-audit` into the runner tool cache. The `lint` job needs no installed tools: it downloads shellcheck and actionlint into the runner tool cache, which needs access to GitHub, `curl`, and `tar` with `xz`. The `release-meta` job of `release.yml` needs `jq` or Python 3 (`scripts/release-meta.sh`). The `create-release` and `publish` jobs need the GitHub CLI (`gh`) and `sha256sum`. The `promote` job of `promote.yml` needs `gh`, `sha256sum`, `awk`, and `sed`. `gh` must accept `--prerelease` on `release create` and `release edit`, and `--latest` on `release edit` |
| `[self-hosted, macOS, ARM64]` | The Xcode command line tools, and access to GitHub to install `strip-cfg-test` into the runner tool cache for the `repo-gates` tests |

## Tests by platform

| Platform | Job | What runs |
|---|---|---|
| `linux-x64` or `linux-arm64` | `check` on `[self-hosted, Linux]` | `scripts/check.sh test`: every test of every workspace crate that is not `#[ignore]` (the eight `#[ignore]` tests of `tasty-plugin-codex` and the two WebKitGTK tests do not run here) |
| `linux-x64` or `linux-arm64` | `check`, step "test (linux, webview layout)" | The two `#[ignore]` WebKitGTK tests of `tasty-plugin-markdown` under `xvfb-run`, with the same commands as tasty's `crossplatform-check.yml`. As in tasty, a failure is reported but does not fail the job yet |
| `windows-x64` | `test` on `[self-hosted, Windows, X64]` | `scripts/check.sh lint`, then `scripts/check.sh test --lib --bins --no-fail-fast`, then `cargo test --locked -p plugin-guards` |
| `macos-arm64` | `test` on `[self-hosted, macOS, ARM64]` | `scripts/check.sh lint`, then `scripts/check.sh test --lib --bins --no-fail-fast`, then `cargo test --locked -p plugin-guards`, then `cargo test --locked -p repo-gates` with `strip-cfg-test` from `scripts/install-judge.sh` |
| `windows-arm64` | `build` | Build only. The plugin code branches on the operating system, not the CPU, so the Windows x64 test run covers the same code paths |

The `--lib --bins` scope is the one tasty uses for its Windows and macOS unit tests.

The `repo-gates` integration tests run the plugin version check, its fixup, the git
hooks, and the output lock of `scripts/dev-bundle.sh` on fixture repositories. On macOS
they run those shell scripts with the BSD tools (`sed`, `awk`, `comm`, `mktemp`, `mv`)
and the bash on `PATH`, which Linux does not cover. The tests of `scripts/dev-bundle.sh`
run it with the system `/bin/bash` instead, the bash 3.2 that macOS ships (see
`crates/repo-gates/tests/dev_bundle_lock.rs`). Before them the job runs `cargo
fetch --locked`, because the fixtures resolve with `cargo --offline`, and installs
`strip-cfg-test` as `plugin-version-check.yml` does. They do not run on Windows.

Clippy (`scripts/check.sh lint`) runs natively on Linux (`check` job, on whichever Linux
runner takes it), `windows-x64`, and `macos-arm64` (`test` job). It does not run for
`windows-arm64`. The Linux run covers the Linux code paths for both Linux targets.

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
The download, checksum, and publish steps described below are in
`scripts/lib/tool-cache.sh`, which `scripts/install-cargo-deny.sh` uses too.

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
this crate and of `plugin-pin-check` only, so it never enters a plugin build. Function
bodies are found with the span reader of this crate (`plugin_guards::fn_spans`). One check,
`no_local_doc_citation`, also runs `git` in a Git checkout to list the files and read
the index.

- `notify_mod_language_lists`: the language lists in the Claude mod's TypeScript (the
  notify heading, the queue heading, the gate reason, and the tests of both headings)
  match `tasty_i18n::BUILTIN_CODES`.
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
- `no_emoji_in_source`: no file whose path ends in `.rs` under `crates/` and no
  `tasty-plugin.toml` anywhere in the repository holds a code point in U+1F000..=U+1FAFF.
  Icons come from the design SVGs. The check walks the tree itself and follows folder
  links. It skips target, dist, and node_modules folders, git and worktree folders, the
  local agent folders, and folders marked as build caches.
- `no_clipboard_write`: the masked Rust code under `crates/`, read as tokens, holds none
  of the clipboard write calls listed in the test (arboard, Wayland, X11, macOS
  pasteboard, Windows, egui, and the copy paths tasty used). It follows `use ... as`
  renames of those functions and counts clearing an arboard clipboard. The test file
  (`SHAPES`, `offenders_in`) holds the exact rules. The one exception is the `set_text` of the claude
  screen test double in `error_scan.rs`, allowed only for its receivers inside the listed
  tests, each of which must still use it.
- `bundled_namespace_dispatch`: every host method in the `METHOD_TABLE` of the pinned tasty
  commit whose prefix a plugin declares as its `ipc_namespace` has a string arm in that
  plugin's `handle_ipc_method` (its body found by `plugin_guards::fn_spans`), and the
  compared method set matches the record for the pin. No Rust file (extension `rs`) under the `src/` of any plugin defines that function
  more than once.
- `manifest_cli_catalog_keys`: each non-empty CLI description and argument help in a
  plugin manifest has its translation key, the English catalog value equals the manifest
  text, and the catalog of every built-in language loads a non-blank value.
- `no_local_doc_citation`: no text file of the repository cites a document other
  developers cannot open. What follows is a summary. The exact shapes, with their
  separators, boundaries, character ranges, and case, are defined by `PATTERNS` and the
  find functions in the test file. The letters, digits, and case they check are ASCII.
  The ten patterns look for:
  - `todo-number`: a TODO followed by a number, in any case, or a number with the
    Korean ordinal suffix `번` before the TODO.
  - `conductor-ticket`: a numbered ticket of the local conductor list.
  - `workspace-subdir`: a path from the local work folder into its TODO, plan, or
    conductor sub folders.
  - `dated-slug`: a design changelog slug, a date and a word joined by hyphens.
  - `anchor-number`: a TODO number inside a `#` anchor.
  - `local-folder`: a mention of the local agent folders. The shorter name counts only
    with its leading dot, and only it has a home exemption, because a user setting
    folder of that name sits in the home folder: it is allowed right after a home prefix
    such as `~` or `$HOME`, or with a home word shortly before it. An absolute home path
    such as one under `/Users` is not a home prefix.
  - `prose-todo`: upper case TODO that is not a task marker such as `TODO:` or
    `TODO(scope):`, in files whose name does not end in `.md` (in any case). It matches
    the string, so an identifier that holds it counts too.
  - `split-number`: a zero-padded split number, a zero and a nonzero digit in round
    brackets.
  - `plan-coordinate`: a plan coordinate of upper case letters and a number joined by
    dots.
  - `round-number`: a review round, an upper case R followed by two or more digits.

  `.gitignore` may hold the local folder pattern, and the test file may hold the prose,
  split number, plan coordinate, and round number patterns; each exemption is for that
  exact path. In a Git checkout (a `.git` folder or file at the root) the paths come
  from Git: the tracked paths and the untracked paths that Git does not ignore. Files
  with a binary extension and the vendored minified bundles of the markdown plugin are
  left out; files without an extension and the plugin's `assets/NOTICE.md` are read. A
  tracked path is read from the index, a tracked link as its target text, and a
  submodule entry is not read. A tracked regular file is also read from the work tree
  when its text, with line endings made the same, differs from the index copy and its
  path passes through no link. An
  untracked path is read only when it is a regular file, and it is left out when a part
  of it has one of the pruned names of this check (build, dependency, editor and tool
  folders, and the local agent files and folders) or when it sits under a folder marked
  as a build cache. Without Git the tree is walked, skipping the same names and the
  folders marked as build caches. A file that is not UTF-8 is listed and fails the
  check, and a file that cannot be read fails it with the error. The vendored list is
  checked as well: each listed bundle is a file, the markdown `assets/` folder holds no
  unlisted `.min.js` or `.min.css` file, and no other unlisted path with such a name is
  tracked, or untracked and not left out by the pruned names and cache markers above
  (without Git, found by the walk).

The checks that walk the sources fail when they find fewer files or plugins than a floor
measured in this repository, so an empty walk does not pass.

### Checks tasty ran on the copies and this repository does not run

- Gate threshold copies: tasty compared copies of its gate thresholds with the files that
  define them. None of those files exist here, and the one numeric gate value here, the
  license `confidence-threshold` in `deny.toml`, is already fixed by the `repo-gates`
  deny policy test.
- File size limits: tasty's exceptions and frozen budgets for the markdown renderer are
  members of its file size gate. This repository has no file size gate to move them into.
- Hook commands on the recording path: the check depends on tasty's CLI code to tell
  which methods are hooks. tasty still checks the plugin manifests when it takes a new
  release, and no hook subcommand here declares the fields that would leave the path.
- CLI entry points of host methods: that check judges tasty's own documentation and method
  table. The plugin manifests are only its input, so there is nothing to judge here.

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

## License check

The `licenses` job in `ci.yml` checks the licenses of every package in the dependency
graph (all features, every target, dev-dependencies included) against the policy in
`deny.toml`. It runs `scripts/install-cargo-deny.sh` and then `scripts/check.sh licenses`,
which runs `cargo deny --locked --workspace check licenses`. `--workspace` takes every
workspace member as a root.

- The allow list is the one in tasty's `deny.toml`, so the plugins and the host they ship
  with follow one policy. Entries that no crate here uses stay, and
  `unused-allowed-license = "allow"` keeps them from warning. Change the list in tasty
  first and copy it here.
- Only the license check runs. Advisories stay with the `audit` job, so cargo-deny does
  not download an advisory database, and bans and sources are not checked. A plain
  `cargo deny check` would run those checks with their defaults and is not the check of
  this repository. A `repo-gates` test (`deny_policy`) parses `deny.toml` and accepts
  only the pinned keys and values: `[graph]` with `all-features = true`, and `[licenses]`
  with `version`, `confidence-threshold`, `unused-allowed-license`, and `allow`, whose
  set must equal tasty's list. Any other table or key, such as advisories, bans, sources,
  exceptions, clarifications, private-crate skips, or graph exclusions, fails it. It also
  runs `scripts/check.sh licenses` with a `cargo` that records its arguments, with
  `GITHUB_ACTIONS` unset and set to `true` and with a fixed set of sample extra arguments.
  Each run must call `cargo` exactly once with `deny --locked --workspace check licenses`,
  and a failing `cargo` must fail the run. This does not prove every branch on other
  arguments; CI calls the command without any.
- No exceptions: `deny.toml` has no `[[licenses.exceptions]]` and no allow entry for one
  crate. When a crate fails the check, update it or replace the dependency that brings it
  in. When neither is possible, the maintainers decide; the check is not widened to pass.
- The result depends only on the commit, unlike `audit`, so it is a job of its own: a new
  advisory does not hide a license result.

`scripts/install-cargo-deny.sh` installs cargo-deny 0.20.2 from its GitHub release into
`$RUNNER_TOOL_CACHE/cargo-deny/cargo-deny-<version>-<triple>`, with the SHA-256 sums of
the Linux x86_64 and aarch64 musl archives pinned in the script, and never into
`~/.cargo/bin`. Outside CI it uses `$CARGO_DENY_CACHE`, else
`${XDG_CACHE_HOME:-~/.cache}/tasty-official-plugins/cargo-deny`. To update the tool,
change the version and both sums together; the release publishes a `.sha256` file next to
each archive. Run the check locally with
`PATH="$(bash scripts/install-cargo-deny.sh):$PATH" scripts/check.sh licenses`.

## Plugin version check

`plugin-version-check.yml` runs `scripts/check-plugin-version-bump.sh --range` on the two
commits of the event: the pull request base and head, or the push before and after
(`HEAD^` and `HEAD` for a manual run). It runs for every change except Markdown outside
`src`, `lang`, `assets`, `THIRD_PARTY_LICENSES.md`, and `LICENSES/`, and `docs/`. A newer
pull request run cancels the older one on the same ref. Push runs and manual runs are
neither cancelled nor replaced: each run has a concurrency group of its own (its run id),
because each one checks only its own before..after range and a cancelled or replaced run
would leave that range unchecked.

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
- Path names are read as they are, also outside ASCII. A name that git still prints
  quoted stops the check where it could be judged; `git_quotes` and its callers in the
  script, and the tests on quoted names, give the cases.
- Besides the plugin directories and their path dependencies, the check judges the root
  build inputs. A tasty pin move or a changed `[workspace.dependencies]` entry needs a
  bump of each plugin whose dependency closure holds a changed crate. A crate at the root
  of its git repository is compared as the whole repository (`judge_pin_moves`). A pin
  move whose changed paths git cannot list is undecidable, and a changed submodule or
  link without its target counts as changed (`pin_package_changed`). Its tests, benches,
  examples, and Markdown are left out, except a file that shipped code includes
  (`include_paths`). A crate that inherits from the root of its workspace is judged
  against the inherited sources of that root (`pin_root_inherited_changed`). A pinned
  crate whose `build` names a script next to the manifest of another package, or cannot
  be read, is undecidable (`pin_check_build_script`). An
  update of a crates.io crate in `Cargo.lock` needs one too, unless the plugin is already
  above its version at the tip of `origin/main` (the local `main` only when there is no
  `origin/main`). `main` moves only for releases and hotfixes, so its tip is the baseline.
  Neither tags nor the state of Releases are read, and the tip can be ahead of what is
  published: between a tag push and its publish, and while a pre-release waits for
  promotion. A local check uses the `origin/main` the clone last
  fetched: if it was not fetched after a release, the baseline is older and a second
  update can pass locally; the CI checkout fetches it on every run. `[workspace]` and
  unknown sections are undecidable, and so is a changed `[workspace.dependencies]` line
  that cannot be read or that no package in `Cargo.lock` answers to, or whose package
  name cannot be read (`dep_entries`). The table names are
  read as TOML keys, so quotes and spaces around the dots do not matter (tabs are not
  supported); `dep_entries` in the script decides which tables it reads. A root `Cargo.toml`
  with a multi-line string (`"""` or `'''`), with unbalanced brackets, or with a backslash
  escape in a table name or key (`[profile."rele\u0061se"]`; escapes are not decoded) is
  undecidable as a whole. The path patch check below reads the root `Cargo.toml` on every
  run, so while such a spelling is in it, every commit is undecidable, also one that does
  not touch the file. This is the policy: the repository does not use these spellings now,
  and they stay out of the root `Cargo.toml` and the cargo config.
- Build settings change every plugin binary, so a change asks every plugin with a
  `tasty-plugin.toml` for a bump, once per release (the rule of a `Cargo.lock` update):
  a `[profile.*]` table in the inheritance chain of the release profile `dist` (now
  `dist` → `release`), `.cargo/config.toml` or `.cargo/config` and the files they
  include, and `rust-toolchain.toml`. The chain is followed on both sides through
  `inherits`, over the root `Cargo.toml` and the cargo config, which wins. A profile
  outside the chain (`dev`, `test`), a cargo config change only in `[alias]`, `[term]`,
  `[net]`, or `[http]`, and a toolchain change only in `components` or `profile` pass
  with a note. A chain that cannot be followed (`dist` missing, a user profile without
  `inherits`, a cycle, a profile that cannot be read) and an include outside the
  repository or with a glob are undecidable. The gate, `release.yml`, and
  `scripts/package-plugin.sh` name the same release profile; a `repo-gates` test checks
  it.
- Archive inputs outside the crates, listed in `scripts/lib/archive-inputs.txt`, also ask
  every plugin with an archive for a bump, once per release: the notice set (`LICENSE`,
  `THIRD_PARTY_LICENSES.md`, every file in `LICENSES/`), `.gitattributes`, and the
  packaging code (`scripts/package-plugin.sh`, `scripts/lib/notice-set.sh`,
  `scripts/lib/plugin-build.sh`, `scripts/lib/archive-modes.sh`, `scripts/check.sh`, and
  the `package` job of
  `release.yml` with the top-level `env` and `defaults` it inherits). A file directly in
  `crates/`, outside every crate (`crates/.gitattributes`), counts the same way, because
  it decides how the files of every crate check out.
- The notice set and `.gitattributes` are compared by path, file mode, and blob, so a
  move, a mode change, or a file that becomes a link counts. A symbolic link among them
  (a root notice file, or a file in `LICENSES/`) is undecidable on every run: the
  packaging copies through the link, so its target could change unseen. The scripts and
  the text of the regions are compared as they are, so a change of a comment or a blank
  line also asks every plugin with an archive for a bump, once per release. Whether a
  line that starts with `#` is a comment or data of a string, a here-document, a continued
  line, or a YAML block scalar takes a shell or YAML parser to tell, and a review changed
  the build through one such line in a multi-line `awk` program. Region markers and the
  end of a top or job region may still carry a trailing comment. Comment lines no deeper
  than the key of a top or job region, and blank lines, right before its end belong to
  what follows (the head comment of the next job, a comment before `jobs:`), not to the
  region; a deeper comment there could be data of a block scalar and counts.
- `scripts/check.sh` is compared whole, like the other scripts: any change, also one of a
  subcommand that does not build, like `licenses` or `audit`, or of the hook warning,
  asks every plugin with an archive for a bump, once per release. Leaving parts of
  `check.sh` out by text matching could not be made safe: a one-line help branch, a function closed by an indented brace,
  or compared code that calls a left-out function could change the build unseen. The hook
  warning runs as `( warn_hooks ) || true`, so it cannot change variables or functions of
  the shell that builds; that keeps the warning from changing the build shell and is not
  a rule of the version check.
- On every run, not only when they change, these are undecidable:
  - a `source` or `.` in a listed script or shell region file (`check.sh`,
    `package-plugin.sh`, `notice-set.sh`, `plugin-build.sh`, `archive-modes.sh`) of a file
    the list does not
    name, or of a path that is not written out (a variable or a command substitution;
    none is in use now). A command position is the start of a line (also of a line inside
    `$( )`, `<( )`, `>( )`, or a backtick substitution, but not inside arithmetic), a
    place after one of `; & | ( )` or the words `{` and `}`, the start of `$( )`, `<( )`,
    `>( )`, or a backtick substitution, the place after the `)` of a case pattern, and a
    place after `then`, `do`, `else`, `elif`, `if`, `while`, `until`, `!`, `coproc` (and
    its name), `time`, `exec`, `builtin`, `command` (and their options that start with
    `-`, `--` too), an assignment (also `a[0]=` and `+=`), or a redirection (`> /dev/null`, `2>&1`,
    `</dev/null`, `&>x`, with the target attached or next). Quoted text, text after an
    unquoted `#`, braces inside a word (`find ... -exec rm {} \;`), the words after a
    substitution that ends inside a word (`echo $(pwd) a\ b`), and case patterns are not
    commands, so a comment or a string that mentions a source does not stop the check.
    Here-documents start at `<<WORD` or `<<-WORD`, several on one line, also with `<<`
    attached anywhere in a word (`cat<<EOF`, `cmd>f<<EOF`; not `<<<`). When the end word
    has a quote or a backslash (`<<'EOF'`, `<<"EOF"`, `<<\EOF`), the body is data, as in
    bash. When it has none, the body is text, but bash expands `$( )`, backticks, and
    `${ }` in it, so those are read as outside quotes (a source in `$(. x)` in the body
    is found); quotes in the body are plain characters, `\$` and `` \` `` are escaped,
    and the end word counts only outside those substitutions. A here-document without
    its end word stops the check, and so do a quote and a substitution, arithmetic, or
    `${ }` still open at the end of the file (the message names the file and the line
    where it opens): the check could not tell where the text or the commands in them
    end. Arithmetic (`$(( ))`, `(( ))`, `for (( ))`) is not commands either: `<<` and
    `<<=` in it are shifts, not here-documents, and its parentheses are counted, while a
    `$( )` inside it is read as commands. Inside an unquoted `${ }` (`${x:-$(. x)}`), a
    `$( )` and a backtick substitution are read as commands too; the rest of `${ }`
    (`${#x}`, `${x%%*/}`, `${x:-a b}`) is one word. A quoted target
    (`. "scripts/lib/x.sh"`) is read as its path. `release.yml` is no shell region file
    and is not read this way.
  - a word in command position written with a backslash or with `$'...'` or `$"..."`
    quoting (`\. x`, `sou\rce x`, `$'.' x`). The check does not take such words apart;
    the message names the file and the line.
  - a `uses:` key in the `package` job of `release.yml` whose value is not
    `owner/repo@ref` or `owner/repo/path@ref` on the same line (quotes and a trailing
    comment are fine). A local action or reusable workflow (`./`), a Docker image, a value
    on the next line or in a block scalar, and a flow mapping are undecidable, because
    the content they run would not be judged. So are YAML forms in that job that could
    spell a `uses:` key another way. Each line is read as indentation, `- ` items, a key,
    and a value, and only that structure is checked: an explicit key (`? uses`), a
    double-quoted key with a backslash escape (`"u\x73es"`), a merge key (`<<:`), a key
    or an item that starts with a tag (`!!str uses:`), an anchor (`&name`), an alias
    (`*name`), or a flow collection, and a value that starts with one of these. So is a
    quote in the place of a key (at the start of a line or after `- `) that its line does
    not close: a quoted key does not run over several lines in a workflow. A one-line
    flow sequence of plain words (`[self-hosted, Linux, X64]`, as in the runner lists of
    the matrix) cannot hold a key and passes. Text inside a scalar value (JSON in a `run:`
    line, `"a: *b"`, `*.sha256,*.txt`) is not read, nor are the body of a block scalar
    (`run: |`, skipped by its indentation) and the rest of a quoted value over several
    lines.
  - a change under `crates/<dir>/` where `<dir>` has no `Cargo.toml` on either side. Such
    a folder is no crate, so no plugin version would cover the change. A file directly in
    `crates/` stays an archive input (above).
- The gate is there so that honest changes do not miss a bump. A change that twists shell
  or YAML spelling on purpose to slip past the checks is for review, shellcheck, and
  actionlint to stop. Spellings that cost little to close are closed as undecidable (the
  odd command words and YAML forms above); the gate does not parse shell or YAML in full.
- A file that a listed script runs in another way than `source` or `.` (`bash x.sh`,
  `eval "$(cat x)"`) is judged only through the line that calls it: the commit that adds
  the call needs a bump, but later changes of that file alone are not seen. Review adds
  such a file to the list. Today the only such call runs `scripts/check.sh`, which is
  listed. The same holds for a source inside a quoted command substitution
  (`"$(. x.sh)"`, also inside a quoted `"${x:-$(. x.sh)}"`), since quoted text is not read
  as commands. A here-document opened inside a substitution in the body of another
  here-document is not modelled, and the check can misread the lines after it; none is in
  use now.
- The list is read from both trees that are compared: the before rev, and the after rev or
  the index. Its blob and script lines count from either side, so a commit cannot drop an
  input from the list and change it at once. When the region lines of a path differ
  between the two lists, that path counts as changed. An `except` line, which an earlier
  form of the list used to leave parts of a script out, is an unknown kind in the after
  list (undecidable) and is dropped from the before list. A side without the list (a
  commit before it existed) uses the list of the other side. A region of `release.yml`
  that cannot be found is undecidable; a trailing comment on a marker, or on the key line
  of another job, does not move a region.
- In a crate, `lang/`, `assets/NOTICE.md`, and `.gitattributes` files are compared by mode
  and blob, so a final newline counts, and the mode of `tasty-plugin.toml` counts too;
  they ask only for a bump of that plugin. The archive bytes are not compared: tar keeps
  file times, so the same content packs differently every time.
- A `[patch]` entry of the root `Cargo.toml` is judged by the change it makes in
  `Cargo.lock`: a new or moved git source needs a bump of each plugin that links the
  crate on every commit, and a move to crates.io once per release, as above. When the
  lock rows of the patched package stay the same, no bump is needed. A quoted key in an
  entry (`{ "path" = ... }`) reads like the bare key. The `path` and `package` of an entry
  are read as its own fields, and a `package` that cannot be read is undecidable
  (`field_value` and `judge_patch_entries` in the script). A patched package in no lock, two
  git sources of one package name, a key with a backslash escape, and a `[patch]` spelling
  other than `[patch.<source>]` entries, dotted keys, or `[patch.<source>.<name>]` tables
  are undecidable.
- A path `[patch]` in the root `Cargo.toml` or in the repository cargo config (and what it
  includes), and a `paths` override, are undecidable on every check, not only when they
  change: a clean clone and CI do not have the folder. Use git with the pinned rev, or
  `scripts/dev-bundle.sh --tasty`, which passes a local tasty copy with `cargo --config`
  in a copy of the repository. Two `pin` tests in `crates/plugin-pin-check/tests/pin.rs`
  reject the same: `no_path_patch_in_the_committed_cargo_config` reads the declarations,
  and `no_path_sources_outside_the_repository` fails on any package without a source whose
  manifest is not a tracked file of this repository. They run in the `check` job of
  `ci.yml` (`scripts/check.sh test`) on every push and pull request.
- A plugin's dependency closure is its normal and build dependencies for all targets
  (`cargo tree --target all`), so a crate that only Windows or macOS links counts on the
  Linux runner too. Every judgement that reads a closure (a change in a path dependency
  crate, a pin move, a root dependency change, a `Cargo.lock` change) reads the crates of
  every target offline, so they must be downloaded first. A build fetches only the host's
  crates; `scripts/dev-setup.sh` runs `cargo fetch --locked` once, and after a dependency
  change a local check may need it again.
- Two `pin` tests in `crates/plugin-pin-check/tests/pin.rs` tie the icon notice to its
  sources: `icon_license_texts_match_tasty_at_the_pin` compares the Lucide and Feather
  texts and the Lucide version with tasty at the pin, and
  `lucide_row_names_the_plugins_that_link_tasty_icons` compares the plugins the Lucide row
  of `THIRD_PARTY_LICENSES.md` names with the plugins whose closure, read the same way,
  holds `tasty-icons`. The second reads every target offline, so the `check` job of
  `ci.yml` runs `cargo fetch --locked` before the tests, and a local run needs it too. A
  crate that is not downloaded fails the test with that hint; it is not skipped.

## Static C runtime on Windows

`.cargo/config.toml` adds `-C target-feature=+crt-static` for both Windows MSVC targets,
so the plugins run without the VC++ Redistributable. Changing the file bumps every plugin
(see [Plugin version check](#plugin-version-check)). On Windows, the CI build job
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
   plugin. Windows targets also get the PDB that the MSVC linker writes next to the
   executable, under the name the executable records. `<id>` is the manifest `id`, for
   example `com.tasty.claude`. Then, on Linux and macOS, the staged folder gets mode 755
   for its folders and executable and 644 for every other file, whatever the source modes
   and the umask are. On Windows, `chmod` cannot change the modes that Git Bash reports, so
   this holds only on Unix; there `pack` writes the archives under umask 022 and checks the
   modes, as described below.
2. `pack`: one `<id>-<version>-<platform>.tar.gz` per plugin. `<version>` is the
   manifest `version`.
3. `pack` then writes `SHA256SUMS-<platform>.txt` (the `sha256sum` format) and
   `plugins-<platform>.tsv` with the columns `id`, `version`, `platform`, `file`,
   `sha256`.

The packages are not signed. `pack` fails if the stage holds a `.sig` file or if an
archive contains one. It also fails if an archive lacks a file of the notice set, or the
`NOTICE.md` of a crate that has one, or if a `windows-*` archive does not hold exactly one
PDB next to the executable, or another archive holds one.

`pack` also checks the mode and kind of every archive entry, as read by `LC_ALL=C tar -tzf`
and `tar -tvzf` line by line:

| Entry | Mode |
|---|---|
| A folder (`<id>/`, `lang/`, `LICENSES/`) | `drwxr-xr-x` |
| `<id>/<command>`, the `[entry]` command of the manifest, with `.exe` on `windows-*` | `-rwxr-xr-x` |
| Every other file (manifest, `lang/`, notice set, `NOTICE.md`, PDB) | `-rw-r--r--` |

A link or another kind of entry fails, and so does an archive without the entry command.
`tar -tvzf` shows a hard link with `link to` after its name, which also fails. On Windows,
Git Bash makes the modes up from the file names and contents and takes the umask off them,
and `chmod` does not change them, so `pack` writes the archives under umask 022 and logs
the tar version and the caller's umask once. The functions are in
`scripts/lib/archive-modes.sh`, and `repo-gates` tests them.

The archives are POSIX USTAR, and every entry has uid 0, gid 0, and no user or group name,
so no account name of the build machine is stored, and an extract as root gives the files
to root. `pack` runs GNU tar with `--format=ustar --owner=0 --group=0 --numeric-owner` and
bsdtar with `--format ustar --uid 0 --gid 0 --numeric-owner --no-mac-metadata`, with
`TAR_OPTIONS` empty (an owner map there overrides `--owner`) and `COPYFILE_DISABLE=1`;
another tar stops it, and so does a missing `gzip` or `od`. A listing shows a stored name
`0` the same as no name, so `pack` reads the headers instead, with `gzip` and `od`:

- every header is USTAR (magic `ustar\0`, version `00`) with a right checksum and octal
  numbers, and has type `0` (file) or `5` (folder, size 0);
- a PAX or GNU header, a file whose stored path ends with `/`, and an AppleDouble entry (a
  last path part that starts with `._`) fail;
- every entry has uid 0, gid 0, and user and group names of NUL bytes only;
- the archive ends with two zero blocks, followed by zero bytes only, and holds as many
  headers as `tar -tzf` lists names;
- a failed `gzip -dc`, and an `od` that exits nonzero or prints fewer bytes than the
  archive holds at that point, fail the check.

The errors of this check never print a stored name, as release logs are public; the mode
check above prints the listing line. A path fits USTAR when it fits the 100-byte name
field, or splits at a slash into a prefix of up to 155 bytes and a name of up to 100; the
trailing slash of a folder counts as part of the name. GNU tar stops at a path that does
not fit, while bsdtar skips the entry and exits 0, so `pack` also compares the number of
entries in the archive with the stage.

The script finds each executable through the `executable` field of cargo's
`compiler-artifact` messages, and the PDB through the `.pdb` entry of the same message's
`filenames` list. It copies the manifest and `lang/` from `crates/<crate>/`
in this repository.

Example:

```
scripts/package-plugin.sh all --all --platform linux-x64 --target x86_64-unknown-linux-gnu --out target/package
```

Each archive holds one folder, `<id>/`. For example,
`com.tasty.claude-0.3.11-linux-x64.tar.gz` lists:

```
com.tasty.claude/
com.tasty.claude/LICENSE
com.tasty.claude/THIRD_PARTY_LICENSES.md
com.tasty.claude/LICENSES/
com.tasty.claude/LICENSES/KaTeX-MIT.txt
com.tasty.claude/LICENSES/mermaid-MIT.txt
com.tasty.claude/LICENSES/Ubuntu-Ubuntu-font-1.0.txt
com.tasty.claude/LICENSES/emoji-icon-font-MIT.txt
com.tasty.claude/LICENSES/NotoEmoji-OFL-1.1.txt
com.tasty.claude/LICENSES/Feather-MIT.txt
com.tasty.claude/LICENSES/Lucide-ISC.txt
com.tasty.claude/LICENSES/Hack-MIT-Bitstream-Vera.txt
com.tasty.claude/LICENSES/highlightjs-BSD-3-Clause.txt
com.tasty.claude/tasty-plugin-claude
com.tasty.claude/lang/
com.tasty.claude/lang/ja.toml
com.tasty.claude/lang/en.toml
com.tasty.claude/lang/zh-Hans.toml
com.tasty.claude/lang/ko.toml
com.tasty.claude/tasty-plugin.toml
```

The entries under `LICENSES/` come in the order the staging folder lists them, which
depends on the file system.

The markdown archive also lists `com.tasty.markdown/NOTICE.md`. A Windows archive also
lists the PDB beside the executable, for example `com.tasty.claude/tasty_plugin_claude.pdb`
next to `com.tasty.claude/tasty-plugin-claude.exe`. The release profile keeps the
symbol table and strips only debug info, so a plugin crash backtrace shows function
names on Linux and macOS, and the PDB gives Windows the same.

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

The repository version is in `VERSION` (`MAJOR.MINOR.PATCH`, a SemVer core version
without leading zeros). A release is the push of the tag `v` + `VERSION` on a `main`
commit. The tag goes on the commit that bumps `VERSION`: `create-release` uses the body
of the tag commit as the Release notes. The release steps are in
[branching.md](branching.md).

A release has two stages:

1. The tag push publishes the Release as a **pre-release**. GitHub never makes a
   pre-release Latest, so a person who downloads "the latest plugins" does not get files
   that no released Tasty reads yet. The files are public, so tasty's packaging, which
   downloads by tag without authentication, can bundle them. A draft would not do: its
   files cannot be downloaded without authentication.
2. After a published Tasty release bundles the tag and the release has been verified
   with it, `promote.yml` marks it Latest and records the Tasty versions in its notes
   (see [Promotion](#promotion)). This is the only path to Latest.

### Jobs

| Job | Runner | Permissions | What it does |
|---|---|---|---|
| `create-release` | `[self-hosted, Linux]` | `contents: write` | Tag runs only. Fails unless the tag commit is an ancestor of `origin/main` and the tag equals `v` + `VERSION` in the form `scripts/release-check.sh tag` accepts. Fails if a published Release with the tag exists. Reuses a draft Release only if the `Commit:` line of its marker block names the same commit, and fails otherwise. Without a Release, creates a draft pre-release whose notes end with the marker block (see [Release notes](#release-notes)) |
| `release-meta` | `[self-hosted, Linux]` | `contents: read` | Writes `release-meta.toml` with `scripts/release-meta.sh` and uploads it as the artifact `release-meta` |
| `package` | the five platform runners | `contents: read` | Builds and packages the eight plugins and uploads the artifact `plugins-<platform>` |
| `publish` | `[self-hosted, Linux]` | `contents: write` | Tag runs only. Downloads the artifacts. For each platform, reads `SHA256SUMS-<platform>.txt` with `scripts/release-check.sh sums`, the line check that check 7 of promotion also uses, checks that the list names exactly the archives of the artifact (read with `scripts/release-check.sh archives`, which also refuses an artifact file name that such a list cannot hold or that starts with a dot), runs `sha256sum -c`, and checks that there are 8 archives. Checks `plugins-<platform>.tsv` against that list with `scripts/release-check.sh rows`, the row check that check 7 also uses. Then checks that `release-meta.toml` names this tag, commit, and the 8 plugins, and that there are 51 files. Uploads only these checked files to the draft. Settles and checks the marker block against `release-meta.toml`, then publishes the Release as a pre-release with the checked notes. Last, checks that the Release is a published pre-release and not Latest |

On a tag run, `release-meta` and `package` start only after `create-release` succeeds.
`publish` checks out the tag commit only for `scripts/release-check.sh`.

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
There is no merged hash list or TSV across platforms. This URL works the same for a
pre-release and for Latest, so tasty, which pins a tag in its lock file, needs no change
for either stage.

### Release notes

The notes start with the body of the tag commit, which is free text. They end with a
marker block, the last paragraph, which the release scripts read and write:

```
Commit: <commit of this repository the tag points at>
tasty rev: <tasty commit the plugins were built from>
Requires Tasty: pending (a Tasty release that includes tasty commit <tasty rev>)
```

Promotion replaces the pending line with `Requires Tasty: <version> or later` and adds
`Verified with Tasty: <minimum> (minimum), <bundling> (bundled) on <platforms>`. It does
not change the `Commit:` and `tasty rev:` lines; after promotion, `tasty rev:` is the only
build pin left in the notes. The pending line does not name a Tasty version, because no
Tasty release bundles the plugins when the tag is pushed.

`scripts/release-check.sh` is the one parser of the block: `create-release`, `publish`,
and `scripts/release-promote.sh` call it. Its head comment describes the block and each
command, and `crates/repo-gates/tests/release_check.rs` pins what it accepts and refuses.
Its `sums` command is likewise the one reader of the lines of a `SHA256SUMS-<platform>.txt`
list, and its `rows` command the one reader of the rows of a `plugins-<platform>.tsv` list,
for `publish` and for check 7 of promotion. Its `archives` command reads the file names of
an artifact for `publish`.
Before publishing, `publish` makes the block hold exactly one pending line (adding it to
a reused draft that lacks it, keeping one of identical copies) and checks the `Commit:`,
`tasty rev:`, and pending pin against the `release-meta.toml` of the run. A block it
cannot settle or that does not match keeps the Release a draft.

### Failures and reruns

If `create-release` fails, no Release is created. If a later job fails or is cancelled
before the publish request, the Release stays a draft. In both cases nothing is
published. A rerun of the workflow for the same tag reuses the draft. `publish` does not
overwrite an asset: an asset already on the draft is skipped if its SHA-256 equals the
new file, and the job fails if it differs. The job also fails if the draft has an asset
that is not one of the 51 files. In that case, delete the draft (or the asset) by hand
and rerun.

A failure after the publish request (in "Check the published state") leaves the Release
published. A rerun does not repair it, and neither path writes to the published
Release: a rerun of the whole workflow stops in `create-release` ("already published"),
and a rerun of the failed jobs stops in the upload step of `publish` ("not a draft").
Instead, read the state, notes, and files of the Release and decide:

- **Only the state is off.** The marker block and the files match the run: one `Commit:`
  line with the peeled tag commit and the `commit` of the run's `release-meta.toml`, one
  `tasty rev:` line and one pending line pinned to its `tasty_rev`, and the 51 published
  files with the names and hashes of the run's checked files. If the state is also right
  (a pre-release, not Latest), the release is done; record it. If it is not a pre-release
  or is Latest, set it back to a pre-release by hand (`gh release edit <tag>
  --prerelease`) and check Latest again. Do not upload files again.
- **The notes or files do not match.** Setting the state back does not fix the notes or
  files, so the tag does not count as released. Keep the published notes and files as
  they are; if the state is off, set it back to a pre-release only. Promotion refuses
  such a Release on its own (check 7 of `scripts/release-promote.sh` compares the files
  with those the tag commit packages, and the marker block as `publish` does), so it
  never becomes Latest. Report the mismatch and
  release the fix under the next patch tag.

A rerun uses the workflow file of the tag commit. Runs of tags made before the
pre-release stage (`v0.1.0` to `v0.1.2`) keep their old `release.yml`; their Releases
are all published, so a rerun stops in `create-release`.

### Promotion

`promote.yml` marks a pre-release Latest. The plugins maintainer runs it when both of
these hold: a published Tasty release pins the tag in its `official-plugins.lock.toml`
(the tasty maintainers report the Tasty release), and the verification below passed.
Run it from `main`; the job fails on another ref:

```
gh workflow run promote.yml --ref main -f tag=vX.Y.Z -f min_tasty=A.B.C \
  -f new_tasty=A.B.C -f verified_on="linux-x64, macos-arm64, windows-x64"
```

| Input | Meaning |
|---|---|
| `tag` | The plugin Release to promote |
| `min_tasty` | The oldest Tasty release the Release supports. It is written as `Requires Tasty: <min_tasty> or later` |
| `new_tasty` | The Tasty release that bundles the Release |
| `verified_on` | The platforms where both Tasty releases were verified, as release platform names separated by commas. Promotion checks the names and that the required platforms of [Verification before promotion](#verification-before-promotion) are there; the head comment of `scripts/release-promote.sh` gives the form it takes |

The job runs `scripts/release-promote.sh`, which also runs locally with `gh` signed in
and `GH_REPO` set. It changes nothing unless all of its checks pass; its head comment
lists them, and `crates/repo-gates/tests/release_promote.rs` runs each refusal with a
fake `gh`. In short, the Release must be a published pre-release of the tag commit, the
two Tasty versions must be published Tasty releases with `min_tasty` not above
`new_tasty`, the lock file at the `new_tasty` tag must pin this repository, tag, and
commit with the same archive hashes, the current Latest must be older (or there is none),
and the published files must be exactly those the tag commit packages (its own plugin
list and manifests decide the archive names), with lists, `release-meta.toml`, lock
entries, and marker block that agree with them as `publish` checked, and the manifests of
the tag commit must require `min_tasty` as their `min_tasty_version`, unless the tag is
from before that key ([Minimum Tasty version](#minimum-tasty-version)). Then
one `gh release edit` writes the notes, clears the pre-release flag, and marks the
Release Latest, and the script checks that Latest is the tag.

- **Rerun.** If the edit went through but its reply was lost, run the same inputs again:
  a Release that is already Latest with exactly these lines succeeds without a change.
  Latest with other lines fails.
- **One at a time.** All runs share one concurrency group, so two promotions never read
  the same Latest and both pass.
- **Order.** A pre-release that is never promoted, because a newer Release was promoted
  first, stays a pre-release. An older tag is never made Latest again.
- **Where the workflow must be.** `workflow_dispatch` needs the workflow file on the
  default branch, `develop`, to be listed, and `--ref main` runs the scripts of `main`.
  The pre-release stage of `release.yml` and `promote.yml` with its script must both be
  on `main` before the first tag that uses them, and `promote.yml` also on `develop`.
  Otherwise a published pre-release has no way to Latest.
- **Checks of `main`.** For the same reason, a check that becomes stricter on `main`
  also applies to a pre-release published before the change. For example, check 7 now
  judges the TSV lists with `scripts/release-check.sh rows`, which refuses a TSV whose
  header line or columns differ from what `scripts/package-plugin.sh` writes; the older
  check took such a TSV.
- **Releases from before.** `v0.1.2` became Latest before this stage existed and stays
  Latest until a newer Release is promoted. Promoting `v0.1.2` fails at the first
  check: it is already Latest, and its notes do not hold the promoted lines. Promoting
  `v0.1.1` or `v0.1.0` fails at the next one: they are not pre-releases.

The supported minimum Tasty of a Release is, by default, the first Tasty release that
bundles it: `min_tasty` equals `new_tasty`. That version passed tasty's lock file checks
and was shipped with the plugins. An older version is named as `min_tasty` only after the
same verification on it.
The manifests carry the same version as `min_tasty_version`, by default the first
bundling release. To name an older version under the exception above, the manifests must
carry that older version too, so it is chosen before the tag: promotion refuses a
`min_tasty` that differs from the manifests, and changing their value takes a new tag.

### Verification before promotion

The verification uses the published pre-release files themselves. The bundling host is
the published package of `new_tasty`. When `min_tasty` is older, the pre-release
archives are installed by hand into the published package of `min_tasty`. The evidence
goes into the maintainer's release report; the Release notes keep only the versions and
platforms (`Verified with Tasty`). A platform in `verified_on` says that both `min_tasty`
and `new_tasty` were verified on it (one host when they are the same version). Promotion
does not check that the verification took place; the report holds it.

- **Hosts.** Evidence that needs a direct install (agent-stream, which has
  `bundle = false`, and every archive put into a `min_tasty` host) comes from a GUI host:
  `plugin install` needs the GUI, and a headless host cannot install. A headless host
  gives evidence only for the plugins already bundled in its package.
- **Direct install into an older host.** The host must not hold a bundled copy of the
  same plugin id: the GUI installs its bundled plugins at start, and a direct install
  into an id that already exists fails. Use a separate install state with no install
  record whose bundled plugin folder is empty (for example through
  `TASTY_BUILTIN_PLUGINS_DIR` pointing at an empty folder). Unsigned official archives
  need `--allow-unverified`.
- **Platforms.** At least one platform of each OS family: linux-x64, macos-arm64, and
  windows-x64. Promotion checks that `verified_on` names these three
  (`REQUIRED_PLATFORMS` in `scripts/release-promote.sh`, changed together with this
  list). For linux-arm64 and windows-arm64, the download with SHA-256 check and
  the packaging checks of the Tasty release CI stand in, for bundled plugins only: Tasty
  packaging downloads only plugins with `bundle = true`. For agent-stream, the archive
  check of `publish` on all five platforms covers the files, and install and run are
  verified only on the platforms where it was installed by hand. The report names those
  platforms and each platform that had the archive check only. Such a platform stays out
  of `verified_on`. When both hosts were verified on linux-arm64 or windows-arm64 too,
  that platform may go into `verified_on`, each one on its own.
- **Evidence per host and platform.**
  1. The plugin executable and `tasty-plugin.toml` in each installed plugin folder have
     the hash of the same files in the pre-release archive, for bundled and for directly
     installed copies.
  2. No manifest is refused: no `skipped` warning from the loader or the CLI dynamic
     commands, nothing in the discovery log, no `invalid plugin manifest` from
     `plugin install`, and no minimum version refusal. `plugin install` reports a Tasty
     older than the manifest's `min_tasty_version` before it validates the manifest, as
     an error of its own that names the required and the current version, not as
     `invalid plugin manifest`. All eight plugins carry one value for the whole Release;
     agent-stream is not in the bundle download (`bundle = false`), so only its direct
     install on the published package shows that the host accepts it.
  3. `plugin list` shows each bundled plugin as `connected` without `last_failure`, with
     the version of its `[[plugin]]` entry in `release-meta.toml`. agent-stream gets the
     same check, and the manifest check of item 2, after its direct install on a GUI host.
  4. One basic action per plugin, such as opening its surface kind or running one of its
     CLI commands.
  5. Each IPC path that this Release has and the previous Release did not, called at
     least once on the `min_tasty` host and on the `new_tasty` host. The path list is
     below.

  **New IPC paths (item 5).** The previous Release is the Latest Release when the report
  is written. Compare its tag commit (the `Commit:` line of its marker block, or the
  peeled tag for a Release without one) and its tasty rev (the root `Cargo.toml` of that
  commit) with those of this tag: the `crates/tasty-plugin-*/` diff (sources and
  `tasty-plugin.toml`) between the two tag commits, and the `crates/tasty-plugin-protocol`
  and `crates/tasty-plugin-sdk` diff between the two tasty revs. A path counts as new
  when it is one of these:

  1. A host method a plugin calls (`HostHandle::call` or `notify`, or a plugin helper
     around them) that is new, or whose sent or returned fields changed. An event key a
     plugin newly subscribes to or publishes on the SDK bus (`BusHandle`) counts too.
     `self_invoke` stays inside the plugin; the host calls of the handler it reaches
     count.
  2. An `ipc_method` or CLI subcommand that a manifest newly declares (host to plugin).
  3. A wire message that the protocol or SDK crate changed, or a host method the SDK
     newly calls on its own.
  4. A host-to-plugin path with the same method name and wire types whose caller,
     trigger, or receiving handler is new or changed. A `[[contributes.*]]` declaration
     that was removed, moved, or replaced by a declaration at run time, and a manifest
     value that names one, are the leads. Items 1 to 3 do not show it, so look for it on
     its own.

  The diffs are where the search starts, not the list: a grep of method names misses
  names on the next line or in variables. Follow each change from its caller or trigger
  through the wire message to the handler on the other side, and write it down as one
  path; the same method with another trigger or handler is another path. A wire value
  that the diff shows but no plugin of this Release sends or receives (such as a value
  only the host's own popups use) needs no call: the report names it, says that no plugin
  sends or receives it, and gives the host and plugin code that shows this.

  The report gives the Latest tag it compared with, the two commit and rev pairs, the
  path list, and the result of each path on each host. Each path has its own result: the
  success of another path that uses the same method does not stand in for it. With no
  new path, the report says 0 and gives a count of the same diffs that is not 0, such as
  the number of changed files. A call that differs by platform (code under `cfg`) is made
  on that platform. When there is no Latest Release, the report says so and every IPC
  path of this Release is in the list. A rerun after the edit went through (check 0) uses
  the report of the first run.

  The Latest Release can change between the report and the promotion. Before the
  dispatch, wait until every promotion that is running, waiting, or run locally has
  ended, then check that the Latest Release is still the tag of the report. If it is
  not, extend the path list and the evidence against the new Latest first. Until the
  dispatched run ends, maintainers agree among themselves on any other promotion: the
  concurrency group of `promote.yml` only queues workflow runs, and it does not see the
  tag a maintainer compared with or a local run. Whether the Latest Release is older than
  the tag is check 5 of `scripts/release-promote.sh`.

### Minimum Tasty version

A plugin manifest may name the oldest Tasty release that accepts the plugin with the
top-level key `min_tasty_version`. tasty defines the key and how a host judges it
([plugin system, 최소 Tasty 버전](https://github.com/zilhak/tasty/blob/develop/docs/features/plugin-system/index.md#최소-tasty-버전)):
a Tasty release that knows the key refuses a plugin that requires a newer Tasty, and
releases up to v0.14.0 ignore it.

- **Value.** The supported minimum Tasty that promotion will write as `min_tasty`: by
  default the first Tasty release that will bundle the Release, or an older release
  under the exception at the end of [Promotion](#promotion), chosen before the tag. All
  eight manifests carry the same value: a Release builds them from one tasty rev and its
  notes have one `Requires Tasty` line. The value changes with every Release. It goes in
  after the tasty maintainers have fixed the release number and before the tag, in the
  step of [Promoting to release](branching.md#promoting-to-release) that hotfixes share.
  A guessed number above the version of the Tasty release that bundles the tag stops
  tasty's release build when it fetches the plugins.
- **Format.** `crates/plugin-pin-check/tests/pin.rs` reads the eight manifests with
  tasty's parser (`Manifest::read` and `validate`), so with a pinned tasty rev that knows
  the key the format of the value is judged by the host's own code. Promotion checks only
  the version form.
- **Promotion.** Check 8 of `scripts/release-promote.sh` compares the value of each
  manifest of the tag commit with `min_tasty`. Its head comment and
  `crates/repo-gates/tests/release_promote.rs` are the reference for the manifest shapes
  it reads, those it refuses as unreadable, and tags from before the key. It does not
  judge whether a manifest is TOML: the check CI of the tag commit parses the manifests,
  and the tag push does not wait for it, so look at that CI run before promoting.
- **Limit.** Check 8 catches a value left from the previous Release only when `min_tasty`
  is the right new value: a stale value promoted with the same stale `min_tasty` passes.
  The update step before each tag is what prevents it.
- **A different release number.** With the default value, if Tasty is released under
  another number than the manifests carry, take the new number from the tasty
  maintainers, put it into the eight manifests, bump the eight plugins and `VERSION`, and
  release a new tag for tasty to pin; once a published Tasty already bundles the old tag,
  the new tag takes the number of the next Tasty release. Naming an older release instead
  falls under the exception at the end of [Promotion](#promotion). Which inputs promotion
  accepts is judged by the head comment of `scripts/release-promote.sh` and
  `crates/repo-gates/tests/release_promote.rs`.

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
tasty_rev = "ed10dc4eacf24b87f1398d227da5f1352ba79949"
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
