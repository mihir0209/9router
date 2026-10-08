/**
 * Model Access Control (ACL) matcher (upstream #4538, adapted).
 *
 * Our key semantics: an EMPTY allowlist (or one containing "*") means
 * unrestricted. Upstream's original returns false for empty; that is
 * intentionally inverted here so existing keys keep working.
 *
 * Matching, in order, per allowlist entry (case-insensitive):
 * 1. Exact match against the raw request id, the model slug, the provider,
 *    or the full provider/model pair.
 * 2. Wildcard patterns ("openai/*", "*gpt*", "deepseek*").
 * 3. Family/prefix match with a boundary ("deepseek" matches "deepseek-chat"
 *    but not "deepseekx").
 */

function escapeRegex(str) {
  return str.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function patternToRegex(pattern) {
  return new RegExp(`^${pattern.split("*").map(escapeRegex).join(".*")}$`, "i");
}

function parseAllowlist(allowed) {
  if (Array.isArray(allowed)) return allowed.filter((e) => typeof e === "string").map((e) => e.trim()).filter(Boolean);
  if (typeof allowed !== "string") return [];
  const trimmed = allowed.trim();
  if (!trimmed) return [];
  try {
    const parsed = JSON.parse(trimmed);
    if (Array.isArray(parsed)) return parsed.filter((e) => typeof e === "string").map((e) => String(e).trim()).filter(Boolean);
  } catch { /* comma-separated below */ }
  return trimmed.split(",").map((s) => s.trim()).filter(Boolean);
}

function splitParts(requestedModel, modelInfo) {
  const req = String(requestedModel || "").trim();
  const lower = req.toLowerCase();
  const provider = String(modelInfo?.provider || (req.includes("/") ? req.slice(0, req.indexOf("/")) : "")).toLowerCase().trim();
  const model = String(modelInfo?.model || (req.includes("/") ? req.slice(req.indexOf("/") + 1) : req)).toLowerCase().trim();
  return { req, lower, provider, model, full: provider && model ? `${provider}/${model}` : "" };
}

function matchPrefixWithBoundary(target, prefix) {
  if (!target.startsWith(prefix)) return false;
  const next = target.slice(prefix.length, prefix.length + 1);
  return !next || /[/\-_.:0-9]/.test(next);
}

export function isModelAllowed(allowed, requestedModel, modelInfo = null) {
  const list = parseAllowlist(allowed);
  // Empty allowlist (or "*") = unrestricted. This is OUR contract;
  // upstream's original treats empty as deny.
  if (list.length === 0 || list.includes("*")) return true;

  const { req, lower, provider, model, full } = splitParts(requestedModel, modelInfo);
  if (!req) return false;

  for (const raw of list) {
    const p = raw.toLowerCase();
    if (!p || p === "*") return true;
    const itemModel = p.includes("/") ? p.slice(p.indexOf("/") + 1) : p;
    const itemProvider = p.includes("/") ? p.slice(0, p.indexOf("/")) : "";

    if (p === lower || p === model || (full && p === full) || (provider && p === provider)) return true;
    if (provider && itemProvider && itemProvider === provider && (itemModel === model || itemModel === lower)) return true;
    if (p.includes("*")) {
      const re = patternToRegex(p);
      if (re.test(req) || (model && re.test(model)) || (full && re.test(full))) return true;
      continue;
    }
    if (matchPrefixWithBoundary(lower, p) || (model && matchPrefixWithBoundary(model, p))) return true;
  }
  return false;
}

/**
 * Filter catalogue entries to those the allowlist permits.
 * Entries expose { id } (combos additionally carry comboModels, matched by name).
 */
export function filterAllowedModels(modelsList, allowed) {
  const list = parseAllowlist(allowed);
  if (list.length === 0 || list.includes("*")) return modelsList;
  if (!Array.isArray(modelsList)) return [];
  return modelsList.filter((entry) => {
    const id = typeof entry === "string" ? entry : (entry?.id || entry?.model || entry?.name);
    return isModelAllowed(list, id);
  });
}
