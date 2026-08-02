"use client";

import { useCallback, useEffect, useRef, useState } from "react";

const HISTORY_KEY = "playground.history";

const ENDPOINTS = [
  { value: "chat/completions", label: "Chat Completions", defaultBody: { messages: [{ role: "user", content: "Hello!" }], stream: false } },
  { value: "responses", label: "Responses", defaultBody: { input: "Hello!", stream: false } },
  { value: "completions", label: "Completions", defaultBody: { prompt: "Hello!", max_tokens: 100 } },
  { value: "embeddings", label: "Embeddings", defaultBody: { input: "Hello world" } },
  { value: "images/generations", label: "Image Generation", defaultBody: { prompt: "A sunset over mountains", n: 1, size: "1024x1024" } },
  { value: "audio/speech", label: "Text-to-Speech", defaultBody: { model: "tts-1", input: "Hello world", voice: "alloy" } },
  { value: "audio/transcriptions", label: "Audio Transcription", defaultBody: {} },
  { value: "moderations", label: "Moderations", defaultBody: { input: "Hello world" } },
];

function safeParse(str, fallback) {
  try { return JSON.parse(str); } catch { return fallback; }
}

function formatMs(ms) {
  if (ms < 1000) return `${ms}ms`;
  return `${(ms / 1000).toFixed(2)}s`;
}

function ModelCombobox({ models, value, onChange }) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const inputRef = useRef(null);
  const listRef = useRef(null);

  const filtered = models.filter(m =>
    m.id.toLowerCase().includes(query.toLowerCase())
  );

  useEffect(() => {
    if (open) {
      setQuery("");
      setTimeout(() => inputRef.current?.focus(), 0);
    }
  }, [open]);

  useEffect(() => {
    if (!open) return;
    const handler = (e) => {
      if (listRef.current && !listRef.current.contains(e.target) && !inputRef.current?.contains(e.target)) {
        setOpen(false);
      }
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [open]);

  if (!open) {
    return (
      <button onClick={() => setOpen(true)}
        className="w-full rounded-lg border border-neutral-700 bg-neutral-900 px-3 py-2 text-sm text-left flex items-center justify-between">
        <span className="truncate">{value || "Select model..."}</span>
        <span className="material-symbols-outlined text-[18px] ml-1">expand_more</span>
      </button>
    );
  }

  return (
    <div className="relative" ref={listRef}>
      <div className="flex items-center rounded-lg border border-blue-500 bg-neutral-900 px-2">
        <span className="material-symbols-outlined text-[18px] text-neutral-400">search</span>
        <input ref={inputRef} value={query} onChange={e => setQuery(e.target.value)}
          placeholder="Type to search..."
          className="flex-1 bg-transparent px-2 py-2 text-sm outline-none" />
        <button onClick={() => setOpen(false)} className="text-neutral-400 hover:text-white">
          <span className="material-symbols-outlined text-[18px]">close</span>
        </button>
      </div>
      <div className="absolute z-50 mt-1 w-full max-h-60 overflow-auto rounded-lg border border-neutral-700 bg-neutral-900 shadow-xl">
        {filtered.length === 0 ? (
          <div className="px-3 py-2 text-sm text-neutral-500">No models found</div>
        ) : (
          filtered.map(m => (
            <button key={m.id} onClick={() => { onChange(m.id); setOpen(false); }}
              className={`w-full text-left px-3 py-2 text-sm hover:bg-neutral-800 ${m.id === value ? "bg-neutral-800 text-blue-400" : "text-neutral-200"}`}>
              {m.id}
            </button>
          ))
        )}
      </div>
    </div>
  );
}

export default function PlaygroundClient() {
  const [host, setHost] = useState("");
  const [apiKeys, setApiKeys] = useState([]);
  const [selectedKey, setSelectedKey] = useState("");
  const [models, setModels] = useState([]);
  const [selectedModel, setSelectedModel] = useState("");
  const [selectedEndpoint, setSelectedEndpoint] = useState("chat/completions");
  const [requestBody, setRequestBody] = useState("");
  const [response, setResponse] = useState(null);
  const [activeTab, setActiveTab] = useState("output");
  const [loading, setLoading] = useState(false);
  const [history, setHistory] = useState([]);
  const abortRef = useRef(null);

  useEffect(() => {
    setHost(window.location.origin);
    setHistory(safeParse(localStorage.getItem(HISTORY_KEY), []));
  }, []);

  useEffect(() => {
    fetch("/api/keys")
      .then(r => r.json())
      .then(data => {
        const keys = Array.isArray(data) ? data : data?.keys || [];
        setApiKeys(keys.filter(k => k.isActive !== false));
        if (keys.length > 0 && !selectedKey) setSelectedKey(keys[0].key);
      })
      .catch(() => {});
  }, []);

  useEffect(() => {
    if (!selectedKey) return;
    fetch("/v1/models", { headers: { Authorization: `Bearer ${selectedKey}` } })
      .then(r => r.json())
      .then(data => {
        const list = data?.data || [];
        setModels(list);
        if (list.length > 0 && !selectedModel) setSelectedModel(list[0].id);
      })
      .catch(() => {});
  }, [selectedKey]);

  useEffect(() => {
    if (!selectedModel) return;
    const ep = ENDPOINTS.find(e => e.value === selectedEndpoint);
    setRequestBody(JSON.stringify({
      model: selectedModel,
      ...ep.defaultBody,
    }, null, 2));
  }, [selectedModel, selectedEndpoint]);

  const sendRequest = useCallback(async () => {
    if (loading) {
      abortRef.current?.abort();
      setLoading(false);
      return;
    }
    let body;
    try { body = JSON.parse(requestBody); } catch { setResponse({ error: "Invalid JSON" }); return; }

    setLoading(true);
    setResponse(null);
    setActiveTab("output");
    const t0 = performance.now();
    abortRef.current = new AbortController();

    try {
      const res = await fetch(`${host}/v1/${selectedEndpoint}`, {
        method: "POST",
        headers: { "Content-Type": "application/json", Authorization: `Bearer ${selectedKey}` },
        body: JSON.stringify(body),
        signal: abortRef.current.signal,
      });
      const elapsed = Math.round(performance.now() - t0);
      const text = await res.text();
      let parsed;
      try { parsed = JSON.parse(text); } catch { parsed = text; }
      const entry = { status: res.status, elapsed, body: parsed, model: body.model || selectedEndpoint, ts: Date.now() };
      setResponse(entry);
      setHistory(prev => {
        const next = [entry, ...prev].slice(0, 50);
        localStorage.setItem(HISTORY_KEY, JSON.stringify(next));
        return next;
      });
    } catch (err) {
      if (err.name !== "AbortError") {
        setResponse({ error: err.message, elapsed: Math.round(performance.now() - t0) });
      }
    } finally {
      setLoading(false);
    }
  }, [host, selectedKey, selectedEndpoint, requestBody, loading]);

  const endpointLabel = ENDPOINTS.find(e => e.value === selectedEndpoint)?.label || selectedEndpoint;

  return (
    <div className="max-w-6xl mx-auto p-6 space-y-4">
      <h1 className="text-2xl font-bold">API Playground</h1>

      <div className="grid grid-cols-3 gap-4">
        <div>
          <label className="block text-sm font-medium mb-1">API Key</label>
          <select value={selectedKey} onChange={e => setSelectedKey(e.target.value)}
            className="w-full rounded-lg border border-neutral-700 bg-neutral-900 px-3 py-2 text-sm">
            {apiKeys.map(k => (
              <option key={k.id} value={k.key}>{k.name || k.key.slice(0, 12) + "..."}</option>
            ))}
          </select>
        </div>
        <div>
          <label className="block text-sm font-medium mb-1">Model</label>
          <ModelCombobox models={models} value={selectedModel} onChange={setSelectedModel} />
        </div>
        <div>
          <label className="block text-sm font-medium mb-1">Endpoint</label>
          <select value={selectedEndpoint} onChange={e => setSelectedEndpoint(e.target.value)}
            className="w-full rounded-lg border border-neutral-700 bg-neutral-900 px-3 py-2 text-sm">
            {ENDPOINTS.map(ep => (
              <option key={ep.value} value={ep.value}>{ep.label}</option>
            ))}
          </select>
        </div>
      </div>

      <div>
        <label className="block text-sm font-medium mb-1">Request URL</label>
        <input readOnly value={`${host}/v1/${selectedEndpoint}`}
          className="w-full rounded-lg border border-neutral-700 bg-neutral-800 px-3 py-2 text-sm text-neutral-400" />
      </div>

      <div>
        <label className="block text-sm font-medium mb-1">Request Body</label>
        <textarea value={requestBody} onChange={e => setRequestBody(e.target.value)} rows={12}
          className="w-full rounded-lg border border-neutral-700 bg-neutral-900 px-3 py-2 text-sm font-mono" />
      </div>

      <button onClick={sendRequest}
        className={`px-6 py-2 rounded-lg font-medium text-sm ${loading ? "bg-red-600 hover:bg-red-700" : "bg-brand-500 hover:bg-brand-600"} text-white`}>
        {loading ? "Stop" : "Send Request"}
      </button>

      {response && (
        <div className="rounded-lg border border-neutral-700 overflow-hidden">
          <div className="flex border-b border-neutral-700">
            {["output", "raw", "timing"].map(tab => (
              <button key={tab} onClick={() => setActiveTab(tab)}
                className={`px-4 py-2 text-sm font-medium capitalize ${activeTab === tab ? "bg-neutral-800 text-white" : "text-neutral-400 hover:text-white"}`}>
                {tab}
              </button>
            ))}
          </div>
          <div className="p-4 bg-neutral-900 max-h-96 overflow-auto">
            {activeTab === "output" && (
              <pre className="text-sm whitespace-pre-wrap">
                {typeof response.body === "string" ? response.body : JSON.stringify(response.body, null, 2)}
              </pre>
            )}
            {activeTab === "raw" && (
              <pre className="text-sm whitespace-pre-wrap">{JSON.stringify(response, null, 2)}</pre>
            )}
            {activeTab === "timing" && (
              <div className="space-y-2 text-sm">
                <div>Status: <span className={response.error ? "text-red-400" : "text-green-400"}>{response.error ? "Error" : response.status}</span></div>
                <div>Time: {formatMs(response.elapsed)}</div>
                {response.body?.usage && (
                  <>
                    <div>Prompt tokens: {response.body.usage.prompt_tokens}</div>
                    <div>Completion tokens: {response.body.usage.completion_tokens}</div>
                    <div>Total tokens: {response.body.usage.total_tokens}</div>
                  </>
                )}
              </div>
            )}
          </div>
        </div>
      )}

      {history.length > 0 && (
        <div>
          <h2 className="text-lg font-semibold mb-2">History</h2>
          <div className="space-y-2">
            {history.map((h, i) => (
              <div key={i} className="flex items-center gap-3 text-sm p-2 rounded-lg bg-neutral-800/50">
                <span className={h.error ? "text-red-400" : "text-green-400"}>{h.error ? "ERR" : h.status}</span>
                <span className="text-neutral-400">{h.model}</span>
                <span className="text-neutral-500">{formatMs(h.elapsed)}</span>
                <span className="text-neutral-600 text-xs">{new Date(h.ts).toLocaleTimeString()}</span>
              </div>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}
