import React from 'react';
import { RefreshCw, Terminal, Layers } from 'lucide-react';

export default function Header({ title, subtitle, onRefresh, isRefreshing, extraAction }) {
  return (
    <header className="h-16 px-8 border-b border-slate-800/80 bg-slate-900/40 backdrop-blur-sm flex items-center justify-between">
      <div>
        <h1 className="text-xl font-bold tracking-tight text-white flex items-center gap-2">
          {title}
        </h1>
        {subtitle && <p className="text-xs text-slate-400 mt-0.5">{subtitle}</p>}
      </div>

      <div className="flex items-center gap-3">
        {extraAction}

        <button
          onClick={onRefresh}
          disabled={isRefreshing}
          className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg border border-slate-700/80 bg-slate-800/60 hover:bg-slate-800 text-xs text-slate-300 hover:text-white transition-all disabled:opacity-50"
          title="Refresh view"
        >
          <RefreshCw className={`w-3.5 h-3.5 ${isRefreshing ? 'animate-spin text-cyan-400' : ''}`} />
          <span>Refresh</span>
        </button>
      </div>
    </header>
  );
}
