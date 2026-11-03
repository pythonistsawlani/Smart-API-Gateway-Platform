// ============================================================
// Smart API Gateway - Type Definitions
// ============================================================

// --- Sort Mode ---

export type ModelSortMode = "latest" | "fastest" | "custom";

// --- Channel ---

export interface Channel {
  id: string;
  name: string;
  api_type: ApiType;
  base_url: string;
  api_key: string;
  available_models: ModelInfo[];
  selected_models: string[];
  enabled: boolean;
  last_fetch_at: number;
  notes: string;
  upstream_headers: string;
  response_ms: string;
  created_at: number;
  updated_at: number;
}

export type ApiType = "openai" | "claude" | "gemini" | "azure" | "custom" | "responses" | "other";

export const API_TYPE_OPTIONS: { value: ApiType; label: string }[] = [
  { value: "custom", label: "Custom (OpenAI-compatible)" },
  { value: "openai", label: "OpenAI" },
  { value: "claude", label: "Anthropic" },
  { value: "gemini", label: "Google Gemini" },
  { value: "azure", label: "Azure OpenAI" },
  { value: "responses", label: "OpenAI Responses (Beta)" },
  { value: "other", label: "Other (Exact URL)" },
];

export const API_TYPE_DEFAULT_URLS: Record<ApiType, string> = {
  openai: "https://api.openai.com",
  claude: "https://api.anthropic.com",
  gemini: "https://generativelanguage.googleapis.com",
  azure: "",
  custom: "",
  responses: "https://api.openai.com",
  other: "",
};

export interface ModelInfo {
  id: string;
  name: string;
  owned_by?: string;
}

export interface CreateChannelParams {
  name: string;
  api_type: ApiType;
  base_url: string;
  api_key: string;
  notes?: string;
  upstream_headers?: string;
}

export interface UpdateChannelParams {
  id: string;
  name?: string;
  api_type?: ApiType;
  base_url?: string;
  api_key?: string;
  enabled?: boolean;
  notes?: string;
  upstream_headers?: string;
}

// --- API Entry ---

export interface ApiEntry {
  id: string;
  channel_id: string;
  model: string;
  display_name: string;
  group_name: string;
  sort_index: number;
  enabled: boolean;
  cooldown_until?: number | null;
  circuit_state: CircuitState;
  created_at: number;
  updated_at: number;
  // Joined from channel
  channel_name?: string;
  channel_api_type?: ApiType;
  // Model provider (e.g. "openai", "anthropic", "google")
  owned_by?: string;
  // Response time from speed test (milliseconds string, or "X")
  response_ms?: string | null;
  score: number;
  provider_logo?: string | null;
  release_date?: string | null;
  model_meta_zh?: string | null;
  model_meta_en?: string | null;
}

export interface CreateEntryParams {
  channel_id: string;
  model: string;
  display_name?: string;
  provider_logo?: string;
  release_date?: string;
  model_meta_zh?: string;
  model_meta_en?: string;
  group_name?: string;
}

export type CircuitState = "closed" | "open" | "half_open";

// --- Access Key ---

export interface AccessKey {
  id: string;
  name: string;
  key: string;
  enabled: boolean;
  created_at: number;
}

// --- Usage Log ---

export interface UsageLog {
  id: number;
  type: number;
  content: string;
  access_key_id: string | null;
  access_key_name: string;
  token_name: string;
  api_entry_id: string;
  channel_id: string;
  channel_name: string;
  model: string;
  requested_model: string;
  quota: number;
  is_stream: boolean;
  prompt_tokens: number;
  completion_tokens: number;
  latency_ms: number;
  first_token_ms: number;
  use_time: number;
  status_code: number;
  success: boolean;
  request_id: string;
  log_group: string;
  other: string;
  error_message: string | null;
  ip: string | null;
  created_at: number;
}

export interface UsageLogFilter {
  start_time?: number;
  end_time?: number;
  model?: string;
  request_id?: string;
  channel_id?: string;
  access_key_id?: string;
  success?: boolean;
  page?: number;
  page_size?: number;
}

export interface PaginatedResult<T> {
  items: T[];
  total: number;
  page: number;
  page_size: number;
}

// --- Dashboard Stats ---

export interface DashboardStats {
  total_requests: number;
  today_requests: number;
  total_prompt_tokens: number;
  total_completion_tokens: number;
  today_prompt_tokens: number;
  today_completion_tokens: number;
  rpm: number;
  tpm: number;
  success_rate: number;
  avg_latency_ms: number;
}

export interface ChartDataPoint {
  time: string;
  model: string;
  value: number;
}

export interface ModelRanking {
  model: string;
  count: number;
  prompt_tokens: number;
  completion_tokens: number;
}

export interface UserRanking {
  access_key_name: string;
  count: number;
  prompt_tokens: number;
  completion_tokens: number;
}

export interface DashboardFilter {
  start_time?: number;
  end_time?: number;
  granularity?: "hour" | "day" | "week";
}

// --- Config ---

export interface AppSettings {
  proxy_enabled: boolean;
  listen_port: number;
  access_key_required: boolean;
  circuit_failure_threshold: number;
  proxy_connect_timeout_secs: number;
  circuit_recovery_secs: number;
  circuit_disable_codes: string;
  circuit_retry_codes: string;
  disable_keywords: string;
  keyword_freeze_scope: string;
  locale: string;
  theme: "light" | "dark" | "system";
  autostart: boolean;
  start_minimized: boolean;
  show_guide: boolean;
  default_sort_mode: ModelSortMode;
  // Remembers the default/restored group for the API Management UI only.
  active_group: string;
  show_conversation_model: boolean;
  disable_reasoning: boolean;
  record_raw_protocol_data: boolean;
  passthrough_header_injection: boolean;
  web_admin_enabled: boolean;
  web_admin_username: string;
  web_admin_password: string;
  web_admin_port: number;
  bind_external: boolean;
  rate_limit_enabled: boolean;
  rate_limit_rpm_per_ip: number;
  rate_limit_rpm_per_key: number;
  rate_limit_rpm_per_model: number;
  cors_allowed_origins: string;
  tls_enabled: boolean;
  tls_cert_path: string;
  tls_key_path: string;
  app_version: string;
}

export interface VersionedAppSettings extends AppSettings {
  _version: number;
}

export interface ChannelModelImportPreview {
  incomingChannels: number;
  incomingModels: number;
  currentChannels: number;
  currentModels: number;
  containsApiKeys: boolean;
  hasEmptyModelChannel: boolean;
  warnings: string[];
}

export interface ChannelModelImportResult {
  success: boolean;
  message: string;
  channelCount: number;
  modelCount: number;
}

export const DEFAULT_SETTINGS: VersionedAppSettings = {
  proxy_enabled: false,
  listen_port: 9090,
  access_key_required: true,
  circuit_failure_threshold: 5,
  proxy_connect_timeout_secs: 30,
  circuit_recovery_secs: 300,
  circuit_disable_codes: "401,403,410",
  circuit_retry_codes: "100-199,300-399,401-407,409-499,500-503,505-523,525-599",
  disable_keywords: "Your credit balance is too low\nThis organization has been disabled.\nYou exceeded your current quota\nPermission denied\nThe security token included in the request is invalid\nOperation not allowed\nYour account is not authorized\ninsufficient_quota\nquota_exceeded_error\ntoken plan limit exhausted\nUpstream rate limit exceeded\ninvalid api key\nUnauthorized - Invalid token",
  keyword_freeze_scope: "model",
  locale: "en",
  theme: "light",
  autostart: false,
  start_minimized: false,
  show_guide: true,
  default_sort_mode: "custom",
  active_group: "auto",
  show_conversation_model: false,
  disable_reasoning: true,
  record_raw_protocol_data: false,
  passthrough_header_injection: false,
  web_admin_enabled: false,
  web_admin_username: "",
  web_admin_password: "",
  web_admin_port: 9099,
  bind_external: false,
  rate_limit_enabled: true,
  rate_limit_rpm_per_ip: 120,
  rate_limit_rpm_per_key: 240,
  rate_limit_rpm_per_model: 600,
  cors_allowed_origins: "",
  tls_enabled: false,
  tls_cert_path: "",
  tls_key_path: "",
  app_version: __APP_VERSION__,
  _version: 0,
};

// --- Proxy ---

export interface ProxyStatus {
  running: boolean;
  address: string;
  port: number;
}

export interface AdminStatus {
  running: boolean;
  address: string;
  port: number;
}

// --- Platform Shell ---

export type PlatformShell = "web" | "desktop" | "android";

export interface PlatformCapabilities {
  shell: PlatformShell;
  isTauri: boolean;
  isAndroidShell: boolean;
  canOpenAndroidSettings: boolean;
  canUseConnectionApps: boolean;
}

// --- Limit Query ---

export type LimitCredentialStatus = "valid" | "expired" | "not_found" | "parse_error";

export interface LimitTier {
  name: string;
  utilization: number;
  resetsAt: string | null;
}

export interface LimitQueryResult {
  provider: string;
  credentialStatus: LimitCredentialStatus;
  credentialMessage: string | null;
  success: boolean;
  tiers: LimitTier[];
  error: string | null;
  queriedAt: number | null;
  raw: unknown | null;
}

// --- Test Chat ---

export interface TestChatResponse {
  content: string;
  latency_ms: number;
  usage?: {
    prompt_tokens: number;
    completion_tokens: number;
    total_tokens: number;
  };
}

// --- Translation Relay ---

/**
 * Request payload for translation relay.
 */
export interface TranslationRelayRequest {
  /** Text to be translated */
  text: string;
  /** Optional source language code */
  sourceLang?: string;
  /** Optional target language code */
  targetLang?: string;
  /** Optional identifier of the model/entry used for translation */
  entryId?: string;
}

/**
 * Payload representing the latest translation result.
 */
export interface TranslationRelayPayload {
  /** Original source text */
  sourceText: string;
  /** Translated text */
  translatedText: string;
  /** Source language, if known */
  sourceLang?: string;
  /** Target language, if known */
  targetLang?: string;
  /** Whether translation succeeded */
  success: boolean;
  /** Error message if failed */
  error?: string | null;
  /** Timestamp of the update (ms since epoch) */
  updatedAt: number;
}

/**
 * HTTP response shape for translation relay endpoint.
 */
export interface TranslationRelayResponse {
  /** Latest result, or null if none */
  latest: TranslationRelayPayload | null;
}

// --- Connection Apps ---

export interface ConnectionAppItem {
  id: string;
  name: string;
  description: string;
  icon: string;
  configMode: "write" | "clipboard";
  status?: "available" | "coming_soon";
  config: {
    file?: string;
    format?: "json" | "jsonc" | "toml";
    instructions?: string;
  };
}

export interface AppConfigResult {
  action: "write" | "clipboard";
  file_path?: string;
  backup_path?: string;
  content?: string;
  instructions?: string;
}
