# Tasty official plugins

Build and release repository for the official plugins of [Tasty](https://github.com/zilhak/tasty).

The plugin sources still live in the tasty repository. Until they move here, this
repository builds the tasty plugin crates as git dependencies pinned to a single tasty
commit. It holds no copy of their sources.

## Pinned tasty commit

`d70cf8d3b20aed9608e18747ea11bb1fe7b77d52`

All tasty crates come from this one commit. The pin is set in the root `Cargo.toml`:
every entry under `[workspace.dependencies]` and `[patch.crates-io]` that points at
`https://github.com/zilhak/tasty` uses the same `rev`.

To move the pin:

1. Replace the rev in every tasty entry of `Cargo.toml`. Do not give crates different revs.
   When tasty has a release tag to follow, `rev = "..."` can become `tag = "..."`.
2. Start `Cargo.lock` from the tasty `Cargo.lock` at the new commit, then run
   `cargo metadata --format-version 1 > /dev/null` so Cargo trims it to this workspace.
   Do not run `cargo update`; the external crates should stay at the versions tasty
   locks.
3. Update the sha in this README and run `scripts/check.sh`.

If Cargo reports `patch location ... does not contain packages matching epaint`, tasty
has removed its copy. Its `docs/dev-guide/dep-issues.md` gives the condition for that.
Remove the `epaint` patch and its pin test here too.

The `pin` tests (`crates/plugin-pin-check/tests/pin.rs`) fail if the tasty entries use
different revs or if a tasty crate in `Cargo.lock` comes from another source.

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

`tasty-plugin-protocol`, `tasty-plugin-manifest`, and `tasty-plugin-sdk` stay in tasty and
come in as dependencies of these crates.
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
scripts/check.sh test            # cargo test --workspace --locked
scripts/check.sh build [ARGS]    # cargo build --locked -p <each plugin> [ARGS]
scripts/check.sh list-plugins    # plugin crates to build, one per line
scripts/check.sh list-bins       # plugin crates that produce an executable, one per line
```

`build` passes its arguments to `cargo build` unchanged, for example
`scripts/check.sh build --profile dist --target aarch64-pc-windows-msvc`. With `all`
(the default), extra arguments go to the build step. Every cargo command uses `--locked`,
so a check never changes `Cargo.lock`. The script is bash and runs under Git Bash on
Windows.

The plugin list lives only in `scripts/check.sh`. A test checks that it matches the
dependencies of `crates/plugin-pin-check`.

## Notes

- Cargo prints `ignoring invalid dependency ... which is missing a lib target` for each
  binary-only plugin crate. Depending on a binary crate needs artifact dependencies,
  which are nightly-only, so the warning cannot be avoided on stable. The crates are
  still locked and build with `-p`.
- Cargo cannot run the tests of a git dependency that is not a workspace member. The
  plugin crates' own tests keep running in tasty CI until the sources move here. The
  tests in this repository check the pin instead.
- `fmt` and `lint` cover only this repository's own crate. Cargo does not lint git
  dependencies.

## Known advisories

This repository has no `deny.toml` and runs no `cargo deny` check. `cargo deny` reads
`cargo metadata`, which leaves out the bin-only plugin crates and their dependencies, so
it would check only part of `Cargo.lock`. tasty keeps its decisions in its own
`deny.toml`. The note below is the same decision for what this repository builds.

RUSTSEC-2026-0192: `ttf-parser` 0.25.1 is unmaintained. It has no known vulnerability, and
the advisory names no fixed version.

- It parses fonts at run time; it is not a build tool. It reaches the plugins through
  `ab_glyph`, which `epaint` and `tasty-i18n` use, and `tasty-i18n` also uses it directly.
- Its inputs are the fonts built into egui, the system CJK font, and the language pack
  font the host passes in `TASTY_LOCALE_FONT`. A language pack font can be a file made by
  a third party.
- Version 0.25.1 is `forbid(unsafe_code)`, so the risk of a memory error inside this crate
  is low.
- A file that fails to parse, or has a line height of zero or less, panics in `epaint`
  0.31.1 (`text/fonts.rs:210`, `text/font.rs:94`). The system CJK font and the language
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
