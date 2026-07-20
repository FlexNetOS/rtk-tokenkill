# Claude Code Native Dispatcher

> Part of [`hooks/`](../README.md) — see also [`src/hooks/`](../../src/hooks/README.md) for installation code.

## Dispatcher contract

- `rtk init -g` registers `rtk hook claude` as the `PreToolUse` command.
- The Rust dispatcher returns the native `updatedInput` response and delegates
  rewrite decisions to `rtk rewrite`.
- The migration removes legacy shell payloads instead of installing a fallback.
- `rtk-awareness.md` is the slim instructions file embedded into CLAUDE.md by
  `rtk init`.

After configuration, restart Claude Code.

## Windows

The native dispatcher needs no Unix shell, Bash, or jq. Keep the RTK binary on
PATH, then run `rtk init -g` to register the dispatcher.
