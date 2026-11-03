# Smart API Gateway Platform

---

> A portable personal AI gateway: multi-provider routing, five-protocol conversion, smart failover, and one place to manage AI services.

Smart API Gateway is a local-first AI API management and forwarding hub. It gives your tools one local endpoint while managing multiple upstream providers and protocols, including OpenAI-compatible APIs, OpenAI Responses, Claude, Gemini, and Azure OpenAI.

> Scope note: Smart API Gateway is designed as a personal local tool. It trusts the local machine by default and is not a public multi-tenant security boundary. If you expose it to the internet, add your own reverse proxy, TLS, authentication, and access controls.

---

---

## Core Capabilities

| Capability                    | Description                                                                                            |
| ----------------------------- | ------------------------------------------------------------------------------------------------------ |
| Unified proxy endpoint        | Point clients to one local OpenAI-compatible endpoint and route requests to many upstream providers.   |
| Five protocol families        | OpenAI-compatible, OpenAI Responses, Claude Messages, Gemini, and Azure OpenAI adapters.               |
| Model pool and group routing  | Manual models, aliases, groups, exact match, fuzzy match, and AUTO fallback.                           |
| Smart ordering                | Custom order, fastest-first, newest-first, speed tests, and recommendation scores.                     |
| Three-layer resilience        | Persistent model cooldown, in-memory circuit breaker, and channel-level freeze.                        |
| Empty-stream protection       | Treats HTTP 200 streams with no valid output as failures instead of letting clients wait forever.      |
| Logs and dashboard            | Request volume, success rate, token usage, model distribution, failure path, and redacted diagnostics. |
| Shared Desktop / Web Admin UI | One React codebase for the Tauri desktop app and browser-based Web Admin.                              |
| Headless mode                 | Run only the backend proxy and admin services on a server, NAS, or remote host.                        |
| App connectors                | Generate or write configuration for tools such as OpenCode CLI, Codex CLI, Claude Code, and Zed.       |

---

## Supported Protocols and Upstreams

| Type              | Authentication                          | Typical behavior                                                                              |
| ----------------- | --------------------------------------- | --------------------------------------------------------------------------------------------- |
| OpenAI-compatible | Bearer Token                            | `/v1/chat/completions`, `/v1/models`                                                      |
| OpenAI Responses  | Bearer Token                            | `/v1/responses` compatibility layer with text, function tools, streaming, and Chat fallback |
| Claude Messages   | `x-api-key`                           | Main-path conversion between Claude Messages and OpenAI Chat                                  |
| Gemini            | query `key` / OpenAI-compatible paths | Gemini OpenAI-compatible endpoint and selected native endpoints                               |
| Azure OpenAI      | `api-key` header                      | Deployment-name-based routing and upstream request construction                               |
| Custom            | Bearer Token                            | Third-party OpenAI-compatible services and relay providers                                    |

Note: the current implementation still uses OpenAI Chat Completions as the primary internal compatibility layer. For advanced Responses, Claude, and Gemini semantics, Smart API Gateway narrows output through protocol-specific allowlists to avoid leaking unsupported fields.

---

## Runtime Modes

### Desktop

The Tauri v2 desktop app includes:

- React management UI
- Local proxy server
- Tauri IPC commands
- System tray
- Local window and settings management

### Web Admin

Web Admin uses the same React pages through HTTP Admin APIs:

- Browser-based management UI
- Bearer-token login and authentication
- Shared channel, model-pool, token, log, and settings management

### Headless / Server-only

For environments without a GUI, start only the backend services:

```bash
smart-api-gateway --headless
# or
SMART_API_GATEWAY_HEADLESS=1 smart-api-gateway
```

This is useful for servers, NAS devices, remote machines, or any setup that only needs the proxy and admin services.

### Android / Mobile

Android is treated as another build shell / compile branch of the same product, not a separate product line. The direction is to reuse the shared core and responsive UI. Real-device proxy listening, WebView lifecycle, loopback/cleartext behavior, and client integration are still future verification areas.

---

## Quick Start

### Download and Run

1. Download a build from [Releases](https://github.com/pythonistsawlani/Smart-API-Gateway-Platform/releases).
2. Start Smart API Gateway.
3. Add upstream API Base URLs and API keys in **Channels**.
4. Fetch models, or manually add models in **API Management**.
5. Point your client to the local Smart API Gateway proxy endpoint.

### Client Configuration

Default proxy port: `9090`.

```text
API Base URL: http://127.0.0.1:9090
API Key: any value; if Access Key enforcement is enabled, use a client token created in Smart API Gateway
Model: auto, a concrete model name, or a group name
```

Common routing inputs:

| Model value         | Behavior                                                                                     |
| ------------------- | -------------------------------------------------------------------------------------------- |
| `auto`            | Selects from enabled, non-cooled entries in the AUTO group.                                  |
| Concrete model name | Prefers exact match, then fuzzy/group matching and AUTO fallback according to routing rules. |
| Group name          | Maps several upstream models behind one stable client-facing name.                           |

---

## Routing and Resilience

Main request path:

```text
Client / AI Tool
  -> Smart API Gateway Proxy Endpoint
  -> protocol parser / compatibility layer
  -> router / failover / cooldown
  -> upstream provider
  -> response converter / stream relay
  -> usage log / dashboard stats
```

Resilience layers:

- **L1 DB cooldown (model-level)**: failed entries are cooled down in SQLite and remain cooled after restart.
- **L2 in-memory circuit breaker (model + channel)**: repeated failures open a short-lived breaker with half-open recovery probes.
- **L3 channel freeze (channel-level)**: account quota, key, or upstream channel failures can temporarily remove the whole channel from routing.
- **Status-code disable**: unrecoverable statuses such as 401 / 403 / 410 can disable the affected entry.
- **Streaming empty-output detection**: long-running SSE streams with no valid output trigger failure handling.
- **Traceable logs**: failure logs include attempt paths to show which candidates and conversion steps were tried.

---

## Management UI

Main pages:

- Dashboard: request count, success rate, token usage, model distribution, trend charts.
- Channels: upstream channels, URL probing, model fetching, protocol detection.
- API Management: model pool, groups, ordering, aliases, enabled state.
- Tokens: client Access Keys.
- Logs: pagination, filters, expanded details, failure diagnostics.
- Settings: ports, authentication, cooldown, theme, language, Web Admin.
- App Connectors: generate or write configuration for common AI tools.

---

## Local Development

Requirements:

- Node.js / pnpm
- Rust 1.85+
- Tauri v2 prerequisites

Common commands:

```bash
pnpm install
pnpm dev              # build Web Admin, then start desktop dev mode
pnpm build            # desktop build
pnpm build:web-admin  # build Web Admin frontend
pnpm typecheck        # TypeScript type check
```

Android commands are kept for the mobile build branch:

```bash
pnpm android:init
pnpm android:dev
pnpm android:build
```

---

## Tech Stack

| Layer    | Technology                  |
| -------- | --------------------------- |
| Desktop  | Tauri v2                    |
| Backend  | Rust, axum, reqwest         |
| Database | SQLite / rusqlite, WAL mode |
| Frontend | React 19, TypeScript, Vite  |
| UI       | Radix UI, Tailwind CSS v4   |
| State    | TanStack React Query        |
| Charts   | Recharts                    |
| i18n     | i18next / react-i18next     |

---

## Data and Security

- Data is stored locally in SQLite.
- Upstream API keys and client Access Keys are separated.
- Web Admin uses login plus Bearer Token authentication.
- Logs redact sensitive values before display.
- This project is not a public multi-tenant gateway; harden it yourself before public exposure.

---

## Documentation

- [Usage Guide](GUIDE.md)
- [Technical Whitepaper](WHITEPAPER.md)

---

## Project Status

Smart API Gateway is actively evolving for personal local use. Planned work includes a neutral internal representation, capability-based routing, more native Gemini endpoints, better preservation of advanced Responses / Claude / Gemini semantics, and Web Admin completion work.

---

If Smart API Gateway Platform helps you, consider giving it a Star on [GitHub](https://github.com/pythonistsawlani/Smart-API-Gateway-Platform).
