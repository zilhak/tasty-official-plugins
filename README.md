# Tasty official plugins

Build and release repository for the official plugins of [Tasty](https://github.com/zilhak/tasty).

The plugin sources live in this repository under `crates/`, and their documents under
`docs/plugins/`. They were copied from tasty commit
`e277fce41d9a2db8bf3c14fda8f56dd6415bfcbc`. The tasty crates they use (the plugin SDK,
protocol, manifest, and shared libraries) come in as git dependencies pinned to a single
tasty commit.

## Branches

`develop` is the integration branch and the default branch, `release` holds release
candidates, and `main` holds the released version. Work branches start from `develop`,
and pull requests target `develop`. See [docs/branching.md](docs/branching.md).

## Releases

A `v*` tag on `main` publishes the eight plugins for five platforms on a GitHub Release
of this repository: one archive per plugin and platform, a SHA-256 list and a TSV list
per platform, and `release-meta.toml`. The tag is `v` + the version in `VERSION`. The
packages are not signed. See [docs/ci.md](docs/ci.md#releases).

## License

MIT. See [LICENSE](LICENSE). Each plugin archive includes a copy as `<id>/LICENSE`.

## Pinned tasty commit

`e277fce41d9a2db8bf3c14fda8f56dd6415bfcbc`

All tasty crates come from this one commit. The pin is set in the root `Cargo.toml`:
every entry under `[workspace.dependencies]` and `[patch.crates-io]` that points at
`https://github.com/zilhak/tasty` uses the same `rev`.

To move the pin:

1. Replace the rev in every tasty entry of `Cargo.toml`. Do not give crates different revs.
   When tasty has a release tag to follow, `rev = "..."` can become `tag = "..."`.
2. Start `Cargo.lock` from the tasty `Cargo.lock` at the new commit, then run
   `cargo metadata --format-version 1 > /dev/null` so Cargo trims it to this workspace.
   Do not run `cargo update`; the external crates should stay at the versions tasty
   locks. The egui family (`egui`, `epaint`, `emath`, `ecolor`, `bytemuck`) must lock
   the same versions as the tasty `Cargo.lock`, because the egui-mesh wire passes their
   types as raw bytes. A pin test checks this.
3. Update the sha in this README and run `scripts/check.sh`.

If Cargo reports `patch location ... does not contain packages matching epaint`, tasty
has removed its copy. Its `docs/dev-guide/dep-issues.md` gives the condition for that.
Remove the `epaint` patch and its pin test here too, along with the notes about the
patched copy in this README and in `docs/migration-plan.md`. The atlas statement under
"Advisory decisions" depends on the patch, so decide again what holds without it.

The `pin` tests (`crates/plugin-pin-check/tests/pin.rs`) fail if the tasty entries use
different revs, if a tasty crate in `Cargo.lock` comes from another source, if a plugin
crate is not a local workspace crate, or if a plugin manifest version differs from its
crate version. They also fail if a crate of the egui family locks other versions than
the tasty `Cargo.lock` at the pin, or if `egui` or `epaint` is not locked.

## What is built

| Crate | Kind |
|---|---|
| `tasty-plugin-agent-common` | library |
| `tasty-plugin-agent-stream` | binary |
| `tasty-plugin-claude` | binary |
| `tasty-plugin-clipboard-viewer` | binary |
| `tasty-plugin-codex` | binary |
| `tasty-plugin-git-viewer` | binary |
| `tasty-plugin-html` | binary |
| `tasty-plugin-image` | binary |
| `tasty-plugin-markdown` | binary |

Each crate is in `crates/<crate>/`. `tasty-plugin-protocol`, `tasty-plugin-manifest`, and
`tasty-plugin-sdk` stay in tasty and come in as dependencies of these crates.
`tasty-plugin-mesh-demo` stays in tasty as an SDK example and is not built here.

The build matches tasty's own plugin build:

- Toolchain: Rust 1.96.0 (`rust-toolchain.toml`).
- Profiles: `release` and `dist` are the same as in tasty.
- Windows MSVC targets link the C runtime statically (`.cargo/config.toml`).
- `tiny_http` is patched to the copy tasty vendors, which `tasty-plugin-agent-stream` uses
  for its SSE server. Cargo applies `[patch]` only from the root workspace, so this
  repository repeats it.
- `epaint` is patched to the copy tasty vendors (`vendor/epaint`, see its `PATCHES.md`).
  A glyph too big for the font atlas is left blank instead of panicking, and the atlas
  image stops growing at its width. A pin test fails if `Cargo.lock` takes `epaint` from
  anywhere else or records an unused patch.
- `Cargo.lock` keeps the external crate versions from the tasty lock file at the pinned
  commit.

## Local commands

```
scripts/check.sh                 # fmt, lint, test, then a debug build of all plugins
scripts/check.sh fmt             # cargo fmt --all --check
scripts/check.sh lint            # cargo clippy --workspace --all-targets --locked -- -D warnings
scripts/check.sh test [ARGS]     # cargo test --workspace --locked [ARGS]
scripts/check.sh build [ARGS]    # cargo build --locked -p <each plugin> [ARGS]
scripts/check.sh audit           # cargo audit --file Cargo.lock (needs cargo-audit, not part of all)
scripts/check.sh list-plugins    # plugin crates to build, one per line
scripts/check.sh list-bins       # plugin crates that produce an executable, one per line
```

`build` and `test` pass their arguments to cargo unchanged, for example
`scripts/check.sh build --profile dist --target aarch64-pc-windows-msvc`. With `all`
(the default), extra arguments go to the build step. Build and check commands use
`--locked` and `audit` only reads `Cargo.lock`, so a check never changes it. The script
is bash and runs under Git Bash on Windows.

The plugin list lives only in `scripts/check.sh`. A test checks that it matches the
`tasty-plugin-*` workspace members and that `LIBS` lists the members without a binary.

## Notes

- `fmt`, `lint`, and `test` cover the plugin crates and `plugin-pin-check`. Cargo does
  not lint or test the tasty git dependencies.
- On Linux, `lint` and `test` need the WebKitGTK 4.1 development files for the
  `tasty-plugin-markdown` dev-dependencies (`libwebkit2gtk-4.1-dev` on Debian and
  Ubuntu). `build` does not.
- Two `tasty-plugin-markdown` tests open a real WebKitGTK view and are `#[ignore]`. CI runs
  them under `xvfb-run`; see [docs/ci.md](docs/ci.md).

## Advisory decisions

The `audit` CI job runs `cargo audit` on `Cargo.lock`. It reads the lock file directly, so
it also checks the crates.io dependencies that only the plugin crates use. It matches advisories only against crates.io packages, so the
tasty crates and the `tiny_http` and `epaint` copies, which come from the tasty repository,
are not matched.

A vulnerability fails the job. Vulnerabilities are not ignored. A crates.io crate is
updated here with `cargo update -p <crate>` only to fix a vulnerability; a fix in a tasty
crate or in the tasty copies arrives with a pin move. The egui family goes through tasty:
if the update changes one of its versions, the pin test fails, so the crate is updated in
tasty first and arrives here with a pin move. Unsound and unmaintained advisories appear as warnings
in the job log and do not fail the job.

This repository has no `deny.toml` and runs no `cargo deny` check. tasty keeps its
decisions in its own `deny.toml`. The notes below are the same decisions for what this
repository builds. It is not a list of every advisory that applies to `Cargo.lock`; the
`audit` job log shows those.

RUSTSEC-2026-0192: `ttf-parser` 0.25.1 is unmaintained. It has no known vulnerability, and
the advisory names no fixed version.

- It parses fonts at run time; it is not a build tool. It reaches the plugins through
  `ab_glyph`, which `epaint` and `tasty-i18n` use, and `tasty-i18n` also uses it directly.
- Its inputs are the fonts built into egui, the system CJK fonts (one on Linux, up to two
  on Windows and macOS), and the language pack font the host passes in
  `TASTY_LOCALE_FONT`. A language pack font can be a file made by a third party.
- Version 0.25.1 is `forbid(unsafe_code)`, so the risk of a memory error inside this crate
  is low.
- A file that fails to parse, or has a line height of zero or less, panics in `epaint`
  0.31.1 (`text/fonts.rs:210`, `text/font.rs:94`). The system CJK fonts and the language
  pack font go through `tasty_i18n::font` validation first.
- Damaged files that pass validation (damaged glyf, no cmap, truncated) did not panic in
  tasty's measurements.
- A font with glyphs bigger than the font atlas passes validation, but the patched copy
  of `epaint` leaves those glyphs blank instead of panicking. tasty measured width, height,
  the replacement glyph paths, and large text sizes with normal fonts.
- These measurements were made in tasty. Plugin processes were not run on these inputs.
- Files outside the measured inputs carry no guarantee. Because the crate is unmaintained,
  a parser defect will not be fixed.
- Review this if `ttf-parser` gets a vulnerability advisory. The note can go when
  `ab_glyph` moves to another parser and `tasty-i18n` drops its direct dependency. Which
  versions do that has not been measured.

RUSTSEC-2024-0370: `proc-macro-error` 1.0.4 is unmaintained. It is not a known
vulnerability.

- It is a library for procedural macros, used only by `glib-macros` 0.18 and
  `gtk3-macros` 0.18. It runs only at compile time.
- It is only in the Linux target graph. It comes in through the Linux dev-dependencies of
  `tasty-plugin-markdown` (`webkit2gtk` 2.0.2, `gtk` 0.18, and `javascriptcore-rs` 1.1.2),
  so it is built for the tests and is not part of a plugin binary.
- `cargo audit` reports it as a warning, and the `audit` job passes.
- It can go when `webkit2gtk` and `gtk` leave the GTK3 0.18 line. Which versions do that
  has not been measured.

RUSTSEC-2024-0429: `glib` 0.18.5 is unsound.

- It is an indirect dependency on the same GTK3 0.18 path: the Linux dev-dependencies of
  `tasty-plugin-markdown` bring it in through `gtk`, `javascriptcore-rs`, and the crates
  under them. It is only in the Linux target graph and is not part of a plugin binary.
- `cargo audit` reports it as a warning, and the `audit` job passes.
- It can go under the same condition as RUSTSEC-2024-0370. Which versions do that has not
  been measured.
