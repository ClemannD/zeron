---
name: upstream-changelog-and-pull
description: Review incoming upstream Zeron changes with a summarized changelog, then after user confirmation merge, resolve conflicts, commit, push, deploy the self-hosted backend, and release affected personal apps.
---

# Upstream changelog and pull

Read the repository's `AGENTS.md` and `docs/self-host.md`. This is a personal
fork with its own backend and private app releases.

## Review first

Inspect the branch, worktree, and remotes, then fetch `upstream`. Fetching
does not authorize a merge. Compare the fork's `main` with `upstream/main`:

```sh
git rev-list --left-right --count main...upstream/main
git log --reverse --format='%h %s%n%b' main..upstream/main
git diff --stat main...upstream/main
git merge-tree --write-tree main upstream/main
git diff main...upstream/main -- edge/wrangler.jsonc edge/vault/wrangler.jsonc .gitignore
```

The merge preview may exit nonzero when it finds conflicts; inspect its
output. Do not modify the worktree during this phase. Inspect relevant
commit diffs and docs to explain behavior rather than repeating titles.

Present a concise changelog grouped by user-visible features, fixes, and
platform changes. State the fetched upstream revision/version and incoming
commit count. Include relevant PR links and explain fork-specific conflicts,
backend/config implications, and which clients would need releases. Distinguish
incoming upstream changes from features already present in this fork.

If there are no incoming changes, report that; do not create an empty merge
or release unless the user separately requested a release.

## Confirm, then execute

After presenting the changelog, ask whether to proceed with merging the
reviewed revision, resolving conflicts, committing and pushing to `origin`,
deploying the backend, and releasing the affected Mac/iPhone apps. Explain
that this confirmation is required by this skill's changelog-first workflow.
Wait for an affirmative reply before those actions. If the user already
approved the presented changelog and these actions in this conversation,
continue without asking again. Honor any narrower platform or action scope.

Use [update-from-upstream](../update-from-upstream/SKILL.md) for the merge,
fork-config reconciliation, checks, commit, push, and backend deployment.
Use [release-fork](../release-fork/SKILL.md) for authorized app publication.
Read both files directly from `.agents/skills/`, the canonical repository
skill directory; `.claude/skills` is a directory symlink to it. Reuse completed
checks and deploys rather than repeating them. Merge the reviewed upstream
revision; if a new fetch adds material changes, summarize those additions
before seeking approval for the expanded scope.

Preserve both upstream behavior and fork features, especially project tabs,
usage cards, self-auth, private updates, tracked `.agents/skills/`, and the
`.claude/skills` directory symlink and ignore exceptions. Inspect auto-merged
integration points too. If unrelated dirty
work or a product decision prevents a safe merge, describe the concrete
issue and ask for direction without discarding work.

Publication must use the fork release scripts and fork Wrangler configs.
Pushing `main` does not deploy or publish either app. Do not replace or
restart the running home Mac app merely to publish updates.

Report the committed/pushed source revision, checks, backend deployment,
Mac private-update version and ZIP path, and iPhone version/build with its
actual processing and internal-testing availability. Clearly identify any
incomplete step; do not claim uploaded or processing builds are installed.
