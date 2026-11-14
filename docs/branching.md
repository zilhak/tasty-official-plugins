# Branches

This repository uses the same three branches as tasty. The rules are defined in tasty's
[branch model](https://github.com/zilhak/tasty/blob/develop/docs/dev-guide/branching.md),
which is the reference. This page summarizes them and lists what differs here.

| Branch | Meaning | What goes in |
|---|---|---|
| `develop` | Version under development. The integration branch and the GitHub default branch | Work branches that pass CI, fast-forwarded. Back-merges from `release` and `main` |
| `release` | Release candidate | A commit chosen from `develop`. Verification on real runs and release fixes. Back-merges from `main` |
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
2. Bump `VERSION` on `release` in its own commit.
3. Verify the candidate on `release`. Commit release fixes on `release`.
4. Back-merge `release` into `develop`. If `develop` is ahead, this is a merge commit.

`release` takes no new features: only verification and release fixes.

## Releasing to main

1. Fast-forward `main` to `release` (`git merge --ff-only release`). If `main` has a
   hotfix that `release` lacks, back-merge `main` into `release` first.
2. Tag the `main` commit with `v` + `VERSION`: `git tag -a vX.Y.Z -m vX.Y.Z`.
3. Push `release`, `main`, and the tag together:
   `git push --atomic origin release main vX.Y.Z`.

The tag push starts `release.yml`, which checks that the tag commit is on `main` and
that the tag equals `v` + `VERSION`, then publishes the Release of the tag
([CI and release workflows](ci.md#releases)). If the run fails, the Release stays a
draft; fix the cause and rerun the workflow for the same tag.

## Hotfixes

1. Branch from `main`, fix, and bump the patch number in `VERSION`. Land the fix and the
   bump on `main`.
2. Tag the `main` commit (`git tag -a vX.Y.Z -m vX.Y.Z`) and push `main` with the tag
   only: `git push --atomic origin main vX.Y.Z`. `release` is not part of this push.
3. Back-merge `main` into `release` and `develop`. If the target is ahead, this is a
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

tasty enforces this table with git hooks. This repository has no git hooks, so the
table is kept by the merge procedure.

## What differs from tasty

- The repository version in `VERSION` names the release and its tag only. Each plugin
  keeps its own version, the `version` in its crate's `Cargo.toml` and
  `tasty-plugin.toml`, and a release does not change it.
- A manual run of `release.yml` builds the release files as workflow artifacts with the
  tag `unreleased` in `release-meta.toml` and creates no Release.
- There are no git hooks.

## Branch protection

GitHub rulesets block deleting `main` and `release` and block force pushes to `main`.

## CI

The `CI` workflow runs the same jobs on pushes to `develop`, `release`, and `main` and on
pull requests that target them. See [CI and release workflows](ci.md).
