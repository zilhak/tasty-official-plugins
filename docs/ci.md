# CI and release workflows

This repository builds and tests the official Tasty plugins from the sources in
`crates/`, against the tasty crates pinned in `Cargo.toml`. The workflows call `scripts/check.sh` and `scripts/package-plugin.sh`, so
the steps that run in CI also run locally. The tasty sha is not repeated in any
workflow.

## Workflows

| File | Trigger | What it does |
|---|---|---|
| `.github/workflows/ci.yml` | push to `develop`, `release`, or `main`, pull request to one of them, manual | `check` job: fmt, clippy, test on Linux, then the WebKitGTK tests. `test` job: unit tests on Windows x64 and macOS arm64. `audit` job: runs `cargo audit` on `Cargo.lock`. `build` job: builds the plugin crates for five platforms. On Windows it also checks that the binaries link the C runtime statically |
| `.github/workflows/release.yml` | manual only (`workflow_dispatch`); packages of the released version are built with `--ref main` | Builds with the `dist` profile, packages each plugin, runs the signing steps (skipped without secrets), and uploads the result as workflow artifacts. It does not create a GitHub Release |

Both workflows use `permissions: contents: read`. In `ci.yml`, a new run waits for the
older run on the same ref instead of cancelling it, as in tasty: on the self-hosted
runners a run can take longer than the time between pushes, and a waiting run is
replaced by the newer push. Pull request runs cancel the older run. A run never waits
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

The `check` and `audit` jobs run on `[self-hosted, Linux, X64]`, and the `test` job on
the Windows x64 and macOS runners.

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
| All | Git, and rustup with the toolchain in `rust-toolchain.toml` (Rust 1.96.0 with `rustfmt` and `clippy`). For `release.yml`, `tar`, `sha256sum` or `shasum`, and `jq` or Python 3 (`scripts/package-plugin.sh`). On Linux the jobs add `$HOME/.cargo/bin` to `PATH`, as tasty does |
| Windows (both) | Git for Windows, whose bash runs `scripts/check.sh` (bash need not be on `PATH`: the jobs find `bin\bash.exe` from the location of `git` and put that folder first on `PATH`, ahead of the WSL bash in System32), PowerShell 7 (`pwsh`), and Visual Studio Build Tools with the MSVC x64 tools, `vswhere.exe`, and `dumpbin.exe` |
| `[win-arm64]` | In addition, the MSVC ARM64 build tools (C dependencies are compiled for ARM64) and the `aarch64-pc-windows-msvc` Rust target installed for the toolchain above |
| `[self-hosted, Linux, X64]` | `pkg-config` and the WebKitGTK 4.1 development files (`libwebkit2gtk-4.1-dev` on Debian and Ubuntu, which brings the GTK 3, JavaScriptCore, and libsoup 3 headers) for the `check` job, `xvfb-run` (`xvfb`) for the WebKitGTK tests, and a C compiler. The `audit` job installs `cargo-audit` into the runner tool cache |
| `[self-hosted, Linux, ARM64]` | A C compiler |
| `[self-hosted, macOS, ARM64]` | The Xcode command line tools |

## Tests by platform

| Platform | Job | What runs |
|---|---|---|
| `linux-x64` | `check` on `[self-hosted, Linux, X64]` | `scripts/check.sh test`: every test of every workspace crate that is not `#[ignore]` (the eight `#[ignore]` tests of `tasty-plugin-codex` and the two WebKitGTK tests do not run here) |
| `linux-x64` | `check`, step "test (linux, webview layout)" | The two `#[ignore]` WebKitGTK tests of `tasty-plugin-markdown` under `xvfb-run`, with the same commands as tasty's `crossplatform-check.yml`. As in tasty, a failure is reported but does not fail the job yet |
| `windows-x64` | `test` on `[self-hosted, Windows, X64]` | `scripts/check.sh lint`, then `scripts/check.sh test --lib --bins --no-fail-fast` |
| `macos-arm64` | `test` on `[self-hosted, macOS, ARM64]` | `scripts/check.sh lint`, then `scripts/check.sh test --lib --bins --no-fail-fast` |
| `windows-arm64`, `linux-arm64` | `build` | Build only. The plugin code branches on the operating system, not the CPU, so the x64 test runs cover the same code paths |

The `--lib --bins` scope is the one tasty uses for its Windows and macOS unit tests.

Clippy (`scripts/check.sh lint`) runs natively on `linux-x64` (`check` job),
`windows-x64`, and `macos-arm64` (`test` job). It does not run for `windows-arm64` or
`linux-arm64`.

The `check` job checks that the WebKitGTK 4.1 development files are installed on the
runner. The `tasty-plugin-markdown` Linux dev-dependencies (`webkit2gtk`, `gtk`,
`javascriptcore-rs`) link against them. The `build` job needs no extra packages,
because it builds no dev-dependencies.

## Actions and cache

- Actions are pinned to a commit sha, with the version in a comment
  (`actions/checkout` v7.0.1, `actions/upload-artifact` v7.0.2). Update the sha and the
  comment together.
- The workflows use no build cache action, as in tasty. `actions/checkout` cleans the
  runner's working directory with `git clean -ffdx`, which also removes `target/`, so
  every job builds from scratch. Downloaded crates stay in the runner user's Cargo
  home. `Swatinem/rust-cache` is not used because its save step cleans up the registry
  in that Cargo home, which tasty's runners on the same machine may share.

## Advisory check

The `audit` job in `ci.yml` installs `cargo-audit` at the version in `CARGO_AUDIT_VERSION`
into the runner tool cache (`$RUNNER_TOOL_CACHE/cargo-audit/<version>`) when it is not
there yet, and runs `scripts/check.sh audit`. To update the tool, change that value.

`cargo audit` reads `Cargo.lock` directly, so it also checks the crates.io dependencies that
only the bin-only plugin crates use. It matches advisories only against crates.io packages.
The `tiny_http` and `epaint` copies and the tasty crates come from git and are not matched.

Only a vulnerability fails the job. Unsound, unmaintained, and yanked results appear as
warnings in the job log.

The job fetches the advisory database on every run, and the database is not pinned. A new
advisory can therefore fail the job on a commit that changed nothing related. This is
intended, and `check` and `build` report their own results. Do not ignore a vulnerability:
update a crates.io crate here with `cargo update -p <crate>`, and fix a tasty crate in tasty
and move the pin. If the log shows that fetching the database failed, rerun
the job.

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
   `tasty-plugin.toml`, and `lang/`. `<id>` is the manifest `id`, for example
   `com.tasty.claude`.
2. Binary codesigning (workflow step, placeholder).
3. `sign-manifests`: write `tasty-plugin.toml.sig` next to each staged manifest.
4. `pack`: one `<id>-<version>-<platform>.tar.gz` per plugin. `<version>` is the
   manifest `version`.
5. `pack` then writes `SHA256SUMS-<platform>.txt` (the `sha256sum` format) and
   `plugins-<platform>.tsv` with the columns `id`, `version`, `platform`, `file`,
   `sha256`. The hashes cover the archives after signing.

The script finds each executable through the `executable` field of cargo's
`compiler-artifact` messages. It copies the manifest and `lang/` from `crates/<crate>/`
in this repository. Signing works on the staged copies, not on the manifests in
`crates/`.

Example:

```
scripts/package-plugin.sh all --platform linux-x64 --target x86_64-unknown-linux-gnu --out target/package
```

The archives are tar.gz on every platform, including Windows, so one format and one
hash list serve all platforms. The script needs bash, tar, `sha256sum` or `shasum`, and
`jq` or Python 3. On Windows it runs under Git Bash.

### Plugins in the packages

The release workflow follows tasty's current rule: plugins whose manifest says
`bundle = false` are left out. That leaves out
`tasty-plugin-agent-stream`, so seven plugins are packaged. The `include_unbundled` input
packages all eight. `tasty-plugin-mesh-demo` stays in tasty as an SDK example and is not
built here.

Decided on 2026-11-08: downloadable packages leave out `bundle = false` plugins, as
tasty does now.

## Signing and release status

The signing steps are placeholders, and nothing is published.

| Step | Secret it reads | Without the secret | With the secret |
|---|---|---|---|
| Manifest signature (`tasty-plugin.toml.sig`) | `PLUGIN_MANIFEST_SIGNING_KEY` (Ed25519 private key, PEM) | Logs `unsigned:` and continues | Signs the staged manifests the same way as tasty's `scripts/sign-bundle.sh`: Ed25519 over the SHA-256 digest of the manifest, raw 64 bytes |
| macOS codesign and notarization | `APPLE_CODESIGN_CERTIFICATE_P12` | Logs `unsigned:` and continues | Fails: not implemented |
| Windows Authenticode | `WINDOWS_CODESIGN_CERTIFICATE` | Logs `unsigned:` and continues | Fails: not implemented |

GitHub Actions cannot use the `secrets` context in `if:`, so each step reads its secret
through `env` and checks in the shell whether it is empty.

The release workflow has no tag trigger and no publish job. It uploads the packages
only as workflow artifacts named `plugins-<platform>`. Packages of the released
version are built from `main` (`gh workflow run release.yml --ref main`). Release tags
go only on `main` commits ([branching.md](branching.md)). When a tag trigger is added,
the workflow must first check that the tagged commit is an ancestor of `origin/main`,
as tasty's release workflow does.

These depend on stage 2 decisions about the plugin contract and trust model:

- The long-term signing key, its secret name, and who holds it.
- Signing scope: manifest only, or the binaries and archives as well.
- Key rotation, and how the host trusts a new key.
- The minimum host version a plugin package declares.
- Where packages are published (tasty Releases, according to the current plan) and the
  file names and hash list format the full-package lock file reads.

## Checks after the first push

| Check | How | Result |
|---|---|---|
| CI passes on `main` | `gh run list --workflow ci.yml --commit <sha>`, then `gh run view <id> --json jobs`: the `check` job and five `build` jobs succeed | Passed: check and all five build jobs (CI run 42402231647, commit 86ed61d) |
| Static CRT on Windows | "Inspect DLL imports with dumpbin (Windows)" step in the two Windows build jobs | Passed on both Windows targets (CI run 42402231647) |
| Release skeleton | `gh workflow run release.yml --ref main`, then `gh run download <id>`: each `plugins-<platform>` artifact holds the archives, `SHA256SUMS-<platform>.txt`, and `plugins-<platform>.tsv`. The signing step logs contain `unsigned:` | Passed: five artifacts, each with 7 archives, the SHA-256 list, and the TSV; `sha256sum -c` passed; every job logged `unsigned:` (release run 42402274255) |
| No release created | `gh release list` is empty | Passed: no releases |
| Extra system packages | Whether the Linux runners need packages beyond the image defaults. A local Linux ARM64 build needed none | None needed on the hosted Linux runners (CI run 42402231647). Since the plugin sources moved here, the plugin tests need WebKitGTK 4.1 and `xvfb` on the `check` runner (see [What the runners need](#what-the-runners-need)); the `build` jobs still need none |
