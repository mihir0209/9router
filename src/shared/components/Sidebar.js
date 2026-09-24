"use client";

import { useState, useEffect, useRef, useCallback } from "react";
import PropTypes from "prop-types";
import Link from "next/link";
import { createPortal } from "react-dom";
import { usePathname } from "next/navigation";
import { cn } from "@/shared/utils/cn";
import { APP_CONFIG, UPDATER_CONFIG } from "@/shared/constants/config";
import { MEDIA_PROVIDER_KINDS } from "@/shared/constants/providers";
import { useCopyToClipboard } from "@/shared/hooks/useCopyToClipboard";
import Button from "./Button";
import { ConfirmModal } from "./Modal";
import NineRemotePromoModal from "./NineRemotePromoModal";

// const VISIBLE_MEDIA_KINDS = ["embedding", "image", "imageToText", "tts", "stt", "webSearch", "webFetch", "video", "music"];
const VISIBLE_MEDIA_KINDS = ["embedding", "image", "video", "tts", "stt", "systemone"];
// Combined entry: webSearch + webFetch share one page at /dashboard/media-providers/web
const COMBINED_WEB_ITEM = { id: "web", label: "Web Fetch & Search", icon: "travel_explore", href: "/dashboard/media-providers/web" };

const navItems = [
  { href: "/dashboard/endpoint", label: "Endpoint & Key", icon: "api" },
  { href: "/dashboard/playground", label: "Playground", icon: "play_arrow" },
  { href: "/dashboard/providers", label: "Providers", icon: "dns" },
  // { href: "/dashboard/basic-chat", label: "Basic Chat", icon: "chat" }, // Hidden
  { href: "/dashboard/combos", label: "Combo & Vision Adapter", icon: "layers" },
  { href: "/dashboard/usage", label: "Usage", icon: "bar_chart" },
  { href: "/dashboard/quota", label: "Quota Tracker", icon: "data_usage" },
  { href: "/dashboard/token-saver", label: "Token Saver", icon: "savings" },
  // { href: "/dashboard/pxpipe", label: "PXPIPE", icon: "image" },
  { href: "/dashboard/cli-tools", label: "CLI Tools", icon: "terminal" },
];

const debugItems = [
  { href: "/dashboard/console-log", label: "Console Log", icon: "terminal" },
  { href: "/dashboard/translator", label: "Translator", icon: "translate" },
];

const systemItems = [
  { href: "/dashboard/proxy-pools", label: "Proxy Pools", icon: "lan" },
  { href: "/dashboard/skills", label: "Skills", icon: "extension" },
];

// Collapsed-rail tooltip. The native `title` attribute is not used because
// browsers anchor it above the hovered element, where it covers the
// neighbouring icons on the narrow rail. This portal label follows the icon
// (fixed positioning is immune to the nav's overflow and the aside's
// backdrop-filter) and appears to the right, vertically centered.
function useRailTooltip(label, enabled) {
  const ref = useRef(null);
  const [pos, setPos] = useState(null);

  const show = useCallback(() => {
    if (!enabled) return;
    const rect = ref.current?.getBoundingClientRect();
    if (rect) setPos({ top: rect.top + rect.height / 2, left: rect.right + 12 });
  }, [enabled]);
  const hide = useCallback(() => setPos(null), []);

  const node = enabled && pos
    ? createPortal(
        <div
          role="tooltip"
          style={{ top: pos.top, left: pos.left }}
          className="pointer-events-none fixed z-[100] -translate-y-1/2 whitespace-nowrap rounded-md border border-border-subtle bg-surface-3 px-2.5 py-1 text-xs font-medium text-text-main shadow-xl"
        >
          {label}
        </div>,
        document.body
      )
    : null;

  return { ref, tooltipHandlers: { onMouseEnter: show, onMouseLeave: hide, onFocus: show, onBlur: hide }, tooltip: node };
}

function SidebarLink({ href, label, icon, collapsed, active, onClick, compact = false, iconClass = "text-[18px]", badge }) {
  const { ref, tooltipHandlers, tooltip } = useRailTooltip(label, collapsed);
  return (
    <>
      <Link
        ref={ref}
        href={href}
        onClick={onClick}
        {...tooltipHandlers}
        className={cn(
          "relative flex items-center rounded-lg py-1 transition-all group",
          compact ? "px-4" : "px-3",
          collapsed ? "justify-center" : "gap-3",
          active
            ? "bg-primary/10 text-primary"
            : "text-text-muted hover:bg-surface-2 hover:text-text-main"
        )}
      >
        <span
          className={cn(
            "material-symbols-outlined",
            iconClass,
            active ? "fill-1" : "group-hover:text-primary transition-colors"
          )}
        >
          {icon}
        </span>
        {!collapsed && <span className="text-[13px] font-medium">{label}</span>}
        {!collapsed && badge && (
          <span className="ml-auto text-[10px] font-semibold px-1.5 py-0.5 rounded-[3px] bg-green-500/15 text-green-400">
            {badge}
          </span>
        )}
      </Link>
      {tooltip}
    </>
  );
}

function LogoLink({ collapsed }) {
  const { ref, tooltipHandlers, tooltip } = useRailTooltip(APP_CONFIG.name, collapsed);
  return (
    <>
      <Link
        ref={ref}
        href="/dashboard"
        className="flex min-w-0 flex-1 items-center gap-3"
        aria-label={APP_CONFIG.name}
        {...tooltipHandlers}
      >
        <div className="flex items-center justify-center size-9 rounded-[10px] bg-gradient-to-br from-brand-500 to-brand-700 shadow-[var(--shadow-warm)]">
          <span className="material-symbols-outlined text-white text-[20px]">hub</span>
        </div>
        <div className={cn("flex-col min-w-0", collapsed ? "hidden" : "flex")}>
          <h1 className="text-lg font-semibold tracking-tight text-text-main">
            {APP_CONFIG.name}
          </h1>
          <span className="text-xs text-text-muted">v{APP_CONFIG.version}</span>
        </div>
      </Link>
      {tooltip}
    </>
  );
}

function CollapseButton({ collapsed, onToggleCollapse }) {
  const { ref, tooltipHandlers, tooltip } = useRailTooltip(collapsed ? "Expand sidebar" : "Collapse sidebar", collapsed);
  return (
    <>
      <button
        ref={ref}
        type="button"
        onClick={onToggleCollapse}
        className={cn(
          "flex shrink-0 items-center justify-center rounded-lg text-text-muted transition-colors hover:bg-surface-2 hover:text-text-main",
          collapsed ? "mx-auto size-9" : "size-8"
        )}
        aria-label={collapsed ? "Expand sidebar" : "Collapse sidebar"}
        aria-expanded={!collapsed}
        {...tooltipHandlers}
      >
        <span className={cn("material-symbols-outlined", collapsed ? "text-[22px]" : "text-[18px]")}>
          {collapsed ? "left_panel_open" : "left_panel_close"}
        </span>
      </button>
      {tooltip}
    </>
  );
}

function MediaProvidersToggle({ collapsed, mediaOpen, onToggle, active }) {
  const { ref, tooltipHandlers, tooltip } = useRailTooltip("Media Providers", collapsed);
  const hasNew = MEDIA_PROVIDER_KINDS.some((k) => VISIBLE_MEDIA_KINDS.includes(k.id) && k.isNew);
  return (
    <>
      <button
        ref={ref}
        onClick={onToggle}
        className={cn(
          "w-full flex items-center px-3 py-1 rounded-lg transition-all group",
          collapsed ? "justify-center" : "gap-3",
          active
            ? "bg-primary/10 text-primary"
            : "text-text-muted hover:bg-surface-2 hover:text-text-main"
        )}
        aria-expanded={mediaOpen}
        {...tooltipHandlers}
      >
        <span className="material-symbols-outlined text-[18px]">perm_media</span>
        {!collapsed && <span className="text-[13px] font-medium flex-1 text-left">Media Providers</span>}
        {!collapsed && hasNew && (
          <span className="text-[10px] font-semibold px-1.5 py-0.5 rounded-[3px] bg-green-500/15 text-green-400">NEW</span>
        )}
        {!collapsed && <span className="material-symbols-outlined text-[14px] transition-transform" style={{ transform: mediaOpen ? "rotate(180deg)" : "rotate(0deg)" }}>
          expand_more
        </span>}
      </button>
      {tooltip}
    </>
  );
}

export default function Sidebar({ onClose, collapsed = false, onToggleCollapse }) {
  const pathname = usePathname();
  const [mediaOpen, setMediaOpen] = useState(false);
  const [isDisconnected, setIsDisconnected] = useState(false);
  const [updateInfo, setUpdateInfo] = useState(null);
  const [showUpdateModal, setShowUpdateModal] = useState(false);
  const [showRemoteModal, setShowRemoteModal] = useState(false);
  const [isUpdating, setIsUpdating] = useState(false);
  const [shutdownCountdown, setShutdownCountdown] = useState(0);
  const [enableTranslator, setEnableTranslator] = useState(false);
  const { copied, copy } = useCopyToClipboard(2000);

  const INSTALL_CMD = UPDATER_CONFIG.installCmdLatest;

  useEffect(() => {
    fetch("/api/settings")
      .then(res => res.json())
      .then(data => { if (data.enableTranslator) setEnableTranslator(true); })
      .catch(() => {});
  }, []);

  // Lazy check for new npm version on mount
  useEffect(() => {
    fetch("/api/version")
      .then(res => res.json())
      .then(data => { if (data.hasUpdate) setUpdateInfo(data); })
      .catch(() => {});
  }, []);

  const isActive = (href) => {
    if (href === "/dashboard/endpoint") {
      return pathname === "/dashboard" || pathname.startsWith("/dashboard/endpoint");
    }
    return pathname.startsWith(href);
  };

  // Open manual update panel (no countdown yet — user must click Copy to trigger shutdown)
  const handleUpdate = () => {
    setShowUpdateModal(false);
    setIsUpdating(true);
  };

  // Triggered by Copy button inside ManualUpdatePanel: copy + countdown + shutdown
  const handleCopyAndShutdown = async () => {
    try { await navigator.clipboard.writeText(INSTALL_CMD); } catch { /* clipboard blocked */ }
    copy(INSTALL_CMD);
    let remaining = UPDATER_CONFIG.shutdownCountdownSec;
    setShutdownCountdown(remaining);
    const timer = setInterval(() => {
      remaining -= 1;
      setShutdownCountdown(remaining);
      if (remaining <= 0) {
        clearInterval(timer);
        fetch("/api/version/shutdown", { method: "POST" }).catch(() => {});
        setIsDisconnected(true);
      }
    }, 1000);
  };

  const handleCancelUpdate = () => {
    setIsUpdating(false);
    setShutdownCountdown(0);
  };

  // Note: legacy updater poll removed. New flow: copy install cmd + shutdown server,
  // user runs the command manually in another terminal.


  return (
    <>
      <aside className={cn(
        "flex flex-col border-r border-border-subtle bg-vibrancy backdrop-blur-xl transition-[width,colors] duration-300 min-h-full",
        collapsed ? "w-20" : "w-72"
      )}>
        {/* Traffic lights */}
        <div className={cn("items-center gap-2 px-6 pt-5 pb-2", collapsed ? "hidden" : "flex")}>
          <div className="w-3 h-3 rounded-full bg-[#FF5F56]" />
          <div className="w-3 h-3 rounded-full bg-[#FFBD2E]" />
          <div className="w-3 h-3 rounded-full bg-[#27C93F]" />
        </div>

        {/* Logo / collapse toggle. Collapsed: the logo is dropped — the rail's
            content width (40px) cannot fit logo + button side by side, so only
            the toggle shows, centered in the logo's place. */}
        <div className={cn("py-4 flex flex-col gap-2", collapsed ? "px-3" : "px-6")}>
          {collapsed ? (
            onToggleCollapse && (
              <CollapseButton collapsed={collapsed} onToggleCollapse={onToggleCollapse} />
            )
          ) : (
            <div className="flex items-center gap-2">
              <LogoLink collapsed={collapsed} />
              {onToggleCollapse && (
                <CollapseButton collapsed={collapsed} onToggleCollapse={onToggleCollapse} />
              )}
            </div>
          )}
          {updateInfo && !collapsed && (
            <div className="flex flex-col gap-1.5 rounded p-1 -m-1">
              <span className="text-xs font-semibold text-green-600 dark:text-amber-500">
                ↑ New version available: v{updateInfo.latestVersion}
              </span>
              <div className="flex items-center gap-2">
                <button
                  onClick={() => setShowUpdateModal(true)}
                  className="px-2 py-1 rounded bg-green-600 hover:bg-green-700 dark:bg-amber-500 dark:hover:bg-amber-600 text-white text-[11px] font-semibold transition-colors cursor-pointer"
                >
                  Update now
                </button>
                <button
                  onClick={() => copy(INSTALL_CMD)}
                  title="Copy install command"
                  className="flex-1 text-left hover:opacity-80 transition-opacity cursor-pointer min-w-0"
                >
                  <code className="block text-[10px] text-green-600/80 dark:text-amber-400/70 font-mono truncate">
                    {copied ? "✓ copied!" : INSTALL_CMD}
                  </code>
                </button>
              </div>
            </div>
          )}
        </div>

        {/* Navigation */}
        <nav className={cn("flex-1 py-2 space-y-0.5 overflow-y-auto custom-scrollbar", collapsed ? "px-3" : "px-4")}>
          {navItems.map((item) => (
            <SidebarLink
              key={item.href}
              href={item.href}
              label={item.label}
              icon={item.icon}
              collapsed={collapsed}
              active={isActive(item.href)}
              onClick={onClose}
            />
          ))}

          {/* System section */}
          <div className="pt-3 mt-2 space-y-0.5">
            <p className={cn("px-4 text-xs font-semibold text-text-muted/60 uppercase tracking-wider mb-2", collapsed && "sr-only")}>
              System
            </p>

            {/* Media Providers accordion */}
            <MediaProvidersToggle collapsed={collapsed} mediaOpen={mediaOpen} onToggle={() => setMediaOpen((v) => !v)} active={pathname.startsWith("/dashboard/media-providers")} />

            {mediaOpen && (
              <div className={collapsed ? "" : "pl-4"}>
                {MEDIA_PROVIDER_KINDS.filter((k) => VISIBLE_MEDIA_KINDS.includes(k.id)).map((kind) => (
                  <SidebarLink
                    key={kind.id}
                    href={`/dashboard/media-providers/${kind.id}`}
                    label={kind.label}
                    icon={kind.icon}
                    collapsed={collapsed}
                    active={pathname.startsWith(`/dashboard/media-providers/${kind.id}`)}
                    onClick={onClose}
                    compact
                    iconClass="text-[16px]"
                    badge={kind.isNew ? "NEW" : undefined}
                  />
                ))}
                <SidebarLink
                  href={COMBINED_WEB_ITEM.href}
                  label={COMBINED_WEB_ITEM.label}
                  icon={COMBINED_WEB_ITEM.icon}
                  collapsed={collapsed}
                  active={pathname.startsWith(COMBINED_WEB_ITEM.href)}
                  onClick={onClose}
                  compact
                  iconClass="text-[16px]"
                />
              </div>
            )}

            {systemItems.map((item) => (
              <SidebarLink
                key={item.href}
                href={item.href}
                label={item.label}
                icon={item.icon}
                collapsed={collapsed}
                active={isActive(item.href)}
                onClick={onClose}
              />
            ))}

            {/* Debug items (inside System section, before Settings) */}
            {debugItems.map((item) => {
              const show = item.href !== "/dashboard/translator" || enableTranslator;
              return show ? (
                <SidebarLink
                  key={item.href}
                  href={item.href}
                  label={item.label}
                  icon={item.icon}
                  collapsed={collapsed}
                  active={isActive(item.href)}
                  onClick={onClose}
                />
              ) : null;
            })}

            {/* Remote — kept from upstream v0.5.81, adapted to collapsed rail */}
            <button
              onClick={() => setShowRemoteModal(true)}
              className={cn(
                "flex items-center gap-3 px-3 py-1 rounded-lg transition-all group w-full",
                "text-text-muted hover:bg-surface-2 hover:text-text-main"
              )}
            >
              <span className="material-symbols-outlined text-[18px] group-hover:text-primary transition-colors">
                computer
              </span>
              {!collapsed && <span className="text-[13px] font-medium">9Remote</span>}
              {!collapsed && (
                <span className="ml-auto text-[10px] font-semibold px-1.5 py-0.5 rounded-[3px] bg-green-500/15 text-green-400">
                  NEW
                </span>
              )}
            </button>

            {/* 9English — kept from upstream v0.5.81 */}
            <a
              href="https://9english.net/"
              target="_blank"
              rel="noreferrer"
              onClick={onClose}
              className={cn(
                "flex items-center gap-3 px-3 py-1 rounded-lg transition-all group w-full",
                "text-text-muted hover:bg-surface-2 hover:text-text-main"
              )}
            >
              <span className="material-symbols-outlined text-[18px] group-hover:text-primary transition-colors">
                translate
              </span>
              {!collapsed && <span className="text-[13px] font-medium">9English</span>}
            </a>

            {/* Settings */}
            <SidebarLink
              href="/dashboard/profile"
              label="Settings"
              icon="settings"
              collapsed={collapsed}
              active={isActive("/dashboard/profile")}
              onClick={onClose}
            />
          </div>
        </nav>

      </aside>

      <NineRemotePromoModal isOpen={showRemoteModal} onClose={() => setShowRemoteModal(false)} />

      {/* Update Confirmation Modal */}
      <ConfirmModal
        isOpen={showUpdateModal}
        onClose={() => setShowUpdateModal(false)}
        onConfirm={handleUpdate}
        title="Update 9Router"
        message={`Show install command for v${updateInfo?.latestVersion || ""}? You can copy it and shutdown to install manually.`}
        confirmText="Show Command"
        cancelText="Cancel"
        variant="primary"
      />

      {/* Disconnected / Updating Overlay */}
      {(isDisconnected || isUpdating) && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/80 backdrop-blur-sm p-6">
          {isUpdating ? (
            <ManualUpdatePanel
              latestVersion={updateInfo?.latestVersion}
              installCmd={INSTALL_CMD}
              copied={copied}
              onCopyAndShutdown={handleCopyAndShutdown}
              onCancel={handleCancelUpdate}
              countdown={shutdownCountdown}
              isDisconnected={isDisconnected}
            />
          ) : (
            <div className="text-center p-8">
              <div className="flex items-center justify-center size-16 rounded-full bg-red-500/20 text-red-500 mx-auto mb-4">
                <span className="material-symbols-outlined text-[32px]">power_off</span>
              </div>
              <h2 className="text-xl font-semibold text-white mb-2">Server Disconnected</h2>
              <p className="text-text-muted mb-6">The proxy server has been stopped.</p>
              <Button variant="secondary" onClick={() => globalThis.location.reload()}>
                Reload Page
              </Button>
            </div>
          )}
        </div>
      )}
    </>
  );
}

Sidebar.propTypes = {
  onClose: PropTypes.func,
  collapsed: PropTypes.bool,
  onToggleCollapse: PropTypes.func,
};

function ManualUpdatePanel({ latestVersion, installCmd, copied, onCopyAndShutdown, onCancel, countdown, isDisconnected }) {  const isCountingDown = countdown > 0;
  return (
    <div className="w-full max-w-lg rounded-xl bg-neutral-900/95 border border-white/10 p-6 text-white">
      <div className="flex items-center gap-3 mb-4">
        <div className="flex items-center justify-center size-11 rounded-full bg-amber-500/20 text-amber-400">
          <span className="material-symbols-outlined text-[24px]">content_copy</span>
        </div>
        <div>
          <h2 className="text-lg font-semibold">Update 9Router{latestVersion ? ` to v${latestVersion}` : ""}</h2>
          <p className="text-xs text-white/60">
            {isDisconnected
              ? "Server stopped. Paste the command into a terminal to install."
              : isCountingDown
                ? `Command copied. Server will stop in ${countdown}s...`
                : "Click the button below to copy the install command and shutdown."}
          </p>
        </div>
      </div>

      <p className="text-sm text-white/80 mb-2">Install command:</p>
      <div className="w-full px-3 py-2 rounded bg-white/5 mb-4">
        <code className="text-xs font-mono text-amber-400 break-all">{installCmd}</code>
      </div>

      <ol className="text-xs text-white/70 space-y-1 list-decimal list-inside mb-4">
        <li>Click <strong>Copy & Shutdown</strong> below.</li>
        <li>Paste the command into your terminal and press Enter.</li>
        <li>Run <code className="px-1 rounded bg-white/10 text-green-400">9router</code> again after install.</li>
      </ol>

      {isDisconnected ? (
        <Button variant="secondary" fullWidth onClick={() => globalThis.location.reload()}>
          Reload Page
        </Button>
      ) : (
        <div className="flex gap-2">
          <Button variant="secondary" onClick={onCancel} disabled={isCountingDown}>
            Cancel
          </Button>
          <Button variant="primary" fullWidth onClick={onCopyAndShutdown} disabled={isCountingDown}>
            {copied ? "✓ Copied — shutting down..." : isCountingDown ? `Shutting down in ${countdown}s` : "Copy & Shutdown"}
          </Button>
        </div>
      )}
    </div>
  );
}

ManualUpdatePanel.propTypes = {
  latestVersion: PropTypes.string,
  installCmd: PropTypes.string.isRequired,
  copied: PropTypes.bool,
  onCopyAndShutdown: PropTypes.func.isRequired,
  onCancel: PropTypes.func.isRequired,
  countdown: PropTypes.number,
  isDisconnected: PropTypes.bool,
};
