// Shared validation for the per-API-key routing pins (forceProvider /
// forceModel). Used by POST /api/keys and PUT /api/keys/[id]. The admin API
// path (adminPolicy.js) has its own equivalent checks and the same messages.

export const FORCE_PIN_MAX_LENGTH = 256;

const FORCE_PIN_FIELDS = ["forceProvider", "forceModel"];
// Any whitespace or ASCII/Unicode control character makes a pin unusable as a
// model or provider id, so reject it instead of silently storing it.
function hasInvalidChars(value) {
  for (const character of value) {
    const code = character.codePointAt(0);
    if (code <= 0x20 || (code >= 0x7f && code <= 0xa0) || /\s/.test(character)) return true;
  }
  return false;
}

function toList(value) {
  if (Array.isArray(value)) return value.map((item) => String(item).trim()).filter(Boolean);
  if (typeof value === "string") return value.split(",").map((item) => item.trim()).filter(Boolean);
  return [];
}

function normalizePin(value, field) {
  // null clears the pin, the same as an empty string.
  if (value === null) return { value: "" };
  if (typeof value !== "string") return { error: `${field} must be a string` };
  const pin = value.trim();
  if (pin.length > FORCE_PIN_MAX_LENGTH) return { error: `${field} must be ${FORCE_PIN_MAX_LENGTH} characters or fewer` };
  if (hasInvalidChars(pin)) return { error: `${field} must not contain whitespace or control characters` };
  return { value: pin };
}

// A bare pin such as `gpt-4o` resolves to `<provider>/gpt-4o` at request time
// and enforcement checks BOTH the requested id and the resolved one against
// blockedModels. So a blocked entry `openai/gpt-4o` also covers the bare pin.
// The reverse does not hold (a prefixed pin never matches a bare blocked entry).
export function modelPinConflicts(pin, blocked) {
  if (!pin) return [];
  const bare = !pin.includes("/");
  return blocked.filter((entry) => entry === pin || (bare && entry.endsWith(`/${pin}`)));
}

function conflictKeys(state) {
  const forceModel = String(state?.forceModel || "").trim();
  const forceProvider = String(state?.forceProvider || "").trim();
  const keys = new Set();
  for (const entry of modelPinConflicts(forceModel, toList(state?.blockedModels))) keys.add(`model|${forceModel}|${entry}`);
  if (forceProvider && toList(state?.blockedProviders).includes(forceProvider)) keys.add(`provider|${forceProvider}`);
  return keys;
}

/**
 * Validate and normalise the pins in a create/update body.
 *
 * @param {object} input     Fields supplied by the caller (undefined = absent).
 * @param {object} [effective] Post-update state used for the blocked-list
 *   conflict check: { forceProvider, forceModel, blockedModels, blockedProviders }.
 *   For a create these equal the supplied values; for an update the caller
 *   merges the existing key with the supplied fields.
 * @param {object|null} [stored] Pre-update state (same shape) for an update.
 *   A pin/blocked-list conflict that already exists in the stored key does not
 *   block the edit, so a user can still fix or clear it, but a conflict the
 *   update newly introduces is rejected.
 * @returns {{ values: object, error: string|null }} `values` holds only the pin
 *   fields that were supplied, trimmed ("" clears a pin).
 */
export function validateForcePins(input = {}, effective = input, stored = null) {
  const values = {};
  for (const field of FORCE_PIN_FIELDS) {
    if (input[field] === undefined) continue;
    const result = normalizePin(input[field], field);
    if (result.error) return { values: {}, error: result.error };
    values[field] = result.value;
  }

  const after = {
    forceModel: values.forceModel !== undefined ? values.forceModel : String(effective.forceModel || "").trim(),
    forceProvider: values.forceProvider !== undefined ? values.forceProvider : String(effective.forceProvider || "").trim(),
    blockedModels: effective.blockedModels,
    blockedProviders: effective.blockedProviders,
  };
  const before = stored ? conflictKeys(stored) : new Set();
  for (const key of conflictKeys(after)) {
    if (before.has(key)) continue;
    return { values: {}, error: key.startsWith("model|") ? "forceModel must not be in blockedModels" : "forceProvider must not be in blockedProviders" };
  }
  return { values, error: null };
}

/**
 * True when the pair (forceProvider, forceModel) after an update differs from
 * the stored pair, i.e. it needs the pair-consistency check. Unchanged pairs
 * are skipped so a key that already holds a stale pair stays editable.
 */
export function forcePairChanged(values, stored) {
  const storedModel = String(stored?.forceModel || "").trim();
  const storedProvider = String(stored?.forceProvider || "").trim();
  const model = values.forceModel !== undefined ? values.forceModel : storedModel;
  const provider = values.forceProvider !== undefined ? values.forceProvider : storedProvider;
  return model !== storedModel || provider !== storedProvider;
}
