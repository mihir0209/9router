"use client";

import { useState, useMemo, useEffect } from "react";
import PropTypes from "prop-types";
import { Badge, Button, Input, Modal, RetryRulesEditor } from "@/shared/components";
import { parseCurlInBrowser, collectJsonPaths, extractValueByPath } from "@/shared/utils/curlParser";

export default function CustomProviderStudioModal({ isOpen, onClose, onCreated }) {
  // Provider Identity
  const [name, setName] = useState("");
  const [prefix, setPrefix] = useState("");
  const [baseUrl, setBaseUrl] = useState("");
  const [retry, setRetry] = useState(null);

  // Two Primary Inputs
  const [curlCommand, setCurlCommand] = useState("");
  const [sampleOutput, setSampleOutput] = useState("");

  // Extracted cURL metadata
  const [parsedMethod, setParsedMethod] = useState("POST");
  const [parsedHeaders, setParsedHeaders] = useState({});
  const [parsedAuthKey, setParsedAuthKey] = useState("");

  // User-Defined Schema Mapping
  const [responseTextPath, setResponseTextPath] = useState("");
  const [promptTokensPath, setPromptTokensPath] = useState("");
  const [completionTokensPath, setCompletionTokensPath] = useState("");
  const [totalTokensPath, setTotalTokensPath] = useState("");
  const [errorPath, setErrorPath] = useState("");

  // Streaming Controls
  const [streamingEnabled, setStreamingEnabled] = useState(false);
  const [streamingParam, setStreamingParam] = useState("stream_body"); // "stream_body", "stream_query", "custom"
  const [customStreamParamName, setCustomStreamParamName] = useState("stream");
  const [streamChunkTextPath, setStreamChunkTextPath] = useState("delta.content");
  const [streamEndMarker, setStreamEndMarker] = useState("[DONE]");

  // UI State
  const [submitting, setSubmitting] = useState(false);
  const [saveError, setSaveError] = useState("");

  // Parse cURL whenever it changes in the browser (100% Client-Side)
  useEffect(() => {
    if (!curlCommand.trim()) return;
    const { method, url, headers, isStreaming } = parseCurlInBrowser(curlCommand);
    setParsedMethod(method);
    setParsedHeaders(headers);

    if (url && !baseUrl) {
      setBaseUrl(url);
    }

    if (url && !prefix) {
      try {
        const hostname = new URL(url).hostname;
        const parts = hostname.split(".");
        const candidate = parts.length > 2 ? parts[parts.length - 2] : parts[0];
        if (candidate && candidate !== "api" && candidate !== "localhost") {
          setPrefix(candidate.toLowerCase());
          if (!name) setName(`${candidate.charAt(0).toUpperCase() + candidate.slice(1)} Custom`);
        }
      } catch {
        // ignore invalid URL
      }
    }

    const authVal = headers["authorization"] || headers["x-api-key"] || headers["api-key"];
    if (authVal) {
      setParsedAuthKey(authVal.replace(/^Bearer\s+/i, ""));
    }

    if (isStreaming) {
      setStreamingEnabled(true);
    }
  }, [curlCommand]);

  // Parse sample output JSON and discover candidate paths
  const parsedSampleJson = useMemo(() => {
    if (!sampleOutput.trim()) return null;
    try {
      return JSON.parse(sampleOutput.trim());
    } catch {
      return null;
    }
  }, [sampleOutput]);

  const candidatePaths = useMemo(() => {
    if (!parsedSampleJson) return [];
    return collectJsonPaths(parsedSampleJson);
  }, [parsedSampleJson]);

  const textCandidates = useMemo(() => {
    return candidatePaths.filter((c) => c.type === "string");
  }, [candidatePaths]);

  // Live extracted text based on user's selected path
  const extractedText = useMemo(() => {
    if (!parsedSampleJson || !responseTextPath) return "";
    const val = extractValueByPath(parsedSampleJson, responseTextPath);
    if (val === undefined) return null;
    return typeof val === "object" ? JSON.stringify(val) : String(val);
  }, [parsedSampleJson, responseTextPath]);

  const handleReset = () => {
    setName("");
    setPrefix("");
    setBaseUrl("");
    setRetry(null);
    setCurlCommand("");
    setSampleOutput("");
    setResponseTextPath("");
    setPromptTokensPath("");
    setCompletionTokensPath("");
    setTotalTokensPath("");
    setErrorPath("");
    setStreamingEnabled(false);
    setSaveError("");
  };

  const handleSave = async () => {
    if (!name.trim()) {
      setSaveError("Provider Name is required.");
      return;
    }
    if (!prefix.trim()) {
      setSaveError("Model Prefix is required.");
      return;
    }
    if (!baseUrl.trim()) {
      setSaveError("Base URL or Endpoint is required.");
      return;
    }
    if (!responseTextPath.trim()) {
      setSaveError("Please define or select the Response Text Path from your sample output.");
      return;
    }

    setSubmitting(true);
    setSaveError("");

    try {
      const payload = {
        name: name.trim(),
        prefix: prefix.trim(),
        baseUrl: baseUrl.trim(),
        type: "custom",
        apiType: "custom",
        method: parsedMethod,
        curlCommand: curlCommand.trim(),
        sampleOutput: sampleOutput.trim(),
        schemaMapping: {
          endpointUrl: baseUrl.trim(),
          responseTextPath: responseTextPath.trim(),
          promptTokensPath: promptTokensPath.trim() || undefined,
          completionTokensPath: completionTokensPath.trim() || undefined,
          totalTokensPath: totalTokensPath.trim() || undefined,
          errorPath: errorPath.trim() || undefined,
          implementationTags: ["openai-compatible", "anthropic-compatible"],
        },
        streaming: {
          enabled: streamingEnabled,
          paramType: streamingParam,
          customParam: customStreamParamName,
          chunkTextPath: streamChunkTextPath.trim(),
          endMarker: streamEndMarker.trim(),
        },
        customHeaders: parsedHeaders,
        ...(parsedAuthKey ? { apiKey: parsedAuthKey } : {}),
        ...(retry ? { retry } : {}),
      };

      const res = await fetch("/api/provider-nodes", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(payload),
      });

      const data = await res.json();
      if (!res.ok) {
        throw new Error(data.error || "Failed to create custom provider");
      }

      const createdNode = data.node || data;

      if (parsedAuthKey && createdNode?.id) {
        try {
          await fetch("/api/providers", {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({
              provider: createdNode.id,
              name: `${name.trim()} (Primary Key)`,
              apiKey: parsedAuthKey,
              authType: "apikey",
            }),
          });
        } catch (connErr) {
          console.warn("Could not auto-register connection for custom provider:", connErr);
        }
      }

      onCreated(createdNode);
      handleReset();
      onClose();
    } catch (err) {
      setSaveError(err.message || "An unexpected error occurred while saving.");
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <Modal
      isOpen={isOpen}
      onClose={onClose}
      title="Custom Provider Studio"
      size="fullscreen"
    >
      <div className="flex flex-col gap-4 text-xs h-full">
        {saveError && (
          <div className="p-2.5 rounded-lg bg-red-500/10 border border-red-500/30 text-red-400 flex items-center gap-2">
            <span className="material-symbols-outlined text-[16px]">error</span>
            <span>{saveError}</span>
          </div>
        )}

        {/* 2-Column Responsive Fullscreen Grid */}
        <div className="grid grid-cols-1 lg:grid-cols-12 gap-5 flex-1 min-h-0">
          {/* Left Column: Provider Info + cURL + Sample Output */}
          <div className="lg:col-span-5 flex flex-col gap-4">
            {/* Basic Info */}
            <div className="p-3.5 rounded-xl border border-border-subtle bg-surface-2/40 flex flex-col gap-3">
              <span className="font-semibold text-text-main flex items-center gap-1.5 text-xs">
                <span className="material-symbols-outlined text-[16px] text-amber-500">dns</span>
                Provider Identity
              </span>
              <div className="grid grid-cols-2 gap-3">
                <Input
                  label="Name"
                  placeholder="e.g. My Custom Provider"
                  value={name}
                  onChange={(e) => setName(e.target.value)}
                />
                <Input
                  label="Prefix"
                  placeholder="e.g. my-custom"
                  value={prefix}
                  onChange={(e) => setPrefix(e.target.value.toLowerCase().replace(/[^a-z0-9_-]/g, ""))}
                />
              </div>
              <Input
                label="Base URL / Target Endpoint"
                placeholder="http://127.0.0.1:20139/api/worst/flat/execute"
                value={baseUrl}
                onChange={(e) => setBaseUrl(e.target.value)}
              />
            </div>

            {/* Input 1: Raw cURL Command */}
            <div className="p-3.5 rounded-xl border border-border-subtle bg-surface-2/40 flex flex-col gap-2">
              <div className="flex items-center justify-between">
                <label className="font-semibold text-text-main flex items-center gap-1.5 text-xs">
                  <span className="material-symbols-outlined text-[16px] text-blue-400">terminal</span>
                  1. Raw cURL Command
                </label>
                <div className="flex items-center gap-2 text-[11px] text-text-muted">
                  <span>Method: <strong className="text-text-main font-mono">{parsedMethod}</strong></span>
                  {parsedAuthKey && <span className="text-green-500 font-medium">Auth Key Detected</span>}
                </div>
              </div>
              <textarea
                rows={6}
                value={curlCommand}
                onChange={(e) => setCurlCommand(e.target.value)}
                placeholder="curl -X POST http://127.0.0.1:20139/api/worst/flat/execute \
  -H 'Content-Type: application/json' \
  -d '{&quot;prompt&quot;: &quot;Hello custom AI&quot;}'"
                className="w-full rounded-lg border border-border-subtle bg-surface p-2.5 font-mono text-xs text-text-main focus:border-primary focus:outline-none custom-scrollbar"
              />
            </div>

            {/* Input 2: Sample Response Output */}
            <div className="p-3.5 rounded-xl border border-border-subtle bg-surface-2/40 flex flex-col gap-2 flex-1 min-h-[140px]">
              <div className="flex items-center justify-between">
                <label className="font-semibold text-text-main flex items-center gap-1.5 text-xs">
                  <span className="material-symbols-outlined text-[16px] text-emerald-400">data_object</span>
                  2. Sample Response Output
                </label>
                {parsedSampleJson && (
                  <Badge variant="success" size="sm">{candidatePaths.length} paths discovered</Badge>
                )}
              </div>
              <textarea
                rows={6}
                value={sampleOutput}
                onChange={(e) => setSampleOutput(e.target.value)}
                placeholder='{"PAYLOAD:::RAW:::UTTERANCE": "Verified message", "RESOURCE:::INBOUND:::QUANTA": 12}'
                className="w-full flex-1 rounded-lg border border-border-subtle bg-surface p-2.5 font-mono text-xs text-text-main focus:border-primary focus:outline-none custom-scrollbar min-h-[100px]"
              />
            </div>
          </div>

          {/* Right Column: Schema Mapping + Streaming + Retry */}
          <div className="lg:col-span-7 flex flex-col gap-4">
            {/* Schema Mapping */}
            <div className="p-3.5 rounded-xl border border-border-subtle bg-surface-2/40 flex flex-col gap-3">
              <span className="font-semibold text-text-main flex items-center gap-1.5 text-xs">
                <span className="material-symbols-outlined text-[16px] text-purple-400">schema</span>
                Output Schema Mapping
              </span>

              {/* Response Text Path */}
              <div className="flex flex-col gap-1.5">
                <Input
                  label="Response Text Path (Required)"
                  placeholder="e.g. PAYLOAD:::RAW:::UTTERANCE or reply.text"
                  value={responseTextPath}
                  onChange={(e) => setResponseTextPath(e.target.value)}
                />

                {/* Candidate Path Chips */}
                {textCandidates.length > 0 && (
                  <div className="flex flex-wrap gap-1.5 items-center pt-1">
                    <span className="text-[11px] text-text-muted">Detected paths:</span>
                    {textCandidates.slice(0, 10).map((cand) => (
                      <button
                        key={cand.path}
                        type="button"
                        onClick={() => setResponseTextPath(cand.path)}
                        className={`px-2 py-0.5 rounded text-[11px] font-mono transition-colors ${
                          responseTextPath === cand.path
                            ? "bg-primary text-primary-foreground font-semibold"
                            : "bg-surface border border-border-subtle text-text-main hover:border-primary/50"
                        }`}
                      >
                        {cand.path}
                      </button>
                    ))}
                  </div>
                )}

                {/* Live Extraction Preview */}
                {responseTextPath && (
                  <div className="p-2.5 rounded-lg bg-surface border border-border-subtle text-xs">
                    <span className="text-[10px] text-text-muted uppercase tracking-wider font-semibold block mb-0.5">Live Extracted Value</span>
                    {extractedText !== null && extractedText !== undefined ? (
                      <p className="font-mono text-green-500 break-all">"{extractedText}"</p>
                    ) : (
                      <p className="text-amber-500 font-mono text-[11px]">Path not found in sample output</p>
                    )}
                  </div>
                )}
              </div>

              {/* Token & Error Metrics Grid */}
              <div className="grid grid-cols-2 sm:grid-cols-4 gap-2 pt-2 border-t border-border-subtle">
                <Input
                  label="Prompt Tokens"
                  placeholder="meta.prompt"
                  value={promptTokensPath}
                  onChange={(e) => setPromptTokensPath(e.target.value)}
                />
                <Input
                  label="Completion Tokens"
                  placeholder="meta.comp"
                  value={completionTokensPath}
                  onChange={(e) => setCompletionTokensPath(e.target.value)}
                />
                <Input
                  label="Total Tokens"
                  placeholder="meta.total"
                  value={totalTokensPath}
                  onChange={(e) => setTotalTokensPath(e.target.value)}
                />
                <Input
                  label="Error Path"
                  placeholder="error.message"
                  value={errorPath}
                  onChange={(e) => setErrorPath(e.target.value)}
                />
              </div>
            </div>

            {/* Streaming Configuration */}
            <div className="p-3.5 rounded-xl border border-border-subtle bg-surface-2/40 flex flex-col gap-3">
              <div className="flex items-center justify-between">
                <div>
                  <span className="font-semibold text-text-main flex items-center gap-1.5 text-xs">
                    <span className="material-symbols-outlined text-[16px] text-cyan-400">stream</span>
                    Streaming (SSE)
                  </span>
                  <span className="text-[11px] text-text-muted">Enable if endpoint supports chunked token streaming</span>
                </div>
                <label className="relative inline-flex items-center cursor-pointer">
                  <input
                    type="checkbox"
                    checked={streamingEnabled}
                    onChange={(e) => setStreamingEnabled(e.target.checked)}
                    className="sr-only peer"
                  />
                  <div className="w-9 h-5 bg-surface-3 peer-focus:outline-none rounded-full peer peer-checked:after:translate-x-full peer-checked:after:border-white after:content-[''] after:absolute after:top-[2px] after:left-[2px] after:bg-white after:border-gray-300 after:border after:rounded-full after:h-4 after:w-4 after:transition-all peer-checked:bg-primary"></div>
                </label>
              </div>

              {streamingEnabled && (
                <div className="flex flex-col gap-2.5 pt-2 border-t border-border-subtle">
                  <div className="grid grid-cols-3 gap-2">
                    <button
                      type="button"
                      onClick={() => setStreamingParam("stream_body")}
                      className={`px-2.5 py-1.5 rounded-lg border text-left text-xs transition ${
                        streamingParam === "stream_body"
                          ? "border-primary bg-primary/10 text-primary font-medium"
                          : "border-border-subtle bg-surface text-text-muted hover:text-text-main"
                      }`}
                    >
                      Body: <code>"stream": true</code>
                    </button>
                    <button
                      type="button"
                      onClick={() => setStreamingParam("stream_query")}
                      className={`px-2.5 py-1.5 rounded-lg border text-left text-xs transition ${
                        streamingParam === "stream_query"
                          ? "border-primary bg-primary/10 text-primary font-medium"
                          : "border-border-subtle bg-surface text-text-muted hover:text-text-main"
                      }`}
                    >
                      Query: <code>?stream=true</code>
                    </button>
                    <button
                      type="button"
                      onClick={() => setStreamingParam("custom")}
                      className={`px-2.5 py-1.5 rounded-lg border text-left text-xs transition ${
                        streamingParam === "custom"
                          ? "border-primary bg-primary/10 text-primary font-medium"
                          : "border-border-subtle bg-surface text-text-muted hover:text-text-main"
                      }`}
                    >
                      Custom Param
                    </button>
                  </div>

                  {streamingParam === "custom" && (
                    <Input
                      label="Custom Parameter Name"
                      placeholder="e.g. is_stream"
                      value={customStreamParamName}
                      onChange={(e) => setCustomStreamParamName(e.target.value)}
                    />
                  )}

                  <div className="grid grid-cols-2 gap-3">
                    <Input
                      label="Chunk Delta Path"
                      placeholder="e.g. delta.text or token:::fragment"
                      value={streamChunkTextPath}
                      onChange={(e) => setStreamChunkTextPath(e.target.value)}
                    />
                    <Input
                      label="Stream End Marker"
                      placeholder="e.g. [DONE]"
                      value={streamEndMarker}
                      onChange={(e) => setStreamEndMarker(e.target.value)}
                    />
                  </div>
                </div>
              )}
            </div>

            {/* Error Retry Rules */}
            <div className="p-3.5 rounded-xl border border-border-subtle bg-surface-2/40 flex flex-col gap-2">
              <span className="font-semibold text-text-main flex items-center gap-1.5 text-xs">
                <span className="material-symbols-outlined text-[16px] text-orange-400">replay</span>
                Error Retry Rules
              </span>
              <RetryRulesEditor value={retry} onChange={setRetry} />
            </div>
          </div>
        </div>

        {/* Footer Actions */}
        <div className="flex items-center justify-end gap-3 pt-3 border-t border-border-subtle">
          <Button onClick={onClose} variant="ghost">
            Cancel
          </Button>
          <Button
            onClick={handleSave}
            disabled={submitting || !name.trim() || !prefix.trim() || !baseUrl.trim() || !responseTextPath.trim()}
            variant="primary"
          >
            {submitting ? "Saving..." : "Save Custom Provider"}
          </Button>
        </div>
      </div>
    </Modal>
  );
}

CustomProviderStudioModal.propTypes = {
  isOpen: PropTypes.bool.isRequired,
  onClose: PropTypes.func.isRequired,
  onCreated: PropTypes.func.isRequired,
};
