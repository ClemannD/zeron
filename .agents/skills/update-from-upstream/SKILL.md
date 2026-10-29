---
name: update-from-upstream
description: Update this fork's main branch from its configured upstream remote when asked to sync upstream changes.
---

# Update from upstream

1. Confirm the worktree is clean, inspect remotes, and fetch `upstream`.
2. Compare `main` with `upstream/main` (`git log --left-right` and commit counts). If already current, report that and skip the merge steps; still follow step 8 if the user requested an app release.
3. Run `git merge-tree --write-tree main upstream/main` before changing the branch. Report any conflicts it finds.
4. Merge `upstream/main` into `main`. Resolve conflicts by retaining both the upstream behavior and relevant local changes; do not discard either without approval.
5. Diff both `edge/wrangler.jsonc` and `edge/vault/wrangler.jsonc` against the pre-merge revision. Mirror required changes into this fork's `edge/wrangler.clemann.jsonc` / `edge/vault/wrangler.clemann.jsonc` as described in `AGENTS.md` ("Rules"). Preserve tracked `.agents/skills/`, the `.claude/skills` directory symlink, and their ignore exceptions if `.gitignore` changed.
6. Run focused formatting/tests and `npm run typecheck && npm test` in `edge/`, commit the merge and fork-config adjustments, and push `main` to `origin`.
7. Redeploy the self-hosted backend as required by `AGENTS.md`: read `docs/self-host.md`, deploy the vault before the edge, and pass `-c wrangler.clemann.jsonc` to every Wrangler command. Pushing `main` does not deploy our backend.
8. If the user also requested shipping the merged changes to devices, follow [release-fork](../release-fork/SKILL.md) for the affected Mac/iPhone apps. Reuse the checks and backend deployment already completed here. Otherwise report that app releases are still separate; a push does not publish Mac updates or TestFlight builds.

If the worktree is dirty, a remote/branch is missing, or a conflict needs a product decision, stop and ask the user.
