# RTK Server and Dashboard

RTK exposes the same read-only observability model through a loopback HTTP API
and a five-view terminal dashboard. The dashboard reads local state by default;
no server is required.

## Local dashboard

```bash
rtk dashboard
rtk tui          # hidden compatibility alias
```

Interactive terminals can switch among Overview, Hooks, Agents,
Savings/Failures, and ICM. Redirected output renders all five views once and
exits, which makes the command safe for logs and automation.

## Authenticated server

Set a strong, process-local bearer token and start the loopback listener:

```bash
export RTK_SERVER_TOKEN="$(openssl rand -hex 32)"
rtk server
```

The default address is `127.0.0.1:8745`. `--bind` accepts loopback addresses
only; RTK rejects wildcard and non-loopback addresses. Every endpoint except
`/health` requires `Authorization: Bearer $RTK_SERVER_TOKEN`.

| Endpoint | Data |
|---|---|
| `/health` | RTK and API version health record |
| `/v1/hooks` | Native-hook registration and compatibility state |
| `/v1/agents` | Full native/plugin/prompt-only integration report |
| `/v1/gain` | Token savings aggregates |
| `/v1/failures` | Parser failure and recovery aggregates |
| `/v1/audit` | Bounded tail of the configured hook audit log |
| `/v1/config` | Recursively redacted configuration |
| `/v1/icm` | Optional ICM localhost health |

Responses use `Cache-Control: no-store`, close each connection, and cap request
and response sizes. Savings and failure queries open the tracking database in
SQLite read-only mode. Agent collection avoids the verifier's live audit write
probe; run `rtk verify --all-agents` when a writability proof is required.

Connect the dashboard to the server with the same environment token:

```bash
rtk dashboard --server http://127.0.0.1:8745
```

## Optional ICM health

Both surfaces accept an ICM base URL. Only plain HTTP on `localhost` or an
explicit loopback IP is accepted.

```bash
rtk server --icm-url http://127.0.0.1:8746
rtk dashboard --icm-url http://127.0.0.1:8746
```

The bridge calls ICM's `/health` endpoint with bounded connect/read timeouts. It
does not link an ICM Rust crate and does not forward the RTK server token.

## Codex trust boundary

The dashboard reports Codex interception as incomplete until the generated
hook is reviewed in Codex `/hooks`. RTK never reads or writes Codex's trust
database. Awareness verification accepts either RTK's exact `RTK.md` artifact
or a managed `RULES.md` containing a complete `rtk-instructions` marker block.
When an environment frontdoor delegates to the immutable RTK payload, the
binary digest follows the executing payload instead of comparing unlike wrapper
and payload bytes.

## Behavioral provenance

The implementation was reconciled against the pinned upstream ICM branches
`feat/web-dashboard`, `feat/rust-hooks`, `feat/pretool-hook`, and
`feat/more-integrations`, plus FlexNetOS `feat/self-hosted-cloud-server`.
Those branches are behavioral references and provenance; their commits were
not blindly cherry-picked into RTK.
