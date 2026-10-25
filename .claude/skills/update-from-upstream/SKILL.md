---
name: update-from-upstream
description: Update this fork's main branch from its configured upstream remote when asked to sync upstream changes.
---

# Update from upstream

1. Confirm the worktree is clean, inspect remotes, and fetch `upstream`.
2. Compare `main` with `upstream/main` (`git log --left-right` and commit counts). If already current, report that and stop.
3. Run `git merge-tree --write-tree main upstream/main` before changing the branch. Report any conflicts it finds.
4. Merge `upstream/main` into `main`. Resolve conflicts by retaining both the upstream behavior and relevant local changes; do not discard either without approval.
5. If the merge changed `edge/wrangler.jsonc` or `edge/vault/wrangler.jsonc`, mirror the changes into this fork's `edge/wrangler.clemann.jsonc` / `edge/vault/wrangler.clemann.jsonc` as described in `AGENTS.md` ("Rules"). Tell the user a redeploy is needed.
6. Run focused formatting/tests when available, commit the merge, and push `main` to `origin`.

If the worktree is dirty, a remote/branch is missing, or a conflict needs a product decision, stop and ask the user.
