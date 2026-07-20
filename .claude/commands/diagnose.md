---
model: haiku
description: RTK environment diagnostics for the native dispatcher and command routing
---

# /diagnose

Verify that the profile-owned RTK executable is available, command routing is
healthy, and Claude is configured with the native dispatcher. Legacy
repository-local shell hooks are intentionally absent and must not be restored.

## Checks

```bash
command -v rtk && rtk --version
rtk hook claude --help
rtk gain --history
git status --short --branch
```

When the active Claude settings are available, confirm that their `PreToolUse`
entry invokes `rtk hook claude` and that no command targets `.claude/hooks/`.

## Repair guidance

- Install or update RTK through the approved profile owner; do not use a
  repository-local shell wrapper.
- Run `rtk init` to migrate an existing Claude configuration. The migration
  removes legacy `rtk-rewrite.sh` payloads and preserves unrelated settings.
- Do not use `chmod`, copy scripts, or recreate `.claude/hooks/` as a repair.

## Output summary

Report the resolved `rtk` path, version, native dispatcher availability,
command-routing result, and any legacy hook path that needs migration.
