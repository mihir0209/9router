"use client";

import { useState, useEffect, useRef, useCallback } from "react";
import PropTypes from "prop-types";
import Link from "next/link";
import { createPortal } from "react-dom";
import { usePathname } from "next/navigation";
import { cn } from "@/shared/utils/cn";
import { APP_CONFIG } from "@/shared/constants/config";
import { MEDIA_PROVIDER_KINDS } from "@/shared/constants/providers";
import useSettingsStore from "@/store/settingsStore";
import Button from "./Button";

// const VISIBLE_MEDIA_KINDS = ["embedding", "image", "imageToText", "tts", "stt", "webSearch", "webFetch", "video", "music"];
const VISIBLE_MEDIA_KINDS = ["embedding", "image", "video", "tts", "stt", "systemone"];
// Combined entry: webSearch + webFetch share one page at /dashboard/media-providers/web
const COMBINED_WEB_ITEM = { id: "web", label: "Web Fetch & Search", icon: "travel_explore", href: "/dashboard/media-providers/web" };

const navItems = [
  { href: "/dashboard/endpoint", label: "Endpoint & Key", icon: "api" },
  { href: "/dashboard/basic-chat", label: "Chat Assistant", icon: "chat" },
  { href: "/dashboard/playground", label: "Playground", icon: "play_arrow" },
  { href: "/dashboard/providers", label: "Providers", icon: "dns" },
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
        prefetch={false}
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
        prefetch={false}
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
  const [enableTranslator, setEnableTranslator] = useState(false);

  useEffect(() => {
    useSettingsStore.getState().fetchSettings().then((data) => {
      if (data?.enableTranslator) setEnableTranslator(true);
    });
  }, []);

  const isActive = (href) => {
    if (href === "/dashboard/endpoint") {
      return pathname === "/dashboard" || pathname.startsWith("/dashboard/endpoint");
    }
    return pathname.startsWith(href);
  };
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

      {/* Disconnected Overlay */}
      {isDisconnected && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/80 backdrop-blur-sm p-6">
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
