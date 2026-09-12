"use client";

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { cn } from "@/shared/utils/cn";
import { useCopyToClipboard } from "@/shared/hooks/useCopyToClipboard";

const HISTORY_KEY = "playground.history";

const ENDPOINTS = [
  { value: "chat/completions", label: "Chat Completions", category: "LLM", method: "POST" },
  { value: "responses", label: "Responses API", category: "LLM", method: "POST" },
  { value: "completions", label: "Legacy Completions", category: "LLM", method: "POST" },
  { value: "embeddings", label: "Embeddings", category: "Media", method: "POST" },
  { value: "images/generations", label: "Image Generation", category: "Media", method: "POST" },
  { value: "audio/speech", label: "Text-to-Speech (TTS)", category: "Media", method: "POST" },
  { value: "audio/transcriptions", label: "Transcriptions (STT)", category: "Media", method: "POST" },
  { value: "moderations", label: "Moderations", category: "Utility", method: "POST" },
];

const ROLES = ["system", "user", "assistant", "developer"];

const TTS_VOICES = ["alloy", "ash", "coral", "echo", "fable", "onyx", "nova", "sage", "shimmer"];
const IMAGE_SIZES = ["1024x1024", "512x512", "256x256", "1792x1024", "1024x1792"];
const IMAGE_QUALITIES = ["standard", "hd"];
const REASONING_EFFORTS = ["none", "low", "medium", "high"];

const CAP_FILTERS = [
  { id: "all", label: "All" },
  { id: "vision", label: "Vision" },
  { id: "reasoning", label: "Reasoning" },
  { id: "search", label: "Search" },
];

function formatContextWindow(n) {
  if (!n || n <= 0) return null;
  if (n >= 1000000) return `${n % 1000000 === 0 ? n / 1000000 : (n / 1000000).toFixed(1)}M`;
  if (n >= 1000) return `${Math.round(n / 1000)}K`;
  return String(n);
}

function CapabilityBadges({ caps, className }) {
  if (!caps) return null;
  const badges = [
    caps.vision && { key: "V", title: "Vision", cls: "bg-blue-500/15 text-blue-400" },
    caps.reasoning && { key: "R", title: "Reasoning", cls: "bg-purple-500/15 text-purple-400" },
    caps.search && { key: "S", title: "Search", cls: "bg-amber-500/15 text-amber-500" },
  ].filter(Boolean);
  const ctx = formatContextWindow(caps.contextWindow);
  if (badges.length === 0 && !ctx) return null;
  return (
    <span className={cn("flex shrink-0 items-center gap-1", className)}>
      {badges.map((b) => (
        <span key={b.key} title={b.title} className={cn("rounded px-1 py-0.5 text-[9px] font-bold leading-none", b.cls)}>
          {b.key}
        </span>
      ))}
      {ctx && (
        <span title="Context window" className="rounded bg-surface-3 px-1 py-0.5 font-mono text-[9px] leading-none text-text-muted">
          {ctx}
        </span>
      )}
    </span>
  );
}

function safeParse(str, fallback) {
  try {
    const parsed = JSON.parse(str);
    return parsed ?? fallback;
  } catch {
    return fallback;
  }
}

function safeParseArray(str) {
  const parsed = safeParse(str, []);
  return Array.isArray(parsed) ? parsed : [];
}

function formatMs(ms) {
  if (ms < 1000) return `${ms}ms`;
  return `${(ms / 1000).toFixed(2)}s`;
}

function ModelCombobox({ models, value, onChange, capsMap = {}, capFilter = "all", onCapFilterChange, placeholder = "Select model..." }) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const inputRef = useRef(null);
  const listRef = useRef(null);

  const matchesCap = useCallback(
    (m) => {
      if (capFilter === "all") return true;
      const caps = capsMap[m.id];
      return !!caps?.[capFilter];
    },
    [capFilter, capsMap]
  );

  const filtered = useMemo(() => {
    let list = models;
    if (capFilter !== "all") list = list.filter(matchesCap);
    if (query) {
      const q = query.toLowerCase();
      list = list.filter((m) => m.id.toLowerCase().includes(q) || (m.owned_by && m.owned_by.toLowerCase().includes(q)));
    }
    return list;
  }, [models, query, capFilter, matchesCap]);

  // Group by provider prefix ("cc/claude-sonnet-5" -> "cc"), models without a
  // prefix land in "Other". Groups and models are sorted alphabetically.
  const groups = useMemo(() => {
    const byProvider = new Map();
    for (const m of filtered) {
      const idx = m.id.indexOf("/");
      const provider = idx > 0 ? m.id.slice(0, idx) : "";
      if (!byProvider.has(provider)) byProvider.set(provider, []);
      byProvider.get(provider).push(m);
    }
    return [...byProvider.entries()]
      .sort((a, b) => {
        if (!a[0]) return 1;
        if (!b[0]) return -1;
        return a[0].localeCompare(b[0]);
      })
      .map(([provider, items]) => [
        provider,
        items.sort((a, b) => a.id.slice(a.id.indexOf("/") + 1).localeCompare(b.id.slice(b.id.indexOf("/") + 1))),
      ]);
  }, [filtered]);

  const capCounts = useMemo(() => {
    const counts = {};
    for (const m of models) {
      const caps = capsMap[m.id];
      if (!caps) continue;
      for (const key of ["vision", "reasoning", "search"]) {
        if (caps[key]) counts[key] = (counts[key] || 0) + 1;
      }
    }
    return counts;
  }, [models, capsMap]);

  useEffect(() => {
    if (open) {
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

  return (
    <div className="relative" ref={listRef}>
      <button
        type="button"
        onClick={() => {
          setQuery("");
          setOpen(true);
        }}
        className="flex w-full items-center justify-between gap-2 rounded-lg border border-border-subtle bg-surface px-3 py-2 text-left text-xs transition-colors hover:border-border hover:bg-surface-2"
      >
        <span className="flex min-w-0 items-center gap-1.5">
          <span className="truncate font-mono text-text-main">{value || placeholder}</span>
          <CapabilityBadges caps={value ? capsMap[value] : null} />
        </span>
        <span className="material-symbols-outlined shrink-0 text-[18px] text-text-muted">expand_more</span>
      </button>

      {open && (
        <div className="absolute z-50 mt-1 w-full overflow-hidden rounded-lg border border-border-subtle bg-surface-2 shadow-xl">
          <div className="flex items-center border-b border-border-subtle bg-surface px-2">
            <span className="material-symbols-outlined text-[18px] text-text-muted">search</span>
            <input
              ref={inputRef}
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder="Search models..."
              className="flex-1 bg-transparent px-2 py-2 text-xs font-mono text-text-main outline-none placeholder:text-text-muted"
            />
            <button
              type="button"
              onClick={() => setOpen(false)}
              className="p-1 text-text-muted hover:text-text-main"
            >
              <span className="material-symbols-outlined text-[16px]">close</span>
            </button>
          </div>

          {Object.keys(capCounts).length > 0 && (
            <div className="flex flex-wrap items-center gap-1 border-b border-border-subtle bg-surface px-2 py-1.5">
              {CAP_FILTERS.map((cf) => {
                const isAll = cf.id === "all";
                if (!isAll && !capCounts[cf.id]) return null;
                return (
                  <button
                    key={cf.id}
                    type="button"
                    onClick={() => onCapFilterChange?.(cf.id)}
                    className={cn(
                      "rounded-full px-2.5 py-0.5 text-[10px] font-semibold transition-colors",
                      capFilter === cf.id
                        ? "bg-primary text-white"
                        : "bg-surface-3 text-text-muted hover:text-text-main"
                    )}
                  >
                    {cf.label}
                    {!isAll && <span className="ml-1 opacity-60">{capCounts[cf.id]}</span>}
                  </button>
                );
              })}
            </div>
          )}

          <div className="max-h-72 overflow-auto p-1">
            {groups.length === 0 ? (
              <div className="px-3 py-2 text-xs text-text-muted">No matching models</div>
            ) : (
              groups.map(([provider, items]) => (
                <div key={provider || "other"} className="mb-1">
                  <div className="sticky top-0 bg-surface-2/95 px-2 py-1 text-[10px] font-bold uppercase tracking-wider text-text-muted backdrop-blur-sm">
                    {provider || "Other"}
                    <span className="ml-1 font-normal opacity-60">{items.length}</span>
                  </div>
                  {items.map((m) => (
                    <button
                      key={m.id}
                      type="button"
                      onClick={() => {
                        onChange(m.id);
                        setOpen(false);
                      }}
                      className={cn(
                        "flex w-full items-center justify-between gap-2 rounded px-2.5 py-1.5 text-left text-xs font-mono transition-colors",
                        m.id === value ? "bg-primary/10 text-primary" : "text-text-main hover:bg-surface-3"
                      )}
                    >
                      <span className="truncate">{m.id.includes("/") ? m.id.slice(m.id.indexOf("/") + 1) : m.id}</span>
                      <span className="flex shrink-0 items-center gap-1.5">
                        <CapabilityBadges caps={capsMap[m.id]} />
                        {m.owned_by && m.id !== value && (
                          <span className="rounded bg-surface px-1.5 py-0.5 text-[9px] text-text-muted">{m.owned_by}</span>
                        )}
                      </span>
                    </button>
                  ))}
                </div>
              ))
            )}
          </div>
        </div>
      )}
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

  // Mode: "structured" (friendly UI controls) vs "json" (raw request body editor)
  const [editorMode, setEditorMode] = useState("structured");

  // Chat structured state
  const [messages, setMessages] = useState([
    { role: "system", content: "You are a helpful, concise AI assistant." },
    { role: "user", content: "Hello! What can you do?" },
  ]);
  const [stream, setStream] = useState(false);
  const [temperature, setTemperature] = useState(0.7);
  const [topP, setTopP] = useState(1.0);
  const [maxTokens, setMaxTokens] = useState("");
  const [reasoningEffort, setReasoningEffort] = useState("medium");
  const [enableReasoning, setEnableReasoning] = useState(false);
  const [jsonMode, setJsonMode] = useState(false);
  const [seed, setSeed] = useState("");
  const [customHeaders, setCustomHeaders] = useState("");

  // Responses API structured state
  const [responsesInput, setResponsesInput] = useState("Hello! Write a brief haiku about code.");
  const [responsesInstructions, setResponsesInstructions] = useState("");

  // Completions state
  const [completionsPrompt, setCompletionsPrompt] = useState("Once upon a time in software engineering");

  // Embeddings state
  const [embeddingsInput, setEmbeddingsInput] = useState("The quick brown fox jumps over the lazy dog");

  // Image generation state
  const [imagePrompt, setImagePrompt] = useState("A serene cyberpunk garden at night, digital art, highly detailed");
  const [imageSize, setImageSize] = useState("1024x1024");
  const [imageQuality, setImageQuality] = useState("standard");
  const [imageN, setImageN] = useState(1);

  // Audio TTS state
  const [ttsInput, setTtsInput] = useState("Welcome to 9Router, the unified gateway for all your AI models.");
  const [ttsVoice, setTtsVoice] = useState("alloy");
  const [ttsSpeed, setTtsSpeed] = useState(1.0);

  // Audio STT (transcriptions) state — sent as multipart/form-data, not JSON
  const [sttFile, setSttFile] = useState(null);
  const [sttModel, setSttModel] = useState("whisper-1");
  const [sttLanguage, setSttLanguage] = useState("");

  // Moderations state
  const [moderationsInput, setModerationsInput] = useState("I love writing code and building useful tools.");

  // Raw JSON editor text — only authoritative while editorMode === "json";
  // seeded from the structured controls when switching to the JSON tab.
  const [rawBodyText, setRawBodyText] = useState("");
  const [rawJsonError, setRawJsonError] = useState("");

  // Capability metadata per model id (vision/reasoning/search/contextWindow),
  // joined from /api/models which merges the daily models.dev catalog sync.
  const [modelCaps, setModelCaps] = useState({});
  const [capFilter, setCapFilter] = useState("all");

  // Execution & results
  const [loading, setLoading] = useState(false);
  const [streamingText, setStreamingText] = useState("");
  const [response, setResponse] = useState(null);
  const [activeTab, setActiveTab] = useState("output");
  const [history, setHistory] = useState([]);
  const abortRef = useRef(null);

  const { copied: curlCopied, copy: copyCurl } = useCopyToClipboard(2000);
  const { copied: outputCopied, copy: copyOutput } = useCopyToClipboard(2000);

  useEffect(() => {
    const loadClientState = () => {
      setHost(window.location.origin);
      setHistory(safeParseArray(localStorage.getItem(HISTORY_KEY)));
    };
    loadClientState();
  }, []);

  useEffect(() => {
    fetch("/api/keys")
      .then((r) => r.json())
      .then((data) => {
        const keys = Array.isArray(data) ? data : data?.keys || [];
        const active = keys.filter((k) => k.isActive !== false);
        setApiKeys(active);
        setSelectedKey((prev) => prev || (active.length > 0 ? active[0].key : prev));
      })
      .catch(() => {});
  }, []);

  useEffect(() => {
    if (!selectedKey) return;
    fetch("/v1/models", { headers: { Authorization: `Bearer ${selectedKey}` } })
      .then((r) => r.json())
      .then((data) => {
        const list = data?.data || [];
        setModels(list);
        setSelectedModel((prev) => prev || (list.length > 0 ? list[0].id : prev));
      })
      .catch(() => {});
  }, [selectedKey]);

  // Capability metadata for badges / filtering / sorting, keyed by routed model
  // id (same "provider/model" shape /v1/models exposes).
  useEffect(() => {
    fetch("/api/models")
      .then((r) => r.json())
      .then((data) => {
        const map = {};
        for (const m of data?.models || []) {
          if (m?.caps && (m.routedModel || m.fullModel)) {
            map[m.routedModel || m.fullModel] = m.caps;
          }
        }
        setModelCaps(map);
      })
      .catch(() => {});
  }, []);

  // Derive structured body from current control state
  const currentStructuredBody = useMemo(() => {
    const base = { model: selectedModel };
    switch (selectedEndpoint) {
      case "chat/completions": {
        const payload = {
          ...base,
          messages: messages.filter((m) => m.content.trim().length > 0),
          stream,
        };
        if (temperature !== 1.0) payload.temperature = temperature;
        if (topP !== 1.0) payload.top_p = topP;
        if (maxTokens && !isNaN(Number(maxTokens))) payload.max_tokens = Number(maxTokens);
        if (enableReasoning) payload.reasoning_effort = reasoningEffort;
        if (jsonMode) payload.response_format = { type: "json_object" };
        if (seed && !isNaN(Number(seed))) payload.seed = Number(seed);
        return payload;
      }
      case "responses": {
        const payload = {
          ...base,
          input: responsesInput,
          stream,
        };
        if (responsesInstructions) payload.instructions = responsesInstructions;
        if (enableReasoning) payload.reasoning_effort = reasoningEffort;
        if (maxTokens && !isNaN(Number(maxTokens))) payload.max_output_tokens = Number(maxTokens);
        return payload;
      }
      case "completions": {
        const payload = {
          ...base,
          prompt: completionsPrompt,
          stream,
        };
        if (temperature !== 1.0) payload.temperature = temperature;
        if (maxTokens && !isNaN(Number(maxTokens))) payload.max_tokens = Number(maxTokens);
        return payload;
      }
      case "embeddings":
        return { ...base, input: embeddingsInput };
      case "images/generations":
        return {
          ...base,
          prompt: imagePrompt,
          size: imageSize,
          quality: imageQuality,
          n: Number(imageN) || 1,
        };
      case "audio/speech":
        return {
          model: selectedModel || "tts-1",
          input: ttsInput,
          voice: ttsVoice,
          speed: ttsSpeed,
        };
      case "audio/transcriptions":
        return {
          model: sttModel || "whisper-1",
          ...(sttLanguage ? { language: sttLanguage } : {}),
          ...(sttFile ? { file: sttFile.name } : {}),
        };
      case "moderations":
        return { ...base, input: moderationsInput };
      default:
        return base;
    }
  }, [
    selectedEndpoint,
    selectedModel,
    messages,
    stream,
    temperature,
    topP,
    maxTokens,
    enableReasoning,
    reasoningEffort,
    jsonMode,
    seed,
    responsesInput,
    responsesInstructions,
    completionsPrompt,
    embeddingsInput,
    imagePrompt,
    imageSize,
    imageQuality,
    imageN,
    ttsInput,
    ttsVoice,
    ttsSpeed,
    sttFile,
    sttModel,
    sttLanguage,
    moderationsInput,
  ]);

  // Build cURL command string for 1-click copying
  const curlCommand = useMemo(() => {
    const url = `${host || "http://localhost:20128"}/v1/${selectedEndpoint}`;
    const auth = selectedKey ? ` \\\n  -H "Authorization: Bearer ${selectedKey}"` : "";
    if (selectedEndpoint === "audio/transcriptions") {
      const parts = [`curl -X POST ${url}${auth}`];
      parts.push(`-F "file=@${sttFile?.name || "audio.mp3"}"`);
      parts.push(`-F "model=${sttModel || "whisper-1"}"`);
      if (sttLanguage) parts.push(`-F "language=${sttLanguage}"`);
      return parts.join(" \\\n  ");
    }
    const bodyStr = editorMode === "json" ? rawBodyText : JSON.stringify(currentStructuredBody);
    const escaped = (bodyStr || "{}").replace(/'/g, "'\\''");
    return `curl -X POST ${url} \\\n  -H "Content-Type: application/json"${auth} \\\n  -d '${escaped}'`;
  }, [host, selectedEndpoint, selectedKey, editorMode, rawBodyText, currentStructuredBody, sttFile, sttModel, sttLanguage]);

  // Message helpers for Chat mode
  const updateMessage = (index, field, val) => {
    setMessages((prev) => {
      const next = [...prev];
      next[index] = { ...next[index], [field]: val };
      return next;
    });
  };
  const addMessage = (role = "user") => {
    setMessages((prev) => [...prev, { role, content: "" }]);
  };
  const removeMessage = (index) => {
    setMessages((prev) => prev.filter((_, i) => i !== index));
  };

  const sendRequest = useCallback(async () => {
    if (loading) {
      abortRef.current?.abort();
      setLoading(false);
      return;
    }

    let body;
    if (selectedEndpoint === "audio/transcriptions" && editorMode === "structured") {
      // Multipart upload — the browser sets the Content-Type boundary itself.
      body = new FormData();
      if (!sttFile) {
        setResponse({ error: "Select an audio file to transcribe first." });
        return;
      }
      body.append("file", sttFile);
      body.append("model", sttModel || "whisper-1");
      if (sttLanguage) body.append("language", sttLanguage);
    } else if (editorMode === "json") {
      try {
        body = JSON.parse(rawBodyText);
      } catch (e) {
        setRawJsonError(e.message);
        setResponse({ error: `Invalid JSON body: ${e.message}` });
        return;
      }
    } else {
      body = currentStructuredBody;
    }

    setLoading(true);
    setResponse(null);
    setStreamingText("");
    setActiveTab("output");
    const t0 = performance.now();
    abortRef.current = new AbortController();

    const isMultipart = typeof FormData !== "undefined" && body instanceof FormData;
    const headers = {
      ...(isMultipart ? {} : { "Content-Type": "application/json" }),
      ...(selectedKey ? { Authorization: `Bearer ${selectedKey}` } : {}),
    };

    if (customHeaders.trim()) {
      for (const line of customHeaders.split("\n")) {
        const idx = line.indexOf(":");
        if (idx > 0) {
          const k = line.slice(0, idx).trim();
          const v = line.slice(idx + 1).trim();
          if (k && v && k.toLowerCase() !== "content-type") headers[k] = v;
        }
      }
    }

    try {
      const res = await fetch(`${host}/v1/${selectedEndpoint}`, {
        method: "POST",
        headers,
        body: isMultipart ? body : JSON.stringify(body),
        signal: abortRef.current.signal,
      });

      const elapsed = Math.round(performance.now() - t0);
      const contentType = res.headers.get("content-type") || "";

      // Handle streaming SSE
      if (body.stream && res.ok && contentType.includes("event-stream") && res.body) {
        const reader = res.body.getReader();
        const decoder = new TextDecoder();
        let accumulated = "";
        let buffer = "";

        while (true) {
          const { done, value } = await reader.read();
          if (done) break;
          buffer += decoder.decode(value, { stream: true });
          const lines = buffer.split("\n");
          buffer = lines.pop() || "";

          for (const line of lines) {
            const trimmed = line.trim();
            if (!trimmed || trimmed.startsWith(":")) continue;
            if (trimmed === "data: [DONE]") continue;
            if (trimmed.startsWith("data: ")) {
              try {
                const parsed = JSON.parse(trimmed.slice(6));
                const delta =
                  parsed?.choices?.[0]?.delta?.content ||
                  parsed?.choices?.[0]?.text ||
                  parsed?.delta?.text ||
                  "";
                accumulated += delta;
                setStreamingText(accumulated);
              } catch {}
            }
          }
        }

        const entry = {
          status: res.status,
          elapsed: Math.round(performance.now() - t0),
          body: accumulated || "(Stream ended with no text content)",
          isStream: true,
          model: (isMultipart ? body.get("model") : body?.model) || selectedEndpoint,
          ts: Date.now(),
        };
        setResponse(entry);
        setHistory((prev) => {
          const next = [entry, ...(Array.isArray(prev) ? prev : [])].slice(0, 50);
          localStorage.setItem(HISTORY_KEY, JSON.stringify(next));
          return next;
        });
        return;
      }

      // Handle binary responses (e.g. audio/speech MP3/WAV)
      if (contentType.includes("audio/") || contentType.includes("application/octet-stream")) {
        const blob = await res.blob();
        const audioUrl = URL.createObjectURL(blob);
        const entry = {
          status: res.status,
          elapsed,
          audioUrl,
          body: { message: "Audio received", sizeBytes: blob.size, mimeType: contentType },
          model: (isMultipart ? body.get("model") : body?.model) || selectedEndpoint,
          ts: Date.now(),
        };
        setResponse(entry);
        return;
      }

      const text = await res.text();
      let parsed;
      try {
        parsed = JSON.parse(text);
      } catch {
        parsed = text;
      }

      const entry = {
        status: res.status,
        elapsed,
        body: parsed,
        model: (isMultipart ? body.get("model") : body?.model) || selectedEndpoint,
        ts: Date.now(),
      };
      setResponse(entry);
      setHistory((prev) => {
        const next = [entry, ...(Array.isArray(prev) ? prev : [])].slice(0, 50);
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
  }, [
    loading,
    editorMode,
    rawBodyText,
    currentStructuredBody,
    selectedKey,
    customHeaders,
    host,
    selectedEndpoint,
    sttFile,
    sttModel,
    sttLanguage,
  ]);

  const outputText = useMemo(() => {
    if (streamingText) return streamingText;
    if (!response) return "";
    if (response.error) return `Error: ${response.error}`;
    const b = response.body;
    if (typeof b === "string") return b;
    if (typeof b?.text === "string") return b.text;
    // Extract standard chat content for primary view
    const chatMsg = b?.choices?.[0]?.message?.content || b?.choices?.[0]?.text;
    if (chatMsg) return chatMsg;
    // Extract responses API output
    const respOut = b?.output?.[0]?.content || b?.output_text;
    if (respOut) return respOut;
    // Extract images
    if (Array.isArray(b?.data) && b.data[0]?.url) {
      return b.data.map((img) => img.url).join("\n\n");
    }
    return JSON.stringify(b, null, 2);
  }, [response, streamingText]);

  return (
    <div className="mx-auto max-w-7xl space-y-4 p-4 sm:p-6">
      {/* Header */}
      <div className="flex flex-col justify-between gap-3 border-b border-border-subtle pb-4 sm:flex-row sm:items-center">
        <div>
          <h1 className="text-xl font-bold tracking-tight text-text-main sm:text-2xl">API Playground</h1>
          <p className="text-xs text-text-muted">
            Test any OpenAI-compatible endpoint, combo, or custom provider model with live streaming & parameter controls.
          </p>
        </div>
        <div className="flex items-center gap-2">
          <button
            type="button"
            onClick={() => copyCurl(curlCommand)}
            className="flex items-center gap-1.5 rounded-lg border border-border-subtle bg-surface px-3 py-1.5 text-xs font-medium text-text-main transition-colors hover:bg-surface-2"
            title="Copy equivalent cURL command"
          >
            <span className="material-symbols-outlined text-[16px]">terminal</span>
            <span>{curlCopied ? "Copied cURL!" : "Copy cURL"}</span>
          </button>
          <div className="flex rounded-lg border border-border-subtle bg-surface p-0.5">
            <button
              type="button"
              onClick={() => setEditorMode("structured")}
              className={cn(
                "rounded px-2.5 py-1 text-xs font-medium transition-all",
                editorMode === "structured"
                  ? "bg-primary text-white shadow-sm"
                  : "text-text-muted hover:text-text-main"
              )}
            >
              Controls
            </button>
            <button
              type="button"
              onClick={() => {
                setRawBodyText(JSON.stringify(currentStructuredBody, null, 2));
                setEditorMode("json");
              }}
              className={cn(
                "rounded px-2.5 py-1 text-xs font-medium transition-all",
                editorMode === "json"
                  ? "bg-primary text-white shadow-sm"
                  : "text-text-muted hover:text-text-main"
              )}
            >
              Raw JSON
            </button>
          </div>
        </div>
      </div>

      {/* Target config bar */}
      <div className="grid grid-cols-1 gap-3 rounded-xl border border-border-subtle bg-surface-2 p-3 sm:grid-cols-3">
        <div>
          <label className="mb-1 block text-xs font-semibold uppercase tracking-wider text-text-muted">API Key</label>
          <select
            value={selectedKey}
            onChange={(e) => setSelectedKey(e.target.value)}
            className="w-full rounded-lg border border-border-subtle bg-surface px-3 py-2 text-xs font-mono text-text-main outline-none focus:border-primary"
          >
            {apiKeys.length === 0 && <option value="">No local keys (local mode)</option>}
            {apiKeys.map((k) => (
              <option key={k.id} value={k.key}>
                {k.name ? `${k.name} (${k.key.slice(0, 10)}...)` : k.key}
              </option>
            ))}
          </select>
        </div>
        <div>
          <label className="mb-1 block text-xs font-semibold uppercase tracking-wider text-text-muted">Endpoint</label>
          <select
            value={selectedEndpoint}
            onChange={(e) => setSelectedEndpoint(e.target.value)}
            className="w-full rounded-lg border border-border-subtle bg-surface px-3 py-2 text-xs font-medium text-text-main outline-none focus:border-primary"
          >
            {ENDPOINTS.map((ep) => (
              <option key={ep.value} value={ep.value}>
                [{ep.category}] /v1/{ep.value}
              </option>
            ))}
          </select>
        </div>
        <div>
          <label className="mb-1 block text-xs font-semibold uppercase tracking-wider text-text-muted">Model</label>
          <ModelCombobox
            models={models}
            value={selectedModel}
            onChange={setSelectedModel}
            capsMap={modelCaps}
            capFilter={capFilter}
            onCapFilterChange={setCapFilter}
          />
        </div>
      </div>

      {/* Main workspace: 2-column layout */}
      <div className="grid grid-cols-1 gap-4 lg:grid-cols-12">
        {/* Left Column: Editor & Params */}
        <div className="space-y-4 lg:col-span-7">
          {editorMode === "structured" ? (
            <div className="space-y-4 rounded-xl border border-border-subtle bg-surface p-4">
              {/* Endpoint-specific controls */}
              {selectedEndpoint === "chat/completions" && (
                <div className="space-y-3">
                  <div className="flex items-center justify-between">
                    <span className="text-xs font-semibold uppercase tracking-wider text-text-muted">Messages</span>
                    <div className="flex gap-1.5">
                      <button
                        type="button"
                        onClick={() => addMessage("user")}
                        className="flex items-center gap-1 rounded bg-surface-2 px-2 py-1 text-[11px] font-medium text-text-main hover:bg-surface-3"
                      >
                        <span className="material-symbols-outlined text-[14px]">add</span>
                        User
                      </button>
                      <button
                        type="button"
                        onClick={() => addMessage("system")}
                        className="flex items-center gap-1 rounded bg-surface-2 px-2 py-1 text-[11px] font-medium text-text-main hover:bg-surface-3"
                      >
                        <span className="material-symbols-outlined text-[14px]">add</span>
                        System
                      </button>
                      <button
                        type="button"
                        onClick={() => addMessage("assistant")}
                        className="flex items-center gap-1 rounded bg-surface-2 px-2 py-1 text-[11px] font-medium text-text-main hover:bg-surface-3"
                      >
                        <span className="material-symbols-outlined text-[14px]">add</span>
                        Assistant
                      </button>
                    </div>
                  </div>

                  <div className="space-y-2.5">
                    {messages.map((m, idx) => (
                      <div key={idx} className="rounded-lg border border-border-subtle bg-surface-2 p-2.5">
                        <div className="mb-1.5 flex items-center justify-between">
                          <select
                            value={m.role}
                            onChange={(e) => updateMessage(idx, "role", e.target.value)}
                            className="rounded bg-surface px-2 py-0.5 text-[11px] font-semibold uppercase tracking-wider text-text-muted"
                          >
                            {ROLES.map((r) => (
                              <option key={r} value={r}>
                                {r}
                              </option>
                            ))}
                          </select>
                          {messages.length > 1 && (
                            <button
                              type="button"
                              onClick={() => removeMessage(idx)}
                              className="text-text-muted hover:text-red-400"
                              title="Delete message"
                            >
                              <span className="material-symbols-outlined text-[16px]">delete</span>
                            </button>
                          )}
                        </div>
                        <textarea
                          value={m.content}
                          onChange={(e) => updateMessage(idx, "content", e.target.value)}
                          rows={Math.min(6, Math.max(2, m.content.split("\n").length))}
                          placeholder={`Enter ${m.role} message content...`}
                          className="w-full resize-y rounded bg-surface p-2 text-xs font-mono text-text-main outline-none placeholder:text-text-muted focus:ring-1 focus:ring-primary/50"
                        />
                      </div>
                    ))}
                  </div>
                </div>
              )}

              {selectedEndpoint === "responses" && (
                <div className="space-y-3">
                  <div>
                    <label className="mb-1 block text-xs font-semibold uppercase tracking-wider text-text-muted">Input</label>
                    <textarea
                      value={responsesInput}
                      onChange={(e) => setResponsesInput(e.target.value)}
                      rows={5}
                      className="w-full rounded-lg border border-border-subtle bg-surface-2 p-2.5 text-xs font-mono text-text-main outline-none focus:border-primary"
                    />
                  </div>
                  <div>
                    <label className="mb-1 block text-xs font-semibold uppercase tracking-wider text-text-muted">Instructions (Optional)</label>
                    <textarea
                      value={responsesInstructions}
                      onChange={(e) => setResponsesInstructions(e.target.value)}
                      rows={3}
                      placeholder="System instructions for the model..."
                      className="w-full rounded-lg border border-border-subtle bg-surface-2 p-2.5 text-xs font-mono text-text-main outline-none focus:border-primary"
                    />
                  </div>
                </div>
              )}

              {selectedEndpoint === "completions" && (
                <div>
                  <label className="mb-1 block text-xs font-semibold uppercase tracking-wider text-text-muted">Prompt</label>
                  <textarea
                    value={completionsPrompt}
                    onChange={(e) => setCompletionsPrompt(e.target.value)}
                    rows={6}
                    className="w-full rounded-lg border border-border-subtle bg-surface-2 p-2.5 text-xs font-mono text-text-main outline-none focus:border-primary"
                  />
                </div>
              )}

              {selectedEndpoint === "embeddings" && (
                <div>
                  <label className="mb-1 block text-xs font-semibold uppercase tracking-wider text-text-muted">Input Text</label>
                  <textarea
                    value={embeddingsInput}
                    onChange={(e) => setEmbeddingsInput(e.target.value)}
                    rows={5}
                    className="w-full rounded-lg border border-border-subtle bg-surface-2 p-2.5 text-xs font-mono text-text-main outline-none focus:border-primary"
                  />
                </div>
              )}

              {selectedEndpoint === "images/generations" && (
                <div className="space-y-3">
                  <div>
                    <label className="mb-1 block text-xs font-semibold uppercase tracking-wider text-text-muted">Prompt</label>
                    <textarea
                      value={imagePrompt}
                      onChange={(e) => setImagePrompt(e.target.value)}
                      rows={4}
                      className="w-full rounded-lg border border-border-subtle bg-surface-2 p-2.5 text-xs text-text-main outline-none focus:border-primary"
                    />
                  </div>
                  <div className="grid grid-cols-3 gap-2">
                    <div>
                      <label className="mb-1 block text-[11px] font-medium text-text-muted">Size</label>
                      <select
                        value={imageSize}
                        onChange={(e) => setImageSize(e.target.value)}
                        className="w-full rounded bg-surface-2 px-2 py-1.5 text-xs text-text-main border border-border-subtle"
                      >
                        {IMAGE_SIZES.map((s) => (
                          <option key={s} value={s}>
                            {s}
                          </option>
                        ))}
                      </select>
                    </div>
                    <div>
                      <label className="mb-1 block text-[11px] font-medium text-text-muted">Quality</label>
                      <select
                        value={imageQuality}
                        onChange={(e) => setImageQuality(e.target.value)}
                        className="w-full rounded bg-surface-2 px-2 py-1.5 text-xs text-text-main border border-border-subtle"
                      >
                        {IMAGE_QUALITIES.map((q) => (
                          <option key={q} value={q}>
                            {q}
                          </option>
                        ))}
                      </select>
                    </div>
                    <div>
                      <label className="mb-1 block text-[11px] font-medium text-text-muted">Images (n)</label>
                      <input
                        type="number"
                        min="1"
                        max="4"
                        value={imageN}
                        onChange={(e) => setImageN(e.target.value)}
                        className="w-full rounded bg-surface-2 px-2 py-1.5 text-xs text-text-main border border-border-subtle"
                      />
                    </div>
                  </div>
                </div>
              )}

              {selectedEndpoint === "audio/speech" && (
                <div className="space-y-3">
                  <div>
                    <label className="mb-1 block text-xs font-semibold uppercase tracking-wider text-text-muted">Text to Read</label>
                    <textarea
                      value={ttsInput}
                      onChange={(e) => setTtsInput(e.target.value)}
                      rows={4}
                      className="w-full rounded-lg border border-border-subtle bg-surface-2 p-2.5 text-xs text-text-main outline-none focus:border-primary"
                    />
                  </div>
                  <div className="grid grid-cols-2 gap-2">
                    <div>
                      <label className="mb-1 block text-[11px] font-medium text-text-muted">Voice</label>
                      <select
                        value={ttsVoice}
                        onChange={(e) => setTtsVoice(e.target.value)}
                        className="w-full rounded bg-surface-2 px-2 py-1.5 text-xs text-text-main border border-border-subtle"
                      >
                        {TTS_VOICES.map((v) => (
                          <option key={v} value={v}>
                            {v}
                          </option>
                        ))}
                      </select>
                    </div>
                    <div>
                      <label className="mb-1 block text-[11px] font-medium text-text-muted">Speed: {ttsSpeed}x</label>
                      <input
                        type="range"
                        min="0.25"
                        max="4.0"
                        step="0.25"
                        value={ttsSpeed}
                        onChange={(e) => setTtsSpeed(parseFloat(e.target.value))}
                        className="w-full accent-primary"
                      />
                    </div>
                  </div>
                </div>
              )}

              {selectedEndpoint === "audio/transcriptions" && (
                <div className="space-y-3">
                  <div>
                    <label className="mb-1 block text-xs font-semibold uppercase tracking-wider text-text-muted">Audio File</label>
                    <label
                      onDragOver={(e) => {
                        e.preventDefault();
                        e.stopPropagation();
                        e.currentTarget.classList.add("border-primary", "bg-primary/5");
                      }}
                      onDragLeave={(e) => {
                        e.preventDefault();
                        e.stopPropagation();
                        e.currentTarget.classList.remove("border-primary", "bg-primary/5");
                      }}
                      onDrop={(e) => {
                        e.preventDefault();
                        e.stopPropagation();
                        e.currentTarget.classList.remove("border-primary", "bg-primary/5");
                        const file = e.dataTransfer?.files?.[0];
                        if (file) setSttFile(file);
                      }}
                      className={cn(
                        "flex cursor-pointer flex-col items-center justify-center gap-1.5 rounded-lg border-2 border-dashed p-6 text-center transition-colors",
                        sttFile
                          ? "border-primary/60 bg-primary/5"
                          : "border-border-subtle bg-surface-2 hover:border-border hover:bg-surface-3"
                      )}
                    >
                      <input
                        type="file"
                        accept="audio/*,video/mp4,video/webm,video/mpeg"
                        className="hidden"
                        onChange={(e) => {
                          const file = e.target.files?.[0] || null;
                          setSttFile(file);
                          e.target.value = "";
                        }}
                      />
                      <span className="material-symbols-outlined text-[28px] text-text-muted">
                        {sttFile ? "audio_file" : "upload_file"}
                      </span>
                      {sttFile ? (
                        <span className="max-w-full truncate text-xs font-medium text-text-main">{sttFile.name}</span>
                      ) : (
                        <span className="text-xs text-text-muted">
                          Click to browse or drag &amp; drop an audio file
                          <span className="mt-0.5 block text-[10px] opacity-70">mp3, wav, m4a, ogg, webm, mp4...</span>
                        </span>
                      )}
                      {sttFile && (
                        <span className="text-[10px] text-text-muted">
                          {(sttFile.size / 1024 / 1024).toFixed(2)} MB — click to replace
                        </span>
                      )}
                    </label>
                  </div>
                  <div className="grid grid-cols-2 gap-2">
                    <div>
                      <label className="mb-1 block text-[11px] font-medium text-text-muted">Model</label>
                      <input
                        type="text"
                        value={sttModel}
                        onChange={(e) => setSttModel(e.target.value)}
                        placeholder="whisper-1"
                        className="w-full rounded border border-border-subtle bg-surface-2 px-2 py-1.5 font-mono text-xs text-text-main"
                      />
                    </div>
                    <div>
                      <label className="mb-1 block text-[11px] font-medium text-text-muted">Language (optional)</label>
                      <input
                        type="text"
                        value={sttLanguage}
                        onChange={(e) => setSttLanguage(e.target.value)}
                        placeholder="e.g. en, hi, fr"
                        className="w-full rounded border border-border-subtle bg-surface-2 px-2 py-1.5 text-xs text-text-main"
                      />
                    </div>
                  </div>
                </div>
              )}

              {selectedEndpoint === "moderations" && (
                <div>
                  <label className="mb-1 block text-xs font-semibold uppercase tracking-wider text-text-muted">Text to Analyze</label>
                  <textarea
                    value={moderationsInput}
                    onChange={(e) => setModerationsInput(e.target.value)}
                    rows={5}
                    className="w-full rounded-lg border border-border-subtle bg-surface-2 p-2.5 text-xs font-mono text-text-main outline-none focus:border-primary"
                  />
                  <p className="mt-1.5 text-[11px] text-text-muted">
                    Classifies the input against harm categories — no model parameters apply to this endpoint.
                  </p>
                </div>
              )}

              {/* Advanced Parameters Accordion / Grid */}
              {["chat/completions", "responses", "completions"].includes(selectedEndpoint) && (
                <div className="border-t border-border-subtle pt-3">
                  <div className="mb-2 text-xs font-semibold uppercase tracking-wider text-text-muted">Parameters</div>
                  <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
                    <div>
                      <label className="mb-1 block text-[11px] text-text-muted">Temperature: {temperature}</label>
                      <input
                        type="range"
                        min="0"
                        max="2"
                        step="0.05"
                        value={temperature}
                        onChange={(e) => setTemperature(parseFloat(e.target.value))}
                        className="w-full accent-primary"
                      />
                    </div>
                    <div>
                      <label className="mb-1 block text-[11px] text-text-muted">Top P: {topP}</label>
                      <input
                        type="range"
                        min="0"
                        max="1"
                        step="0.05"
                        value={topP}
                        onChange={(e) => setTopP(parseFloat(e.target.value))}
                        className="w-full accent-primary"
                      />
                    </div>
                    <div>
                      <label className="mb-1 block text-[11px] text-text-muted">Max Tokens</label>
                      <input
                        type="text"
                        placeholder="Default"
                        value={maxTokens}
                        onChange={(e) => setMaxTokens(e.target.value)}
                        className="w-full rounded border border-border-subtle bg-surface-2 px-2 py-1 text-xs text-text-main"
                      />
                    </div>
                    <div>
                      <label className="mb-1 block text-[11px] text-text-muted">Seed</label>
                      <input
                        type="text"
                        placeholder="Random"
                        value={seed}
                        onChange={(e) => setSeed(e.target.value)}
                        className="w-full rounded border border-border-subtle bg-surface-2 px-2 py-1 text-xs text-text-main"
                      />
                    </div>
                  </div>

                  <div className="mt-3 flex flex-wrap items-center gap-4">
                    <label className="flex cursor-pointer items-center gap-2 text-xs text-text-main">
                      <input
                        type="checkbox"
                        checked={stream}
                        onChange={(e) => setStream(e.target.checked)}
                        className="rounded border-border-subtle accent-primary"
                      />
                      Stream SSE
                    </label>
                    <label className="flex cursor-pointer items-center gap-2 text-xs text-text-main">
                      <input
                        type="checkbox"
                        checked={jsonMode}
                        onChange={(e) => setJsonMode(e.target.checked)}
                        className="rounded border-border-subtle accent-primary"
                      />
                      JSON Object Mode
                    </label>
                    <label className="flex cursor-pointer items-center gap-2 text-xs text-text-main">
                      <input
                        type="checkbox"
                        checked={enableReasoning}
                        onChange={(e) => setEnableReasoning(e.target.checked)}
                        className="rounded border-border-subtle accent-primary"
                      />
                      Reasoning Effort
                    </label>
                    {enableReasoning && (
                      <select
                        value={reasoningEffort}
                        onChange={(e) => setReasoningEffort(e.target.value)}
                        className="rounded border border-border-subtle bg-surface-2 px-2 py-0.5 text-xs text-text-main"
                      >
                        {REASONING_EFFORTS.map((ef) => (
                          <option key={ef} value={ef}>
                            {ef}
                          </option>
                        ))}
                      </select>
                    )}
                  </div>
                </div>
              )}
            </div>
          ) : (
            <div className="space-y-2 rounded-xl border border-border-subtle bg-surface p-4">
              <div className="flex items-center justify-between">
                <span className="text-xs font-semibold uppercase tracking-wider text-text-muted">Request Body (JSON)</span>
                {rawJsonError && <span className="text-xs text-red-400">{rawJsonError}</span>}
              </div>
              <textarea
                value={rawBodyText}
                onChange={(e) => {
                  setRawBodyText(e.target.value);
                  try {
                    JSON.parse(e.target.value);
                    setRawJsonError("");
                  } catch (err) {
                    setRawJsonError(err.message);
                  }
                }}
                rows={16}
                className="w-full rounded-lg border border-border-subtle bg-surface-2 p-3 font-mono text-xs text-text-main outline-none focus:border-primary"
              />
            </div>
          )}

          {/* Action Bar */}
          <div className="flex items-center justify-between rounded-xl border border-border-subtle bg-surface-2 p-3">
            <div className="flex items-center gap-2 text-xs text-text-muted">
              <span className="font-mono text-text-main">POST /v1/{selectedEndpoint}</span>
            </div>
            <button
              type="button"
              onClick={sendRequest}
              disabled={
                (!selectedModel && selectedEndpoint !== "audio/speech" && selectedEndpoint !== "audio/transcriptions") ||
                (selectedEndpoint === "audio/transcriptions" && editorMode === "structured" && !sttFile)
              }
              className={cn(
                "flex items-center gap-2 rounded-lg px-6 py-2 text-xs font-semibold text-white shadow-sm transition-all",
                loading
                  ? "bg-red-600 hover:bg-red-700"
                  : "bg-brand-500 hover:bg-brand-600 disabled:opacity-50"
              )}
            >
              {loading ? (
                <>
                  <span className="material-symbols-outlined animate-spin text-[16px]">progress_activity</span>
                  <span>Stop</span>
                </>
              ) : (
                <>
                  <span className="material-symbols-outlined text-[16px]">send</span>
                  <span>Send Request</span>
                </>
              )}
            </button>
          </div>
        </div>

        {/* Right Column: Output & Inspector */}
        <div className="space-y-4 lg:col-span-5">
          <div className="flex h-full min-h-[480px] flex-col rounded-xl border border-border-subtle bg-surface overflow-hidden">
            {/* Tabs */}
            <div className="flex items-center justify-between border-b border-border-subtle bg-surface-2 px-3 py-1.5">
              <div className="flex gap-1">
                {["output", "raw", "timing"].map((tab) => (
                  <button
                    key={tab}
                    type="button"
                    onClick={() => setActiveTab(tab)}
                    className={cn(
                      "rounded px-3 py-1 text-xs font-medium capitalize transition-all",
                      activeTab === tab
                        ? "bg-surface text-text-main shadow-sm"
                        : "text-text-muted hover:text-text-main"
                    )}
                  >
                    {tab}
                  </button>
                ))}
              </div>
              {response && (
                <div className="flex items-center gap-2">
                  <span
                    className={cn(
                      "rounded px-2 py-0.5 text-[10px] font-mono font-semibold uppercase",
                      response.error || response.status >= 400
                        ? "bg-red-500/10 text-red-400"
                        : "bg-green-500/10 text-green-400"
                    )}
                  >
                    {response.error ? "ERR" : `${response.status} OK`}
                  </span>
                  <button
                    type="button"
                    onClick={() => copyOutput(outputText)}
                    className="p-1 text-text-muted hover:text-text-main"
                    title="Copy output text"
                  >
                    <span className="material-symbols-outlined text-[16px]">
                      {outputCopied ? "check" : "content_copy"}
                    </span>
                  </button>
                </div>
              )}
            </div>

            {/* Viewport */}
            <div className="flex-1 p-4 overflow-auto font-mono text-xs text-text-main">
              {loading && !streamingText && (
                <div className="flex h-48 flex-col items-center justify-center gap-2 text-text-muted">
                  <span className="material-symbols-outlined animate-spin text-[28px] text-primary">progress_activity</span>
                  <span>Waiting for upstream response...</span>
                </div>
              )}

              {activeTab === "output" && (
                <div>
                  {response?.audioUrl && (
                    <div className="mb-4 rounded-lg bg-surface-2 p-3">
                      <audio controls src={response.audioUrl} className="w-full" autoPlay />
                    </div>
                  )}
                  <pre className="whitespace-pre-wrap break-words leading-relaxed">
                    {outputText || (loading ? "" : <span className="text-text-muted">Ready. Configure request and click Send.</span>)}
                  </pre>
                </div>
              )}

              {activeTab === "raw" && (
                <pre className="whitespace-pre-wrap break-words text-text-main">
                  {response ? JSON.stringify(response, null, 2) : <span className="text-text-muted">No response yet.</span>}
                </pre>
              )}

              {activeTab === "timing" && (
                <div className="space-y-3 font-sans text-xs">
                  {response ? (
                    <>
                      <div className="grid grid-cols-2 gap-3 rounded-lg bg-surface-2 p-3">
                        <div>
                          <span className="block text-[10px] uppercase text-text-muted">Status</span>
                          <span className={response.error ? "text-red-400 font-semibold" : "text-green-400 font-semibold"}>
                            {response.error ? "Error" : `${response.status} OK`}
                          </span>
                        </div>
                        <div>
                          <span className="block text-[10px] uppercase text-text-muted">Latency</span>
                          <span className="font-semibold text-text-main">{formatMs(response.elapsed)}</span>
                        </div>
                      </div>

                      {response.body?.usage && (
                        <div className="rounded-lg border border-border-subtle bg-surface-2 p-3">
                          <span className="mb-2 block text-[10px] uppercase font-semibold text-text-muted">Token Breakdown</span>
                          <div className="space-y-1 font-mono text-xs">
                            <div className="flex justify-between">
                              <span className="text-text-muted">Prompt Tokens:</span>
                              <span>{response.body.usage.prompt_tokens ?? "-"}</span>
                            </div>
                            <div className="flex justify-between">
                              <span className="text-text-muted">Completion Tokens:</span>
                              <span>{response.body.usage.completion_tokens ?? "-"}</span>
                            </div>
                            <div className="flex justify-between border-t border-border-subtle pt-1 font-semibold">
                              <span className="text-text-main">Total Tokens:</span>
                              <span className="text-primary">{response.body.usage.total_tokens ?? "-"}</span>
                            </div>
                          </div>
                        </div>
                      )}
                    </>
                  ) : (
                    <span className="text-text-muted">No timing data available.</span>
                  )}
                </div>
              )}
            </div>
          </div>
        </div>
      </div>

      {/* History drawer / bottom section */}
      {Array.isArray(history) && history.length > 0 && (
        <div className="rounded-xl border border-border-subtle bg-surface p-4">
          <div className="mb-3 flex items-center justify-between">
            <h2 className="text-xs font-semibold uppercase tracking-wider text-text-muted">Recent Requests ({history.length})</h2>
            <button
              type="button"
              onClick={() => {
                localStorage.removeItem(HISTORY_KEY);
                setHistory([]);
              }}
              className="text-[11px] text-text-muted hover:text-red-400"
            >
              Clear history
            </button>
          </div>
          <div className="grid grid-cols-1 gap-2 sm:grid-cols-2 lg:grid-cols-3">
            {history.slice(0, 6).map((h, i) => (
              <div
                key={i}
                className="flex items-center justify-between rounded-lg border border-border-subtle bg-surface-2 p-2.5 text-xs font-mono"
              >
                <div className="flex min-w-0 items-center gap-2">
                  <span
                    className={cn(
                      "rounded px-1.5 py-0.5 text-[10px] font-semibold",
                      h.error ? "bg-red-500/10 text-red-400" : "bg-green-500/10 text-green-400"
                    )}
                  >
                    {h.error ? "ERR" : h.status}
                  </span>
                  <span className="truncate text-text-main">{h.model}</span>
                </div>
                <div className="flex shrink-0 items-center gap-2 text-text-muted text-[11px]">
                  <span>{formatMs(h.elapsed)}</span>
                  <span>{new Date(h.ts).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}</span>
                </div>
              </div>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}
