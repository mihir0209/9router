"use client";

import { useEffect, useState } from "react";
import PropTypes from "prop-types";
import Card from "@/shared/components/Card";
import { CardSkeleton } from "@/shared/components";

const fmt = (n) => new Intl.NumberFormat().format(n || 0);
const fmtCost = (n) => `$${(n || 0).toFixed(2)}`;

function resetLabel(resetAt) {
  if (!resetAt) return "";
  const secs = Math.max(0, Math.ceil((new Date(resetAt).getTime() - Date.now()) / 1000));
  return secs > 0 ? `resets in ${secs}s` : "resetting…";
}

function KeyCard({ entry }) {
  const { usage = {}, rateLimit = {} } = entry;
  const tokens = (usage.promptTokens || 0) + (usage.completionTokens || 0);
  const pct = rateLimit.limited && rateLimit.limit > 0 && rateLimit.used != null
    ? Math.min(100, Math.round((rateLimit.used / rateLimit.limit) * 100))
    : null;
  const barColor = pct == null ? "" : pct >= 90 ? "bg-error" : pct >= 70 ? "bg-warning" : "bg-success";

  return (
    <Card className="flex min-w-0 flex-col gap-2 px-3 py-3 sm:px-4">
      <div className="flex min-w-0 items-center justify-between gap-2">
        <span className="truncate text-sm font-bold" title={entry.name || entry.maskedKey}>
          {entry.name || entry.maskedKey || entry.id}
        </span>
        <span className={`shrink-0 rounded-full px-2 py-0.5 text-[10px] font-semibold ${entry.isActive ? "bg-success/15 text-success" : "bg-error/15 text-error"}`}>
          {entry.isActive ? "active" : "disabled"}
        </span>
      </div>
      <span className="truncate font-mono text-[11px] text-text-muted" title={entry.maskedKey}>{entry.maskedKey}</span>

      <div className="grid grid-cols-3 gap-2 text-center">
        <div>
          <div className="text-[10px] uppercase font-semibold text-text-muted">Requests</div>
          <div className="text-sm font-bold">{fmt(usage.requests)}</div>
        </div>
        <div>
          <div className="text-[10px] uppercase font-semibold text-text-muted">Tokens</div>
          <div className="text-sm font-bold">{fmt(tokens)}</div>
        </div>
        <div>
          <div className="text-[10px] uppercase font-semibold text-text-muted">Cost</div>
          <div className="text-sm font-bold">~{fmtCost(usage.cost)}</div>
        </div>
      </div>

      {rateLimit.limited ? (
        <div>
          <div className="flex items-center justify-between text-[11px] text-text-muted">
            <span>Rate: {rateLimit.used ?? "–"}/{rateLimit.limit} per {rateLimit.windowSeconds || 60}s</span>
            <span className="font-semibold text-text-main">
              {rateLimit.remaining != null ? `${rateLimit.remaining} left` : "unknown"} {resetLabel(rateLimit.resetAt) && `· ${resetLabel(rateLimit.resetAt)}`}
            </span>
          </div>
          {pct != null && (
            <div className="mt-1 h-1.5 w-full overflow-hidden rounded-full bg-surface-2">
              <div className={`h-full rounded-full ${barColor}`} style={{ width: `${pct}%` }} />
            </div>
          )}
        </div>
      ) : (
        <div className="text-[11px] text-text-muted">No per-minute rate limit</div>
      )}

      {(entry.tokenLimitDaily > 0 || entry.tokenLimitMonthly > 0) && (
        <div className="text-[11px] text-text-muted">
          {entry.tokenLimitDaily > 0 && <span>Daily cap: {fmt(entry.tokenLimitDaily)} tokens · </span>}
          {entry.tokenLimitMonthly > 0 && <span>Monthly cap: {fmt(entry.tokenLimitMonthly)} tokens</span>}
        </div>
      )}
      {(entry.allowedModels || entry.blockedModels) && (
        <div className="truncate text-[11px] text-text-muted" title={`allow: ${entry.allowedModels || "—"} / block: ${entry.blockedModels || "—"}`}>
          Models: {entry.allowedModels ? `allow ${entry.allowedModels}` : "all"} {entry.blockedModels ? `· block ${entry.blockedModels}` : ""}
        </div>
      )}
    </Card>
  );
}

KeyCard.propTypes = {
  entry: PropTypes.object.isRequired,
};

export default function ApiKeysTab({ period }) {
  const [data, setData] = useState(null); // { period, payload } | { period, error }

  useEffect(() => {
    let cancelled = false;
    fetch(`/api/usage/api-keys?period=${encodeURIComponent(period || "7d")}`)
      .then((r) => r.json())
      .then((json) => { if (!cancelled) setData(json?.error ? { period, error: json.error } : { period, payload: json }); })
      .catch((e) => { if (!cancelled) setData({ period, error: e?.message || "Failed to load" }); });
    return () => { cancelled = true; };
  }, [period]);

  if (!data || data.period !== period) return <CardSkeleton />;
  if (data.error) return <Card className="px-4 py-6 text-center text-sm text-error">{data.error}</Card>;

  const { summary = {}, keys = [] } = data.payload || {};
  return (
    <div className="flex min-w-0 flex-col gap-4">
      <div className="grid min-w-0 grid-cols-2 gap-3 sm:grid-cols-4 sm:gap-4">
        <Card className="flex min-w-0 flex-col items-center text-center gap-1 px-3 py-3">
          <span className="text-text-muted text-xs uppercase font-semibold">Keys</span>
          <span className="text-lg font-bold">{fmt(summary.activeKeys)}/{fmt(summary.totalKeys)}</span>
        </Card>
        <Card className="flex min-w-0 flex-col items-center text-center gap-1 px-3 py-3">
          <span className="text-text-muted text-xs uppercase font-semibold">Requests</span>
          <span className="text-lg font-bold">{fmt(summary.totalRequests)}</span>
        </Card>
        <Card className="flex min-w-0 flex-col items-center text-center gap-1 px-3 py-3">
          <span className="text-text-muted text-xs uppercase font-semibold">Tokens</span>
          <span className="text-lg font-bold text-primary">{fmt(summary.totalTokens)}</span>
        </Card>
        <Card className="flex min-w-0 flex-col items-center text-center gap-1 px-3 py-3">
          <span className="text-text-muted text-xs uppercase font-semibold">Est. Cost</span>
          <span className="text-lg font-bold text-warning">~{fmtCost(summary.totalCost)}</span>
        </Card>
      </div>
      {keys.length === 0
        ? <Card className="px-4 py-6 text-center text-sm text-text-muted">No API keys yet</Card>
        : (
          <div className="grid min-w-0 grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-3">
            {keys.map((entry) => <KeyCard key={entry.id} entry={entry} />)}
          </div>
        )}
    </div>
  );
}

ApiKeysTab.propTypes = {
  period: PropTypes.string,
};
