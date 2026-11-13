# 0066. `main` is protected, and the required checks are derived from the workflows that report them

## Context

`main` accepted a direct push. One account holds admin on the repository, so that account could
push to `main` and merge its own pull request, and the release chain (`Auto Tag` → `Release`)
turns anything on `main` into a published crate. The OpenSSF Scorecard's `Branch-Protection`
check scored this repository **0.0**: "branch protection not enabled for branch 'main'".

The request was for the branch-protection half of that scorecard, without the rest of the
scorecard suite. No SAST workflow, no dependency-review workflow, no scorecard action. The
protection itself, plus the machinery that keeps it true.

Two properties of GitHub's branch protection shape every decision below, and both of them fail
quietly.

**A required status check is matched by exact string.** The name has to be the name the workflow
reports, character for character. When it stops matching, the check is "required" and never
reports, and the pull request cannot be merged. There is no error message on the merge button.
Two of this repository's check names carried a value that moves: `MSRV (1.97.1)` and `Compat
(v0.1.0-rc.1 reads current fixtures)`. The day the MSRV moved or the compatibility pin moved,
every pull request in the repository would have stopped merging, and the reason would have been
one string away from the workflow file.

**A workflow filtered by `paths:` does not run at all when the filter does not match.** It
therefore reports no check run, which is the same failure as the one above. The docs workflow was
filtered on `site/**`, so on every pull request that did not touch the site it reported nothing
at all, and `build` could not be required.

## Decision

`main` is protected with GitHub's classic branch protection, with this shape:

| Setting | Value | Why |
| --- | --- | --- |
| Allow deletions | off | A branch that can be deleted is a branch that can be replaced. |
| Allow force pushes | off | Force-pushing `main` rewrites history that a tag already names. |
| Require a pull request | on | The release chain publishes from `main`, so the review has to happen before the tag. |
| Approving reviews | 1 | See below. |
| Require a code owner review | on | `.github/CODEOWNERS` names both accounts on `*`. |
| Dismiss stale approvals | on | An approval of yesterday's tree is not an approval of today's. |
| Require approval after the last push | on | A new commit is new code, and it has not been read. |
| Enforce for administrators | on | The one account that could bypass the rules is the account most likely to need them. |
| Require branches to be up to date | on | The merge commit has to contain the branch head it was reviewed against. |
| Require conversation resolution | on | A resolved thread is a decision; an open one is a question. |
| Require linear history | off | `main` contains merge commits. Rewriting it to remove them is a different decision. |

**One approving review, not two.** The repository has two accounts, and GitHub does not count a
pull request's author as an approving reviewer. Two required approvals would therefore make
every pull request unmergeable: the author cannot approve, and there is nobody else to ask.
One review, from the other account, is the most this repository can enforce and still merge
anything. The OpenSSF Scorecard's thorough-review tier wants two, so this repository scores 8 of
10 on that check rather than 10, and the two points are not reachable without a third
contributor.

**`.github/CODEOWNERS` names both accounts.** GitHub reads the file from the base branch, so it
has to be on `main` before it can gate a review. The rule is `* @greysquirr3l @sosh-ncampbell`:
a change opened by either account is reviewed by the other. Naming both accounts on one rule is
what keeps either account able to open a pull request.

**The required checks are a derived list, not a copied one.** `.github/branch-protection.json` is
the request body sent to GitHub, and its `required_status_checks.contexts` is the list of names
that have to report. `scripts/required_checks.py` computes the set of names the pull-request
workflows will report, the way GitHub names them, including the matrix expansion, and compares
it against the file. A name in the list that no workflow reports is a dead merge button. A name
a workflow reports that is not in the list is a check that gates nothing. Both are errors. The
script also refuses a required name that carries a version, which is the failure this ADR was
written to prevent.

**Two check names stopped carrying a version.** `MSRV (1.97.1)` became `MSRV`, and `Compat
(v0.1.0-rc.1 reads current fixtures)` became `Compat (pinned release reads current fixtures)`.
The version each one pins still lives in `rust-toolchain.toml` and in `compat/Cargo.toml`, where
it is read from.

**The docs workflow always reports.** Its `paths:` filter is gone and the filter moved onto the
build steps: a "Detect site changes" step diffs against the merge base, and the install and
build steps run only when the site changed. The job itself always runs, so `Site build` always
reports and can be required. The job is also named `Site build` now, because `build` is a name
too generic to identify in a list of thirty-five.

**The release gate is not required.** It only runs on a tag ref, so on a pull request it reports
`skipped`. Requiring it would add a name to the list without adding a signal.

## Consequences

A pull request now needs one review from the other account and all thirty-five checks green
before it merges, and `main` cannot be pushed to, force-pushed, or deleted. The release chain
that publishes to crates.io inherits that: nothing reaches a tag without having been reviewed.

Adding a job to a workflow means adding its name to `.github/branch-protection.json`, and
`scripts/required_checks.py --check` fails the `Scripts` job until that happens. The failure is
a diff, not a deadlock.

Renaming a job has the same cost, and it is a cost that is now visible before the merge rather
than after it.

The docs workflow spends a runner on every pull request, including the ones that change no site
file. It checks out the full history and skips the install and the build, so the extra cost is
a runner start-up rather than a Node install. That is the price of a check that always reports.

## Alternatives rejected

**The whole OpenSSF Scorecard suite.** Rejected on the request: the branch-protection half is
wanted, the SAST and dependency-review workflows are not. The score is measured with the
`scorecard` binary instead, so the number is real without the workflow.

**A ruleset instead of classic protection.** Rejected because the scorecard's ruleset
translation reads a ruleset with any bypass actor as "administrators are not enforced", and the
release chain needs a bypass actor to push a tag with `GITHUB_TOKEN`. Classic protection
expresses the same policy without that ambiguity.

**Two approving reviews.** Rejected because it is unsatisfiable in a two-account repository. The
point of the rule is that it can be met, not that it scores.

**Leaving the docs workflow path-filtered and excluding `build` from the required list.**
Rejected: the site is the adoption path, so its build is worth requiring, and the pattern of
filtering the steps rather than the trigger is what the sibling `protocol-droid` repository
already does. A path filter on the trigger is a check that does not report.

**Moving the `v0.1.0-rc.1` tag.** Rejected, and it is worth saying why, because nothing has
been published to crates.io yet. `compat/Cargo.toml` pins that tag, and the compatibility harness
exists to compile a *released* build and read files the current tree wrote. Moving the tag onto
the current head would make it compile the current tree and read the current tree's files, which
is the one pair that cannot disagree. The tag is the only pre-1.0.0 baseline that exists, and a
tag that moves is a tag nobody can trust.

## Verification

- `python3 scripts/required_checks.py --check` compares the list in
  `.github/branch-protection.json` against the workflows, and is wired into the `Scripts` job.
- `python3 scripts/required_checks.py --self-test` proves the name translation, including the
  matrix expansion and the shapes the script refuses.
- The computed names were compared against the check runs GitHub reported for the head of
  `main` before this change: thirty-four of the thirty-five are the same names, and the
  differences are the two renames and `build` becoming `Site build`.
- `scorecard --repo greysquirr3l/bevy-flock --checks Branch-Protection` measured 0.0 before and
  8 after.
