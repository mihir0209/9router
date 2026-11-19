import React, { useState, useEffect } from 'react';
import {
  Key,
  Plus,
  Copy,
  Check,
  Trash2,
  ShieldCheck,
  AlertCircle,
  Eye,
  EyeOff,
  Sliders,
  Calendar,
} from 'lucide-react';
import Modal from '../components/Modal';
import { api } from '../api/client';

export default function ApiKeysPage({ onRefreshTrigger }) {
  const [keys, setKeys] = useState([]);
  const [loading, setLoading] = useState(true);
  const [copiedId, setCopiedId] = useState(null);
  const [isModalOpen, setIsModalOpen] = useState(false);
  const [newKeyResult, setNewKeyResult] = useState(null);

  // Form state
  const [formData, setFormData] = useState({
    name: '',
    key: '',
    rateLimitRequests: 60,
    rateLimitWindowSeconds: 60,
    allowedModels: '',
  });
  const [isSubmitting, setIsSubmitting] = useState(false);
  const [errorMsg, setErrorMsg] = useState('');

  const loadData = async () => {
    setLoading(true);
    try {
      const data = await api.getKeys();
      setKeys(data);
    } catch (err) {
      console.error(err);
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    loadData();
  }, [onRefreshTrigger]);

  const copyToClipboard = (text, id) => {
    navigator.clipboard.writeText(text);
    setCopiedId(id);
    setTimeout(() => setCopiedId(null), 2000);
  };

  const handleDelete = async (id) => {
    if (!window.confirm('Are you sure you want to revoke this API key? Applications using this key will immediately be blocked.')) return;
    try {
      await api.deleteKey(id);
      setKeys((prev) => prev.filter((k) => k.id !== id));
    } catch (err) {
      alert(`Failed to delete key: ${err.message}`);
    }
  };

  const handleOpenCreate = () => {
    setFormData({
      name: '',
      key: '',
      rateLimitRequests: 60,
      rateLimitWindowSeconds: 60,
      allowedModels: '',
    });
    setErrorMsg('');
    setNewKeyResult(null);
    setIsModalOpen(true);
  };

  const handleSubmit = async (e) => {
    e.preventDefault();
    if (!formData.name.trim()) {
      setErrorMsg('Key name is required');
      return;
    }

    setIsSubmitting(true);
    setErrorMsg('');
    try {
      const res = await api.createKey({
        name: formData.name,
        key: formData.key.trim() || undefined,
        rateLimitRequests: Number(formData.rateLimitRequests) || undefined,
        rateLimitWindowSeconds: Number(formData.rateLimitWindowSeconds) || 60,
        allowedModels: formData.allowedModels.trim() ? formData.allowedModels.split(',').map((s) => s.trim()) : undefined,
      });

      const createdKey = res.key || res;
      setNewKeyResult(createdKey);
      await loadData();
    } catch (err) {
      setErrorMsg(err.message || 'Failed to generate key');
    } finally {
      setIsSubmitting(false);
    }
  };

  return (
    <div className="p-8 max-w-7xl mx-auto space-y-6">
      {/* Top Banner */}
      <div className="flex flex-col md:flex-row md:items-center justify-between gap-4 pb-2">
        <div>
          <h2 className="text-2xl font-bold text-white tracking-tight">Client API Keys</h2>
          <p className="text-sm text-slate-400 mt-1">
            Generate and manage authentication keys for client applications, IDEs, and agentic workflows.
          </p>
        </div>

        <button
          onClick={handleOpenCreate}
          className="flex items-center gap-2 px-4 py-2.5 rounded-xl bg-gradient-to-r from-cyan-500 to-blue-600 hover:from-cyan-400 hover:to-blue-500 text-white font-medium text-sm shadow-lg shadow-cyan-500/25 transition-all transform active:scale-95"
        >
          <Plus className="w-4 h-4" />
          <span>Generate New Key</span>
        </button>
      </div>

      {/* Newly Created Key Alert */}
      {newKeyResult && (
        <div className="p-4 rounded-2xl bg-cyan-500/10 border border-cyan-500/30 space-y-2 animate-fade-in">
          <div className="flex items-center justify-between">
            <span className="text-xs font-semibold uppercase tracking-wider text-cyan-400 flex items-center gap-1.5">
              <ShieldCheck className="w-4 h-4" /> API Key Created Successfully
            </span>
            <button
              onClick={() => setNewKeyResult(null)}
              className="text-xs text-slate-400 hover:text-white"
            >
              Dismiss
            </button>
          </div>
          <p className="text-xs text-slate-300">
            Please copy this key now. For your security, the raw key will not be shown again:
          </p>
          <div className="flex items-center gap-2">
            <input
              readOnly
              value={newKeyResult.key || '9r-live-generated-key'}
              className="flex-1 bg-slate-950 font-mono text-sm px-3.5 py-2 rounded-xl text-cyan-300 border border-cyan-500/40 select-all"
            />
            <button
              onClick={() => copyToClipboard(newKeyResult.key || '9r-live-generated-key', 'newly-created')}
              className="px-4 py-2 rounded-xl bg-cyan-500 hover:bg-cyan-400 text-slate-950 font-semibold text-xs flex items-center gap-1.5 transition-colors"
            >
              {copiedId === 'newly-created' ? (
                <>
                  <Check className="w-3.5 h-3.5" /> Copied!
                </>
              ) : (
                <>
                  <Copy className="w-3.5 h-3.5" /> Copy Key
                </>
              )}
            </button>
          </div>
        </div>
      )}

      {/* Keys Table / List */}
      <div className="bg-slate-900/60 border border-slate-800 rounded-2xl overflow-hidden shadow-sm">
        <div className="p-4 border-b border-slate-800 flex items-center justify-between">
          <div className="flex items-center gap-2">
            <Key className="w-4 h-4 text-cyan-400" />
            <h3 className="text-sm font-semibold text-white">Active Access Keys ({keys.length})</h3>
          </div>
          <span className="text-xs text-slate-400">Axum Bearer & X-API-Key Supported</span>
        </div>

        {loading ? (
          <div className="p-8 space-y-4">
            {[1, 2, 3].map((i) => (
              <div key={i} className="h-12 bg-slate-800/40 rounded-xl animate-pulse" />
            ))}
          </div>
        ) : keys.length === 0 ? (
          <div className="p-12 text-center text-slate-400 space-y-3">
            <Key className="w-8 h-8 mx-auto text-slate-600" />
            <p className="text-sm">No API keys generated yet.</p>
            <button
              onClick={handleOpenCreate}
              className="text-xs text-cyan-400 hover:text-cyan-300 underline"
            >
              Generate your first key
            </button>
          </div>
        ) : (
          <div className="overflow-x-auto">
            <table className="w-full text-left text-xs">
              <thead className="bg-slate-950/50 text-slate-400 font-medium border-b border-slate-800">
                <tr>
                  <th className="py-3 px-4">Name / Purpose</th>
                  <th className="py-3 px-4">Token Preview</th>
                  <th className="py-3 px-4">Rate Limits</th>
                  <th className="py-3 px-4">Allowed Models</th>
                  <th className="py-3 px-4">Created</th>
                  <th className="py-3 px-4 text-right">Actions</th>
                </tr>
              </thead>
              <tbody className="divide-y divide-slate-800/60">
                {keys.map((k) => {
                  const keyDisplay = k.maskedKey || (k.key ? `${k.key.slice(0, 7)}...${k.key.slice(-4)}` : '9r-****-****');
                  const isCopied = copiedId === k.id;
                  return (
                    <tr key={k.id} className="hover:bg-slate-800/30 transition-colors group">
                      <td className="py-3.5 px-4">
                        <div className="font-medium text-white">{k.name}</div>
                        <div className="text-[11px] text-slate-400 font-mono mt-0.5">{k.id}</div>
                      </td>

                      <td className="py-3.5 px-4 font-mono">
                        <div className="flex items-center gap-2">
                          <span className="bg-slate-950 px-2 py-1 rounded border border-slate-800 text-slate-300">
                            {keyDisplay}
                          </span>
                          <button
                            onClick={() => copyToClipboard(k.key || keyDisplay, k.id)}
                            className="p-1 text-slate-400 hover:text-white rounded hover:bg-slate-800 transition-colors"
                            title="Copy key"
                          >
                            {isCopied ? <Check className="w-3.5 h-3.5 text-emerald-400" /> : <Copy className="w-3.5 h-3.5" />}
                          </button>
                        </div>
                      </td>

                      <td className="py-3.5 px-4 text-slate-300">
                        {k.rateLimitRequests ? (
                          <span>
                            <span className="font-semibold text-white">{k.rateLimitRequests}</span> req /{' '}
                            {k.rateLimitWindowSeconds || 60}s
                          </span>
                        ) : (
                          <span className="text-slate-400">Unlimited</span>
                        )}
                      </td>

                      <td className="py-3.5 px-4 text-slate-400">
                        {k.allowedModels ? (
                          <span className="truncate max-w-[140px] inline-block font-mono text-[11px]" title={k.allowedModels}>
                            {k.allowedModels}
                          </span>
                        ) : (
                          <span className="text-slate-400">All models (*)</span>
                        )}
                      </td>

                      <td className="py-3.5 px-4 text-slate-400">
                        {k.createdAt ? new Date(k.createdAt).toLocaleDateString() : 'Active'}
                      </td>

                      <td className="py-3.5 px-4 text-right">
                        <button
                          onClick={() => handleDelete(k.id)}
                          className="p-1.5 text-slate-500 hover:text-rose-400 rounded-lg hover:bg-rose-500/10 transition-colors"
                          title="Revoke key"
                        >
                          <Trash2 className="w-4 h-4" />
                        </button>
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        )}
      </div>

      {/* Create Key Modal */}
      <Modal isOpen={isModalOpen} onClose={() => setIsModalOpen(false)} title="Generate New Client API Key">
        <form onSubmit={handleSubmit} className="space-y-4">
          {errorMsg && (
            <div className="p-3 rounded-lg bg-rose-500/10 border border-rose-500/20 text-rose-400 text-xs flex items-center gap-2">
              <AlertCircle className="w-4 h-4 shrink-0" />
              <span>{errorMsg}</span>
            </div>
          )}

          <div>
            <label className="block text-xs font-medium text-slate-300 mb-1.5">Key Name / Client ID</label>
            <input
              type="text"
              placeholder="e.g. Next.js Web App, Cursor Agent, CLI"
              value={formData.name}
              onChange={(e) => setFormData({ ...formData, name: e.target.value })}
              className="w-full px-3.5 py-2 rounded-xl bg-slate-950/80 border border-slate-800 text-sm text-white placeholder-slate-500 focus:outline-none focus:border-cyan-500 transition-colors"
            />
          </div>

          <div>
            <label className="block text-xs font-medium text-slate-300 mb-1.5">Custom Key String (Optional)</label>
            <input
              type="text"
              placeholder="Leave blank to auto-generate 9r-live-..."
              value={formData.key}
              onChange={(e) => setFormData({ ...formData, key: e.target.value })}
              className="w-full px-3.5 py-2 rounded-xl bg-slate-950/80 border border-slate-800 text-sm text-white font-mono placeholder-slate-500 focus:outline-none focus:border-cyan-500 transition-colors"
            />
          </div>

          <div className="grid grid-cols-2 gap-3">
            <div>
              <label className="block text-xs font-medium text-slate-300 mb-1.5">Max Requests</label>
              <input
                type="number"
                placeholder="60"
                value={formData.rateLimitRequests}
                onChange={(e) => setFormData({ ...formData, rateLimitRequests: e.target.value })}
                className="w-full px-3.5 py-2 rounded-xl bg-slate-950/80 border border-slate-800 text-sm text-white focus:outline-none focus:border-cyan-500 transition-colors"
              />
            </div>
            <div>
              <label className="block text-xs font-medium text-slate-300 mb-1.5">Window (Seconds)</label>
              <input
                type="number"
                placeholder="60"
                value={formData.rateLimitWindowSeconds}
                onChange={(e) => setFormData({ ...formData, rateLimitWindowSeconds: e.target.value })}
                className="w-full px-3.5 py-2 rounded-xl bg-slate-950/80 border border-slate-800 text-sm text-white focus:outline-none focus:border-cyan-500 transition-colors"
              />
            </div>
          </div>

          <div>
            <label className="block text-xs font-medium text-slate-300 mb-1.5">Allowed Models (Optional)</label>
            <input
              type="text"
              placeholder="gpt-4o, claude-3-5-sonnet (or leave empty for all)"
              value={formData.allowedModels}
              onChange={(e) => setFormData({ ...formData, allowedModels: e.target.value })}
              className="w-full px-3.5 py-2 rounded-xl bg-slate-950/80 border border-slate-800 text-sm text-white placeholder-slate-500 focus:outline-none focus:border-cyan-500 transition-colors"
            />
            <p className="text-[11px] text-slate-400 mt-1">Comma-separated list of allowed models</p>
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
              {isSubmitting ? 'Creating...' : 'Create Key'}
            </button>
          </div>
        </form>
      </Modal>
    </div>
  );
}
