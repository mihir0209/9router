import React, { useState, useEffect } from 'react';
import {
  Settings,
  Save,
  Check,
  Database,
  Radio,
  Sliders,
  Shield,
  Zap,
  Server,
  AlertCircle,
} from 'lucide-react';
import { api } from '../api/client';

export default function SettingsPage({ onRefreshTrigger }) {
  const [settings, setSettings] = useState(null);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [saveSuccess, setSaveSuccess] = useState(false);
  const [errorMsg, setErrorMsg] = useState('');

  const loadData = async () => {
    setLoading(true);
    try {
      const data = await api.getSettings();
      setSettings(data);
    } catch (err) {
      console.error(err);
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    loadData();
  }, [onRefreshTrigger]);

  const handleToggle = (key) => {
    setSettings((prev) => ({ ...prev, [key]: !prev[key] }));
  };

  const handleChange = (key, value) => {
    setSettings((prev) => ({ ...prev, [key]: value }));
  };

  const handleSave = async (e) => {
    e.preventDefault();
    setSaving(true);
    setErrorMsg('');
    setSaveSuccess(false);

    try {
      await api.updateSettings(settings);
      setSaveSuccess(true);
      setTimeout(() => setSaveSuccess(false), 3000);
    } catch (err) {
      setErrorMsg(err.message || 'Failed to update settings');
    } finally {
      setSaving(false);
    }
  };

  if (loading || !settings) {
    return (
      <div className="p-8 max-w-4xl mx-auto space-y-6">
        <div className="h-8 w-48 bg-slate-800 rounded animate-pulse" />
        <div className="h-64 bg-slate-900 rounded-2xl border border-slate-800 animate-pulse" />
      </div>
    );
  }

  return (
    <div className="p-8 max-w-4xl mx-auto space-y-6">
      {/* Top Banner */}
      <div className="flex flex-col md:flex-row md:items-center justify-between gap-4 pb-2">
        <div>
          <h2 className="text-2xl font-bold text-white tracking-tight">Gateway Settings</h2>
          <p className="text-sm text-slate-400 mt-1">
            Configure 9Router single-binary runtime, storage engines, and routing behaviors.
          </p>
        </div>

        <button
          onClick={handleSave}
          disabled={saving}
          className="flex items-center gap-2 px-5 py-2.5 rounded-xl bg-cyan-500 hover:bg-cyan-400 text-slate-950 font-semibold text-sm shadow-lg shadow-cyan-500/20 transition-all disabled:opacity-50 active:scale-95"
        >
          {saving ? (
            <span>Saving...</span>
          ) : saveSuccess ? (
            <>
              <Check className="w-4 h-4 text-emerald-950" />
              <span>Saved!</span>
            </>
          ) : (
            <>
              <Save className="w-4 h-4" />
              <span>Save Changes</span>
            </>
          )}
        </button>
      </div>

      {errorMsg && (
        <div className="p-4 rounded-xl bg-rose-500/10 border border-rose-500/20 text-rose-400 text-xs flex items-center gap-2">
          <AlertCircle className="w-4 h-4 shrink-0" />
          <span>{errorMsg}</span>
        </div>
      )}

      {/* System Runtime Card */}
      <div className="bg-slate-900/70 border border-slate-800 rounded-2xl p-6 shadow-sm space-y-4">
        <div className="flex items-center gap-2 border-b border-slate-800 pb-3">
          <Server className="w-4 h-4 text-cyan-400" />
          <h3 className="text-base font-semibold text-white">Runtime Environment</h3>
        </div>

        <div className="grid grid-cols-1 md:grid-cols-2 gap-4 text-xs">
          <div>
            <label className="block text-slate-400 mb-1">Server Listen Port</label>
            <input
              type="number"
              value={settings.port || 20127}
              onChange={(e) => handleChange('port', Number(e.target.value))}
              className="w-full px-3.5 py-2 rounded-xl bg-slate-950/80 border border-slate-800 text-sm text-white font-mono focus:outline-none focus:border-cyan-500"
            />
            <p className="text-[11px] text-slate-400 mt-1">Configured via PORT environment variable</p>
          </div>

          <div>
            <label className="block text-slate-400 mb-1">Data Directory</label>
            <input
              type="text"
              readOnly
              value={settings.dataDir || '~/.9router'}
              className="w-full px-3.5 py-2 rounded-xl bg-slate-950/40 border border-slate-800 text-sm text-slate-400 font-mono select-all"
            />
            <p className="text-[11px] text-slate-400 mt-1">Stores SQLite database and encryption secrets</p>
          </div>
        </div>
      </div>

      {/* Storage & Database Card */}
      <div className="bg-slate-900/70 border border-slate-800 rounded-2xl p-6 shadow-sm space-y-4">
        <div className="flex items-center gap-2 border-b border-slate-800 pb-3">
          <Database className="w-4 h-4 text-cyan-400" />
          <h3 className="text-base font-semibold text-white">Database & Queueing</h3>
        </div>

        <div className="space-y-3 text-xs">
          <div className="flex items-center justify-between p-3 rounded-xl bg-slate-950/60 border border-slate-800/80">
            <div>
              <div className="font-medium text-white">SQLite WAL Engine</div>
              <p className="text-slate-400 mt-0.5">Write-Ahead Logging with 5000ms busy timeout for concurrent safety</p>
            </div>
            <span className="text-emerald-400 font-medium px-2 py-1 rounded bg-emerald-500/10 border border-emerald-500/20 text-[11px]">
              Enabled
            </span>
          </div>

          <div className="flex items-center justify-between p-3 rounded-xl bg-slate-950/60 border border-slate-800/80">
            <div>
              <div className="font-medium text-white">Write-Behind Queue (tokio::mpsc)</div>
              <p className="text-slate-400 mt-0.5">Asynchronous zero-latency batching for request and token logs</p>
            </div>
            <span className="text-cyan-400 font-medium px-2 py-1 rounded bg-cyan-500/10 border border-cyan-500/20 text-[11px]">
              Active (2048 buffer)
            </span>
          </div>
        </div>
      </div>

      {/* Proxy Routing & Fallbacks */}
      <div className="bg-slate-900/70 border border-slate-800 rounded-2xl p-6 shadow-sm space-y-4">
        <div className="flex items-center gap-2 border-b border-slate-800 pb-3">
          <Zap className="w-4 h-4 text-cyan-400" />
          <h3 className="text-base font-semibold text-white">Proxy Routing Policies</h3>
        </div>

        <div className="space-y-4 text-xs">
          <div className="flex items-center justify-between">
            <div>
              <div className="font-medium text-white">Vertex AI Gemini Carbon Fallback</div>
              <p className="text-slate-400 mt-0.5">
                Automatically route <code className="text-cyan-400">gemini-4-carbon</code> requests to Google global endpoint
              </p>
            </div>
            <button
              type="button"
              onClick={() => handleToggle('vertexCarbonFallback')}
              className={`w-11 h-6 rounded-full transition-colors relative ${
                settings.vertexCarbonFallback !== false ? 'bg-cyan-500' : 'bg-slate-800'
              }`}
            >
              <div
                className={`w-4 h-4 rounded-full bg-white transition-transform absolute top-1 left-1 ${
                  settings.vertexCarbonFallback !== false ? 'translate-x-5' : ''
                }`}
              />
            </button>
          </div>

          <div className="flex items-center justify-between pt-3 border-t border-slate-800/60">
            <div>
              <div className="font-medium text-white">Request-Scoped Automatic Retries</div>
              <p className="text-slate-400 mt-0.5">Retry upstream network failures up to max attempts</p>
            </div>
            <select
              value={settings.maxRetries || 3}
              onChange={(e) => handleChange('maxRetries', Number(e.target.value))}
              className="px-3 py-1.5 rounded-lg bg-slate-950 border border-slate-800 text-white font-medium text-xs focus:outline-none focus:border-cyan-500"
            >
              <option value="1">1 retry</option>
              <option value="2">2 retries</option>
              <option value="3">3 retries (Default)</option>
              <option value="5">5 retries</option>
            </select>
          </div>

          <div className="flex items-center justify-between pt-3 border-t border-slate-800/60">
            <div>
              <div className="font-medium text-white">Bidirectional Format Translation Engine</div>
              <p className="text-slate-400 mt-0.5">Translate between OpenAI Chat completions and Anthropic Messages formats</p>
            </div>
            <button
              type="button"
              onClick={() => handleToggle('enableTranslator')}
              className={`w-11 h-6 rounded-full transition-colors relative ${
                settings.enableTranslator !== false ? 'bg-cyan-500' : 'bg-slate-800'
              }`}
            >
              <div
                className={`w-4 h-4 rounded-full bg-white transition-transform absolute top-1 left-1 ${
                  settings.enableTranslator !== false ? 'translate-x-5' : ''
                }`}
              />
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
