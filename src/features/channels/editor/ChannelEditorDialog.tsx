import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { toast } from 'sonner';
import { useQueryClient } from '@tanstack/react-query';
import { useTranslation } from 'react-i18next';
import { ChevronRight, RefreshCw, Save, X, Zap, Type } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { Checkbox } from '@/components/ui/checkbox';
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select';
import { cn } from '@/lib/utils';
import { getCatalogModel, formatTokenCount } from '@/lib/modelsCatalog';
import { useApiAdapter } from '@/lib/useApiAdapter';
import { getChannelErrorMessage } from '../channelErrors';
import type { ModelInfo, UpdateChannelParams, CreateChannelParams, FetchModelsResult, SaveChannelWithModelsParams } from '../types';
import type { ChannelFormState, UrlProbeResult } from './types';
import { DEFAULT_FORM, API_TYPES, channelToForm, UI_TO_BACKEND_API_TYPE } from './types';
import { buildEntryCatalogMeta, formatReleaseDate } from './utils';

type EditorModelInfo = ModelInfo & {
  sourceProtocol: string;
  temporary?: boolean;
};

/** Header injection control component */
const HeaderInjectionControl: React.FC<{
  value: string;
  onChange: (value: string) => void;
}> = ({ value, onChange }) => {
  const { t } = useTranslation();
  const [expanded, setExpanded] = useState(() => value.trim().length > 0);
  const [mode, setMode] = useState<'custom' | 'claude' | 'codex'>('custom');
  const [jsonText, setJsonText] = useState(value);
  const [jsonError, setJsonError] = useState<string | null>(null);

  // Preset templates
  const templates = {
    claude: JSON.stringify({
      "user-agent": "claude-cli/2.1.176 (external, cli)",
      "x-app": "cli",
      "anthropic-beta": "claude-code-20250219"
    }, null, 2),
    codex: JSON.stringify({
      "originator": "codex_cli_rs"
    }, null, 2)
  };

  // Validate JSON
  const validateJson = (text: string): boolean => {
    if (!text.trim()) {
      setJsonError(null);
      return true;
    }
    try {
      const parsed = JSON.parse(text);
      if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) {
        setJsonError('Must be a JSON object');
        return false;
      }
      if (!Object.values(parsed).every((value) => typeof value === 'string')) {
        setJsonError('Header name and value must be strings');
        return false;
      }
      setJsonError(null);
      return true;
    } catch {
      setJsonError('Invalid JSON format');
      return false;
    }
  };

  // Handle mode switch
  const handleModeChange = (newMode: 'custom' | 'claude' | 'codex') => {
    setMode(newMode);
    setExpanded(true);
    if (newMode !== 'custom') {
      const template = templates[newMode];
      setJsonText(template);
      onChange(template);
      setJsonError(null);
    } else {
      setJsonText(value);
      validateJson(value);
    }
  };

  // Handle text change
  const handleTextChange = (text: string) => {
    setJsonText(text);
    if (mode === 'custom') {
      validateJson(text);
      onChange(text);
    }
  };

  return (
    <div className="space-y-2">
      <button
        type="button"
        onClick={() => setExpanded(!expanded)}
        className="flex items-center gap-1.5 text-sm font-medium leading-none"
      >
        <ChevronRight
          className={cn(
            "h-4 w-4 shrink-0 text-muted-foreground transition-transform",
            expanded && "rotate-90"
          )}
        />
        Header injection config
      </button>
      <div className="flex rounded-md overflow-hidden border border-input">
        <Button
          type="button"
          size="sm"
          variant={mode === 'custom' ? "default" : "ghost"}
          onClick={() => handleModeChange('custom')}
          className="flex-1 rounded-none"
        >
          Custom
        </Button>
        <div className="w-px bg-border" />
        <Button
          type="button"
          size="sm"
          variant={mode === 'claude' ? "default" : "ghost"}
          onClick={() => handleModeChange('claude')}
          className="flex-1 rounded-none"
        >
          CLAUDE
        </Button>
        <div className="w-px bg-border" />
        <Button
          type="button"
          size="sm"
          variant={mode === 'codex' ? "default" : "ghost"}
          onClick={() => handleModeChange('codex')}
          className="flex-1 rounded-none"
        >
          CODEX
        </Button>
      </div>
      {expanded && (
        <>
          <textarea
            id="upstream-headers"
            value={jsonText}
            onChange={(e) => handleTextChange(e.target.value)}
            placeholder='{"x-custom": "value"}'
            rows={2}
            className={cn(
              "flex w-full resize-y rounded-md border border-input bg-transparent px-3 py-1.5 text-sm font-mono shadow-sm outline-none transition-colors placeholder:text-muted-foreground focus-visible:ring-1 focus-visible:ring-ring disabled:cursor-not-allowed disabled:opacity-50",
              jsonError && "border-destructive focus-visible:ring-destructive"
            )}
          />
          {jsonError && (
            <div className="text-xs text-destructive">{jsonError}</div>
          )}
          <div className="text-[11px] text-muted-foreground">
            Only static JSON objects are supported. Header names and values must be strings.
          </div>
        </>
      )}
    </div>
  );
};

/** Channel editor dialog component */
export const ChannelEditorDialog: React.FC<{
  open: boolean;
  channel: import('../types').Channel | null;
  onOpenChange: (value: boolean) => void;
  onSaved: () => Promise<void>;
}> = ({ open, channel, onOpenChange, onSaved }) => {
  const { t } = useTranslation();
  const api = useApiAdapter();
  const queryClient = useQueryClient();
  const [form, setForm] = useState<ChannelFormState>(DEFAULT_FORM);
  const [showApiKey, setShowApiKey] = useState(false);
  const [fetchingModels, setFetchingModels] = useState(false);
  const [modelsValidated, setModelsValidated] = useState(false);
  const [modelSearch, setModelSearch] = useState('');
  const [availableModels, setAvailableModels] = useState<EditorModelInfo[]>([]);
  const [selectedModels, setSelectedModels] = useState<string[]>([]);
  const [urlProbe, setUrlProbe] = useState<UrlProbeResult | null>(null);
  const [probingUrl, setProbingUrl] = useState(false);
  const [availableProtocols, setAvailableProtocols] = useState<string[]>([]);
  const [showModels, setShowModels] = useState(false);
  // Time range selection: 3/6/12 months
  const [timeRange, setTimeRange] = useState<3 | 6 | 12>(3);
  // Model latency test state
  const [testingModels, setTestingModels] = useState(false);
  const [modelTestResults, setModelTestResults] = useState<Record<string, { success: boolean; latency?: number; reason?: string; statusCode?: number }>>({});

  const probeSeqRef = useRef(0);
  const fetchSeqRef = useRef(0);
  const testSeqRef = useRef(0);
  const existingModelsRef = useRef<Set<string>>(new Set());
  const [saving, setSaving] = useState(false);

  // Initialize form data
  useEffect(() => {
    if (!open) return;
    setSaving(false);
    setModelSearch('');
    setTimeRange(3);
    setShowApiKey(false);
    setAvailableProtocols([]);
    existingModelsRef.current = new Set();
    // Default hide model section: only expand by default if channel has models
    const hasModels = !!channel && ((channel.available_models?.length || 0) > 0 || (channel.selected_models?.length || 0) > 0);
    setModelsValidated(!!channel && ((channel.available_models?.length || 0) > 0));
    setShowModels(hasModels);
    if (channel) {
      setForm(channelToForm(channel));
      setAvailableModels((channel.available_models || []).map((model) => ({
        ...model,
        sourceProtocol: channel.api_type,
      })));
      setSelectedModels(channel.selected_models || []);
    } else {
      setForm(DEFAULT_FORM);
      setAvailableModels([]);
      setSelectedModels([]);
    }
   }, [channel, open]);

  const apiKeys = useMemo(() => form.api_key.split('\n').map((key) => key.trim()).filter(Boolean), [form.api_key]);
  const primaryApiKey = apiKeys[0] ?? '';

  // URL detection
  useEffect(() => {
    const seq = ++probeSeqRef.current;
    if (!form.base_url.trim()) {
      setUrlProbe(null);
      setProbingUrl(false);
      return;
    }
    setUrlProbe(null);
    setProbingUrl(true);
    const timer = setTimeout(async () => {
      try {
        const result = await api.channels.probeUrl(form.base_url.trim(), UI_TO_BACKEND_API_TYPE[form.api_type] ?? form.api_type, primaryApiKey);
        if (probeSeqRef.current === seq) {
          setUrlProbe(result as UrlProbeResult);
          if (result.reachable) {
            const types = result.available_types?.length
              ? result.available_types
              : result.detected_type
                ? [result.detected_type]
                : [];
            setAvailableProtocols(types);
            if (result.detected_type || result.corrected_base_url) {
              setForm(prev => ({
                ...prev,
                api_type: result.detected_type || prev.api_type,
                base_url: result.corrected_base_url || prev.base_url,
              }));
            }
          }
        }
      } catch {
        if (probeSeqRef.current === seq) {
          setUrlProbe({ reachable: false, status_code: undefined, latency_ms: 0, detected_type: undefined, corrected_base_url: undefined, message: t('channel.editor.probeFailedGeneric', 'Probe failed') });
        }
      } finally {
        if (probeSeqRef.current === seq) {
          setProbingUrl(false);
        }
      }
    }, 800);
    return () => clearTimeout(timer);
  }, [api, form.api_type, form.base_url, primaryApiKey, t]);

  // canSave must check 4 items: name, api_type, base_url, api_key
  const canSave = !!(form.name.trim() && form.api_type.trim() && form.base_url.trim() && primaryApiKey);
  
  // canFetchModels: Attempt if basic inputs are met; URL probe failure doesn't block fetching
  const canFetchModels = !!(form.name.trim() && form.api_type.trim() && form.base_url.trim() && primaryApiKey && !fetchingModels);

  const setValue = <K extends keyof ChannelFormState>(key: K, value: ChannelFormState[K]) => {
    if (key === 'api_type' || key === 'base_url' || key === 'api_key') {
      setModelsValidated(false);
      setUrlProbe(null);
      setAvailableModels([]);
      setSelectedModels([]);
      if (key !== 'api_type') setAvailableProtocols([]);
    }
    setForm((current) => ({ ...current, [key]: value }));
  };

  const handleApiTypeChange = (type: string) => {
    setForm((prev) => ({
      ...prev,
      api_type: type,
      base_url: prev.base_url,
    }));
    setAvailableModels([]);
    setSelectedModels([]);
    setModelsValidated(false);
  };

  useEffect(() => {
    if (!open || !channel?.id) {
      existingModelsRef.current = new Set();
      return;
    }
    let cancelled = false;
    api.pool.list()
      .then((entries) => {
        if (cancelled) return;
        existingModelsRef.current = new Set(
          entries
            .filter((entry) => entry.channel_id === channel.id)
            .map((entry) => entry.model.toLowerCase()),
        );
      })
      .catch(() => {
        if (!cancelled) {
          existingModelsRef.current = new Set();
        }
      });
    return () => {
      cancelled = true;
    };
  }, [api, channel?.id, open]);

  // Auto select models: models released in selected time range + existing models + temp created models
  const autoSelectModels = useCallback((models: EditorModelInfo[]): string[] => {
    const rangeStart = new Date();
    rangeStart.setMonth(rangeStart.getMonth() - timeRange);
    const rangeStartStr = rangeStart.toISOString().slice(0, 10);

    const selected = new Set<string>();

    for (const model of models) {
      const catalog = getCatalogModel(model.name);
      if (model.temporary || (catalog?.release_date && formatReleaseDate(catalog.release_date) >= rangeStartStr)) {
        selected.add(model.name);
      }
    }

    for (const model of models) {
      if (existingModelsRef.current.has(model.name.toLowerCase())) {
        selected.add(model.name);
      }
    }

    return Array.from(selected);
  }, [timeRange]);

  useEffect(() => {
    if (!showModels || availableModels.length === 0) return;
    setSelectedModels(autoSelectModels(availableModels));
  }, [showModels, availableModels, autoSelectModels]);

  const createEditorModels = (models: Array<Record<string, unknown>>, sourceProtocol: string): EditorModelInfo[] =>
    models.map((item, index) => ({
      id: String(item.id ?? item.name ?? index),
      name: String(item.name ?? ''),
      owned_by: typeof item.owned_by === 'string' ? item.owned_by : undefined,
      sourceProtocol,
    })).filter((item) => item.name);

  const mergeModelsByName = (groups: EditorModelInfo[][]): EditorModelInfo[] => {
    const map = new Map<string, EditorModelInfo>();
    for (const group of groups) {
      for (const model of group) {
        const key = model.name.toLowerCase();
        if (!map.has(key)) map.set(key, model);
      }
    }
    return Array.from(map.values());
  };

  const withTimeout = async <T,>(promise: Promise<T>, ms: number, message: string): Promise<T> => {
    let timer: ReturnType<typeof setTimeout> | undefined;
    const timeout = new Promise<never>((_, reject) => {
      timer = setTimeout(() => reject(new Error(message)), ms);
    });
    try {
      return await Promise.race([promise, timeout]);
    } finally {
      if (timer) clearTimeout(timer);
    }
  };

  const fetchModelsByProtocol = async (apiType: string): Promise<{ result: FetchModelsResult; models: EditorModelInfo[] }> => {
    const backendApiType = UI_TO_BACKEND_API_TYPE[apiType] ?? apiType;
    const result = await api.channels.fetchModelsDirect(backendApiType, form.base_url, primaryApiKey, false, form.upstream_headers || undefined);
    if (result.models.length > 0) {
      setAvailableProtocols(prev => Array.from(new Set([...prev, apiType])));
    }
    const sourceProtocol = result.detected_type || apiType;
    return {
      result,
      models: createEditorModels(result.models || [], sourceProtocol),
    };
  };

  // Fetch model list
  const handleFetchModels = async () => {
    const seq = ++fetchSeqRef.current;

    setFetchingModels(true);
    setModelsValidated(false);
    try {
      const fetched = await withTimeout(
        fetchModelsByProtocol(form.api_type),
        10_000,
        t('channel.editor.fetchModelsTimeout', 'Fetch models timeout'),
      );
      if (fetchSeqRef.current !== seq) return;

      const finalModels = mergeModelsByName([fetched.models]);
      setShowModels(true);
      setSelectedModels([]);
      setAvailableModels(finalModels);
      setModelTestResults({});
      setModelsValidated(true);
      const nextSelected = autoSelectModels(finalModels);
      if (fetchSeqRef.current !== seq) return;
      setSelectedModels(nextSelected);

      if (finalModels.length === 0) {
        toast.warning(t('channel.editor.noModelsFetched', 'No models fetched'));
      }
    } catch (err) {
      if (fetchSeqRef.current === seq) {
        toast.error(getChannelErrorMessage(err, t('channel.editor.fetchModelsFailed', 'Failed to fetch model list')));
      }
    } finally {
      if (fetchSeqRef.current === seq) {
        setFetchingModels(false);
      }
    }
  };

  const toggleModel = (modelName: string) => {
    setSelectedModels((prev) =>
      prev.includes(modelName)
        ? prev.filter((m) => m !== modelName)
        : [...prev, modelName],
    );
  };

  const selectAllFiltered = () => {
    const names = filteredModels.map((m) => m.name);
    setSelectedModels((prev) => Array.from(new Set([...prev, ...names])));
  };

  const clearAllSelected = () => {
    if (modelSearch.trim()) {
      const namesToClear = new Set(filteredModels.map((model) => model.name));
      setSelectedModels((prev) => prev.filter((name) => !namesToClear.has(name)));
      return;
    }
    setSelectedModels([]);
  };

  const createTemporaryModel = () => {
    const name = modelSearch.trim();
    if (!name) return;
    const exists = availableModels.some((model) => model.name.toLowerCase() === name.toLowerCase());
    if (exists) return;
    const model: EditorModelInfo = {
      id: `temp-${name}`,
      name,
      sourceProtocol: form.api_type,
      temporary: true,
    };
    setAvailableModels((prev) => [...prev, model]);
    setSelectedModels((prev) => Array.from(new Set([...prev, name])));
  };

  const handleTestModels = async () => {
    if (filteredModels.length > 30) {
      setModelTestResults({
        __limit__: {
          success: false,
          reason: t('channel.editor.tooManyModels', 'Too many models ({{count}}). Test will take too long. Please reduce filter range.', { count: filteredModels.length }),
        },
      });
      return;
    }

    const seq = ++testSeqRef.current;
    setTestingModels(true);
    setModelTestResults({});
    const results: Record<string, { success: boolean; latency?: number; reason?: string; statusCode?: number }> = {};
    try {
        await withTimeout(
        (async () => {
          for (const model of filteredModels) {
            if (testSeqRef.current !== seq) return;
            try {
              const result = await api.channels.testChannelDirect({
                api_type: UI_TO_BACKEND_API_TYPE[form.api_type] ?? form.api_type,
                base_url: form.base_url,
                api_key: primaryApiKey,
                model: model.name,
                upstream_headers: form.upstream_headers || undefined,
              });
              results[model.name] = result.success
                ? { success: true, latency: result.latency_ms }
                : { success: false, reason: result.message, statusCode: result.status_code };
            } catch (err) {
              results[model.name] = { success: false, reason: err instanceof Error ? err.message : String(err) };
            }
            if (testSeqRef.current === seq) setModelTestResults({ ...results });
          }
        })(),
        10_000,
        t('channel.editor.testModelsTimeout', 'Test timeout'),
      );
    } catch (err) {
      if (testSeqRef.current === seq) {
        setModelTestResults((prev) => ({
          ...prev,
          __timeout__: {
            success: false,
            reason: err instanceof Error ? err.message : String(err),
          },
        }));
      }
    } finally {
      if (testSeqRef.current === seq) {
        setTestingModels(false);
      }
    }
  };



  const handleSave = async () => {
    if (saving || fetchingModels) return;
    if (!canSave) {
      toast.error(t('channel.editor.requiredFieldsHint', 'Please fill in a valid Base URL and API Key before saving'));
      return;
    }

    // Validate upstream_headers JSON
    if (form.upstream_headers.trim()) {
      try {
        const parsed = JSON.parse(form.upstream_headers);
        if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) {
          toast.error(t('channel.editor.headerInjectionInvalidObject', 'Header injection config must be a valid JSON object'));
          return;
        }
        if (!Object.values(parsed).every((value) => typeof value === 'string')) {
          toast.error(t('channel.editor.headerInjectionInvalidType', 'Header name and value must be strings'));
          return;
        }
      } catch {
        toast.error(t('channel.editor.headerInjectionInvalidJson', 'Header injection config is not a valid JSON format'));
        return;
      }
    }

    setSaving(true);
    try {
      const keys = form.id ? [primaryApiKey] : apiKeys;
      const successNames: string[] = [];
      const failedNames: string[] = [];

      for (let i = 0; i < keys.length; i++) {
        const key = keys[i];
        const name = keys.length > 1 ? `${form.name}-${i + 1}` : form.name;
        
        const saveParams: SaveChannelWithModelsParams = {
          id: form.id || undefined,
          name: name,
          api_type: UI_TO_BACKEND_API_TYPE[form.api_type] ?? form.api_type,
          base_url: form.base_url,
          api_key: key,
          notes: form.notes,
          enabled: form.enabled,
          selected_models: selectedModels,
          available_models: availableModels,
          catalog_meta: selectedModels.map(buildEntryCatalogMeta),
          response_ms: urlProbe?.reachable && urlProbe.latency_ms > 0 ? String(urlProbe.latency_ms) : undefined,
          upstream_headers: form.upstream_headers || undefined,
        };

        try {
          const result = await api.channels.saveChannelWithModels(saveParams);
          successNames.push(name);
          if (result.warnings && result.warnings.length > 0) {
            result.warnings.forEach((w: string) => { toast.warning(w); });
          }
        } catch (err) {
          failedNames.push(name);
          if (keys.length === 1) throw err;
          toast.error(`${name}: ${getChannelErrorMessage(err, t('channel.editor.saveFailed', 'Failed to save channel'))}`);
        }
      }

      if (successNames.length > 0 && keys.length > 1) {
        toast.success(t('channel.editor.batchSaveSummary', 'Batch save completed: {{success}} succeeded, {{failed}} failed', { success: successNames.length, failed: failedNames.length }));
      }
      if (successNames.length === 0 && failedNames.length > 0) {
        throw new Error(t('channel.editor.batchSaveAllFailed', 'All model creation and saves failed'));
      }

      await onSaved();
      queryClient.invalidateQueries({ queryKey: ['entries'] });
      onOpenChange(false);
    } catch (err) {
      toast.error(getChannelErrorMessage(err, t('channel.editor.saveFailed', 'Failed to save channel')));
    } finally {
      setSaving(false);
    }
  };

  const handleClose = () => {
    queryClient.invalidateQueries({ queryKey: ['channels'] });
    queryClient.invalidateQueries({ queryKey: ['entries'] });
    onOpenChange(false);
  };

  const filteredModels = useMemo(() => {
    if (!modelSearch) return availableModels;
    return availableModels.filter((m) => m.name.toLowerCase().includes(modelSearch.toLowerCase()));
  }, [availableModels, modelSearch]);


return (
    <Dialog open={open} onOpenChange={(value) => {
      if (!value) setSaving(false);
      if (!value) {
        queryClient.invalidateQueries({ queryKey: ['channels'] });
        queryClient.invalidateQueries({ queryKey: ['entries'] });
      }
      onOpenChange(value);
    }}>
      <DialogContent className="flex max-h-[calc(100dvh-1rem)] flex-col overflow-hidden sm:max-w-[600px] sm:max-h-[calc(100dvh-2rem)]">
        <DialogHeader>
          <DialogTitle>{channel ? t('channel.editor.editTitle') : t('channel.editor.title')}</DialogTitle>
        </DialogHeader>

        <div className="min-h-0 flex-1 space-y-4 overflow-y-auto pr-1">
          {/* Channel info section */}
          <div className="min-w-0 space-y-4 rounded-md border border-border/60 p-3 sm:p-4">

            <div className="space-y-2">
              <Label htmlFor="channel-name">{t('channel.editor.channelName')}</Label>
              <Input id="channel-name" value={form.name} onChange={(event) => setValue('name', event.target.value)} placeholder={t('channel.form.placeholderName')} />
            </div>

            <div className="space-y-2">
              <Label htmlFor="channel-apitype">{t('channel.editor.apiType')}</Label>
              <Select value={form.api_type} onValueChange={handleApiTypeChange}>
                <SelectTrigger id="channel-apitype">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {API_TYPES.map((item) => {
                    const isAvailable = availableProtocols.includes(item.value);
                    return (
                      <SelectItem
                        key={item.value}
                        value={item.value}
                        className={cn(isAvailable && "bg-green-600 text-white data-[highlighted]:bg-green-700 data-[highlighted]:text-white")}
                      >
                        {item.label}
                      </SelectItem>
                    );
                  })}
                </SelectContent>
              </Select>
            </div>

            <div className="space-y-2">
              <Label htmlFor="channel-baseurl">{t('channel.editor.baseUrl')}</Label>
              <div className="relative min-w-0">
                <Input
                  id="channel-baseurl"
                  value={form.base_url}
                  onChange={(event) => setValue('base_url', event.target.value)}
                  placeholder={form.api_type === 'other' ? 'https://api-inference.huggingface.co/models/YOUR_MODEL/v1/chat/completions' : t('channel.form.placeholderBaseUrl')}
                  className={urlProbe ? (urlProbe.reachable ? 'pr-24 border-green-500/50 focus-visible:ring-green-500/30' : 'pr-12 border-red-500/50 focus-visible:ring-red-500/30') : 'pr-8'}
                />
                <div className="absolute right-1.5 top-1/2 -translate-y-1/2 flex items-center gap-1 pointer-events-none">
                  {probingUrl ? (
                    <RefreshCw className="h-3.5 w-3.5 animate-spin text-muted-foreground" />
                  ) : urlProbe?.reachable ? (
                    <span className="text-[10px] text-green-600 font-medium whitespace-nowrap">✓ {urlProbe.latency_ms}ms</span>
                  ) : urlProbe ? (
                    <span className="text-[10px] text-red-500" title={urlProbe.message}>✗</span>
                  ) : null}
                </div>
              </div>
              {form.api_type === 'other' && (
                <p className="text-[11px] text-muted-foreground leading-snug">
                  ⚡ <strong>Other (Exact URL)</strong>: Enter the complete API endpoint URL. The request will be sent directly to this URL without any path appended.
                  <br />
                  Example for Hugging Face: <code className="text-xs bg-muted px-1 rounded">https://api-inference.huggingface.co/models/meta-llama/Meta-Llama-3-8B-Instruct/v1/chat/completions</code>
                </p>
              )}
            </div>


            <div className="space-y-2">
              <Label htmlFor="channel-apikey">{t('channel.editor.apiKey')}</Label>
              <div className="relative min-w-0">
                <textarea
                  id="channel-apikey"
                  rows={1}
                  value={form.api_key}
                  onChange={(event) => setValue('api_key', event.target.value)}
                  placeholder={channel ? undefined : t('channel.editor.multiKeyPlaceholder', 'One API Key per line. First line is used to fetch models. Saves will batch create channels.')}
                  className="flex min-h-9 w-full resize-y rounded-md border border-input bg-transparent px-3 py-2 pr-20 text-sm shadow-sm outline-none transition-colors placeholder:text-muted-foreground focus-visible:ring-1 focus-visible:ring-ring disabled:cursor-not-allowed disabled:opacity-50"
                  style={{ WebkitTextSecurity: showApiKey ? 'none' : 'disc' } as React.CSSProperties}
                />
                <Button type="button" variant="ghost" size="sm" className="absolute right-1 top-1 h-7 px-2 text-xs hover:bg-transparent" onClick={() => setShowApiKey(!showApiKey)}>
                  {showApiKey ? t('channel.editor.hidePassword') : t('channel.editor.showPassword')}
                </Button>
              </div>
            </div>

            {/* Header injection config */}
            <HeaderInjectionControl
              value={form.upstream_headers}
              onChange={(value) => setValue('upstream_headers', value)}
            />

            <div className="space-y-2">
              <Label htmlFor="channel-notes">{t('channel.editor.notes')}</Label>
              <textarea
                id="channel-notes"
                value={form.notes}
                onChange={(event) => setValue('notes', event.target.value)}
                rows={2}
                className="flex min-h-14 w-full resize-y rounded-md border border-input bg-transparent px-3 py-1.5 text-sm shadow-sm outline-none transition-colors placeholder:text-muted-foreground focus-visible:ring-1 focus-visible:ring-ring disabled:cursor-not-allowed disabled:opacity-50"
              />
            </div>

            {/* Fetch models button - full width, at bottom of channel section */}
            <Button 
              className="w-full gap-1.5" 
              onClick={handleFetchModels} 
              disabled={!canFetchModels || fetchingModels}
            >
              <RefreshCw className={cn('h-4 w-4', fetchingModels && 'animate-spin')} />
              {fetchingModels ? t('channel.editor.fetching') : t('channel.editor.fetchModels')}
            </Button>
          </div>

          {/* Model info section - only show when expanded */}
          {showModels && (
            <div className="min-w-0 space-y-3 rounded-md border border-border/60 p-3 sm:p-4">
              {/* Time range selection: 3/6/12 months */}
              <div className="flex overflow-hidden rounded-md border border-input">
                {([3, 6, 12] as const).map((months) => (
                  <Button
                    key={months}
                    size="sm"
                    variant={timeRange === months ? "default" : "ghost"}
                    onClick={() => setTimeRange(months)}
                    className="flex-1 rounded-none border-0 border-r last:border-r-0"
                  >
                    {t('channel.editor.months', { count: months, defaultValue: `${months} months` })}
                  </Button>
                ))}
              </div>

              {/* Search and action buttons */}
              <div className="flex flex-wrap gap-2 items-center">
                <Input 
                  id="model-search" 
                  placeholder={t('channel.editor.searchPlaceholder', 'Search/Create model')} 
                  value={modelSearch} 
                  onChange={(e) => setModelSearch(e.target.value)} 
                  className="h-8 min-w-full flex-1 text-sm sm:min-w-48"
                />
                <Button size="sm" variant="outline" onClick={selectAllFiltered}>{t('common.selectAll', 'Select All')}</Button>
                <Button size="sm" variant="outline" onClick={clearAllSelected}>{t('common.clear')}</Button>
              </div>

              {/* Model list */}
              <div className="max-h-[42dvh] min-h-48 overflow-y-auto rounded-md border border-border bg-background pr-2 sm:h-[262px] sm:max-h-none">
                {filteredModels.map((model) => {
                  const testResult = modelTestResults[model.name];
                  return (
                    <label key={model.id || model.name} htmlFor={`model-${model.id || model.name}`} className="flex cursor-pointer items-center gap-2 border-b border-border py-2 pl-3 pr-1 text-sm last:border-b-0 hover:bg-accent">
                      <Checkbox id={`model-${model.id || model.name}`} checked={selectedModels.includes(model.name)} onCheckedChange={() => toggleModel(model.name)} />
                      <span className={cn(
                        "truncate",
                        testResult?.success === true && "text-green-600",
                        testResult?.success === false && "text-red-500"
                      )} title={testResult?.reason}>
                        {model.name}
                        {testResult?.success === true && testResult.latency && (
                          <span className="text-xs ml-1">({(testResult.latency / 1000).toFixed(2)}s)</span>
                        )}
                         {testResult?.success === false && (
                           <span className="text-xs ml-1">({testResult.statusCode ? `HTTP ${testResult.statusCode}` : t('channel.editor.testFailed')})</span>
                         )}
                      </span>
                      <span className="ml-auto shrink-0 text-xs text-muted-foreground bg-muted px-1.5 py-0.5 rounded">
                        {model.sourceProtocol}
                      </span>
                    </label>
                  );
                })}
                {filteredModels.length === 0 && modelSearch.trim() ? (
                  <button
                    type="button"
                    className="flex w-full items-center justify-between px-3 py-2 text-left text-sm hover:bg-accent"
                    onClick={createTemporaryModel}
                  >
                    <span>{t('channel.editor.createModel', { name: modelSearch.trim(), defaultValue: `Create model "${modelSearch.trim()}"` })}</span>
                    <span className="text-xs text-muted-foreground bg-muted px-1.5 py-0.5 rounded">{form.api_type}</span>
                  </button>
                ) : null}
                {filteredModels.length === 0 && !modelSearch.trim() ? (
                  <div className="px-3 py-6 text-center text-sm text-muted-foreground">
                    {modelsValidated ? t('channel.editor.emptyPlaceholder2') : t('channel.editor.emptyPlaceholder')}
                  </div>
                ) : null}
              </div>

              <div className="text-right text-[11px] leading-none text-muted-foreground">
                {selectedModels.length}/{availableModels.length}
              </div>

              {(modelTestResults.__limit__?.reason || modelTestResults.__timeout__?.reason) && (
                <div className="rounded-md border border-destructive/25 bg-destructive/5 px-3 py-2 text-xs text-destructive">
                  {modelTestResults.__limit__?.reason || modelTestResults.__timeout__?.reason}
                </div>
              )}

              {/* Test Models button - full width */}
              <Button 
                className="w-full gap-1.5" 
                variant="outline"
                onClick={handleTestModels} 
                disabled={testingModels || filteredModels.length === 0}
              >
                <Zap className={cn('h-4 w-4', testingModels && 'animate-pulse')} />
                {testingModels 
                  ? t('channel.editor.testingModels', 'Testing...') 
                  : t('channel.editor.testModels', 'Test Models')
                }
                {filteredModels.length > 0 && !testingModels && (
                  <span className="text-xs text-muted-foreground ml-1">
                    ({filteredModels.length})
                  </span>
                )}
              </Button>
            </div>
          )}
        </div>

        <DialogFooter className="shrink-0 border-t pt-3">
          <Button variant="outline" onClick={handleClose} disabled={saving || fetchingModels} className="gap-1.5"><X className="h-4 w-4" />{t('common.cancel')}</Button>
          <Button onClick={handleSave} disabled={saving || fetchingModels} className="gap-1.5">
            <Save className="h-4 w-4" />
            {saving ? t('channel.editor.saving') : t('common.save')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
};




