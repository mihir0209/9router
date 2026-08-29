"use client";

import { useState } from "react";
import PropTypes from "prop-types";
import Button from "./Button";
import Input from "./Input";
import Modal from "./Modal";
import Select from "./Select";

const DEFAULTS = {
  embedding: "https://api.example.com/v1",
  image: "https://api.example.com/v1",
  video: "https://api.example.com/v1",
  tts: "https://api.example.com/v1",
  stt: "https://api.example.com/v1",
};

function parseHeaders(value) {
  const result = {};
  for (const line of value.split("\n")) {
    const colon = line.indexOf(":");
    if (colon > 0) result[line.slice(0, colon).trim()] = line.slice(colon + 1).trim();
  }
  return result;
}

function CustomMediaProviderForm({ kind, node, onClose, onSaved }) {
  const [form, setForm] = useState(() => ({
    name: node?.name || "",
    prefix: node?.prefix || "",
    baseUrl: node?.baseUrl || DEFAULTS[kind] || "",
    endpointUrl: node?.endpointUrl || "",
    authHeader: node?.authHeader || "bearer",
    models: (node?.models || []).join(", "),
    headers: Object.entries(node?.customHeaders || {}).map(([key, value]) => `${key}: ${value}`).join("\n"),
  }));
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");

  const save = async () => {
    setSaving(true);
    setError("");
    try {
      const res = await fetch(node ? `/api/provider-nodes/${node.id}` : "/api/provider-nodes", {
        method: node ? "PUT" : "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          type: "custom-media",
          mediaKind: kind,
          name: form.name,
          prefix: form.prefix,
          baseUrl: form.baseUrl,
          endpointUrl: form.endpointUrl,
          authHeader: form.authHeader,
          models: form.models.split(",").map((value) => value.trim()).filter(Boolean),
          customHeaders: parseHeaders(form.headers),
        }),
      });
      const data = await res.json();
      if (!res.ok) throw new Error(data.error || "Failed to save provider");
      onSaved(data.node);
    } catch (saveError) {
      setError(saveError.message);
    } finally {
      setSaving(false);
    }
  };

  return (
    <Modal isOpen title={`${node ? "Edit" : "Add"} Custom ${kind}`} onClose={onClose}>
      <div className="flex flex-col gap-4">
        <Input label="Display name" value={form.name} onChange={(e) => setForm({ ...form, name: e.target.value })} placeholder="My media API" />
        <Input label="Provider prefix" value={form.prefix} onChange={(e) => setForm({ ...form, prefix: e.target.value })} placeholder="my-media" hint="Used in model IDs, for example my-media/model-id." />
        <Input label="Base URL" value={form.baseUrl} onChange={(e) => setForm({ ...form, baseUrl: e.target.value })} placeholder="https://api.example.com/v1" hint="9Router appends the standard OpenAI-compatible path." />
        <Input label="Full endpoint URL" value={form.endpointUrl} onChange={(e) => setForm({ ...form, endpointUrl: e.target.value })} placeholder="https://api.example.com/custom/images/generations" hint="Optional. When set, this takes precedence over Base URL." />
        <Select label="Authentication" value={form.authHeader} onChange={(e) => setForm({ ...form, authHeader: e.target.value })} options={[
          { value: "bearer", label: "Authorization: Bearer" },
          { value: "x-api-key", label: "x-api-key" },
          { value: "api-key", label: "api-key" },
        ]} />
        <Input label="Model IDs" value={form.models} onChange={(e) => setForm({ ...form, models: e.target.value })} placeholder="model-a, model-b" hint="Comma-separated upstream model IDs." />
        <div>
          <label className="mb-1 block text-xs text-text-muted">Custom headers</label>
          <textarea className="min-h-24 w-full rounded-lg border border-border bg-background px-3 py-2 font-mono text-sm focus:border-primary focus:outline-none" value={form.headers} onChange={(e) => setForm({ ...form, headers: e.target.value })} placeholder={"X-Organization: team-a\nX-Region: us-east"} />
          <p className="mt-1 text-xs text-text-muted">One header per line. Authorization, cookies, host, and content length are blocked; use the API key connection for secrets.</p>
        </div>
        {error && <p className="text-sm text-red-500">{error}</p>}
        <div className="flex gap-2">
          <Button fullWidth onClick={save} disabled={saving || !form.name.trim() || !form.prefix.trim() || (!form.baseUrl.trim() && !form.endpointUrl.trim())}>{saving ? "Saving..." : "Save"}</Button>
          <Button fullWidth variant="ghost" onClick={onClose}>Cancel</Button>
        </div>
      </div>
    </Modal>
  );
}

CustomMediaProviderForm.propTypes = {
  kind: PropTypes.string.isRequired,
  node: PropTypes.object,
  onClose: PropTypes.func.isRequired,
  onSaved: PropTypes.func.isRequired,
};

export default function CustomMediaProviderModal({ isOpen, kind, node, onClose, onSaved }) {
  if (!isOpen) return null;
  return <CustomMediaProviderForm key={`${node?.id || "new"}-${kind}`} kind={kind} node={node} onClose={onClose} onSaved={onSaved} />;
}

CustomMediaProviderModal.propTypes = {
  isOpen: PropTypes.bool.isRequired,
  kind: PropTypes.string.isRequired,
  node: PropTypes.object,
  onClose: PropTypes.func.isRequired,
  onSaved: PropTypes.func.isRequired,
};
