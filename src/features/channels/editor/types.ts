// Channel Editor internal types and constants
import type { Channel, ModelCatalogMetaUpdate } from '../types';

/** Form state */
export interface ChannelFormState {
  id?: string;
  name: string;
  api_type: string;
  base_url: string;
  api_key: string;
  notes: string;
  enabled: boolean;
  upstream_headers: string;
}

/** URL Probe result */
export interface UrlProbeResult {
  reachable: boolean;
  latency_ms: number;
  status_code?: number;
  detected_type?: string;
  corrected_base_url?: string;
  available_types?: string[];
  message: string;
}

/** Default form values */
export const DEFAULT_FORM: ChannelFormState = {
  name: '',
    api_type: 'openai',
  base_url: '',
  api_key: '',
  notes: '',
  enabled: true,
  upstream_headers: '',
};

/** API type list */
export const API_TYPES = [
  { value: 'openai', label: 'OpenAI-compatible' },
  { value: 'responses', label: 'OpenAI-Responses' },
  { value: 'anthropic', label: 'Anthropic/Claude' },
  { value: 'gemini', label: 'Google-Gemini' },
  { value: 'azure', label: 'Microsoft-Azure' },
  { value: 'other', label: 'Other (Exact URL)' },
] as const;

/** Map backend stored api_type values to UI dropdown values */
const BACKEND_TO_UI_API_TYPE: Record<string, string> = {
  custom: 'openai',   // legacy 'custom' stored value → OpenAI-compatible dropdown
  claude: 'anthropic', // backend stores 'claude', UI shows 'anthropic'
  openai: 'openai',
  responses: 'responses',
  gemini: 'gemini',
  azure: 'azure',
  other: 'other',
  anthropic: 'anthropic',
};

/** Map UI dropdown values back to backend api_type values to send on save */
export const UI_TO_BACKEND_API_TYPE: Record<string, string> = {
  openai: 'openai',
  anthropic: 'claude',  // UI shows 'anthropic', backend expects 'claude'
  gemini: 'gemini',
  azure: 'azure',
  responses: 'responses',
  other: 'other',
};

export function channelToForm(channel: Channel): ChannelFormState {
  return {
    id: channel.id,
    name: channel.name,
    base_url: channel.base_url,
    api_key: channel.api_key,
    notes: channel.notes ?? '',
    api_type: BACKEND_TO_UI_API_TYPE[channel.api_type] ?? channel.api_type,
    enabled: channel.enabled,
    upstream_headers: channel.upstream_headers ?? '',
  };
}


