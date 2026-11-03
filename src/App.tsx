import { useState, useEffect, Suspense, lazy, useRef } from "react";
import { useTranslation } from "react-i18next";
import { WelcomeGuide } from "@/components/WelcomeGuide";
import { ErrorBoundary } from "@/components/ErrorBoundary";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { MainShell, type MainPage } from "@/features/shell/MainShell";
import { useApiAdapter, isTauriRuntime } from "@/lib/useApiAdapter";
import { LoginScreen } from "@/components/LoginScreen";
import { AUTH_EXPIRED_EVENT, clearToken, getToken, validateToken, type AuthExpiredDetail, TOKEN_KEY } from "@/lib/webAuth";
import { checkUpdate, type UpdateInfo } from "@/lib/api";

const ApiPoolPage = lazy(() => import("@/pages/ApiPoolPage").then((m) => ({ default: m.ApiPoolPage })));
const ChannelPage = lazy(() => import("@/pages/ChannelPage").then((m) => ({ default: m.ChannelPage })));
const TokenPage = lazy(() => import("@/pages/TokenPage").then((m) => ({ default: m.TokenPage })));
const LinkPage = lazy(() => import("@/pages/LinkPage").then((m) => ({ default: m.LinkPage })));
const LogPage = lazy(() => import("@/pages/LogPage").then((m) => ({ default: m.LogPage })));
const DashboardPage = lazy(() => import("@/pages/DashboardPage").then((m) => ({ default: m.DashboardPage })));
const SettingsPage = lazy(() => import("@/pages/SettingsPage").then((m) => ({ default: m.SettingsPage })));
type WebAuthViewState =
  | { state: "checking" }
  | { state: "authenticated" }
  | { state: "login"; message?: string }
  | { state: "server_unreachable"; message: string }
  | { state: "expired"; message: string };

const GUIDE_BASE = "https://github.com/pythonistsawlani/Smart-API-Gateway-Platform/blob/main/";

function MainApp({ onLogout }: { onLogout?: () => void }) {
  const { i18n } = useTranslation();
  const api = useApiAdapter();
  const isDesktop = isTauriRuntime();
  const [currentPage, setCurrentPage] = useState<MainPage>("apiPool");

  const { data: settings } = useQuery({
    queryKey: ["settings"],
    queryFn: () => api.settings.get(),
  });

  const { data: proxyStatus } = useQuery({
    queryKey: ["proxyStatus"],
    queryFn: () => api.proxy.getStatus(),
    refetchInterval: 2000,
  });

  const { data: adminStatus } = useQuery({
    queryKey: ["adminStatus"],
    queryFn: () => api.getAdminStatus(),
    refetchInterval: 2000,
  });

  const { data: platformCapabilities } = useQuery({
    queryKey: ["platformCapabilities"],
    queryFn: () => api.getPlatformCapabilities(),
    staleTime: Infinity,
  });

  // State version check: check once on mount, no polling
  // Dashboard and other pages no longer auto-refresh every 2 seconds, having data once is enough
  const queryClient = useQueryClient();
  const lastVersion = useRef<Record<string, number> | null>(null);
  useQuery({
    queryKey: ["state-version"],
    queryFn: async () => {
      const res = await api.getStateVersion();
      if (lastVersion.current !== null) {
        const changed = Object.keys(res).some(
          (k) => res[k as keyof typeof res] !== lastVersion.current![k]
        );
        if (changed) {
          queryClient.invalidateQueries({ predicate: (q) => q.queryKey[0] !== 'state-version' });
        }
      }
      lastVersion.current = res;
      return res;
    },
  });

  const [guideOpen, setGuideOpen] = useState(false);
  const [updateInfo, setUpdateInfo] = useState<{ current: string; latest: string; url: string } | null>(null);

  useEffect(() => {
    if (!isDesktop) return;
    import("@tauri-apps/api/app").then(({ getVersion }) => {
      getVersion().then((v: string) => { document.title = `Smart API Gateway - ${v}`; });
    });
  }, [isDesktop]);

  useEffect(() => {
    if (!isDesktop) return;
    checkUpdate().then((info) => {
      if (info) setUpdateInfo(info);
    });
  }, [isDesktop]);

  useEffect(() => {
    if (!settings) return;
    if (isDesktop && settings.show_guide !== false) setGuideOpen(true);
  }, [settings?.show_guide, isDesktop]);

  const handleGuideDismiss = (dontShowAgain: boolean) => {
    if (dontShowAgain && settings) api.settings.patchSettings({ show_guide: false });
  };

  useEffect(() => {
    if (!settings) return;
    const saved = localStorage.getItem("smart-api-gateway-locale");
    if (!saved && settings.locale) i18n.changeLanguage(settings.locale);
    const root = document.documentElement;
    if (settings.theme === "dark") root.classList.add("dark");
    else if (settings.theme === "light") root.classList.remove("dark");
    else if (window.matchMedia("(prefers-color-scheme: dark)").matches) root.classList.add("dark");
    else root.classList.remove("dark");
  }, [settings]);

  const openExternal = (url: string) => {
    if (isDesktop) import("@tauri-apps/plugin-opener").then(({ openUrl }) => openUrl(url));
    else window.open(url, "_blank", "noopener,noreferrer");
  };

  const renderPage = () => {
    const page = (() => {
      switch (currentPage) {
        case "apiPool": return <ApiPoolPage />;
        case "channels": return <ChannelPage />;
        case "tokens": return <TokenPage />;
        case "link": return <LinkPage />;
        case "logs": return <LogPage />;
        case "dashboard": return <DashboardPage />;
        case "settings": return <SettingsPage />;
      }
    })();
    return <ErrorBoundary key={currentPage}>{page}</ErrorBoundary>;
  };

  return (
    <MainShell
      currentPage={currentPage}
      proxyStatus={proxyStatus}
      adminStatus={adminStatus}
      platformCapabilities={platformCapabilities}
      settings={settings}
      updateInfo={updateInfo}
      onUpdateDismiss={() => setUpdateInfo(null)}
      onUpdateOpen={(url) => openExternal(url)}
      onNavigate={setCurrentPage}
      onOpenGuide={(path) => openExternal(GUIDE_BASE + path)}
      onLogout={onLogout}
      renderPage={() => (
        <Suspense fallback={<div className="flex items-center justify-center min-h-screen">Loading...</div>}>
          {renderPage()}
        </Suspense>
      )}
    >
      {isDesktop && settings?.show_guide !== false && (
        <WelcomeGuide open={guideOpen} onOpenChange={setGuideOpen} onDismiss={handleGuideDismiss} />
      )}
    </MainShell>
  );
}

/**
 * Gate: in web mode, validate token before rendering MainApp.
 * In desktop mode, skip directly to MainApp.
 * This wrapper avoids React Hooks ordering issues —
 * no hooks are called conditionally.
 */
export default function App() {
  const isDesktop = isTauriRuntime();
  const [webAuth, setWebAuth] = useState<WebAuthViewState>(() =>
    isDesktop ? { state: "authenticated" } : (getToken() ? { state: "checking" } : { state: "login" })
  );

  const checkWebTokenIdRef = useRef(0);

  const invalidateWebTokenCheck = () => {
    checkWebTokenIdRef.current += 1;
  };

  const checkWebToken = () => {
    setWebAuth({ state: "checking" });
    const currentId = ++checkWebTokenIdRef.current;
    validateToken().then((result) => {
      if (currentId !== checkWebTokenIdRef.current) return;
      if (result.status === "valid") {
        setWebAuth({ state: "authenticated" });
        return;
      }

      if (result.status === "unreachable") {
        setWebAuth({ state: "server_unreachable", message: "Cannot connect to Web Admin service. Please ensure the service is running." });
        return;
      }

      clearToken();
      if (result.status === "invalid") {
        setWebAuth({ state: "expired", message: "Session expired. Please log in again." });
      } else {
        setWebAuth({ state: "login", message: result.message });
      }
    });
  };

  const handleLogout = () => {
    const token = getToken();
    invalidateWebTokenCheck();
    clearToken();
    setWebAuth({ state: "login" });
    toast.success("Logged out successfully");

    // fire-and-forget, do not wait for backend response
    if (token) {
      fetch("/admin/logout", {
        method: "POST",
        headers: { Authorization: `Bearer ${token}` },
        keepalive: true,
      }).catch(() => {});
    }
  };

  // Validate existing token on mount (web only)
  useEffect(() => {
    if (isDesktop || webAuth.state !== "checking") return;
    checkWebToken();
  }, [isDesktop, webAuth.state]);

  useEffect(() => {
    if (isDesktop) return;
    const handler = (event: Event) => {
      const detail = (event as CustomEvent<AuthExpiredDetail>).detail;
      // Only process when currently authenticated or checking, avoid redundant state flapping.
      if (webAuth.state !== "authenticated" && webAuth.state !== "checking") return;
      invalidateWebTokenCheck();
      clearToken();
      setWebAuth({ state: "expired", message: detail?.message || "Session expired. Please log in again." });
      toast.error("Session expired, please log in again", { id: "web-admin-auth-expired" });
    };
    window.addEventListener(AUTH_EXPIRED_EVENT, handler);
    return () => window.removeEventListener(AUTH_EXPIRED_EVENT, handler);
  }, [isDesktop, webAuth.state]);

  useEffect(() => {
    if (isDesktop) return;
    const handler = (event: StorageEvent) => {
      if (event.key !== TOKEN_KEY) return;
      if (!event.newValue) {
        invalidateWebTokenCheck();
        // Only switch state if currently authenticated, to avoid overriding valid state flow
        if (webAuth.state === "authenticated") {
          setWebAuth({ state: "login", message: "You have been logged out from another tab." });
          toast.info("Logged out from another tab", { id: "web-admin-storage-logout" });
        }
        return;
      }
      // token replaced: re-validate to sync login state from other tabs
      if (webAuth.state !== "checking") {
        checkWebToken();
      }
    };
    window.addEventListener("storage", handler);
    return () => window.removeEventListener("storage", handler);
  }, [isDesktop, webAuth.state]);

  if (isDesktop) return <MainApp />;

  if (webAuth.state === "checking") {
    return <div className="flex items-center justify-center min-h-screen text-muted-foreground">Loading...</div>;
  }

  if (webAuth.state === "login" || webAuth.state === "server_unreachable" || webAuth.state === "expired") {
    return (
      <LoginScreen
        message={webAuth.message}
        onRetry={webAuth.state === "server_unreachable" ? checkWebToken : undefined}
        onAuthenticated={() => setWebAuth({ state: "authenticated" })}
      />
    );
  }

  return <MainApp onLogout={handleLogout} />;
}
