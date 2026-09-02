# open-mcp

`omcp` (open-mcp) is a CLI MCP client that connects one model endpoint to one or more MCP servers. It supports HTTP Streamable MCP servers and JSON-lines stdio MCP servers.

## Install From Launchpad

The `fossware/open-mcp` PPA currently publishes packages for Ubuntu Noble (24.04). Add the PPA, refresh apt metadata, and install the package:

```bash
sudo add-apt-repository ppa:fossware/open-mcp
sudo apt update
sudo apt install open-mcp
```

Verify the installation:

```bash
omcp --version
mcp --help
filesystem-mcp-server --help
```

The package installs these commands under `/usr/bin`:

```text
omcp
mcp
```

## Build

The project targets Rust 1.75, matching the supported Ubuntu Launchpad toolchain.

```bash
cargo build --release
```

The binaries are `target/release/omcp` and `target/release/mcp`.

## Configuration Directory

The current implementation reads configuration from `~/.mcp`, not `~/.omcp`.

| File | Purpose |
| --- | --- |
| `~/.mcp/config` | Named `omcp` profiles. |
| `~/.mcp/hosts` | Reusable, named MCP host definitions. |
| `~/.mcp/settings` | Global settings. |
| `~/.mcp/*.json` | Optional per-profile meta files. |

## `mcp` CLI

`mcp` is the ad-hoc CLI. It accepts one model and one or more MCP hosts.

```text
mcp -host <MCP_URL|stdio:COMMAND> [-nsname <NAME>] [-headers <JSON>] [-body <JSON>] [-timeout <SECONDS>]
    -model <MODEL_NAME|MODEL_URL> [-headers <JSON>] [-body <JSON>] [-timeout <SECONDS>] [-messages <JSON>]
    [-tools <NAME,NAME,...>] [-message <TEXT>] [-meta <PATH>]
    [-prompt <TEXT>] [-prompt-type system|user|assistant] [-protocol-version <VERSION>]
```

### Endpoint Fields

`-host` selects an MCP server. It may be repeated. A plain name is resolved from `~/.mcp/hosts`; an HTTP URL uses Streamable HTTP transport; a value beginning with `stdio:` starts a JSON-lines stdio server process.

`-model` selects the model backend. A non-URL value is treated as an Ollama model name and uses `http://localhost:11434/api/chat`. A URL value is used as the model endpoint unchanged; provide that provider's model identifier in its `-body` JSON.

After `-host`, the following fields attach only to that host until another `-host` or `-model` occurs:

| Field | Meaning |
| --- | --- |
| `-nsname <NAME>` | Model-visible namespace for this host. Must immediately follow its `-host`. Allows ASCII letters, numbers, `_`, and `-`. |
| `-headers <JSON>` | HTTP header JSON object. Repeatable; later values override matching keys. HTTP only. |
| `-body <JSON>` | JSON object merged into each MCP JSON-RPC request. Repeatable; nested objects merge. |
| `-timeout <SECONDS>` | HTTP deadline for this host. `-1` means no timeout. |

After `-model`, the following fields attach only to that model endpoint until another selector occurs:

| Field | Meaning |
| --- | --- |
| `-headers <JSON>` | HTTP header JSON object for model requests. Repeatable. |
| `-body <JSON>` | JSON object merged into each model request. Repeatable. For Ollama, use `{"options":{"num_ctx":32768}}`. |
| `-timeout <SECONDS>` | HTTP deadline for model requests. `-1` means no timeout. |
| `-messages <JSON>` | JSON array of initial chat messages. Must follow `-model`. |

JSON headers use normal HTTP names and string values. For secrets, use `env:NAME` in a meta-file auth/header value rather than putting a credential into shell history.

### Session Fields

| Field | Meaning |
| --- | --- |
| `-tools <NAME,NAME,...>` | Restricts real MCP tools exposed to the model. Strongly recommended for large servers. Router tools remain available. |
| `-message <TEXT>` | Sends a one-shot user message, prints the final answer, then exits. |
| `-meta <PATH>` | Loads a per-profile JSON meta file. `~/` expands to the home directory. |
| `-prompt <TEXT>` | Adds one initial message to the chat history. |
| `-prompt-type <TYPE>` | Role for `-prompt`: `system` (default), `user`, or `assistant`. |
| `-protocol-version <VERSION>` | Overrides the MCP protocol version requested during initialization. |

### CLI Examples

Doc Mason via a named host and local Ollama:

```bash
mcp \
  -host docmason \
  -nsname docmason \
  -model granite4.2:latest \
  -body '{"options":{"num_ctx":32768}}' \
  -timeout -1 \
  -tools template.list,template.get \
  -prompt 'You are already authenticated. Use MCP tools before making claims.' \
  -message 'List my templates.'
```

Two MCP servers, one HTTP and one stdio:

```bash
mcp \
  -model granite4.2:latest \
  -body '{"options":{"num_ctx":32768}}' \
  -host docmason \
  -nsname docmason \
  -host 'stdio:/home/me/bin/filesystem-mcp-server --start-dir /home/me/project' \
  -nsname files
```

The model sees namespaced tools such as `docmason.template.list` and `files.ls`. `mcp.search_namespaces` and `mcp.search_tools` are always available in multi-host sessions.

## `omcp` Profile CLI

`omcp` reads a named entry from `~/.mcp/config`:

```bash
omcp list
omcp tools work
omcp chat work
omcp chat work 'List my templates.'
```

The optional trailing chat message runs non-interactively.

## `~/.mcp/config`

Configuration entries use ssh-config-like `name` blocks. All currently supported profile directives are shown below.

```text
name work
    # Required fallback MCP host. Used when mcp_hosts is not supplied.
    mcp https://docmason.co/mcp
    mcp_headers {"Authorization":"Bearer env:DOCMASON_API_KEY"}
    mcp_body {"client_context":{"source":"omcp"}}
    mcp_timeout 120

    # Optional multi-host collection. Each entry is a host string or object.
    mcp_hosts [{"host":"docmason"},{"host":"stdio:/home/me/bin/filesystem-mcp-server --start-dir /home/me/project","headers":{"X-Client":"omcp"},"body":{"client":"local"}}]

    # Required model name. A plain name uses the configured model_host.
    model granite4.2:latest
    # Optional model endpoint; defaults to model_host.
    model_host http://localhost:11434
    model_endpoint http://localhost:11434
    model_headers {"X-Client":"omcp"}
    model_body {"options":{"num_ctx":32768,"temperature":0.2}}
    model_timeout -1

    # Initial messages passed to the model.
    messages [{"role":"system","content":"Use tools when useful."}]

    # Optional per-profile JSON configuration.
    meta .mcp/work.json
```

Profile fields:

| Field | Meaning |
| --- | --- |
| `name` | Starts a profile; required. |
| `mcp` | Required fallback MCP endpoint. HTTP URL or `stdio:COMMAND`. |
| `mcp_headers` | JSON headers for the fallback MCP endpoint. |
| `mcp_body` | JSON body defaults for the fallback MCP endpoint. |
| `mcp_timeout` | Fallback MCP HTTP timeout in seconds; `-1` disables it. |
| `mcp_hosts` | JSON array of named aliases, endpoint strings, or endpoint objects with `host`, optional `headers`, and optional `body`. Replaces the fallback `mcp` endpoint collection. |
| `model` | Required model name. |
| `model_host` | Model base URL; defaults to `http://localhost:11434`. |
| `model_endpoint` | Explicit model endpoint; defaults to `model_host`. |
| `model_headers` | JSON headers for model requests. |
| `model_body` | JSON body defaults for model requests. |
| `model_timeout` | Model HTTP timeout in seconds; `-1` disables it. |
| `messages` | JSON array of initial messages. |
| `meta` | Per-profile meta JSON path. Relative paths resolve from `$HOME`. |

## `~/.mcp/hosts`

Use named hosts to keep credentials and MCP endpoint settings out of repeated command lines.

```text
# ~/.mcp/hosts
name docmason
    host https://docmason.co/mcp
    headers {"Authorization":"Bearer env:DOCMASON_API_KEY"}
    body {"client_context":{"source":"omcp"}}

name files
    host stdio:/home/me/bin/filesystem-mcp-server --start-dir /home/me/project
```

Fields in each host block:

| Field | Meaning |
| --- | --- |
| `name` | Alias used by `-host <name>`, `mcp_hosts`, and `/add-host -host <name>`. |
| `host` or `mcp` | HTTP MCP URL or `stdio:COMMAND`. |
| `headers` | JSON HTTP headers for that named host. |
| `body` | JSON body defaults for MCP requests. |

## `~/.mcp/settings`

The current global setting is `timeout`. It applies to HTTP MCP and model requests unless the selected endpoint has a CLI or profile timeout override.

```text
# Default HTTP timeout in seconds. -1 disables the client timeout.
timeout 420
```

With no settings file, the built-in default is `420` seconds. To disable the default timeout globally:

```text
timeout -1
```

Timeout precedence is endpoint `-timeout`, profile `mcp_timeout` / `model_timeout`, `~/.mcp/settings`, then the 420-second built-in default.

## Meta JSON

`-meta` and profile `meta` files support these fields:

```json
{
  "protocol_version": "2025-06-18",
  "auth": { "type": "bearer", "token": "env:DOCMASON_API_KEY" },
  "headers": { "X-Custom": "value" },
  "system_prompt": "Use tools when useful.",
  "allowed_tools": ["template.list", "template.get"],
  "model_params": { "num_ctx": 32768, "temperature": 0.2 }
}
```

| Field | Meaning |
| --- | --- |
| `protocol_version` | MCP initialization protocol version. |
| `auth` | Optional convenience auth object. Types: `bearer` with `token`; `basic` with `username` and `password`; `header` with `name` and `value`; `none`. |
| `headers` | Extra MCP HTTP headers. |
| `system_prompt` | A system message inserted before other initial messages. |
| `allowed_tools` | Tool allow-list used when no CLI `-tools` value is supplied. |
| `model_params` | Provider-specific parameters nested under `options` for the current model client. |

Values beginning with `env:` in meta auth/header values resolve environment variables, for example `env:DOCMASON_API_KEY`.

## Dynamic Hosts

In an interactive multi-host session, attach a host without restarting:

```text
/add-host -host docmason
/add-host -host https://example.test/mcp -headers {"Authorization":"Bearer token"} -body {"client":"omcp"}
```

`/add-host` supports `-host`, `-headers`, and `-body` only. The new host's tools are namespaced and become available to the model on its next request.