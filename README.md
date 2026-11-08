# Tasty official plugins

Build and release repository for the official plugins of [Tasty](https://github.com/zilhak/tasty).

The plugin sources still live in the tasty repository. Until they move here, this
repository builds the tasty plugin crates as git dependencies pinned to a single tasty
commit. It holds no copy of their sources.

## Pinned tasty commit

`b9f02af4597d9a16e5067f78b8cdf53363a1a144`

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
