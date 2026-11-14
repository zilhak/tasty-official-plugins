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

MIT. See [LICENSE](LICENSE). Each plugin archive includes a copy as `<id>/LICENSE`, next
to the third-party notices: [THIRD_PARTY_LICENSES.md](THIRD_PARTY_LICENSES.md) and the
license texts in [LICENSES/](LICENSES), and the markdown archive also holds
`<id>/NOTICE.md`, the sources of its bundled assets. A change of the root notice set
(`LICENSE`, `THIRD_PARTY_LICENSES.md`, `LICENSES/`) or of the packaging code changes every
archive and needs a patch bump of every plugin, once per release. A change of
`assets/NOTICE.md` changes only the markdown archive and needs a bump of that plugin.

## Pinned tasty commit

`e277fce41d9a2db8bf3c14fda8f56dd6415bfcbc`

The plugin crates are workspace members (`members = ["crates/*"]`) and are not pinned.
The tasty crates that stay in tasty come from this one commit: the plugin SDK, protocol,
and manifest (`tasty-plugin-sdk`, `tasty-plugin-protocol`, `tasty-plugin-manifest`), the
shared libraries the plugins link, such as `tasty-i18n`, `tasty-egui-theme`, and
`tasty-git-core`, the dev-dependencies `tasty-test-support`, `tasty-themes`, and
`tasty-doc-guards`, and the `tiny_http` and `epaint` copies. The pin is set in the root
`Cargo.toml`: every entry under `[workspace.dependencies]` and `[patch.crates-io]` that
points at `https://github.com/zilhak/tasty` uses the same `rev`, a commit and not a tag.

To move the pin:

1. Replace the rev in every tasty entry of `Cargo.toml`. Do not give crates different revs.
2. Run `cargo metadata --format-version 1 > /dev/null` without `--locked`, so Cargo
   records the new tasty packages in `Cargo.lock`. Do not run a plain `cargo update`
   (see [Cargo.lock](#cargolock)). The egui family (`egui`, `epaint`, `emath`, `ecolor`,
   `bytemuck`) must lock the same versions as the tasty `Cargo.lock` at the new commit,
   because the egui-mesh wire passes their types as raw bytes. A pin test checks this.
3. Update the sha in this README and run `scripts/check.sh`.
4. Bump the patch version of each plugin whose dependency closure holds a tasty crate
   that changed between the two commits, or a crates.io package whose `Cargo.lock` entry
   the move changed. The version check reports them
   (`bash scripts/check-plugin-version-bump.sh --staged`).

If Cargo reports `patch location ... does not contain packages matching epaint`, tasty
has removed its copy. Its `docs/dev-guide/dep-issues.md` gives the condition for that.
Remove the `epaint` patch and its pin test here too, along with the notes about the
patched copy in this README and in `docs/migration-plan.md`. The atlas statement under
"Advisory decisions" depends on the patch, so decide again what holds without it.

The `pin` tests (`crates/plugin-pin-check/tests/pin.rs`) fail if the tasty entries use
different revs, if a package in `Cargo.lock` comes from the tasty repository at another
rev, if a plugin crate is not a local workspace crate, or if the plugin list in
`scripts/check.sh` differs from the `tasty-plugin-*` workspace members. They also fail if
a plugin manifest version differs from its crate version, if `epaint` does not come from
the patched copy at the pin or `Cargo.lock` records an unused patch, if a crate of the
egui family locks other versions than the tasty `Cargo.lock` at the pin, if `egui` or
`epaint` is not locked, if a plugin's `PLUGIN_ID` constant differs from the id in its
manifest or is missing, if a package without a source comes from a folder that is not a
tracked part of this repository, and if the root `Cargo.toml` or the committed cargo
config declares a path `[patch]` or a `paths` override.

### Cargo.lock

`Cargo.lock` belongs to this repository. It started from the tasty lock file at the
pinned commit when the sources moved here, so the crates.io packages are still at the
versions tasty locked then.

- Build and check commands use `--locked`, so a check never changes it.
- A vulnerability is not ignored: the crate is updated here with `cargo update -p <crate>`
  (see [Advisory decisions](#advisory-decisions)).
- A change of a crates.io package in `Cargo.lock` needs a patch bump of every plugin whose
  dependency closure holds it. Several such changes before one release need one bump. The
  version check enforces this.
- A new crate or dependency should add only its own packages to `Cargo.lock`; check the
  diff.
- A `[patch]` entry is judged by the source change it makes in `Cargo.lock`. A path patch
  cannot be committed; use a local tasty copy with `scripts/dev-bundle.sh --tasty`.

The lock is not what Cargo would resolve from scratch. `cargo metadata --locked` accepts
it and `cargo update --workspace --dry-run` changes nothing, but Cargo reports more than
a hundred dependencies behind the latest compatible versions, among them `bytemuck` of
the egui family, and it keeps three `windows-sys` versions (0.59.0, 0.60.2, 0.61.2)
where a fresh resolution keeps fewer. This is kept so that the plugins build with the
crate versions tasty locked and every version change is a deliberate, bumped one. Do not
delete and regenerate the lock or run a plain `cargo update`: it would move all of these
at once, fail the egui pin test, and require a bump of every plugin.

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

Each crate's source is in `crates/<crate>/` in this repository. `tasty-plugin-protocol`,
`tasty-plugin-manifest`, and `tasty-plugin-sdk` stay in tasty and come in as git
dependencies at the pin. `tasty-plugin-mesh-demo` stays in tasty as an SDK example and is
not built here.

Three more workspace crates belong to this repository only and are not published
(`publish = false`): `plugin-pin-check` (the pin and lock tests above), `repo-gates` (the
tests of the plugin version check, its fixup, and the git hooks), and `plugin-guards`
(source checks on the plugin crates that moved here from tasty, see
[docs/ci.md](docs/ci.md#source-checks-on-the-plugins)). They hold only tests and their
helpers and are not part of any plugin archive.

The build matches tasty's own plugin build:

- Toolchain: Rust 1.96.0 (`rust-toolchain.toml`).
- Profiles: `release` and `dist` are the same as in tasty.
- Windows MSVC targets link the C runtime statically (`.cargo/config.toml`).
- Changing the toolchain, the release profiles, or `.cargo/config.toml` bumps every plugin
  (once per release). A change only in `components` or `profile` of the toolchain, or only
  in `[alias]`, `[term]`, `[net]`, or `[http]` of the cargo config, passes with a note.
- `tiny_http` is patched to the copy tasty vendors, which `tasty-plugin-agent-stream` uses
  for its SSE server. Cargo applies `[patch]` only from the root workspace, so tasty's
  patch does not reach this repository, which sets the same one.
- `epaint` is patched to the copy tasty vendors (`vendor/epaint`, see its `PATCHES.md`).
  A glyph too big for the font atlas is left blank instead of panicking, and the atlas
  image stops growing at its width. A pin test fails if `Cargo.lock` takes `epaint` from
  anywhere else or records an unused patch.
- The crates.io packages in `Cargo.lock` started at the versions of the tasty lock file
  when the sources moved. This repository keeps its own lock since then (see
  [Cargo.lock](#cargolock)).

## Local commands

```
scripts/check.sh                 # fmt, lint, test, then a debug build of all plugins
scripts/check.sh fmt             # cargo fmt --all --check
scripts/check.sh lint            # cargo clippy --workspace --all-targets --locked -- -D warnings
scripts/check.sh test [ARGS]     # cargo test --workspace --locked [ARGS]
scripts/check.sh build [ARGS]    # cargo build --locked -p <each plugin> [ARGS]
scripts/check.sh audit           # cargo audit --file Cargo.lock (needs cargo-audit, not part of all)
scripts/check.sh licenses        # cargo deny check licenses (needs cargo-deny, not in all)
scripts/check.sh list-plugins    # the PLUGINS list: the nine plugin crates, one per line
scripts/check.sh list-bins       # PLUGINS without LIBS: the eight with an executable
bash scripts/dev-setup.sh        # use the .githooks hooks, then cargo fetch --locked
bash scripts/install-judge.sh    # install strip-cfg-test for the plugin version check
bash scripts/check-plugin-version-bump.sh --staged   # plugin version check on the index
bash scripts/check-plugin-version-bump.sh --range A B  # the same between two commits
bash scripts/plugin-bump-fixup.sh  # raise the versions the HEAD commit needs, amend HEAD
scripts/package-plugin.sh ...    # stage and pack release archives (docs/ci.md#packaging)
scripts/dev-bundle.sh --out DIR  # plugins for a tasty checkout (below)
```

`dev-setup.sh` sets `core.hooksPath` to `.githooks` once per clone and then runs
`cargo fetch --locked`, because the version check reads the dependency closure of every
target offline. The version check exits 0 on pass, 1 when a plugin needs a bump, and 2
when it cannot decide; see [docs/ci.md](docs/ci.md#plugin-version-check).

`scripts/check.sh` warns when the hooks are not set: `fmt`, `lint`, `test`, `build`, and
`all` print a warning on stderr when `core.hooksPath` of the clone is unset, points to a
missing folder, or points to another tree. The warning does not change the exit code, and
CI runs stay quiet.

`build` and `test` pass their arguments to cargo unchanged, for example
`scripts/check.sh build --profile dist --target aarch64-pc-windows-msvc`. With `all`
(the default), extra arguments go to the build step. Build and check commands use
`--locked` and `audit` only reads `Cargo.lock`, so a check never changes it. The script
is bash and runs under Git Bash on Windows.

`build`, `list-plugins`, and `list-bins` read the `PLUGINS` and `LIBS` lists in
`scripts/check.sh`. A pin test compares `PLUGINS` with the `tasty-plugin-*` workspace
members that `cargo metadata` reports and `LIBS` with those members that have no binary
target, so a crate added under `crates/` must be added there too.

### Plugins for a tasty checkout

```
scripts/dev-bundle.sh --out ../tasty/target/debug/builtin-plugins
scripts/dev-bundle.sh --out ../tasty/target/debug/builtin-plugins --tasty ../tasty
scripts/dev-bundle.sh --out DIR [--profile debug|release|dist] [--tasty DIR] [--target TRIPLE]
```

`dev-bundle.sh` builds every plugin, including those whose manifest says
`bundle = false`, and puts each one in `DIR/<id>/` with its executable,
`tasty-plugin.toml`, and `lang/`, the layout of tasty's `builtin-plugins` folder. It
replaces only those `<id>` folders and leaves anything else in `DIR` alone. `debug`, the
default, is cargo's dev profile. Relative paths in `--out`, `--tasty`, and
`CARGO_TARGET_DIR` are taken from the directory the script is run from.

Without `--tasty`, the plugins are built against the tasty commit that `Cargo.lock` pins,
with `--locked`. With `--tasty`, every package that `Cargo.lock` takes from the tasty git
repository, including the `tiny_http` and `epaint` copies, is built from that local
checkout instead, so changes to the plugin SDK or protocol there reach the plugins. The
build then runs in a copy of this repository under `target/dev-bundle/` with
`cargo --config 'patch...'` options, so `Cargo.lock` here does not change, and it fails
unless every one of those packages is built from that checkout.

Nothing is signed and no `.sig` file is written. A tasty debug build does not check plugin
signatures. A tasty release or dist build loads these plugins only after tasty signs their
manifests with the key it was built with, for example with
`scripts/sign-bundle.sh --key <key> --manifest DIR/<id>/tasty-plugin.toml` in tasty.

## Notes

- `fmt`, `lint`, and `test` cover every workspace member: the plugin crates,
  `plugin-pin-check`, `plugin-guards`, and `repo-gates`. Cargo does not lint or test the
  tasty git dependencies.
- On Linux, `lint` and `test` need the WebKitGTK 4.1 development files for the
  `tasty-plugin-markdown` dev-dependencies (`libwebkit2gtk-4.1-dev` on Debian and
  Ubuntu). `build` does not.
- Two `tasty-plugin-markdown` tests open a real WebKitGTK view and are `#[ignore]`. CI runs
  them under `xvfb-run`; see [docs/ci.md](docs/ci.md).

## Advisory decisions

The `audit` CI job runs `cargo audit` on `Cargo.lock`. It reads the lock file directly, so
it also checks the crates.io dependencies that only the plugin crates use. It matches
advisories only against crates.io packages, so the tasty crates and the `tiny_http` and
`epaint` copies, which come from the tasty repository, are not matched.

A vulnerability fails the job. Vulnerabilities are not ignored. This repository keeps
its own `Cargo.lock`, so a crates.io crate, including one only the plugins use, is
updated here with `cargo update -p <crate>`, with a patch bump of each plugin whose
dependency closure holds it ([Cargo.lock](#cargolock)). A fix in a tasty crate or in the
tasty copies arrives with a pin move. The egui family goes through tasty: if the update
changes one of its versions, the pin test fails, so the crate is updated in tasty first
and arrives here with a pin move. Unsound and unmaintained advisories appear as warnings
in the job log and do not fail the job.

`deny.toml` holds only the license policy, which the `licenses` job checks with
`cargo deny check licenses` (see [docs/ci.md](docs/ci.md#license-check)); advisories are
left to `cargo audit`, and no advisory decision is kept in `deny.toml`. tasty keeps its
advisory decisions in its own `deny.toml`. The notes below are the same decisions for
what this repository builds. It is not a list of every advisory that applies to
`Cargo.lock`; the `audit` job log shows those.

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
