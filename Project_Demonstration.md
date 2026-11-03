# ?? Smart API Gateway Platform
### Project Demonstration & Presentation

**Course:** Software Engineering / Computer Science Project  
**Developer:** Karan Sawlani  
**Repository:** [github.com/pythonistsawlani/Smart-API-Gateway-Platform](https://github.com/pythonistsawlani/Smart-API-Gateway-Platform)  
**Version:** 0.8.42  
**Date:** August 2026

---

## ?? Table of Contents
1. [Executive Summary](#1-executive-summary)
2. [Problem Statement](#2-problem-statement)
3. [Proposed Solution](#3-proposed-solution)
4. [System Architecture](#4-system-architecture)
5. [Key Features Demo](#5-key-features-demo)
6. [Supported AI Providers](#6-supported-ai-providers)
7. [Technical Stack](#7-technical-stack)
8. [Live Demo Walkthrough](#8-live-demo-walkthrough)
9. [Code Highlights](#9-code-highlights)
10. [Challenges & Innovations](#10-challenges--innovations)
11. [Timeline & Milestones](#11-timeline--milestones)
12. [Future Roadmap](#12-future-roadmap)
13. [References](#13-references)

---

## 1. Executive Summary

> **Smart API Gateway Platform** is a local-first, cross-platform desktop application that acts as a **unified reverse proxy and management hub** for multiple Artificial Intelligence (AI) APIs.

Instead of managing multiple API keys, different endpoint formats, and varying protocols across many AI services, this platform gives developers **one single local endpoint** that internally routes, converts, load-balances, and monitors all AI traffic — completely on the user''s own machine.

**In one line:** *"One endpoint to rule all your AI models."*

---

## 2. Problem Statement

### The Pain Points of Modern AI Development

Modern developers face a critical fragmentation problem when integrating AI:

```
Developer''s App
      ¦
      +--- OpenAI API Key 1  --?  OpenAI GPT-4  
      +--- OpenAI API Key 2  --?  OpenAI GPT-3.5
      +--- Anthropic Key     --?  Claude 3.5 Sonnet
      +--- Google Key        --?  Gemini 1.5 Pro
      +--- Azure Key         --?  Azure GPT-4
      +--- HuggingFace Key   --?  Llama 3 / Custom Models
```

### Problems This Creates:

| Problem | Impact |
|---------|--------|
| ?? **Multiple API Keys** | Hard to manage, high risk if leaked |
| ?? **Different API Formats** | Each provider has a different request/response format |
| ?? **No Failover** | If one API fails, the entire app breaks |
| ?? **No Cost Visibility** | Cannot see which model costs what |
| ?? **Keys in Code** | Developers often hardcode keys (security risk) |
| ?? **No Monitoring** | No unified dashboard to track usage |

---

## 3. Proposed Solution

### Smart API Gateway — How It Fixes Everything

```
Developer''s App
      ¦
      +--- ONE LOCAL ENDPOINT: http://localhost:9090
                    ¦
                    ?
         +---------------------+
         ¦  Smart API Gateway  ¦
         ¦  -----------------  ¦
         ¦  Protocol Conv.     ¦
         ¦  Load Balancing     ¦
         ¦  Failover Logic     ¦
         ¦  Usage Logging      ¦
         ¦  Key Management     ¦
         +---------------------+
              ¦    ¦    ¦    ¦
              ?    ?    ?    ?
           OpenAI Claude Gemini Azure
```

---

## 4. System Architecture

### Technology Layers

| Layer | Technology | Purpose |
|-------|-----------|---------|
| **Desktop Shell** | Tauri v2 (Rust + WebView) | Cross-platform desktop app |
| **Frontend UI** | React + TypeScript + Vite | Admin dashboard |
| **UI Styling** | Tailwind CSS + Shadcn UI | Component library |
| **Backend Engine** | Rust + Axum | High-performance async server |
| **Database** | SQLite (via Rusqlite) | Local data persistence |
| **HTTP Client** | Reqwest | Outbound requests to AI providers |
| **Async Runtime** | Tokio | Non-blocking concurrent handling |

---

## 5. Key Features Demo

### 5.1 Multi-Provider Channel Management
Configure connections to any AI provider:
- Provider name and API Key
- API Type: OpenAI, Claude, Gemini, Azure, **Other (Exact URL)** ? NEW
- Smart URL detection and validation

### 5.2 Model Pool and Smart Routing
```
Incoming request: model = "gpt-4"
         ?
Gateway checks Model Pool:
  +- gpt-4 via OpenAI Ch.1   [1.2s] [ACTIVE]
  +- gpt-4 via OpenAI Ch.2   [1.8s] [ACTIVE]
  +- gpt-4 via Azure         [0.9s] [ACTIVE]
         ?
Routes to FASTEST: Azure (0.9s)
```

### 5.3 Three-Layer Resilience System
```
Request fails
    ? Layer 1: In-memory Circuit Breaker (auto-retry on next model)
    ? Layer 2: Persistent Cooldown (survives restarts)
    ? Layer 3: Channel Freeze (entire channel frozen if all models fail)

Result: ZERO downtime for your application!
```

### 5.4 Protocol Translation Engine
```json
// What YOUR app sends (OpenAI format):
{ "model": "claude-3-5-sonnet", "messages": [...] }

// Gateway automatically translates to Claude format:
{ "model": "claude-3-5-sonnet", "messages": [...], "max_tokens": 4096 }
// + Headers: { "x-api-key": "sk-ant-..." }

// Returns OpenAI-compatible response to YOUR app
```

### 5.5 Real-time Dashboard
- Total requests, success rate, token usage
- Model distribution and failure analysis
- API keys NEVER appear in logs

### 5.6 Access Keys Security Layer
- Create local client tokens for connected apps
- Revoke tokens instantly
- Your real API keys stay hidden

---

## 6. Supported AI Providers

| Provider | Type | Notable Models |
|----------|------|----------------|
| **OpenAI** | OpenAI-compatible | GPT-4o, GPT-4, GPT-3.5-turbo |
| **Anthropic** | Claude Protocol | Claude 3.5 Sonnet, Claude 3 Opus |
| **Google** | Gemini Protocol | Gemini 1.5 Pro, Gemini Flash |
| **Microsoft Azure** | Azure OpenAI | GPT-4 via Azure deployments |
| **Groq** | OpenAI-compatible | Llama 3, Mixtral (ultra-fast) |
| **Hugging Face** | Other (Exact URL) | Any model via Inference API |
| **Ollama** | OpenAI-compatible | Local models (Llama, Mistral) |
| **LM Studio** | OpenAI-compatible | Any local GGUF model |
| **DeepSeek** | OpenAI-compatible | DeepSeek-V3, DeepSeek-Coder |

---

## 7. Technical Stack

### Frontend
```
React 18 + TypeScript
  Vite (Build Tool)
  Tailwind CSS (Styling)
  Shadcn UI (Components)
  Tanstack Query (State Management)
  React i18next (i18n)
  Recharts (Analytics)
```

### Backend
```
Rust
  Tauri v2 (Desktop Framework)
  Axum (Async HTTP Server)
  Tokio (Async Runtime)
  Reqwest (HTTP Client)
  Rusqlite (SQLite Database)
  Serde (JSON Serialization)
```

### Why Rust?
- Performance: Handles hundreds of concurrent proxy requests
- Memory Safety: No garbage collector, no data races
- Binary Size: Single small executable
- Cross-platform: Windows, macOS, Linux

---

## 8. Live Demo Walkthrough

### Step 1: Launch
```
Double-click Smart API Gateway.exe
? Proxy starts on port 9090
? Admin UI opens
```

### Step 2: Add OpenAI Channel
```
Channels ? + Add Channel
  Name: "OpenAI Production"
  Type: OpenAI-compatible
  URL:  https://api.openai.com
  Key:  sk-proj-xxxx...
? Fetch Model List ? Save
```

### Step 3: Test from Any App
```bash
curl http://localhost:9090/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{"model":"gpt-4","messages":[{"role":"user","content":"Hello!"}]}'
```

### Step 4: Add Hugging Face (New Feature)
```
Channels ? + Add Channel
  Type: Other (Exact URL)  ? NEW
  URL:  https://api-inference.huggingface.co/v1/chat/completions
  Key:  hf_xxxx...
? Save

Models ? + Add Model
  Client ID: "llama-3"
  Actual ID: "meta-llama/Meta-Llama-3-8B-Instruct"
  Channel:   Hugging Face
? Save. Done!
```

### Step 5: View Dashboard
```
Dashboard ?
  Total Requests: 247
  Success Rate:   94.3%
  Tokens Used:    128,491
  Top Model:      gpt-4 (62%)
```

---

## 9. Code Highlights

### Protocol Adapter Pattern (Rust)
```rust
pub trait ProtocolAdapter {
    fn build_chat_url(&self, base_url: &str, model: &str) -> String;
    fn apply_auth(&self, builder: RequestBuilder, api_key: &str) -> RequestBuilder;
    fn transform_request(&self, body: &mut Value, actual_model: &str);
    fn transform_response(&self, body: &mut Value);
    fn needs_sse_transform(&self) -> bool;
}

pub fn get_adapter(api_type: &str) -> Box<dyn ProtocolAdapter + Send + Sync> {
    match api_type {
        "claude" | "anthropic" => Box::new(claude::ClaudeAdapter),
        "gemini"               => Box::new(gemini::GeminiAdapter),
        "azure"                => Box::new(azure::AzureAdapter),
        "responses"            => Box::new(responses::ResponsesAdapter),
        "other"                => Box::new(other::OtherAdapter),  // we added this!
        _                      => Box::new(openai::OpenAiAdapter),
    }
}
```

### New "Other" Adapter (Our Feature)
```rust
pub struct OtherAdapter;

impl ProtocolAdapter for OtherAdapter {
    fn build_chat_url(&self, base_url: &str, _model: &str) -> String {
        // KEY: no path appended, use exact URL as-is
        base_url.to_string()
    }
}
```

### API Type Mapping (TypeScript)
```typescript
export const UI_TO_BACKEND_API_TYPE: Record<string, string> = {
  openai: "openai",
  anthropic: "claude",   // UI says "Anthropic", backend stores "claude"
  gemini: "gemini",
  azure: "azure",
  other: "other",        // our new type
};
```

---

## 10. Challenges & Innovations

| Challenge | Our Solution |
|-----------|-------------|
| Protocol Heterogeneity | Adapter Pattern in Rust — each provider has its own transformation module |
| Streaming SSE Transformation | Custom SSE pipeline converts chunks in real-time without buffering |
| Hugging Face URL Bug | New "Other (Exact URL)" type bypasses all URL manipulation |
| API Type Mapping Mismatch | Explicit bidirectional mapping tables applied at all 4 usage points |
| Full Codebase Re-branding | Python replacement script + manual config file updates |

---

## 11. Timeline & Milestones

| Date | Milestone | Status |
|------|-----------|--------|
| 16–20 July 2026 | Problem Statement Allocation | ? Done |
| 31 July 2026 | Project Proposal Submission | ? Done |
| 14 August 2026 | Literature Survey Submission | ?? In Progress |
| 28 August 2026 | Design & Methodology Review | ?? Upcoming |
| 04 September 2026 | Progress Review-I | ?? Upcoming |
| 14–18 September 2026 | Mid-Term Evaluation | ?? Upcoming |
| 09 October 2026 | Progress Review-II | ?? Upcoming |
| 30 October 2026 | Progress Review-III | ?? Upcoming |
| 13 November 2026 | Draft Report Submission | ?? Upcoming |
| 23–27 November 2026 | End-Term Demonstration | ?? Upcoming |
| 27 November 2026 | Final Report Submission | ?? Upcoming |
| 30 November 2026 | Course Closure | ?? Upcoming |

### Work Done So Far:
- ? Full application setup and build environment configured
- ? Complete repository re-branding
- ? Removed all Chinese documentation and screenshots
- ? Fixed update checker to point to our GitHub
- ? Implemented "Other (Exact URL)" API type (new feature)
- ? Fixed critical api_type mapping bug
- ? Zero errors in cargo check and pnpm typecheck

---

## 12. Future Roadmap

### Phase 2 (Mid-Term)
- [ ] Pre-configured templates for Groq, Together AI
- [ ] Token usage estimation for non-OpenAI providers
- [ ] Cost estimation dashboard
- [ ] Export/import channel configurations

### Phase 3 (End-Term)
- [ ] Mobile-friendly Web Admin UI
- [ ] Rate limiting per access key
- [ ] Automatic API key rotation
- [ ] Docker support for headless deployment

---

## 13. References

1. OpenAI API Reference — https://platform.openai.com/docs/api-reference
2. Anthropic Claude API — https://docs.anthropic.com/claude/reference
3. Google Gemini API — https://ai.google.dev/api
4. Tauri v2 Framework — https://tauri.app/v2/guide/
5. Rust Async (Tokio) — https://tokio.rs/tokio/tutorial
6. Axum Web Framework — https://docs.rs/axum
7. React + TypeScript — https://react.dev/learn/typescript
8. Hugging Face Inference API — https://huggingface.co/docs/api-inference

---

## Appendix: Quick Setup

```bash
# Prerequisites: Node.js, pnpm, Rust, VS Build Tools (Windows)

git clone https://github.com/pythonistsawlani/Smart-API-Gateway-Platform.git
cd Smart-API-Gateway-Platform
pnpm install
pnpm tauri dev         # Development mode
pnpm tauri build       # Production build
```

**Default Ports:**
- Proxy Server:  http://localhost:9090  (point your AI tools here)
- Web Admin:     http://localhost:9091  (browser management)

---
*Smart API Gateway Platform v0.8.42 | Karan Sawlani | pythonistsawlani*
