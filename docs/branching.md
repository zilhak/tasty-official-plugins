# Branches

This repository uses the same three branches as tasty. The rules are defined in tasty's
[branch model](https://github.com/zilhak/tasty/blob/develop/docs/dev-guide/branching.md),
which is the reference. This page summarizes them and lists what differs here.

| Branch | Meaning | What goes in |
|---|---|---|
| `develop` | Version under development. The integration branch and the GitHub default branch | Work branches that pass CI, fast-forwarded. Back-merges from `release` and `main` |
| `release` | Release candidate | A commit chosen from `develop`. Verification on real runs and release fixes. Back-merges from `main` |
| `main` | Released version. Release tags (`v*`) go only on `main` commits | `release` at release time. Hotfixes |

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
3. Back-merge the fixes from `release` into `develop`. If `develop` is ahead, this is a
   merge commit.

`release` takes no new features: only verification and release fixes.

## Releasing to main

1. Fast-forward `main` to `release` (`git merge --ff-only release`). If `main` has a
   hotfix that `release` lacks, back-merge `main` into `release` first.
2. Push `release` and `main` together: `git push --atomic origin release main`.
3. Build the packages of the released version from `main`:
   `gh workflow run release.yml --ref main`.

This repository has no tag-triggered release yet (see [CI and release workflows](ci.md)).
When tags are used, a tag goes on a `main` commit and is pushed in the same atomic push
as `release` and `main`, as in tasty.

## Hotfixes

1. Branch from `main`, fix, and land the fix on `main`. Push `main` alone; `release` is
   not part of this push.
2. After CI passes on `main`, build the packages from `main`:
   `gh workflow run release.yml --ref main`.
3. Back-merge `main` into `release` and `develop`. If the target is ahead, this is a
   merge commit.

When tags are used, the hotfix tag goes on the `main` commit and is pushed with `main`
only (`git push --atomic origin main vX.Y.Z`), as in tasty.

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

- This repository has no version of its own. Plugin versions are the `version` values in
  each crate's `Cargo.toml` and `tasty-plugin.toml` in this repository. There is no version
  commit on `release`.
- The release workflow is manual only and has no tag trigger or publish job yet. The
  tag-triggered release waits for the stage 2 release decisions in [ci.md](ci.md).
- There are no git hooks.

## Branch protection

GitHub rulesets block deleting `main` and `release` and block force pushes to `main`.

## CI

The `CI` workflow runs the same jobs on pushes to `develop`, `release`, and `main` and on
pull requests that target them. See [CI and release workflows](ci.md).
