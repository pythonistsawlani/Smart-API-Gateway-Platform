import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Slider } from "@/components/ui/slider";
import { Switch } from "@/components/ui/switch";
import { useState, useRef, useEffect } from "react";
import { useTranslation } from "react-i18next";
import { DEFAULT_SETTINGS, type AppSettings, type ProxyStatus } from "@/types";

export interface SettingsEditorProps {
  settings?: AppSettings | null;
  proxyStatus?: ProxyStatus | null;
  appVersion?: string;
  isWeb?: boolean;
  groups?: string[];
  onChange: <K extends keyof AppSettings>(
    key: K,
    value: AppSettings[K],
  ) => void;
  onProxyToggle?: (enabled: boolean) => void | Promise<void>;
}

export function SettingsEditor({
  settings,
  proxyStatus,
  appVersion,
  isWeb = false,
  groups = ["auto"],
  onChange,
  onProxyToggle,
}: SettingsEditorProps) {
  const { t } = useTranslation();
  const s = { ...DEFAULT_SETTINGS, ...settings };

  // Local state for text inputs that should only save on blur
  const [editUsername, setEditUsername] = useState(s.web_admin_username);
  const [editPassword, setEditPassword] = useState(s.web_admin_password);
  const [editPort, setEditPort] = useState(s.listen_port);
  const [editThreshold, setEditThreshold] = useState(
    s.circuit_failure_threshold,
  );
  const [editTimeout, setEditTimeout] = useState(s.proxy_connect_timeout_secs);
  const [editDisableCodes, setEditDisableCodes] = useState(
    s.circuit_disable_codes,
  );
  const [editAdminPort, setEditAdminPort] = useState(s.web_admin_port);
  const usernameEditing = useRef(false);
  const passwordEditing = useRef(false);
  const portEditing = useRef(false);
  const thresholdEditing = useRef(false);
  const timeoutEditing = useRef(false);
  const disableCodesEditing = useRef(false);
  const adminPortEditing = useRef(false);

  // Sync from props when not actively editing
  useEffect(() => {
    if (!usernameEditing.current) setEditUsername(s.web_admin_username);
  }, [s.web_admin_username]);
  useEffect(() => {
    // Don't sync empty password (backend getter may clear it for security)
    if (!passwordEditing.current && s.web_admin_password) {
      setEditPassword(s.web_admin_password);
    }
  }, [s.web_admin_password]);
  useEffect(() => {
    if (!portEditing.current) setEditPort(s.listen_port);
  }, [s.listen_port]);
  useEffect(() => {
    if (!thresholdEditing.current) {
      setEditThreshold(s.circuit_failure_threshold);
    }
  }, [s.circuit_failure_threshold]);
  useEffect(() => {
    if (!timeoutEditing.current) {
      setEditTimeout(s.proxy_connect_timeout_secs);
    }
  }, [s.proxy_connect_timeout_secs]);
  useEffect(() => {
    if (!disableCodesEditing.current) {
      setEditDisableCodes(s.circuit_disable_codes);
    }
  }, [s.circuit_disable_codes]);
  useEffect(() => {
    if (!adminPortEditing.current) setEditAdminPort(s.web_admin_port);
  }, [s.web_admin_port]);

  return (
    <div className="space-y-6">
      <Card>
        <CardHeader>
          <CardTitle className="text-base">
            {t("settings.ports.title")}
          </CardTitle>
        </CardHeader>
        <CardContent className="space-y-4">
          {isWeb ? (
            <div className="space-y-2">
              <div className="space-y-2">
                <Label>{t("settings.webAdmin.username")}</Label>
                <Input
                  value={editUsername}
                  onFocus={() => {
                    usernameEditing.current = true;
                  }}
                  onChange={(event) => setEditUsername(event.target.value)}
                  onBlur={() => {
                    usernameEditing.current = false;
                    onChange("web_admin_username", editUsername);
                  }}
                />
              </div>
              <div className="space-y-2">
                <Label>{t("settings.webAdmin.password")}</Label>
                <Input
                  type="password"
                  value={editPassword}
                  placeholder={t("settings.webAdmin.passwordPlaceholder")}
                  onFocus={() => {
                    passwordEditing.current = true;
                  }}
                  onChange={(event) => setEditPassword(event.target.value)}
                  onBlur={() => {
                    passwordEditing.current = false;
                    if (editPassword) {
                      onChange("web_admin_password", editPassword);
                    }
                  }}
                />
              </div>
              <div className="text-sm text-muted-foreground pt-1">
                {t("settings.webAdmin.address")}: http://127.0.0.1:
                {s.web_admin_port}
              </div>
            </div>
          ) : (
            <>
              <div className="space-y-2">
                <div className="flex items-center justify-between">
                  <div>
                    <Label>{t("settings.proxy.enabled")}</Label>
                  </div>
                  <Switch
                    checked={proxyStatus?.running ?? s.proxy_enabled}
                    onCheckedChange={(value) => {
                      if (onProxyToggle) {
                        onProxyToggle(value);
                      } else {
                        onChange("proxy_enabled", value);
                      }
                    }}
                  />
                </div>
                <div className="flex items-center justify-between">
                  <Label>{t("settings.proxy.port")}</Label>
                  <Input
                    type="number"
                    className="w-32"
                    value={editPort}
                    onFocus={() => {
                      portEditing.current = true;
                    }}
                    onChange={(event) =>
                      setEditPort(parseInt(event.target.value) || 9090)
                    }
                    onBlur={() => {
                      portEditing.current = false;
                      onChange("listen_port", editPort);
                    }}
                  />
                </div>
                {(proxyStatus?.running ?? s.proxy_enabled) && (
                  <div className="text-sm text-muted-foreground">
                    {t("settings.proxy.address")}: http://127.0.0.1:
                    {proxyStatus?.port ?? s.listen_port}
                  </div>
                )}
              </div>
              <hr className="border-border" />
              <div className="space-y-2">
                <div className="flex items-center justify-between">
                  <div>
                    <Label>{t("settings.webAdmin.enabled")}</Label>
                    <p className="text-xs text-muted-foreground">
                      {t("settings.webAdmin.enabledDesc")}
                    </p>
                  </div>
                  <Switch
                    checked={s.web_admin_enabled}
                    onCheckedChange={(value) =>
                      onChange("web_admin_enabled", value)
                    }
                  />
                </div>
                <div className="flex items-center justify-between">
                  <Label>{t("settings.webAdmin.port")}</Label>
                  <Input
                    type="number"
                    min={1}
                    max={65535}
                    className="w-32"
                    value={editAdminPort}
                    onFocus={() => {
                      adminPortEditing.current = true;
                    }}
                    onChange={(event) =>
                      setEditAdminPort(
                        Math.min(
                          65535,
                          Math.max(1, parseInt(event.target.value) || 9099),
                        ),
                      )
                    }
                    onBlur={() => {
                      adminPortEditing.current = false;
                      onChange("web_admin_port", editAdminPort);
                    }}
                  />
                </div>
                <div className="space-y-2">
                  <Label>{t("settings.webAdmin.username")}</Label>
                  <Input
                    value={editUsername}
                    onFocus={() => {
                      usernameEditing.current = true;
                    }}
                    onChange={(event) => setEditUsername(event.target.value)}
                    onBlur={() => {
                      usernameEditing.current = false;
                      onChange("web_admin_username", editUsername);
                    }}
                  />
                </div>
                <div className="space-y-2">
                  <Label>{t("settings.webAdmin.password")}</Label>
                  <Input
                    type="password"
                    value={editPassword}
                    placeholder={t("settings.webAdmin.passwordPlaceholder")}
                    onFocus={() => {
                      passwordEditing.current = true;
                    }}
                    onChange={(event) => setEditPassword(event.target.value)}
                    onBlur={() => {
                      passwordEditing.current = false;
                      if (editPassword) {
                        onChange("web_admin_password", editPassword);
                      }
                    }}
                  />
                  <p className="text-xs text-muted-foreground">
                    {t("settings.webAdmin.singlePortDesc")}
                  </p>
                </div>
                {s.web_admin_enabled &&
                  s.web_admin_username &&
                  s.web_admin_password && (
                    <div className="text-sm text-muted-foreground">
                      {t("settings.webAdmin.address")}: http://127.0.0.1:
                      {s.web_admin_port}
                    </div>
                  )}
              </div>
            </>
          )}
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle className="text-base">
            {t("settings.circuit.title")}
          </CardTitle>
        </CardHeader>
        <CardContent className="space-y-4">
          <div className="flex items-center justify-between">
            <Label>{t("settings.circuit.threshold")}</Label>
            <Input
              type="number"
              className="w-32"
              value={editThreshold}
              onFocus={() => {
                thresholdEditing.current = true;
              }}
              onChange={(event) =>
                setEditThreshold(parseInt(event.target.value) || 1)
              }
              onBlur={() => {
                thresholdEditing.current = false;
                onChange("circuit_failure_threshold", editThreshold);
              }}
            />
          </div>
          <div className="flex items-center justify-between">
            <div>
              <Label>{t("settings.circuit.connectTimeout")}</Label>
              <p className="text-xs text-muted-foreground">
                {t("settings.circuit.connectTimeoutDesc")}
              </p>
            </div>
            <Input
              type="number"
              min={1}
              max={300}
              className="w-32"
              value={editTimeout}
              onFocus={() => {
                timeoutEditing.current = true;
              }}
              onChange={(event) =>
                setEditTimeout(
                  Math.min(
                    300,
                    Math.max(1, parseInt(event.target.value) || 30),
                  ),
                )
              }
              onBlur={() => {
                timeoutEditing.current = false;
                onChange("proxy_connect_timeout_secs", editTimeout);
              }}
            />
          </div>
          <div className="space-y-2">
            <div className="flex items-center justify-between">
              <Label>{t("settings.circuit.recovery")}</Label>
              <span className="text-sm text-muted-foreground w-16 text-right">
                {s.circuit_recovery_secs}s
              </span>
            </div>
            <Slider
              min={30}
              max={1800}
              step={30}
              value={s.circuit_recovery_secs}
              onValueChange={(value) =>
                onChange("circuit_recovery_secs", value)
              }
            />
            <p className="text-xs text-muted-foreground">30s – 1800s</p>
          </div>
          <div className="space-y-2">
            <Label>{t("settings.circuit.disableCodes")}</Label>
            <Input
              value={editDisableCodes}
              onFocus={() => {
                disableCodesEditing.current = true;
              }}
              onChange={(event) => setEditDisableCodes(event.target.value)}
              onBlur={() => {
                disableCodesEditing.current = false;
                onChange("circuit_disable_codes", editDisableCodes);
              }}
            />
            <p className="text-xs text-muted-foreground">
              {t("settings.circuit.disableDesc")}
            </p>
          </div>
          <div className="space-y-2">
            <Label>{t("settings.circuit.disableKeywords")}</Label>
            <textarea
              className="flex min-h-[120px] w-full rounded-md border border-input bg-transparent px-3 py-2 text-sm shadow-sm placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring disabled:cursor-not-allowed disabled:opacity-50"
              value={s.disable_keywords}
              onChange={(event) =>
                onChange("disable_keywords", event.target.value)
              }
              placeholder={t("settings.circuit.disableKeywords")}
            />
            <p className="text-xs text-muted-foreground">
              {t("settings.circuit.disableKeywordsDesc")}
            </p>
          </div>
          <div className="flex items-center justify-between">
            <div>
              <Label>{t("settings.circuit.keywordFreezeScope")}</Label>
              <p className="text-xs text-muted-foreground">
                {t("settings.circuit.keywordFreezeScopeDesc")}
              </p>
            </div>
            <Select
              value={s.keyword_freeze_scope}
              onValueChange={(value) => onChange("keyword_freeze_scope", value)}
            >
              <SelectTrigger className="w-44">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="model">
                  {t("settings.circuit.keywordFreezeScopeModel")}
                </SelectItem>
                <SelectItem value="channel">
                  {t("settings.circuit.keywordFreezeScopeChannel")}
                </SelectItem>
              </SelectContent>
            </Select>
          </div>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle className="text-base">
            {t("settings.general.title")}
          </CardTitle>
        </CardHeader>
        <CardContent className="space-y-4">
          <div className="flex items-center justify-between">
            <Label>{t("settings.general.language")}</Label>
            <Select
              value={s.locale}
              onValueChange={(value) => onChange("locale", value)}
            >
              <SelectTrigger className="w-32">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="zh">Chinese</SelectItem>
                <SelectItem value="en">English</SelectItem>
              </SelectContent>
            </Select>
          </div>
          <div className="flex items-center justify-between">
            <div>
              <Label>{t("settings.security.forceKey")}</Label>
              <p className="text-xs text-muted-foreground">
                {t("settings.security.forceKeyDesc")}
              </p>
            </div>
            <Switch
              checked={s.access_key_required}
              onCheckedChange={(value) =>
                onChange("access_key_required", value)
              }
            />
          </div>
          <div className="flex items-center justify-between">
            <div>
              <Label>{t("settings.general.showConversationModel")}</Label>
              <p className="text-xs text-muted-foreground">
                {t("settings.general.showConversationModelDesc")}
              </p>
            </div>
            <Switch
              checked={s.show_conversation_model}
              onCheckedChange={(value) =>
                onChange("show_conversation_model", value)
              }
            />
          </div>
          <div className="flex items-center justify-between">
            <div>
              <Label>{t("settings.general.disableReasoning")}</Label>
              <p className="text-xs text-muted-foreground">
                {t("settings.general.disableReasoningDesc")}
              </p>
            </div>
            <Switch
              checked={s.disable_reasoning}
              onCheckedChange={(value) => onChange("disable_reasoning", value)}
            />
          </div>
          <div className="flex items-center justify-between">
            <div>
              <Label>{t("settings.general.passthroughHeaderInjection")}</Label>
              <p className="text-xs text-muted-foreground">
                {t("settings.general.passthroughHeaderInjectionDesc")}
              </p>
            </div>
            <Switch
              checked={s.passthrough_header_injection}
              onCheckedChange={(value) =>
                onChange("passthrough_header_injection", value)
              }
            />
          </div>
          <div className="flex items-center justify-between">
            <div>
              <Label>{t("settings.general.recordRawProtocolData")}</Label>
              <p className="text-xs text-muted-foreground">
                {t("settings.general.recordRawProtocolDataDesc")}
              </p>
            </div>
            <Switch
              checked={s.record_raw_protocol_data}
              onCheckedChange={(value) =>
                onChange("record_raw_protocol_data", value)
              }
            />
          </div>
          <div className="flex items-center justify-between">
            <Label>{t("settings.tray.autostart")}</Label>
            {isWeb ? (
              <span className="text-sm text-muted-foreground">
                {s.autostart ? t("common.enabled") : t("common.disabled")}
              </span>
            ) : (
              <Switch
                checked={s.autostart}
                onCheckedChange={(value) => onChange("autostart", value)}
              />
            )}
          </div>
          <div className="flex items-center justify-between">
            <Label>{t("settings.tray.startMinimized")}</Label>
            {isWeb ? (
              <span className="text-sm text-muted-foreground">
                {s.start_minimized ? t("common.enabled") : t("common.disabled")}
              </span>
            ) : (
              <Switch
                checked={s.start_minimized}
                onCheckedChange={(value) => onChange("start_minimized", value)}
              />
            )}
          </div>
          {appVersion && (
            <div className="flex items-center justify-between pt-2 border-t">
              <Label className="text-muted-foreground">
                {t("settings.general.currentVersion")}
              </Label>
              <span className="text-sm font-mono text-muted-foreground">
                {appVersion}
              </span>
            </div>
          )}
        </CardContent>
      </Card>
    </div>
  );
}
