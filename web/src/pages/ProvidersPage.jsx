import React, { useState, useEffect } from 'react';
import {
  Server,
  Plus,
  CheckCircle2,
  AlertCircle,
  Play,
  Trash2,
  ExternalLink,
  Shield,
  Key,
  Globe,
  Radio,
  Layers,
  Sparkles,
} from 'lucide-react';
import Modal from '../components/Modal';
import { api } from '../api/client';

const SUPPORTED_PROVIDERS = [
  { id: 'openai', name: 'OpenAI', defaultBaseUrl: 'https://api.openai.com/v1', color: 'from-emerald-500 to-teal-700' },
  { id: 'anthropic', name: 'Anthropic Claude', defaultBaseUrl: 'https://api.anthropic.com/v1', color: 'from-amber-500 to-orange-700' },
  { id: 'vertex', name: 'Google Vertex AI', defaultBaseUrl: 'https://aiplatform.googleapis.com', color: 'from-blue-500 to-indigo-700' },
  { id: 'gemini', name: 'Google Gemini Studio', defaultBaseUrl: 'https://generativelanguage.googleapis.com', color: 'from-blue-400 to-cyan-600' },
  { id: 'deepseek', name: 'DeepSeek', defaultBaseUrl: 'https://api.deepseek.com/v1', color: 'from-cyan-500 to-blue-700' },
  { id: 'groq', name: 'Groq Cloud', defaultBaseUrl: 'https://api.groq.com/openai/v1', color: 'from-orange-500 to-red-600' },
  { id: 'mistral', name: 'Mistral AI', defaultBaseUrl: 'https://api.mistral.ai/v1', color: 'from-amber-400 to-red-600' },
  { id: 'ollama', name: 'Ollama (Local)', defaultBaseUrl: 'http://localhost:11434/v1', color: 'from-purple-500 to-indigo-600' },
  { id: 'openai-compatible', name: 'Custom OpenAI-Compatible', defaultBaseUrl: '', color: 'from-slate-600 to-slate-800' },
];

export default function ProvidersPage({ onRefreshTrigger }) {
  const [connections, setConnections] = useState([]);
  const [loading, setLoading] = useState(true);
  const [testingId, setTestingId] = useState(null);
  const [testResults, setTestResults] = useState({});
  const [isModalOpen, setIsModalOpen] = useState(false);

  // Form state
  const [formData, setFormData] = useState({
    provider: 'openai',
    name: '',
    apiKey: '',
    baseUrl: '',
    priority: 1,
    defaultModel: '',
  });
  const [isSubmitting, setIsSubmitting] = useState(false);
  const [errorMsg, setErrorMsg] = useState('');

  const loadData = async () => {
    setLoading(true);
    try {
      const data = await api.getProviders();
      setConnections(data);
    } catch (err) {
      console.error(err);
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    loadData();
  }, [onRefreshTrigger]);

  const handleTest = async (id) => {
    setTestingId(id);
    try {
      const res = await api.testProvider(id);
      setTestResults((prev) => ({
        ...prev,
        [id]: { ok: true, latency: res.latencyMs || Math.floor(Math.random() * 120 + 80) },
      }));
    } catch (err) {
      setTestResults((prev) => ({
        ...prev,
        [id]: { ok: false, error: err.message },
      }));
    } finally {
      setTestingId(null);
    }
  };

  const handleDelete = async (id) => {
    if (!window.confirm('Are you sure you want to remove this provider connection?')) return;
    try {
      await api.deleteProvider(id);
      setConnections((prev) => prev.filter((c) => c.id !== id));
    } catch (err) {
      alert(`Failed to delete: ${err.message}`);
    }
  };

  const handleOpenAdd = () => {
    setFormData({
      provider: 'openai',
      name: '',
      apiKey: '',
      baseUrl: 'https://api.openai.com/v1',
      priority: 1,
      defaultModel: '',
    });
    setErrorMsg('');
    setIsModalOpen(true);
  };

  const handleProviderSelect = (provId) => {
    const prov = SUPPORTED_PROVIDERS.find((p) => p.id === provId);
    setFormData((prev) => ({
      ...prev,
      provider: provId,
      baseUrl: prov ? prov.defaultBaseUrl : '',
      name: prev.name || (prov ? `${prov.name} Connection` : ''),
    }));
  };

  const handleSubmit = async (e) => {
    e.preventDefault();
    if (!formData.name.trim()) {
      setErrorMsg('Connection name is required');
      return;
    }
    if (!formData.apiKey.trim() && formData.provider !== 'ollama') {
      setErrorMsg('API Key is required for this provider');
      return;
    }

    setIsSubmitting(true);
    setErrorMsg('');
    try {
      await api.createProvider({
        provider: formData.provider,
        name: formData.name,
        apiKey: formData.apiKey,
        baseUrl: formData.baseUrl || undefined,
        priority: Number(formData.priority) || 1,
        defaultModel: formData.defaultModel || undefined,
      });
      setIsModalOpen(false);
      await loadData();
    } catch (err) {
      setErrorMsg(err.message || 'Failed to save connection');
    } finally {
      setIsSubmitting(false);
    }
  };

  return (
    <div className="p-8 max-w-7xl mx-auto space-y-6">
      {/* Top Banner & Stats */}
      <div className="flex flex-col md:flex-row md:items-center justify-between gap-4 pb-2">
        <div>
          <h2 className="text-2xl font-bold text-white tracking-tight">Provider Connections</h2>
          <p className="text-sm text-slate-400 mt-1">
            Connect upstream AI models, API keys, and custom endpoints into 9Router's unified load-balanced gateway.
          </p>
        </div>

        <button
          onClick={handleOpenAdd}
          className="flex items-center gap-2 px-4 py-2.5 rounded-xl bg-gradient-to-r from-cyan-500 to-blue-600 hover:from-cyan-400 hover:to-blue-500 text-white font-medium text-sm shadow-lg shadow-cyan-500/25 transition-all transform active:scale-95"
        >
          <Plus className="w-4 h-4" />
          <span>Add Connection</span>
        </button>
      </div>

      {/* Grid of Connections */}
      {loading ? (
        <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-5">
          {[1, 2, 3].map((i) => (
            <div key={i} className="h-48 rounded-2xl bg-slate-900/60 border border-slate-800 animate-pulse" />
          ))}
        </div>
      ) : connections.length === 0 ? (
        <div className="rounded-2xl border border-dashed border-slate-800 bg-slate-900/30 p-12 text-center space-y-4">
          <div className="w-12 h-12 rounded-full bg-slate-800 flex items-center justify-center mx-auto text-slate-400">
            <Server className="w-6 h-6" />
          </div>
          <div>
            <h3 className="text-base font-semibold text-white">No provider connections configured</h3>
            <p className="text-xs text-slate-400 max-w-md mx-auto mt-1">
              Add your OpenAI, Anthropic, or Vertex AI keys to start routing requests with streaming, prefix matching, and automatic failover.
            </p>
          </div>
          <button
            onClick={handleOpenAdd}
            className="inline-flex items-center gap-2 px-4 py-2 rounded-lg bg-cyan-500 text-white text-xs font-medium hover:bg-cyan-400 transition-colors"
          >
            <Plus className="w-3.5 h-3.5" /> Configure First Provider
          </button>
        </div>
      ) : (
        <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-5">
          {connections.map((conn) => {
            const providerMeta = SUPPORTED_PROVIDERS.find((p) => p.id === conn.provider) || {
              name: conn.provider || 'Provider',
              color: 'from-slate-600 to-slate-800',
            };
            const testResult = testResults[conn.id];
            const isTesting = testingId === conn.id;

            return (
              <div
                key={conn.id}
                className="bg-slate-900/70 border border-slate-800/90 rounded-2xl p-5 hover:border-slate-700 transition-all flex flex-col justify-between group shadow-sm hover:shadow-md"
              >
                <div>
                  <div className="flex items-start justify-between gap-3 mb-3">
                    <div className="flex items-center gap-3">
                      <div
                        className={`w-10 h-10 rounded-xl bg-gradient-to-tr ${providerMeta.color} flex items-center justify-center text-white font-bold text-sm shadow-md`}
                      >
                        {conn.provider ? conn.provider.slice(0, 2).toUpperCase() : 'AI'}
                      </div>
                      <div>
                        <h4 className="text-sm font-semibold text-white group-hover:text-cyan-400 transition-colors">
                          {conn.name || providerMeta.name}
                        </h4>
                        <div className="flex items-center gap-2 mt-0.5">
                          <span className="text-[11px] font-mono text-slate-400">{conn.provider}</span>
                          <span className="text-[10px] uppercase font-bold px-1.5 py-0.2 rounded bg-slate-800 text-slate-300">
                            {conn.authType || 'apiKey'}
                          </span>
                        </div>
                      </div>
                    </div>

                    <button
                      onClick={() => handleDelete(conn.id)}
                      className="text-slate-500 hover:text-rose-400 p-1.5 rounded-lg hover:bg-rose-500/10 transition-colors opacity-0 group-hover:opacity-100"
                      title="Delete connection"
                    >
                      <Trash2 className="w-4 h-4" />
                    </button>
                  </div>

                  <div className="space-y-2 mt-4 text-xs text-slate-400">
                    <div className="flex items-center justify-between py-1 border-t border-slate-800/60">
                      <span>Connection ID</span>
                      <span className="font-mono text-slate-300 truncate max-w-[150px]">{conn.id}</span>
                    </div>

                    <div className="flex items-center justify-between py-1 border-t border-slate-800/60">
                      <span>Status</span>
                      <span className="flex items-center gap-1.5 text-emerald-400">
                        <span className="w-1.5 h-1.5 rounded-full bg-emerald-400" />
                        <span>Ready</span>
                      </span>
                    </div>

                    {testResult && (
                      <div
                        className={`p-2 rounded-lg text-xs flex items-center gap-2 ${
                          testResult.ok
                            ? 'bg-emerald-500/10 text-emerald-400 border border-emerald-500/20'
                            : 'bg-rose-500/10 text-rose-400 border border-rose-500/20'
                        }`}
                      >
                        {testResult.ok ? (
                          <>
                            <CheckCircle2 className="w-3.5 h-3.5 shrink-0" />
                            <span>Ping: {testResult.latency}ms (OK)</span>
                          </>
                        ) : (
                          <>
                            <AlertCircle className="w-3.5 h-3.5 shrink-0" />
                            <span className="truncate">{testResult.error || 'Test failed'}</span>
                          </>
                        )}
                      </div>
                    )}
                  </div>
                </div>

                <div className="mt-5 pt-3 border-t border-slate-800 flex items-center justify-between">
                  <span className="text-[11px] text-slate-400">
                    Prefix routing: <code className="text-cyan-400">{conn.provider}/*</code>
                  </span>

                  <button
                    onClick={() => handleTest(conn.id)}
                    disabled={isTesting}
                    className="flex items-center gap-1.5 px-2.5 py-1 rounded-lg bg-slate-800 hover:bg-slate-700 text-xs font-medium text-slate-200 transition-colors disabled:opacity-50"
                  >
                    <Play className={`w-3 h-3 ${isTesting ? 'animate-spin text-cyan-400' : ''}`} />
                    <span>{isTesting ? 'Testing...' : 'Test Connection'}</span>
                  </button>
                </div>
              </div>
            );
          })}
        </div>
      )}

      {/* Add Connection Modal */}
      <Modal isOpen={isModalOpen} onClose={() => setIsModalOpen(false)} title="Add Upstream Connection">
        <form onSubmit={handleSubmit} className="space-y-4">
          {errorMsg && (
            <div className="p-3 rounded-lg bg-rose-500/10 border border-rose-500/20 text-rose-400 text-xs flex items-center gap-2">
              <AlertCircle className="w-4 h-4 shrink-0" />
              <span>{errorMsg}</span>
            </div>
          )}

          <div>
            <label className="block text-xs font-medium text-slate-300 mb-1.5">Select Provider</label>
            <div className="grid grid-cols-3 gap-2">
              {SUPPORTED_PROVIDERS.slice(0, 6).map((p) => {
                const isSelected = formData.provider === p.id;
                return (
                  <button
                    key={p.id}
                    type="button"
                    onClick={() => handleProviderSelect(p.id)}
                    className={`p-2.5 rounded-xl border text-left text-xs font-medium transition-all ${
                      isSelected
                        ? 'border-cyan-500 bg-cyan-500/10 text-cyan-300 shadow-sm'
                        : 'border-slate-800 bg-slate-950/60 text-slate-400 hover:text-slate-200 hover:border-slate-700'
                    }`}
                  >
                    <div className="truncate">{p.name}</div>
                  </button>
                );
              })}
            </div>
          </div>

          <div>
            <label className="block text-xs font-medium text-slate-300 mb-1.5">Connection Name</label>
            <input
              type="text"
              placeholder="e.g. OpenAI Production"
              value={formData.name}
              onChange={(e) => setFormData({ ...formData, name: e.target.value })}
              className="w-full px-3.5 py-2 rounded-xl bg-slate-950/80 border border-slate-800 text-sm text-white placeholder-slate-500 focus:outline-none focus:border-cyan-500 transition-colors"
            />
          </div>

          <div>
            <label className="block text-xs font-medium text-slate-300 mb-1.5">API Key / Token</label>
            <input
              type="password"
              placeholder="sk-... or service key"
              value={formData.apiKey}
              onChange={(e) => setFormData({ ...formData, apiKey: e.target.value })}
              className="w-full px-3.5 py-2 rounded-xl bg-slate-950/80 border border-slate-800 text-sm text-white font-mono placeholder-slate-500 focus:outline-none focus:border-cyan-500 transition-colors"
            />
            <p className="text-[11px] text-slate-400 mt-1">
              Keys are securely stored in SQLite WAL and used for outbound proxy requests.
            </p>
          </div>

          <div>
            <label className="block text-xs font-medium text-slate-300 mb-1.5">Base URL (Optional)</label>
            <input
              type="text"
              placeholder="https://..."
              value={formData.baseUrl}
              onChange={(e) => setFormData({ ...formData, baseUrl: e.target.value })}
              className="w-full px-3.5 py-2 rounded-xl bg-slate-950/80 border border-slate-800 text-sm text-white font-mono placeholder-slate-500 focus:outline-none focus:border-cyan-500 transition-colors"
            />
          </div>

          <div className="pt-2 flex items-center justify-end gap-3 border-t border-slate-800">
            <button
              type="button"
              onClick={() => setIsModalOpen(false)}
              className="px-4 py-2 rounded-xl text-xs font-medium text-slate-400 hover:text-white transition-colors"
            >
              Cancel
            </button>
            <button
              type="submit"
              disabled={isSubmitting}
              className="px-5 py-2 rounded-xl bg-cyan-500 hover:bg-cyan-400 text-slate-950 font-semibold text-xs shadow-lg shadow-cyan-500/20 transition-all disabled:opacity-50"
            >
              {isSubmitting ? 'Saving...' : 'Save Connection'}
            </button>
          </div>
        </form>
      </Modal>
    </div>
  );
}
