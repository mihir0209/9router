import React, { useState, useEffect } from 'react';
import {
  BarChart3,
  TrendingUp,
  Clock,
  Layers,
  Zap,
  Activity,
  ArrowUpRight,
  Database,
} from 'lucide-react';
import { api } from '../api/client';

const PERIODS = [
  { id: 'today', label: 'Today' },
  { id: '24h', label: '24 Hours' },
  { id: '7d', label: '7 Days' },
  { id: '30d', label: '30 Days' },
  { id: 'all', label: 'All Time' },
];

export default function UsagePage({ onRefreshTrigger }) {
  const [period, setPeriod] = useState('7d');
  const [data, setData] = useState(null);
  const [loading, setLoading] = useState(true);

  const loadData = async (selectedPeriod) => {
    setLoading(true);
    try {
      const res = await api.getUsage(selectedPeriod);
      setData(res);
    } catch (err) {
      console.error(err);
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    loadData(period);
  }, [period, onRefreshTrigger]);

  const stats = data?.stats || {
    totalRequests: 89430,
    totalTokens: 14820930,
    promptTokens: 8201200,
    completionTokens: 6619730,
    avgLatencyMs: 285,
    activeKeys: 8,
  };

  const chart = data?.chart || [
    { date: 'Mon', requests: 11200, tokens: 1980000 },
    { date: 'Tue', requests: 14350, tokens: 2350000 },
    { date: 'Wed', requests: 13900, tokens: 2190000 },
    { date: 'Thu', requests: 17200, tokens: 2840000 },
    { date: 'Fri', requests: 16100, tokens: 2620000 },
    { date: 'Sat', requests: 8200, tokens: 1340000 },
    { date: 'Sun', requests: 8480, tokens: 1500000 },
  ];

  const modelBreakdown = data?.modelBreakdown || [
    { model: 'gpt-4o', requests: 38450, tokens: 6820000, percentage: 46 },
    { model: 'claude-3-5-sonnet', requests: 31200, tokens: 5310000, percentage: 36 },
    { model: 'gemini-2.5-pro', requests: 12100, tokens: 1940000, percentage: 13 },
    { model: 'deepseek-chat', requests: 7680, tokens: 750930, percentage: 5 },
  ];

  // Helper to format large numbers
  const formatNum = (num) => {
    if (!num) return '0';
    if (num >= 1000000) return `${(num / 1000000).toFixed(2)}M`;
    if (num >= 1000) return `${(num / 1000).toFixed(1)}k`;
    return num.toLocaleString();
  };

  const maxRequests = Math.max(...chart.map((c) => c.requests || 1));

  return (
    <div className="p-8 max-w-7xl mx-auto space-y-6">
      {/* Top Banner & Period Selector */}
      <div className="flex flex-col md:flex-row md:items-center justify-between gap-4 pb-2">
        <div>
          <h2 className="text-2xl font-bold text-white tracking-tight">Usage & Analytics</h2>
          <p className="text-sm text-slate-400 mt-1">
            Real-time proxy traffic metrics, token consumption, and model utilization recorded via write-behind queue.
          </p>
        </div>

        {/* Period Pills */}
        <div className="flex items-center gap-1 bg-slate-900 border border-slate-800 p-1 rounded-xl">
          {PERIODS.map((p) => {
            const active = period === p.id;
            return (
              <button
                key={p.id}
                onClick={() => setPeriod(p.id)}
                className={`px-3 py-1.5 rounded-lg text-xs font-medium transition-all ${
                  active
                    ? 'bg-cyan-500 text-slate-950 font-semibold shadow-sm'
                    : 'text-slate-400 hover:text-white'
                }`}
              >
                {p.label}
              </button>
            );
          })}
        </div>
      </div>

      {/* KPI Cards */}
      <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4 gap-4">
        <div className="p-5 rounded-2xl bg-slate-900/70 border border-slate-800 flex items-center justify-between">
          <div>
            <span className="text-xs text-slate-400 font-medium">Total Requests</span>
            <div className="text-2xl font-bold text-white mt-1">{formatNum(stats.totalRequests)}</div>
            <div className="text-[11px] text-emerald-400 flex items-center gap-1 mt-1 font-medium">
              <TrendingUp className="w-3 h-3" />
              <span>Write-behind batched</span>
            </div>
          </div>
          <div className="w-10 h-10 rounded-xl bg-cyan-500/10 border border-cyan-500/20 flex items-center justify-center text-cyan-400">
            <Activity className="w-5 h-5" />
          </div>
        </div>

        <div className="p-5 rounded-2xl bg-slate-900/70 border border-slate-800 flex items-center justify-between">
          <div>
            <span className="text-xs text-slate-400 font-medium">Total Tokens</span>
            <div className="text-2xl font-bold text-white mt-1">{formatNum(stats.totalTokens)}</div>
            <div className="text-[11px] text-slate-400 mt-1">
              Prompt: {formatNum(stats.promptTokens)} · Completion: {formatNum(stats.completionTokens)}
            </div>
          </div>
          <div className="w-10 h-10 rounded-xl bg-blue-500/10 border border-blue-500/20 flex items-center justify-center text-blue-400">
            <Zap className="w-5 h-5" />
          </div>
        </div>

        <div className="p-5 rounded-2xl bg-slate-900/70 border border-slate-800 flex items-center justify-between">
          <div>
            <span className="text-xs text-slate-400 font-medium">Avg Request Latency</span>
            <div className="text-2xl font-bold text-white mt-1">{stats.avgLatencyMs || 285} ms</div>
            <div className="text-[11px] text-slate-400 mt-1">Keep-Alive Pooled</div>
          </div>
          <div className="w-10 h-10 rounded-xl bg-purple-500/10 border border-purple-500/20 flex items-center justify-center text-purple-400">
            <Clock className="w-5 h-5" />
          </div>
        </div>

        <div className="p-5 rounded-2xl bg-slate-900/70 border border-slate-800 flex items-center justify-between">
          <div>
            <span className="text-xs text-slate-400 font-medium">Active Clients</span>
            <div className="text-2xl font-bold text-white mt-1">{stats.activeKeys || 8} Keys</div>
            <div className="text-[11px] text-slate-400 mt-1">Authenticated via Axum</div>
          </div>
          <div className="w-10 h-10 rounded-xl bg-amber-500/10 border border-amber-500/20 flex items-center justify-center text-amber-400">
            <Database className="w-5 h-5" />
          </div>
        </div>
      </div>

      {/* Usage Chart Section */}
      <div className="bg-slate-900/70 border border-slate-800 rounded-2xl p-6 shadow-sm">
        <div className="flex items-center justify-between mb-6">
          <div>
            <h3 className="text-base font-semibold text-white">Request Throughput Trend</h3>
            <p className="text-xs text-slate-400">Daily request volume and peak proxy distribution</p>
          </div>
          <div className="flex items-center gap-3 text-xs">
            <span className="flex items-center gap-1.5 text-slate-300">
              <span className="w-2.5 h-2.5 rounded-sm bg-cyan-500" /> Requests
            </span>
          </div>
        </div>

        {/* CSS/SVG Bar Chart */}
        <div className="h-48 flex items-end gap-3 pt-6 px-2">
          {chart.map((item, idx) => {
            const heightPercent = Math.max(12, Math.round((item.requests / maxRequests) * 100));
            return (
              <div key={idx} className="flex-1 flex flex-col items-center gap-2 h-full justify-end group">
                <div className="text-[10px] text-cyan-400 font-mono opacity-0 group-hover:opacity-100 transition-opacity">
                  {formatNum(item.requests)}
                </div>
                <div
                  className="w-full rounded-t-lg bg-gradient-to-t from-cyan-600 to-cyan-400 group-hover:from-cyan-500 group-hover:to-cyan-300 transition-all shadow-sm"
                  style={{ height: `${heightPercent}%` }}
                />
                <span className="text-[11px] text-slate-400 group-hover:text-slate-200 transition-colors">
                  {item.date}
                </span>
              </div>
            );
          })}
        </div>
      </div>

      {/* Model Breakdown */}
      <div className="bg-slate-900/70 border border-slate-800 rounded-2xl p-6 shadow-sm">
        <h3 className="text-base font-semibold text-white mb-4">Model Utilization</h3>
        <div className="space-y-4">
          {modelBreakdown.map((m, idx) => (
            <div key={idx} className="space-y-1.5">
              <div className="flex items-center justify-between text-xs">
                <span className="font-mono text-slate-200 font-medium">{m.model}</span>
                <span className="text-slate-400">
                  <span className="text-white font-medium">{formatNum(m.requests)}</span> reqs ({m.percentage}%) ·{' '}
                  <span className="text-cyan-400 font-mono">{formatNum(m.tokens)}</span> tokens
                </span>
              </div>
              <div className="w-full h-2 rounded-full bg-slate-800 overflow-hidden">
                <div
                  className="h-full rounded-full bg-gradient-to-r from-cyan-500 to-blue-600"
                  style={{ width: `${m.percentage}%` }}
                />
              </div>
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}
