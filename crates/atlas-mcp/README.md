# atlas-mcp — read-only MCP server for Atlas

`atlas-mcp` is the R2 "bring your own AI client" adapter (tech-stack.md §4.7). You
register this binary in your own MCP client (Claude Desktop, ChatGPT, or any MCP
host); the client speaks **MCP — JSON-RPC 2.0 over stdio** to `atlas-mcp`, and
`atlas-mcp` translates each tool call into **read-only `AtlasQuery` RPCs** over
the running service's named pipe.

It hosts **no model**. The client's model does the reasoning and writes the
answer; Atlas only supplies grounded, redacted evidence.

## Read-only by construction

This process builds **only** the `AtlasQuery` client — never `AtlasControl` or
`AtlasRules`. There is no tool that suspends, kills, or reconfigures anything.
The tool catalogue is asserted at test time to map exclusively to non-mutating
query RPCs (`no_tool_maps_to_a_mutating_rpc`). Process names, command lines, and
window titles in results are carried as inert JSON string values — untrusted
data, never interpreted as instructions — so an injected "kill this process"
cannot act.

## Tools

| Tool | AtlasQuery RPC |
| --- | --- |
| `top_consumers` | `GetSnapshot` |
| `query_timeline` | `QueryRange` |
| `find_events` | `ListEvents` |
| `search` | `Search` |
| `list_incidents` | `ListIncidents` |
| `explain_incident` | `Diagnose` |
| `explain_process` | `GetProcessDetail` |
| `list_services` | `ListServices` |
| `list_startup` | `ListStartup` |
| `list_connections` | `ListConnections` |
| `list_scheduled_tasks` | `ListScheduledTasks` |

Every result is **self-describing**: it carries a `grounding` block (source RPC,
capture time, a suggested citation string, redaction mode) and passes through the
RPC's own honesty markers (`available` / `unavailable_reason` / `truncated` /
`limited`).

## Redaction (MCP-strict, default-ON)

A tool result **egresses to the client's model provider** the moment the client
reads it, so redaction here is default-on and stricter than the in-app views.
Before anything leaves the process, every output field is scrubbed:

- file paths → `<PATH>`
- user names / SIDs → `<USER>`
- computer name → `<HOST>`
- DNS domains → `<DOMAIN>`
- command lines → `<CMD>`
- application / image names → `<APP>` (configurable)

Relax individual axes with flags: `--no-redact-paths`, `--no-redact-user-names`,
`--no-redact-computer-name`, `--no-redact-domains`, `--no-redact-command-lines`,
`--no-redact-app-names`.

## Usage

Start the service, then register `atlas-mcp` in your client:

```jsonc
{
  "mcpServers": {
    "system-atlas": {
      "command": "atlas-mcp",
      "args": ["--pipe", "mysession"]
    }
  }
}
```

`--pipe` matches the service's `serve --pipe` discriminator; omit it to use the
current user's default pipe. If the service isn't running, tool calls return a
clean MCP error, never a crash.

## The honest limitation

Atlas guarantees its MCP tools return **grounded, citation-ready evidence** (see
each result's `grounding` block). It **cannot** guarantee the external model's
final answer contains no unsupported claims — Atlas controls the tool results,
the client controls the conversation and the response. The grounding guarantee is
*citation-ready evidence*, not *every external answer is cited*.
