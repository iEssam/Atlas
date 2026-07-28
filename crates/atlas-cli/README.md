# atlas — Atlas CLI

A scriptable, **read-only** command-line interface over the running Atlas
service (PRD §18.3 / §7.5, tech-stack §4.8). It speaks the same gRPC surface as
the app and MCP server, over the same Windows named pipe (via `atlas-ipc`), and
runs only query RPCs.

## Read-only guarantee

The CLI never calls a mutating RPC. There is no end-task, no rule
create/enable/delete, no privacy-alert mutation — those are performed in the
Atlas app. Each subcommand maps 1:1 onto a read-only RPC; the mapping is a
static table (`commands::COMMAND_RPCS`) and a unit test asserts none of them
carries a mutating verb. The only `AtlasRules` call the CLI makes is the
read-only `ListRules`.

## Building

```sh
cargo build -p atlas-cli        # produces target/debug/atlas(.exe)
```

## Usage

Connect to a running service (`atlas-service serve`). `--pipe <disc>` selects the
pipe discriminator (matching `serve --pipe`); the default is the current user's
pipe. `--json` is a global flag on every command.

| Command | RPC | Notes |
| --- | --- | --- |
| `atlas top [--limit N]` | `AtlasQuery.GetSnapshot` | process table + system gauges |
| `atlas ports` | `AtlasQuery.ListListeningPorts` | listening TCP/UDP ports |
| `atlas connections` | `AtlasQuery.ListConnections` | active connections |
| `atlas locks <path>` | `AtlasQuery.FindResourceOwners` | who holds this file |
| `atlas history --metric sys-cpu --minutes N` | `AtlasQuery.QueryRange` | decimated min/max/avg |
| `atlas incidents [--minutes N]` | `AtlasQuery.ListIncidents` | detected incidents |
| `atlas diagnose --incident <id>` | `AtlasQuery.Diagnose` | evidence-based diagnosis |
| `atlas services [--filter X]` | `AtlasQuery.ListServices` | services inventory |
| `atlas startup` | `AtlasQuery.ListStartup` | startup inventory |
| `atlas tasks [--filter X]` | `AtlasQuery.ListScheduledTasks` | scheduled tasks |
| `atlas search <query>` | `AtlasQuery.Search` | full-text search |
| `atlas rules` | `AtlasRules.ListRules` | list rules (READ ONLY) |
| `atlas capabilities` | `AtlasQuery.GetCapabilities` | version + capability flags |

`history` accepts a metric id or friendly alias with hyphens or underscores:
`sys-cpu`, `sys-mem`, `sys-commit`, `sys-process-count`, `cpu`, `working-set`,
`private-bytes`, `read-bps`, `write-bps` (or the proto SCREAMING name).

When the service isn't running the CLI prints
`cannot reach the Atlas service … Is 'atlas-service serve' running?` and exits
non-zero.

### JSON / scriptability

Add `--json` to any command to get a single machine-readable JSON document
instead of the human table — this is the automation contract (§18.3):

```sh
atlas top --limit 5 --json
atlas capabilities --json
```

## PowerShell module

`atlas.psm1` wraps a few `atlas … --json` calls into cmdlets that parse the JSON
into objects (demonstrating the §7.5 automation story). It is thin and
read-only.

```powershell
Import-Module ./atlas.psm1
Get-AtlasProcess -Limit 5 | Format-Table pid, cpu_percent, image_name
Get-AtlasProcess | Where-Object { $_.cpu_percent -gt 10 }
Get-AtlasIncident -Minutes 120
Get-AtlasCapability
```

Cmdlets: `Get-AtlasProcess`, `Get-AtlasSystem`, `Get-AtlasIncident`,
`Get-AtlasListeningPort`, `Get-AtlasService`, `Get-AtlasCapability`, plus the
shared `Invoke-Atlas`. Each accepts `-Pipe` (service discriminator) and
`-AtlasPath` (path to the `atlas` binary, defaults to `atlas` on PATH).
