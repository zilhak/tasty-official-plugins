# Branches

This repository uses the same three branches as tasty. The rules are defined in tasty's
[branch model](https://github.com/zilhak/tasty/blob/develop/docs/dev-guide/branching.md),
which is the reference. This page summarizes them and lists what differs here.

| Branch | Meaning | What goes in |
|---|---|---|
| `develop` | Version under development. The integration branch and the GitHub default branch | Work branches that pass CI, fast-forwarded. Back-merges from `release` and `main` |
| `release` | Release candidate | A commit chosen from `develop`. Verification on real runs, release fixes, and the version commit. Back-merges from `main` |
| `main` | Released version. Release tags (`v*`) go only on `main` commits | `release` at release time. Hotfixes |

The repository version is in `VERSION`. A release tag is `v` + `VERSION`, for example
`v0.1.0` for `0.1.0`.

Because `develop` is the default branch, a new clone checks it out, pull requests
target it, and a manual workflow run without `--ref` runs on it.

## Work branches

- Start work branches from `develop` and rebase them onto `develop`.
- Land them with `git merge --ff-only <work-branch>`. Do not merge a work branch with a
  merge commit.
- Pull requests target `develop`.

## Promoting to release

1. Move `release` to a commit chosen from `develop`. If `release` is an ancestor of that
   commit, use `git merge --ff-only <commit>`. Otherwise (a release fix is not yet on
   `develop`) merge it with a merge commit.
2. Verify the candidate on `release`. Commit release fixes on `release`.
3. When verification is done, make the version commit on `release`: it bumps `VERSION`,
   and its body is the release note.
4. Back-merge the fixes and the version commit from `release` into `develop`. If
   `develop` is ahead, this is a merge commit.

`release` takes no new features: only verification, release fixes, and the version
commit.

## Releasing to main

1. Fast-forward `main` to `release` (`git merge --ff-only release`). The release notes
   are the body of the tag commit, so the tag goes on the version commit, not on a
   merge commit. If `main` has a hotfix that `release` lacks, back-merge `main` into
   `release` first and make the version commit on top of that merge.
2. Tag the version commit with `v` + `VERSION`: `git tag -a vX.Y.Z -m vX.Y.Z`.
3. Push `release`, `main`, and the tag together:
   `git push --atomic origin release main vX.Y.Z`.

The tag push starts `release.yml`, which checks that the tag commit is on `main` and
that the tag equals `v` + `VERSION`, then publishes the Release of the tag
([CI and release workflows](ci.md#releases)). If `create-release` fails, no Release is
created. If a later job fails, the Release stays a draft. Either way, fix the cause and
rerun the workflow for the same tag.

## Hotfixes

1. Branch from `main`, fix, and land the fix on `main`.
2. On `main`, make the version commit: it bumps the patch number in `VERSION`, and its
   body is the release note.
3. Tag the version commit (`git tag -a vX.Y.Z -m vX.Y.Z`) and push `main` with the tag
   only: `git push --atomic origin main vX.Y.Z`. `release` is not part of this push.
4. Back-merge `main` into `release` and `develop`. If the target is ahead, this is a
   merge commit.

Do not back-merge with cherry-pick. A cherry-picked commit has a different hash, so the
next `release` to `main` promotion cannot fast-forward.

## Merge commits

Merge commits are made only in these cases:

| Current branch | Source branch | Kind |
|---|---|---|
| `release` | `main` | back-merge |
| `develop` | `main` | back-merge |
| `develop` | `release` | back-merge |
| `release` | `develop` | promotion |
| `main` | `release` | promotion |

The `pre-merge-commit` and `pre-commit` hooks enforce this table. Install them with
`bash scripts/dev-setup.sh`, which sets `core.hooksPath` to `.githooks` and then runs
`cargo fetch --locked` for the plugin version check.

## What differs from tasty

- Releases are tagged with the repository's own semver, `v` + `VERSION`, starting at
  `v0.1.0`. The repository version in `VERSION` names the release and its tag only.
  Each plugin keeps its own version, the `version` in its crate's `Cargo.toml` and
  `tasty-plugin.toml`, which move in lockstep, and a release does not change it.
- Each plugin's patch version goes up in the commit that changes its build output,
  including a pin move that changes a tasty crate in its dependency closure and an update
  of a crates.io crate in that closure (one bump per release covers several such updates:
  a plugin already above its version at the tip of `origin/main` needs no other bump);
  the `pre-commit` hook and the Plugin version check workflow check this. Lane worktrees
  may defer it with `git config --worktree tasty.pluginBump deferred`; the merge step runs
  `scripts/plugin-bump-fixup.sh` per commit and checks each commit again. The loop stops
  at the first commit that fails:

  ```
  git rebase --exec 'bash scripts/plugin-bump-fixup.sh' <base>
  for c in $(git rev-list --reverse <base>..HEAD); do bash scripts/check-plugin-version-bump.sh --range "$c^" "$c" || exit 1; done
  ```
- The git hooks are in `.githooks/`, as in tasty, with fewer checks: `pre-commit` checks
  merge commits, formatting, and plugin versions, `pre-merge-commit` blocks merge commits
  outside the table above, and `pre-push` compares the plugin versions of the commits to
  push with what the remote already has.
- A manual run of `release.yml` builds the release files as workflow artifacts with the
  tag `unreleased` in `release-meta.toml` and creates no Release.

## Branch protection

GitHub rulesets block deleting `main` and `release` and block force pushes to `main`.

## CI

The `CI` workflow runs the same jobs on pushes to `develop`, `release`, and `main` and on
pull requests that target them. See [CI and release workflows](ci.md).
