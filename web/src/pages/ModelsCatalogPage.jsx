import React, { useState, useEffect } from 'react';
import {
  Cpu,
  Search,
  Copy,
  Check,
  Sparkles,
  Zap,
  Layers,
  Code2,
  Eye,
  MessageSquare,
  FileCode,
} from 'lucide-react';
import { api } from '../api/client';

export default function ModelsCatalogPage({ onRefreshTrigger }) {
  const [models, setModels] = useState([]);
  const [loading, setLoading] = useState(true);
  const [searchQuery, setSearchQuery] = useState('');
  const [selectedProvider, setSelectedProvider] = useState('all');
  const [copiedId, setCopiedId] = useState(null);

  const loadData = async () => {
    setLoading(true);
    try {
      const data = await api.getModels();
      setModels(data);
    } catch (err) {
      console.error(err);
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    loadData();
  }, [onRefreshTrigger]);

  const copyModelId = (id) => {
    navigator.clipboard.writeText(id);
    setCopiedId(id);
    setTimeout(() => setCopiedId(null), 2000);
  };

  const providers = ['all', ...Array.from(new Set(models.map((m) => m.provider || m.owned_by || 'other')))];

  const filteredModels = models.filter((m) => {
    const matchesSearch =
      m.id.toLowerCase().includes(searchQuery.toLowerCase()) ||
      (m.description && m.description.toLowerCase().includes(searchQuery.toLowerCase()));
    const matchesProvider =
      selectedProvider === 'all' || (m.provider || m.owned_by) === selectedProvider;
    return matchesSearch && matchesProvider;
  });

  return (
    <div className="p-8 max-w-7xl mx-auto space-y-6">
      {/* Top Banner */}
      <div className="flex flex-col md:flex-row md:items-center justify-between gap-4 pb-2">
        <div>
          <h2 className="text-2xl font-bold text-white tracking-tight">Unified Model Catalog</h2>
          <p className="text-sm text-slate-400 mt-1">
            Browse and query models exposed via the OpenAI-compatible <code className="text-cyan-400 font-mono">/v1/models</code> endpoint.
          </p>
        </div>

        <div className="text-xs text-slate-400 bg-slate-900 border border-slate-800 px-3 py-1.5 rounded-lg flex items-center gap-2">
          <span className="w-2 h-2 rounded-full bg-emerald-400" />
          <span>{models.length} Total Routable Models</span>
        </div>
      </div>

      {/* Search and Filters */}
      <div className="flex flex-col sm:flex-row gap-3 items-center justify-between">
        <div className="relative w-full sm:w-80">
          <Search className="w-4 h-4 text-slate-500 absolute left-3.5 top-1/2 -translate-y-1/2" />
          <input
            type="text"
            placeholder="Search models..."
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
            className="w-full pl-10 pr-4 py-2 rounded-xl bg-slate-900/80 border border-slate-800 text-sm text-white placeholder-slate-500 focus:outline-none focus:border-cyan-500 transition-colors"
          />
        </div>

        {/* Provider Pills */}
        <div className="flex items-center gap-1.5 overflow-x-auto w-full sm:w-auto pb-1">
          {providers.map((p) => {
            const active = selectedProvider === p;
            return (
              <button
                key={p}
                onClick={() => setSelectedProvider(p)}
                className={`px-3 py-1.5 rounded-lg text-xs font-medium capitalize transition-all shrink-0 ${
                  active
                    ? 'bg-cyan-500/20 text-cyan-300 border border-cyan-500/30'
                    : 'bg-slate-900 text-slate-400 hover:text-slate-200 border border-slate-800'
                }`}
              >
                {p}
              </button>
            );
          })}
        </div>
      </div>

      {/* Models Grid */}
      {loading ? (
        <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-5">
          {[1, 2, 3, 4, 5, 6].map((i) => (
            <div key={i} className="h-44 rounded-2xl bg-slate-900/60 border border-slate-800 animate-pulse" />
          ))}
        </div>
      ) : filteredModels.length === 0 ? (
        <div className="rounded-2xl border border-dashed border-slate-800 bg-slate-900/30 p-12 text-center text-slate-400">
          No models matched your search criteria.
        </div>
      ) : (
        <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-5">
          {filteredModels.map((m) => {
            const isCopied = copiedId === m.id;
            const capabilities = m.capabilities || ['chat', 'streaming'];

            return (
              <div
                key={m.id}
                className="bg-slate-900/70 border border-slate-800 rounded-2xl p-5 hover:border-slate-700 transition-all flex flex-col justify-between group shadow-sm hover:shadow-md"
              >
                <div>
                  <div className="flex items-start justify-between gap-2 mb-2">
                    <span className="font-mono text-sm font-semibold text-white group-hover:text-cyan-400 transition-colors break-all">
                      {m.id}
                    </span>
                    <button
                      onClick={() => copyModelId(m.id)}
                      className="text-slate-500 hover:text-cyan-400 p-1 rounded hover:bg-slate-800 transition-colors shrink-0"
                      title="Copy model ID"
                    >
                      {isCopied ? <Check className="w-3.5 h-3.5 text-emerald-400" /> : <Copy className="w-3.5 h-3.5" />}
                    </button>
                  </div>

                  {m.description && (
                    <p className="text-xs text-slate-400 line-clamp-2 mb-3 leading-relaxed">
                      {m.description}
                    </p>
                  )}

                  <div className="flex flex-wrap gap-1.5 mt-2">
                    <span className="px-2 py-0.5 rounded-md bg-slate-800 text-[11px] font-medium text-slate-300">
                      {m.provider || m.owned_by || 'openai'}
                    </span>
                    {m.contextWindow && (
                      <span className="px-2 py-0.5 rounded-md bg-cyan-500/10 text-[11px] font-medium text-cyan-400 border border-cyan-500/20">
                        {m.contextWindow}
                      </span>
                    )}
                    {capabilities.map((c) => (
                      <span
                        key={c}
                        className="px-2 py-0.5 rounded-md bg-slate-800/80 text-[10px] uppercase font-semibold text-slate-400"
                      >
                        {c}
                      </span>
                    ))}
                  </div>
                </div>

                <div className="mt-4 pt-3 border-t border-slate-800/80 flex items-center justify-between text-xs text-slate-500">
                  <span>Routing prefix: <code className="text-slate-400">{m.provider ? `${m.provider}/` : ''}{m.id}</code></span>
                  <button
                    onClick={() => copyModelId(m.id)}
                    className="text-cyan-400 hover:text-cyan-300 text-[11px] font-medium flex items-center gap-1"
                  >
                    <span>Use in API</span>
                  </button>
                </div>
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}
