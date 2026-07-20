# `rtk_nu` byte-first adapter

`rtk_nu` is the separately packaged FlexNetOS adapter for a legacy command
whose stdout and stderr must reach a Nushell/CodeDB ingestion path without
losing bytes. It is deliberately not a mode of `rtk`, and it never runs a Nu
plugin or accepts an already-typed Nu pipeline. The installed `rtk` frontdoor
must be on `PATH`: the adapter delegates process launch to `rtk proxy`, the
repository-owned raw-command execution boundary, then captures the relayed
stdout and stderr byte streams before it performs any envelope serialization.

```bash
rtk_nu --format jsonl -- command --with-flags
rtk_nu --format json -- command --with-flags
rtk_nu --format nuon -- command --with-flags
```

The adapter launches the command with piped stdout and stderr, copies raw byte
chunks before any UTF-8 conversion or filtering, and emits base64 payloads,
stream-local byte offsets, monotonically ordered provisional frame IDs, SHA-256
content IDs, exit details, timing, caller-supplied identity context, and parser
and RTK-filter metadata. JSONL is streamed one raw-frame object at a time and
ends with an `execution_complete` object. JSON and Nuon provide one aggregate
envelope. The Nuon serializer emits native record fields and JSON-quoted string
literals, so `from nuon` has the same byte evidence as `from json`.

Unset identity fields are labelled `provisional:*`; an authoritative
CodeDB/envctl path must replace them only after verifying frame length and
digest and creating the canonical raw object. The selected environment is
represented solely by a caller-provided digest: raw environment values are not
placed in the envelope.

Raw frames are observed in the receiver's arrival order. Their `sequence`
provides the adapter's total order, while `byte_offset` preserves each stream's
exact order. Operating systems do not expose a stronger cross-pipe ordering
guarantee, so consumers must not infer one.
