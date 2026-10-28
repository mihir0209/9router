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

/**
 * Validate and normalise the pins in a create/update body.
 *
 * @param {object} input     Fields supplied by the caller (undefined = absent).
 * @param {object} [effective] Post-update state used for the blocked-list
 *   conflict check: { forceProvider, forceModel, blockedModels, blockedProviders }.
 *   For a create these equal the supplied values; for an update the caller
 *   merges the existing key with the supplied fields.
 * @returns {{ values: object, error: string|null }} `values` holds only the pin
 *   fields that were supplied, trimmed ("" clears a pin).
 */
export function validateForcePins(input = {}, effective = input) {
  const values = {};
  for (const field of FORCE_PIN_FIELDS) {
    if (input[field] === undefined) continue;
    const result = normalizePin(input[field], field);
    if (result.error) return { values: {}, error: result.error };
    values[field] = result.value;
  }

  const forceModel = values.forceModel !== undefined ? values.forceModel : String(effective.forceModel || "").trim();
  const forceProvider = values.forceProvider !== undefined ? values.forceProvider : String(effective.forceProvider || "").trim();
  if (forceModel && toList(effective.blockedModels).includes(forceModel)) {
    return { values: {}, error: "forceModel must not be in blockedModels" };
  }
  if (forceProvider && toList(effective.blockedProviders).includes(forceProvider)) {
    return { values: {}, error: "forceProvider must not be in blockedProviders" };
  }
  return { values, error: null };
}
