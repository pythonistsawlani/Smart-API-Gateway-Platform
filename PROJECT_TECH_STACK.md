# Smart API Gateway — Technology Stack

> Project: **Smart API Gateway** (Personal API Management & Forwarding Center)
> Version: **0.8.42**
> Architecture: **Tauri 2 (Rust backend + React frontend)** — Desktop app with optional Web Admin UI

---

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────────────┐
│                         USER INTERFACES                             │
├─────────────────────┬───────────────────────────────────────────────┤
│  Tauri Desktop App  │              Web Admin (Browser)              │
│  (React + Tauri)    │           (React, served by Rust)             │
└──────────┬──────────┴────────────────────┬──────────────────────────┘
           │                                │
           ▼                                ▼
┌─────────────────────────────────────────────────────────────────────┐
│                      TAURI IPC / HTTP API                           │
│  • invoke (Rust commands)        • REST API (/admin/*, /v1/*)       │
│  • Events (proxy status, etc.)   • WebSocket (SSE for streaming)    │
└────────────────────────────┬────────────────────────────────────────┘
                             │
                             ▼
┌─────────────────────────────────────────────────────────────────────┐
│                         RUST BACKEND (src-tauri)                    │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐  ┌───────────┐  │
│  │   Proxy     │  │   Admin     │  │  Database   │  │  Commands │  │
│  │   Server    │  │   Server    │  │   (SQLite)  │  │  (Tauri)  │  │
│  │  :9090      │  │  :9099      │  │  (WAL mode) │  │           │  │
│  └──────┬──────┘  └──────┬──────┘  └──────┬──────┘  └─────┬─────┘  │
│         │                │                │                │        │
│         ▼                ▼                ▼                ▼        │
│  ┌─────────────────────────────────────────────────────────────┐   │
│  │              PROTOCOL ADAPTERS (OpenAI, Claude, Gemini,     │   │
│  │              Azure, Responses, Custom, Other)               │   │
│  └─────────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────────┘
                             │
                             ▼
┌─────────────────────────────────────────────────────────────────────┐
│                      UPSTREAM AI PROVIDERS                          │
│  OpenAI • Anthropic (Claude) • Google (Gemini) • Azure OpenAI      │
│  Custom OpenAI-compatible • OpenAI Responses API • Others          │
└─────────────────────────────────────────────────────────────────────┘
```

**Data Flow:**

1. **Desktop UI** → Tauri `invoke()` → Rust commands → Database / Proxy / Admin
2. **Web Admin** → HTTP REST → Admin Server (Axum) → Shared AppState → Database / Proxy
3. **Proxy Server** (Axum) → Protocol Adapters → Upstream AI APIs (reqwest)
4. **All components** share `AppState` (Arc<Database></database>, settings, proxy, admin, circuit-breaker state)

---

## Technology Stack

### Programming Languages

| Language                       | Version      | Where Used                 | Why                                                                             |
| ------------------------------ | ------------ | -------------------------- | ------------------------------------------------------------------------------- |
| **Rust**                 | 1.85+ (MSRV) | Backend (`src-tauri/`)   | Memory safety, concurrency, performance for proxy server; Tauri 2 requires Rust |
| **TypeScript**           | ~5.8.3       | Frontend (`src/`)        | Type-safe React development; strict mode enabled                                |
| **JavaScript (ES2022+)** | —           | Vite config, build scripts | Config files, mock server in dev                                                |
| **HTML/CSS**             | —           | Frontend                   | Standard web UI                                                                 |

---

### Frontend Framework / Libraries

| Library                                        | Version                | Purpose                                                              |
| ---------------------------------------------- | ---------------------- | -------------------------------------------------------------------- |
| **React**                                | 19.1.0                 | UI framework (concurrent features, hooks)                            |
| **React DOM**                            | 19.1.0                 | Rendering                                                            |
| **Vite**                                 | 7.0.4                  | Dev server, bundler, HMR                                             |
| **@vitejs/plugin-react**                 | 4.6.0                  | React Fast Refresh                                                   |
| **Tailwind CSS**                         | 4.1.0                  | Utility-first styling (v4 with Vite plugin)                          |
| **@tailwindcss/vite**                    | 4.1.0                  | Tailwind v4 Vite integration                                         |
| **Radix UI** (10+ packages)              | Latest                 | Unstyled accessible primitives (dialog, select, tabs, tooltip, etc.) |
| **@dnd-kit** (core, sortable, utilities) | 6.3.1 / 10.0.0 / 3.2.2 | Drag-and-drop for API pool reordering                                |
| **@tanstack/react-query**                | 5.90.3                 | Server state management, caching, polling                            |
| **React Hook Form**                      | 7.65.0                 | Form validation                                                      |
| **@hookform/resolvers**                  | 5.2.2                  | Zod resolver for RHF                                                 |
| **Zod**                                  | 4.1.12                 | Schema validation                                                    |
| **i18next**                              | 25.5.2                 | Internationalization (zh/en)                                         |
| **react-i18next**                        | 16.0.0                 | React bindings for i18next                                           |
| **Lucide React**                         | 0.542.0                | Icons                                                                |
| **Recharts**                             | 3.5.1                  | Dashboard charts                                                     |
| **Sonner**                               | 2.0.7                  | Toast notifications                                                  |
| **class-variance-authority**             | 0.7.1                  | Component variant styling                                            |
| **clsx**                                 | 2.1.1                  | Conditional classNames                                               |
| **tailwind-merge**                       | 3.3.1                  | Merge Tailwind classes                                               |

---

### Backend Framework / Libraries (Rust)

| Crate                           | Version                                                             | Purpose                                 |
| ------------------------------- | ------------------------------------------------------------------- | --------------------------------------- |
| **Tauri**                 | 2.x                                                                 | Desktop app framework (Rust ↔ WebView) |
| **tauri-plugin-opener**   | 2.x                                                                 | Open URLs/files cross-platform          |
| **tauri-plugin-process**  | 2.x                                                                 | Process management (desktop)            |
| **tauri-build**           | 2.x                                                                 | Build-time code generation              |
| **Axum**                  | 0.7                                                                 | HTTP server (Proxy + Admin)             |
| **axum-server**           | 0.7 (tls-rustls)                                                    | TLS support for Axum                    |
| **Tokio**                 | 1.x (macros, rt-multi-thread, time, sync, signal)                   | Async runtime                           |
| **Hyper**                 | 1.x (full)                                                          | HTTP implementation                     |
| **hyper-util**            | 0.1                                                                 | Hyper utilities                         |
| **http / http-body-util** | 1.x / 0.1                                                           | HTTP types                              |
| **Tower / tower-http**    | 0.4 / 0.5 (cors)                                                    | Middleware, CORS                        |
| **Reqwest**               | 0.12 (rustls-tls-native-roots, json, stream, gzip, brotli, deflate) | HTTP client for upstream APIs           |
| **Rusqlite**              | 0.31 (bundled)                                                      | SQLite bindings (WAL mode)              |
| **Serde / serde_json**    | 1.x (derive, preserve_order)                                        | Serialization                           |
| **Chrono**                | 0.4 (serde)                                                         | Date/time                               |
| **UUID**                  | 1.x (v4)                                                            | Unique IDs                              |
| **Thiserror / Anyhow**    | 2.0 / 1.0                                                           | Error handling                          |
| **Once_cell**             | 1.21                                                                | Lazy statics                            |
| **Regex**                 | 1.10                                                                | Pattern matching                        |
| **Auto-launch**           | 0.5 (optional)                                                      | Auto-start on login                     |
| **Include_dir**           | 0.7                                                                 | Embedded default pool data              |
| **Bytes / Futures**       | 1.5 / 0.3                                                           | Async primitives                        |
| **Log**                   | 0.4                                                                 | Logging facade                          |

---

### Database

| Technology           | Version                         | Configuration                                        |
| -------------------- | ------------------------------- | ---------------------------------------------------- |
| **SQLite**     | via `rusqlite 0.31 (bundled)` | Embedded, file-based                                 |
| **WAL Mode**   | Enabled                         | `PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;` |
| **Connection** | `Mutex<Connection>`           | Single-writer, multi-reader via mutex                |

**Schema (key tables):**

- `channels` — Upstream API providers (OpenAI, Claude, Gemini, Azure, Custom, Responses)
- `api_entries` — Individual models per channel, with group, sort, cooldown, metadata
- `access_keys` — Client API keys for proxy authentication
- `usage_logs` — Request/response logging (tokens, latency, status, error)
- `audit_log` — Admin action audit trail
- `config` — Key-value settings (proxy, circuit breaker, rate limit, TLS, etc.)

---

### APIs & Protocols

| Layer                                   | Technology                       | Details                                                                                   |
| --------------------------------------- | -------------------------------- | ----------------------------------------------------------------------------------------- |
| **Frontend → Backend (Desktop)** | Tauri IPC (`invoke`)           | 40+ generated commands (channels, pool, tokens, usage, config, proxy, admin, translation) |
| **Frontend → Backend (Web)**     | REST + SSE                       | `/admin/*` (Axum), `/v1/*` (Proxy), WebSocket/SSE for streaming                       |
| **Proxy → Upstream**             | HTTP/HTTPS (Reqwest)             | Protocol adapters transform OpenAI format ↔ Provider format                              |
| **Protocol Adapters**             | Custom trait `ProtocolAdapter` | OpenAI, Anthropic (Claude), Gemini, Azure, OpenAI Responses, Custom, Other                |
| **Streaming**                     | SSE (Server-Sent Events)         | Real-time token streaming with usage extraction                                           |
| **Auth (Proxy)**                  | Bearer tokens (Access Keys)      | Optional, enforced by `access_key_required` setting                                     |
| **Auth (Web Admin)**              | JWT-like token (localStorage)    | Login → token → `Authorization: Bearer <token>`                                       |

---

### Authentication & Security

| Feature                    | Implementation                                                                                                  |
| -------------------------- | --------------------------------------------------------------------------------------------------------------- |
| **Proxy Auth**       | Access Keys (UUID) — required if `access_key_required=1`                                                     |
| **Web Admin Auth**   | Username/password (bcrypt not used; token in localStorage) — env vars `API_SWITCH_ADMIN_USER/PASS` supported |
| **Safe Defaults**    | Auto-generates random admin password on first run; blocks `admin/admin` default                               |
| **Circuit Breaker**  | Failure threshold → cooldown → recovery; configurable disable/retry status codes                              |
| **Keyword Freeze**   | Case-insensitive error keyword matching → model/channel cooldown                                               |
| **Rate Limiting**    | Token bucket per IP / Key / Model (RPM) — returns 429 + Retry-After                                            |
| **CORS**             | Configurable allowed origins                                                                                    |
| **TLS/HTTPS**        | Optional, PEM cert/key via `axum-server` with `rustls`                                                      |
| **Header Injection** | Per-channel upstream headers (JSON)                                                                             |

---

### AI / ML Technologies

| Component                     | Description                                                                                                                |
| ----------------------------- | -------------------------------------------------------------------------------------------------------------------------- |
| **Protocol Adapters**   | Transform OpenAI chat format ↔ Provider-native format (request/response/SSE)                                              |
| **Supported Providers** | OpenAI, Anthropic (Claude), Google (Gemini), Azure OpenAI, OpenAI Responses API, Custom OpenAI-compatible, Generic "Other" |
| **Translation Relay**   | Uses LLM (via same proxy pipeline) to translate text; caches result for Web Admin display                                  |
| **Model Discovery**     | `/v1/models` fetch per channel with adapter-specific parsing                                                             |
| **Failover / Routing**  | Auto-group (`group_name='auto'`) priority-based; circuit breaker + keyword freeze                                        |

---

### DevOps / Build / Deployment

| Tool                        | Version                           | Purpose                                                          |
| --------------------------- | --------------------------------- | ---------------------------------------------------------------- |
| **pnpm**              | (lockfile)                        | Package manager                                                  |
| **Vite**              | 7.0.4                             | Frontend build (`dist/`, `dist-web-admin/`)                  |
| **Tauri CLI**         | 2.11.2                            | `tauri dev`, `tauri build`, `tauri android init/dev/build` |
| **Cargo**             | Rust 1.85+                        | Rust build (release profile: LTO, thin, opt-level=s)             |
| **GitHub Actions**    | `.github/workflows/release.yml` | CI/CD release pipeline                                           |
| **Android**           | Gradle (generated)                | Mobile build support                                             |
| **Windows Installer** | WiX (generated)                   | `.msi` / `.exe` output                                       |

**Build Commands:**

```bash
pnpm dev                # Full dev (web admin + Tauri)
pnpm build              # Production desktop build
pnpm android:build      # APK build
cargo build --release --features desktop  # Rust-only
```

---

### Testing Tools

| Tool                         | Scope                                                                                                      |
| ---------------------------- | ---------------------------------------------------------------------------------------------------------- |
| **Rust built-in test** | `#[cfg(test)]` modules in `proxy/protocol/mod.rs` (100+ unit tests for adapters, URL joining, parsing) |
| **Playwright**         | `@playwright/test` 1.59.1 (E2E, configured but tests not visible in repo)                                |
| **TypeScript**         | `pnpm typecheck` (`tsc --noEmit`)                                                                      |

---

### Important Third-Party Services / Integrations

| Service                               | Integration Point                                                   |
| ------------------------------------- | ------------------------------------------------------------------- |
| **OpenAI API**                  | Primary upstream (chat/completions, models, Responses API)          |
| **Anthropic (Claude)**          | Messages API, SSE streaming                                         |
| **Google Gemini**               | OpenAI-compatible endpoint (`/v1beta/openai/...`) + native format |
| **Azure OpenAI**                | Deployment-based URLs with `api-version`                          |
| **Custom OpenAI-compatible**    | DeepSeek, BigModel (Zhipu), etc.                                    |
| **System Tray / Auto-launch**   | OS integration (Windows/macOS/Linux)                                |
| **WebView2 / WebKit / WebView** | Tauri's platform webviews                                           |

---

### Major Dependencies Summary

| Category                     | Key Packages                                                                  |
| ---------------------------- | ----------------------------------------------------------------------------- |
| **Desktop Framework**  | `tauri`, `tauri-plugin-opener`, `tauri-plugin-process`                  |
| **HTTP Servers**       | `axum`, `axum-server`, `tower-http`                                     |
| **Async Runtime**      | `tokio`, `futures`, `hyper`                                             |
| **HTTP Client**        | `reqwest` (rustls, compression)                                             |
| **Database**           | `rusqlite` (bundled SQLite)                                                 |
| **Serialization**      | `serde`, `serde_json`                                                     |
| **Frontend Framework** | `react`, `react-dom`, `vite`                                            |
| **UI Primitives**      | `@radix-ui/*` (10 packages)                                                 |
| **State/Data**         | `@tanstack/react-query`, `react-hook-form`, `zod`                       |
| **Styling**            | `tailwindcss`, `@tailwindcss/vite`, `clsx`, `tailwind-merge`, `cva` |
| **i18n**               | `i18next`, `react-i18next`                                                |
| **Charts**             | `recharts`                                                                  |
| **Drag & Drop**        | `@dnd-kit/*`                                                                |
| **Icons/Toast**        | `lucide-react`, `sonner`                                                  |

---

## Feature Flags (Cargo)

| Feature     | Includes                                                         |
| ----------- | ---------------------------------------------------------------- |
| `core`    | Base library                                                     |
| `gui`     | `core` + `tauri` + `tauri-plugin-opener` + `tauri-build` |
| `desktop` | `gui` + `tray` + `tauri-plugin-process` + `auto-launch`  |
| `mobile`  | `gui`                                                          |
| `tray`    | `gui` + `tauri/tray-icon`                                    |

**Default:** `desktop` (via `pnpm dev` / `pnpm build` → `--features desktop`)

---

## Runtime Modes

| Mode                                   | Description                                                                        |
| -------------------------------------- | ---------------------------------------------------------------------------------- |
| **Combined** (default desktop)   | Single process: Proxy (`:9090`) + Admin (`:9090` same port) + Tauri GUI + Tray |
| **Standalone** (headless/server) | Proxy + Admin (separate port `:9099`) only; no Tauri GUI                         |
| **Web Admin Only**               | Vite dev server (`:1430`) proxies `/admin` to Rust admin (`:9099`)           |

Detection: CLI (`--headless`), Env (`API_SWITCH_HEADLESS=1`), Auto (Linux checks `DISPLAY`/`WAYLAND_DISPLAY`).

---

## Configuration Files

| File                                       | Purpose                                               |
| ------------------------------------------ | ----------------------------------------------------- |
| `package.json`                           | Frontend scripts, deps, pnpm                          |
| `src-tauri/Cargo.toml`                   | Rust crate, features, deps                            |
| `src-tauri/tauri.conf.json`              | Tauri app config (window, bundle, dev/build commands) |
| `vite.config.ts`                         | Main renderer dev/build + mock admin API              |
| `vite.config.web.ts`                     | Web Admin build (stubs Tauri APIs)                    |
| `tsconfig.json` / `tsconfig.node.json` | TypeScript config                                     |
| `.github/workflows/release.yml`          | CI/CD                                                 |

---

## File Structure (Key Directories)

```
Smart API/
├── src/                          # React frontend
│   ├── components/               # UI components (Radix-based)
│   ├── features/                 # Feature modules (shell, channels, pool, settings, translator)
│   ├── pages/                    # Page components (lazy-loaded)
│   ├── lib/                      # Utilities, adapters, hooks, i18n
│   ├── i18n/locales/             # zh.json, en.json
│   ├── main.tsx / App.tsx        # Entry, routing, auth gate
│   └── index.css                 # Tailwind v4 + CSS variables
├── src-tauri/
│   ├── src/
│   │   ├── main.rs / lib.rs      # Entry, Tauri setup, AppState
│   │   ├── admin/                # Admin HTTP server (Axum)
│   │   ├── proxy/                # Proxy server + protocol adapters
│   │   ├── database/             # SQLite schema, DAOs
│   │   ├── services/             # Translation, token, pool, channel, log, import/export
│   │   ├── commands/             # Tauri invoke handlers
│   │   └── *.rs                  # Core types, events, runtime modes
│   ├── Cargo.toml                # Rust deps, features
│   └── tauri.conf.json           # Tauri config
└── PROJECT_TECH_STACK.md         # This file
```

---

## Version References

- **App Version:** 0.8.42 (in `package.json`, `Cargo.toml`, `tauri.conf.json`)
- **Rust MSRV:** 1.85.0
- **Tauri:** 2.x
- **React:** 19.1.0
- **Vite:** 7.0.4
- **TypeScript:** ~5.8.3
- **SQLite:** 3.x (bundled via rusqlite 0.31)

---

*Generated from codebase inspection. No secrets or API keys included.*
