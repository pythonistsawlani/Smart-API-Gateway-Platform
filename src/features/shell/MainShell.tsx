import { useState } from 'react';
import { BarChart3, BookOpen, ExternalLink, FileText, KeyRound, Layers, Link, LogOut, Menu, Power, Route, Settings, X } from 'lucide-react';

import { useTranslation } from 'react-i18next';
import { Button } from '@/components/ui/button';
import { ScrollArea } from '@/components/ui/scroll-area';
import { Separator } from '@/components/ui/separator';
import { cn } from '@/lib/utils';
import type { AdminStatus, AppSettings, PlatformCapabilities, ProxyStatus } from '@/types';

export type MainPage = 'apiPool' | 'channels' | 'tokens' | 'link' | 'logs' | 'dashboard' | 'translator' | 'settings' | 'guide';

const NAV_ITEMS: { key: MainPage; icon: typeof Layers; labelKey: string; androidHidden?: boolean }[] = [
  { key: 'apiPool', icon: Layers, labelKey: 'nav.apiPool' },
  { key: 'channels', icon: Route, labelKey: 'nav.channels' },
  { key: 'tokens', icon: KeyRound, labelKey: 'nav.tokens' },
  { key: 'link', icon: Link, labelKey: 'nav.link', androidHidden: true },
  { key: 'logs', icon: FileText, labelKey: 'nav.logs' },
  { key: 'dashboard', icon: BarChart3, labelKey: 'nav.dashboard' },
  { key: 'settings', icon: Settings, labelKey: 'nav.settings' },
];


export interface MainShellProps {
  currentPage: MainPage;
  proxyStatus?: ProxyStatus | null;
  adminStatus?: AdminStatus | null;
  platformCapabilities?: PlatformCapabilities | null;
  settings?: AppSettings | null;
  updateInfo?: { current: string; latest: string; url: string } | null;
  onUpdateDismiss?: () => void;
  onUpdateOpen?: (url: string) => void;
  onNavigate: (page: MainPage) => void;
  onOpenGuide?: (path: string) => void;
  onLogout?: () => void;
  renderPage: () => React.ReactNode;
  children?: React.ReactNode;
}

export function MainShell({
  currentPage,
  proxyStatus,
  adminStatus,
  platformCapabilities,
  settings,
  updateInfo,
  onUpdateDismiss,
  onUpdateOpen,
  onNavigate,
  onOpenGuide,
  onLogout,
  renderPage,
  children,
}: MainShellProps) {
  const { t, i18n } = useTranslation();
  const [mobileMenuOpen, setMobileMenuOpen] = useState(false);
  const canUseConnectionApps = platformCapabilities?.canUseConnectionApps ?? true;
  const navItems = NAV_ITEMS.filter((item) => !(item.androidHidden && !canUseConnectionApps));

  const openGuide = () => {
    const lang = i18n.language.startsWith('zh') ? 'zh' : 'en';
    const guidePath = lang === 'zh' ? 'GUIDE_CN.md' : 'GUIDE.md';
    onOpenGuide?.(guidePath);
    setMobileMenuOpen(false);
  };

  const navigate = (page: MainPage) => {
    onNavigate(page);
    setMobileMenuOpen(false);
  };

  const statusDots = (
    <div className="flex items-center gap-2">
      <span
        className={cn('inline-block h-4 w-4 rounded-full', proxyStatus?.running ? 'bg-green-500' : 'bg-red-500')}
        title={proxyStatus?.running ? `Proxy: ${proxyStatus.port}` : 'Proxy'}
      />
      <span
        className={cn('inline-block h-4 w-4 rounded-full', adminStatus?.running ? 'bg-green-500' : 'bg-red-500')}
        title={adminStatus?.running ? `Admin: ${adminStatus.port}` : 'Admin'}
      />
    </div>
  );

  const navButtons = (mobile = false) => (
    <nav className={cn('flex flex-col gap-1', mobile && 'p-2')}>
      {navItems.map(({ key, icon: Icon, labelKey }) => (
        <Button
          key={key}
          variant={currentPage === key ? 'secondary' : 'ghost'}
          className={cn(
            'justify-start gap-2 px-3',
            mobile && 'h-10 w-full',
            currentPage === key && 'bg-sidebar-accent text-sidebar-accent-foreground',
          )}
          onClick={() => navigate(key)}
        >
          <Icon className="h-4 w-4" />
          {t(labelKey)}
        </Button>
      ))}
      <Separator className="my-1" />
      <Button variant="ghost" className="w-full justify-start gap-2 px-3" onClick={openGuide}>
        <BookOpen className="h-4 w-4" />
        {t('nav.guide', 'Guide')}
      </Button>
      {onLogout && (
        <>
          <Separator className="my-1" />
          <Button
            variant="ghost"
            className="w-full justify-start gap-2 px-3 text-red-500 hover:text-red-500"
            onClick={() => {
              setMobileMenuOpen(false);
              onLogout();
            }}
          >
            <LogOut className="h-4 w-4" />
            {t('nav.logout', 'Logout')}
          </Button>
        </>
      )}
    </nav>
  );

  return (
    <div className="safe-area-shell flex flex-col overflow-hidden bg-background">
      {updateInfo && (
        <div className="flex shrink-0 items-center justify-center gap-2 bg-primary/10 px-3 py-1.5 text-xs text-primary">
          <span className="truncate">{t('update.newVersion', { version: updateInfo.latest })}</span>
          <button
            type="button"
            onClick={() => onUpdateOpen?.(updateInfo.url)}
            className="inline-flex shrink-0 items-center gap-1 font-medium underline-offset-2 hover:underline"
          >
            {t('update.goDownload')}
            <ExternalLink className="h-3 w-3" />
          </button>
          <button type="button" onClick={onUpdateDismiss} className="ml-1 shrink-0 opacity-60 hover:opacity-100">
            ✕
          </button>
        </div>
      )}

      <header className="flex h-12 shrink-0 items-center justify-between border-b border-border bg-background px-3 md:hidden">
        <Button variant="ghost" size="icon" className="h-9 w-9" onClick={() => setMobileMenuOpen((open) => !open)}>
          {mobileMenuOpen ? <X className="h-5 w-5" /> : <Menu className="h-5 w-5" />}
        </Button>
        <div className="min-w-0 truncate text-base font-semibold tracking-wide">SMART API GATEWAY</div>
        {statusDots}
      </header>

      {mobileMenuOpen && (
        <div className="shrink-0 border-b border-border bg-sidebar-background shadow-sm md:hidden">
          {navButtons(true)}
        </div>
      )}

      <div className="flex min-h-0 flex-1">
        <aside className="hidden w-56 shrink-0 flex-col border-r border-sidebar-border bg-sidebar-background md:flex">
          <div className="flex items-center gap-2 px-4 py-4">
            <Power className={cn('h-5 w-5', proxyStatus?.running ? 'text-green-500' : 'text-red-500')} />
            <span className="truncate text-lg font-semibold">
              {proxyStatus?.running ? `Smart API Gateway: ${proxyStatus.port}` : 'Smart API Gateway'}
            </span>
          </div>

          <Separator />

          <ScrollArea className="flex-1 px-2 py-2">{navButtons()}</ScrollArea>

          <div className="px-2 pb-4">
            <div className="mt-2 flex items-center justify-center gap-2 text-xs text-muted-foreground">
              {statusDots}
              <span>Version: {settings?.app_version || __APP_VERSION__}</span>
            </div>
          </div>
        </aside>

        <main className="min-w-0 flex-1 overflow-auto">{renderPage()}</main>
        {children}
      </div>
    </div>
  );
}
