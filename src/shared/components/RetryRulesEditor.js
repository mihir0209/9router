"use client";

import { useEffect, useState } from "react";
import PropTypes from "prop-types";
import { Button, Input } from "@/shared/components";

const EMPTY_RULE = {
  text: "",
  status: "",
  cooldownSec: "30",
  backoff: false,
  retryHttp: false,
  attempts: "1",
  delayMs: "2000",
  shouldFallback: true,
};

function emptyForm() {
  return {
    rules: [],
    status502: "",
    delay502: "",
    status503: "",
    delay503: "",
    status429: "",
    delay429: "",
  };
}

function toFormState(value) {
  if (!value || typeof value !== "object") return emptyForm();
  const http = value.httpRetry || value.retry || {};
  const r502 = http[502] || http["502"] || {};
  const r503 = http[503] || http["503"] || {};
  const r429 = http[429] || http["429"] || {};
  const rules = Array.isArray(value.errorRules || value.rules)
    ? (value.errorRules || value.rules).map((r) => ({
        text: r.text || "",
        status: r.status != null ? String(r.status) : "",
        cooldownSec: r.cooldownMs != null ? String(Math.round(Number(r.cooldownMs) / 1000)) : "30",
        backoff: !!r.backoff,
        retryHttp: !!(r.retryHttp || r.httpRetry),
        attempts: r.attempts != null ? String(r.attempts) : "1",
        delayMs: r.delayMs != null ? String(r.delayMs) : "2000",
        shouldFallback: r.shouldFallback !== false,
      }))
    : [];
  return {
    rules,
    status502: r502.attempts != null ? String(r502.attempts) : "",
    delay502: r502.delayMs != null ? String(r502.delayMs) : "",
    status503: r503.attempts != null ? String(r503.attempts) : "",
    delay503: r503.delayMs != null ? String(r503.delayMs) : "",
    status429: r429.attempts != null ? String(r429.attempts) : "",
    delay429: r429.delayMs != null ? String(r429.delayMs) : "",
  };
}

function fromFormState(form) {
  const httpRetry = {};
  const setHttp = (code, attemptsStr, delayStr) => {
    if (attemptsStr === "" || attemptsStr == null) return;
    const attempts = parseInt(attemptsStr, 10);
    const delayMs = parseInt(delayStr, 10);
    if (!Number.isFinite(attempts)) return;
    httpRetry[code] = {
      attempts,
      delayMs: Number.isFinite(delayMs) && delayMs >= 0 ? delayMs : 0,
    };
  };
  setHttp(502, form.status502, form.delay502);
  setHttp(503, form.status503, form.delay503);
  setHttp(429, form.status429, form.delay429);

  const errorRules = (form.rules || [])
    .filter((r) => (r.text && r.text.trim()) || r.status)
    .map((r) => {
      const rule = {};
      if (r.text?.trim()) rule.text = r.text.trim().toLowerCase();
      if (r.status) {
        const s = parseInt(r.status, 10);
        if (Number.isFinite(s)) rule.status = s;
      }
      if (r.backoff) rule.backoff = true;
      else if (r.cooldownSec !== "" && r.cooldownSec != null) {
        const sec = parseFloat(r.cooldownSec);
        if (Number.isFinite(sec) && sec >= 0) rule.cooldownMs = Math.round(sec * 1000);
      }
      if (r.shouldFallback === false) rule.shouldFallback = false;
      if (r.retryHttp) {
        rule.retryHttp = true;
        const attempts = parseInt(r.attempts, 10);
        rule.attempts = Number.isFinite(attempts) && attempts > 0 ? attempts : 1;
        const delayMs = parseInt(r.delayMs, 10);
        rule.delayMs = Number.isFinite(delayMs) && delayMs >= 0 ? delayMs : 2000;
      }
      return rule;
    });

  if (!errorRules.length && Object.keys(httpRetry).length === 0) return null;
  return { httpRetry, errorRules };
}

/**
 * Always-visible editor for provider plain-text + HTTP retry rules.
 * Match: lower = body.toLowerCase(); if (lower.includes("text")) …
 * Local draft + Save (does not autosave on every keystroke).
 */
export default function RetryRulesEditor({ value, onChange, compact = false }) {
  const [form, setForm] = useState(() => toFormState(value));
  const [saving, setSaving] = useState(false);
  const [dirty, setDirty] = useState(false);
  const [savedFlash, setSavedFlash] = useState(false);

  useEffect(() => {
    if (!dirty) setForm(toFormState(value));
  }, [value, dirty]);

  const patch = (partial) => {
    setForm((prev) => ({ ...prev, ...partial }));
    setDirty(true);
  };

  const updateRule = (idx, partial) => {
    setForm((prev) => ({
      ...prev,
      rules: prev.rules.map((r, i) => (i === idx ? { ...r, ...partial } : r)),
    }));
    setDirty(true);
  };

  const addRule = () => {
    setForm((prev) => ({ ...prev, rules: [...prev.rules, { ...EMPTY_RULE }] }));
    setDirty(true);
  };

  const removeRule = (idx) => {
    setForm((prev) => ({ ...prev, rules: prev.rules.filter((_, i) => i !== idx) }));
    setDirty(true);
  };

  const handleSave = async () => {
    setSaving(true);
    try {
      await onChange(fromFormState(form));
      setDirty(false);
      setSavedFlash(true);
      setTimeout(() => setSavedFlash(false), 1500);
    } finally {
      setSaving(false);
    }
  };

  const handleClear = async () => {
    setForm(emptyForm());
    setDirty(false);
    setSaving(true);
    try {
      await onChange(null);
      setSavedFlash(true);
      setTimeout(() => setSavedFlash(false), 1500);
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className={`flex flex-col gap-3 ${compact ? "" : "border border-border rounded-lg p-3 sm:p-4 bg-black/[0.02] dark:bg-white/[0.02]"}`}>
      <div className="flex flex-col gap-2 sm:flex-row sm:items-start sm:justify-between">
        <div className="min-w-0">
          <div className="text-sm font-semibold">Error retry rules</div>
          <p className="text-xs text-text-muted mt-0.5">
            On provider error: <code className="text-[11px]">lower = body.toLowerCase()</code>
            {" "}then <code className="text-[11px]">if (lower.includes(&quot;your text&quot;))</code>
            {" "}→ same-account retry and/or account fallback.
          </p>
        </div>
        <div className="flex flex-wrap items-center gap-2 shrink-0">
          {savedFlash && <span className="text-xs text-green-600">Saved</span>}
          {(value || dirty) && (
            <Button size="sm" variant="ghost" onClick={handleClear} disabled={saving}>
              Clear
            </Button>
          )}
          <Button size="sm" onClick={handleSave} disabled={saving || !dirty}>
            {saving ? "Saving..." : dirty ? "Save rules" : "Saved"}
          </Button>
        </div>
      </div>

      <div className="flex flex-col gap-2">
        <div className="text-xs font-medium">Plain-text match (substring)</div>
        {form.rules.length === 0 && (
          <p className="text-xs text-text-muted">
            No custom text rules yet. Click <strong>Add text rule</strong> — e.g.{" "}
            <em>insufficient tokens</em>, <em>rate limit</em>, <em>free daily limit</em>.
          </p>
        )}
        {form.rules.map((rule, idx) => (
          <div key={idx} className="flex flex-col gap-2 border border-border rounded-md p-3 bg-background">
            <Input
              label="Text contains (case-insensitive)"
              value={rule.text}
              onChange={(e) => updateRule(idx, { text: e.target.value })}
              placeholder="e.g. insufficient tokens"
              hint='Matched as: lower.includes("…")'
            />
            <div className="grid grid-cols-1 sm:grid-cols-2 gap-2">
              <Input
                label="Or HTTP status (optional)"
                type="number"
                value={rule.status}
                onChange={(e) => updateRule(idx, { status: e.target.value })}
                placeholder="429"
              />
              {!rule.backoff && (
                <Input
                  label="Account cooldown (seconds)"
                  type="number"
                  min={0}
                  value={rule.cooldownSec}
                  onChange={(e) => updateRule(idx, { cooldownSec: e.target.value })}
                />
              )}
            </div>
            <div className="flex flex-wrap items-center gap-x-4 gap-y-2 text-xs">
              <label className="flex items-center gap-1.5 cursor-pointer">
                <input
                  type="checkbox"
                  checked={rule.backoff}
                  onChange={(e) => updateRule(idx, { backoff: e.target.checked })}
                />
                Exp. backoff (rate limits)
              </label>
              <label className="flex items-center gap-1.5 cursor-pointer">
                <input
                  type="checkbox"
                  checked={rule.retryHttp}
                  onChange={(e) => updateRule(idx, { retryHttp: e.target.checked })}
                />
                Same-account HTTP retry first
              </label>
              <label className="flex items-center gap-1.5 cursor-pointer">
                <input
                  type="checkbox"
                  checked={rule.shouldFallback !== false}
                  onChange={(e) => updateRule(idx, { shouldFallback: e.target.checked })}
                />
                Then account fallback
              </label>
              {rule.retryHttp && (
                <>
                  <div className="w-20">
                    <Input
                      label="Attempts"
                      type="number"
                      min={1}
                      value={rule.attempts}
                      onChange={(e) => updateRule(idx, { attempts: e.target.value })}
                    />
                  </div>
                  <div className="w-24">
                    <Input
                      label="Delay ms"
                      type="number"
                      min={0}
                      value={rule.delayMs}
                      onChange={(e) => updateRule(idx, { delayMs: e.target.value })}
                    />
                  </div>
                </>
              )}
              <Button size="sm" variant="ghost" onClick={() => removeRule(idx)}>
                Remove
              </Button>
            </div>
          </div>
        ))}
        <Button size="sm" variant="secondary" icon="add" onClick={addRule}>
          Add text rule
        </Button>
      </div>

      <details className="text-xs">
        <summary className="cursor-pointer text-text-muted font-medium py-1">
          Advanced: HTTP status retry attempts (502 / 503 / 429)
        </summary>
        <div className="grid grid-cols-2 sm:grid-cols-3 gap-2 mt-2">
          <Input
            label="502 attempts"
            type="number"
            min={0}
            value={form.status502}
            onChange={(e) => patch({ status502: e.target.value })}
            placeholder="default"
          />
          <Input
            label="502 delay ms"
            type="number"
            min={0}
            value={form.delay502}
            onChange={(e) => patch({ delay502: e.target.value })}
            placeholder="default"
          />
          <Input
            label="503 attempts"
            type="number"
            min={0}
            value={form.status503}
            onChange={(e) => patch({ status503: e.target.value })}
            placeholder="default"
          />
          <Input
            label="503 delay ms"
            type="number"
            min={0}
            value={form.delay503}
            onChange={(e) => patch({ delay503: e.target.value })}
            placeholder="default"
          />
          <Input
            label="429 attempts"
            type="number"
            min={0}
            value={form.status429}
            onChange={(e) => patch({ status429: e.target.value })}
            placeholder="default"
          />
          <Input
            label="429 delay ms"
            type="number"
            min={0}
            value={form.delay429}
            onChange={(e) => patch({ delay429: e.target.value })}
            placeholder="default"
          />
        </div>
      </details>
    </div>
  );
}

RetryRulesEditor.propTypes = {
  value: PropTypes.object,
  onChange: PropTypes.func.isRequired,
  compact: PropTypes.bool,
};

export { toFormState, fromFormState };
