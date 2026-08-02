/**
 * Backup Merge — pure data module, zero imports.
 * Runs entirely client-side. Merges two backup objects with conflict strategies.
 *
 * Backup format: top-level keys are table names.
 *   - Arrays (providerConnections, proxyPools, etc.) — multi-row tables
 *   - Objects (settings, modelAliases, etc.) — singletons / KV maps
 *
 * Strategies:
 *   - newer-wins: later timestamp wins (default)
 *   - older-wins: earlier timestamp wins
 *   - base-wins: keep base value on conflict
 *   - incoming-wins: overwrite with incoming value on conflict
 */

const TIMESTAMP_FIELDS = ["createdAt", "updatedAt", "ts", "timestamp", "lastCheck", "lastSuccess", "lastFailure"];

function pickTimestamp(obj) {
  if (!obj || typeof obj !== "object") return 0;
  for (const f of TIMESTAMP_FIELDS) {
    const v = obj[f];
    if (v) {
      const t = new Date(v).getTime();
      if (!isNaN(t)) return t;
    }
  }
  return 0;
}

function isTableRow(obj) {
  return obj && typeof obj === "object" && (obj.id || obj.key);
}

/**
 * Merge an array table (multi-row).
 */
function mergeArrayTable(base = [], incoming = [], strategy, keyFn) {
  const stats = { added: 0, updated: 0, unchanged: 0, conflict: 0 };
  const baseMap = new Map();
  for (const row of base) {
    const k = keyFn(row);
    if (k) baseMap.set(k, row);
  }

  const result = [...base];

  for (const incRow of incoming) {
    const k = keyFn(incRow);
    if (!k) continue;

    const baseRow = baseMap.get(k);
    if (!baseRow) {
      result.push(incRow);
      stats.added++;
      continue;
    }

    const baseTs = pickTimestamp(baseRow);
    const incTs = pickTimestamp(incRow);

    if (JSON.stringify(baseRow) === JSON.stringify(incRow)) {
      stats.unchanged++;
      continue;
    }

    stats.conflict++;
    let winner;
    switch (strategy) {
      case "older-wins":
        winner = baseTs <= incTs ? baseRow : incRow;
        break;
      case "base-wins":
        winner = baseRow;
        break;
      case "incoming-wins":
        winner = incRow;
        break;
      case "newer-wins":
      default:
        winner = baseTs >= incTs ? baseRow : incRow;
        break;
    }

    const idx = result.findIndex(r => keyFn(r) === k);
    if (idx >= 0) {
      result[idx] = winner;
      stats.updated++;
    }
  }

  return { result, stats };
}

/**
 * Merge an object table (singleton or KV map).
 */
function mergeObjectTable(base = {}, incoming = {}, strategy) {
  const stats = { added: 0, updated: 0, unchanged: 0, conflict: 0 };
  const result = { ...base };

  for (const [key, incVal] of Object.entries(incoming)) {
    const baseVal = base[key];

    if (baseVal === undefined) {
      result[key] = incVal;
      stats.added++;
      continue;
    }

    if (JSON.stringify(baseVal) === JSON.stringify(incVal)) {
      stats.unchanged++;
      continue;
    }

    stats.conflict++;
    switch (strategy) {
      case "older-wins":
        result[key] = baseVal;
        break;
      case "base-wins":
        result[key] = baseVal;
        break;
      case "incoming-wins":
        result[key] = incVal;
        break;
      case "newer-wins":
      default:
        result[key] = incVal;
        break;
    }
  }

  return { result, stats };
}

/**
 * Validate a backup object. Returns { valid, errors }.
 */
export function validateBackup(backup) {
  const errors = [];
  if (!backup || typeof backup !== "object") {
    errors.push("Backup must be a non-null object");
    return { valid: false, errors };
  }
  if (Array.isArray(backup)) {
    errors.push("Backup must be an object with table names as keys, not an array");
    return { valid: false, errors };
  }
  const keys = Object.keys(backup);
  if (keys.length === 0) {
    errors.push("Backup is empty (no tables found)");
  }
  for (const key of keys) {
    const val = backup[key];
    if (val === null || val === undefined) {
      errors.push(`Table "${key}" is null or undefined`);
    }
  }
  return { valid: errors.length === 0, errors };
}

/**
 * Compute merge stats preview (without actually merging).
 */
export function previewMerge(base, incoming, strategy = "newer-wins") {
  const stats = { added: 0, updated: 0, unchanged: 0, conflict: 0, tables: 0 };
  const allKeys = new Set([...Object.keys(base || {}), ...Object.keys(incoming || {})]);

  for (const key of allKeys) {
    const baseVal = base?.[key];
    const incVal = incoming?.[key];

    if (incVal === undefined || incVal === null) continue;
    if (baseVal === undefined || baseVal === null) {
      stats.added++;
      stats.tables++;
      continue;
    }

    const isArray = Array.isArray(incVal);
    const keyFn = isArray
      ? (row) => row?.id || row?.key || row?.name || JSON.stringify(row)
      : null;

    const { stats: ts } = isArray
      ? mergeArrayTable(baseVal, incVal, strategy, keyFn)
      : mergeObjectTable(baseVal, incVal, strategy);

    stats.added += ts.added;
    stats.updated += ts.updated;
    stats.unchanged += ts.unchanged;
    stats.conflict += ts.conflict;
    stats.tables++;
  }

  return stats;
}

/**
 * Merge two backup objects. Returns { merged, stats, errors }.
 */
export function mergeBackups(base, incoming, strategy = "newer-wins") {
  const errors = [];

  const baseCheck = validateBackup(base);
  if (!baseCheck.valid) errors.push(...baseCheck.errors.map(e => `Base: ${e}`));

  const incCheck = validateBackup(incoming);
  if (!incCheck.valid) errors.push(...incCheck.errors.map(e => `Incoming: ${e}`));

  if (errors.length > 0) return { merged: null, stats: null, errors };

  const merged = {};
  const stats = { added: 0, updated: 0, unchanged: 0, conflict: 0, tables: 0 };
  const allKeys = new Set([...Object.keys(base), ...Object.keys(incoming)]);

  for (const key of allKeys) {
    const baseVal = base[key];
    const incVal = incoming[key];

    if (incVal === undefined || incVal === null) {
      merged[key] = baseVal;
      continue;
    }
    if (baseVal === undefined || baseVal === null) {
      merged[key] = incVal;
      stats.added++;
      stats.tables++;
      continue;
    }

    const isArray = Array.isArray(incVal);
    const keyFn = isArray
      ? (row) => row?.id || row?.key || row?.name || JSON.stringify(row)
      : null;

    const { result, stats: ts } = isArray
      ? mergeArrayTable(baseVal, incVal, strategy, keyFn)
      : mergeObjectTable(baseVal, incVal, strategy);

    merged[key] = result;
    stats.added += ts.added;
    stats.updated += ts.updated;
    stats.unchanged += ts.unchanged;
    stats.conflict += ts.conflict;
    stats.tables++;
  }

  return { merged, stats, errors: [] };
}
