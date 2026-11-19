import React from 'react';
import {
  Server,
  Key,
  Cpu,
  BarChart3,
  Settings,
  Zap,
  Activity,
  ExternalLink,
} from 'lucide-react';

const NAV_ITEMS = [
  { id: 'providers', label: 'Providers', icon: Server, desc: 'Upstream connections' },
  { id: 'keys', label: 'API Keys', icon: Key, desc: 'Clients & access control' },
  { id: 'models', label: 'Model Catalog', icon: Cpu, desc: 'Unified models directory' },
  { id: 'usage', label: 'Usage & Stats', icon: BarChart3, desc: 'Metrics & token history' },
  { id: 'settings', label: 'Settings', icon: Settings, desc: 'Gateway configuration' },
];

export default function Sidebar({ currentTab, onSelectTab, isHealthy }) {
  return (
    <aside className="w-64 bg-slate-900/90 border-r border-slate-800/80 flex flex-col justify-between select-none">
      <div>
        {/* Brand Header */}
        <div className="h-16 flex items-center px-6 gap-3 border-b border-slate-800/70">
          <div className="w-9 h-9 rounded-xl bg-gradient-to-tr from-cyan-500 to-blue-600 flex items-center justify-center shadow-lg shadow-cyan-500/20">
            <Zap className="w-5 h-5 text-white fill-white" />
          </div>
          <div>
            <div className="flex items-center gap-2">
              <span className="font-bold text-lg tracking-tight text-white">9Router</span>
              <span className="text-[10px] uppercase font-semibold tracking-wider px-1.5 py-0.5 rounded bg-cyan-500/10 text-cyan-400 border border-cyan-500/20">
                Rust Core
              </span>
            </div>
            <p className="text-xs text-slate-400">High-Performance AI Gateway</p>
          </div>
        </div>

        {/* Navigation Items */}
        <nav className="p-3 space-y-1">
          {NAV_ITEMS.map((item) => {
            const Icon = item.icon;
            const active = currentTab === item.id;
            return (
              <button
                key={item.id}
                onClick={() => onSelectTab(item.id)}
                className={`w-full flex items-center gap-3 px-3.5 py-2.5 rounded-lg text-sm font-medium transition-all group ${
                  active
                    ? 'bg-cyan-500/15 text-cyan-400 border border-cyan-500/30 shadow-sm shadow-cyan-500/10'
                    : 'text-slate-300 hover:text-white hover:bg-slate-800/60'
                }`}
              >
                <Icon
                  className={`w-4 h-4 transition-colors ${
                    active ? 'text-cyan-400' : 'text-slate-400 group-hover:text-slate-200'
                  }`}
                />
                <span className="flex-1 text-left">{item.label}</span>
              </button>
            );
          })}
        </nav>
      </div>

      {/* Gateway Status & Footer */}
      <div className="p-4 border-t border-slate-800/70 space-y-3">
        <div className="p-3 rounded-lg bg-slate-950/60 border border-slate-800">
          <div className="flex items-center justify-between">
            <span className="text-xs text-slate-400 flex items-center gap-1.5">
              <Activity className="w-3.5 h-3.5 text-cyan-400" /> Gateway Status
            </span>
            <span className="flex items-center gap-1.5 text-xs font-medium">
              <span
                className={`w-2 h-2 rounded-full ${
                  isHealthy ? 'bg-emerald-400 animate-pulse' : 'bg-amber-400'
                }`}
              />
              <span className={isHealthy ? 'text-emerald-400' : 'text-amber-400'}>
                {isHealthy ? 'Healthy' : 'Connecting'}
              </span>
            </span>
          </div>
          <div className="mt-2 text-[11px] text-slate-400 flex justify-between">
            <span>Port: 20127</span>
            <span>Single Binary</span>
          </div>
        </div>

        <a
          href="https://github.com/ninerouter/9router"
          target="_blank"
          rel="noreferrer"
          className="flex items-center justify-between text-xs text-slate-400 hover:text-slate-200 px-2 py-1 transition-colors"
        >
          <span>v0.6.0 (Rust)</span>
          <ExternalLink className="w-3.5 h-3.5" />
        </a>
      </div>
    </aside>
  );
}
