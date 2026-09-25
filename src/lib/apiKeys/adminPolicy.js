import { isValidIpRule } from "@/lib/auth/clientIp.js";

const MAX_SAFE_VALUE = Number.MAX_SAFE_INTEGER;
const ISO_DATE = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{1,9})?(?:Z|[+-]\d{2}:\d{2})$/;
const LIST_FIELDS = ["allowedModels", "blockedModels", "allowedIPs", "blockedIPs", "allowedProviders", "blockedProviders", "allowedEndpoints", "blockedEndpoints"];
const STRING_FIELDS = ["name", "description", "machineId"];
const NUMBER_FIELDS = ["rateLimitRequests", "rateLimitWindowSeconds", "maxConcurrentRequests", "tokenLimitDaily", "tokenLimitMonthly", "budgetUsd", "budgetResetHours"];

function normalizeString(value, field, { required = false, maxLength } = {}) {
  if (value === undefined) {
    return required ? { error: `${field} is required` } : { value: undefined };
  }
  if (typeof value !== "string") return { error: `${field} must be a string` };
  const normalized = value.trim();
  if (required && !normalized) return { error: `${field} is required` };
  if (maxLength && normalized.length > maxLength) return { error: `${field} must be ${maxLength} characters or fewer` };
  return { value: normalized };
}

function normalizeList(value, field) {
  if (value === undefined || value === null) return { value: value === null ? "" : undefined };
  if (typeof value === "string") {
    return { value: [...new Set(value.split(",").map((item) => item.trim()).filter(Boolean))].join(",") };
  }
  if (!Array.isArray(value)) return { error: `${field} must be an array or CSV string` };
  if (value.some((item) => typeof item !== "string")) return { error: `${field} must contain only strings` };
  return { value: [...new Set(value.map((item) => item.trim()).filter(Boolean))].join(",") };
}

function normalizeNumber(value, field, { positive = false, integer = false } = {}) {
  if (value === undefined) return { value: undefined };
  if (typeof value !== "number" || !Number.isFinite(value)) return { error: `${field} must be a number` };
  if (value < 0 || (positive && value <= 0)) return { error: `${field} must be ${positive ? "a positive" : "a non-negative"} number` };
  if (value > MAX_SAFE_VALUE) return { error: `${field} must be at most ${MAX_SAFE_VALUE}` };
  if (integer && !Number.isSafeInteger(value)) return { error: `${field} must be a safe integer` };
  return { value };
}

function normalizeDate(value, field, { future = false, isoOnly = false } = {}) {
  if (value === undefined) return { value: undefined };
  if (value === null || value === "") return { value: "" };
  if (typeof value !== "string" || (isoOnly && !ISO_DATE.test(value.trim()))) return { error: `${field} must be a valid ${isoOnly ? "ISO " : ""}date` };
  const date = new Date(value.trim());
  if (Number.isNaN(date.getTime())) return { error: `${field} must be a valid date` };
  if (future && date.getTime() <= Date.now()) return { error: `${field} must be in the future` };
  return { value: date.toISOString() };
}

function normalizeCustomKey(value) {
  if (value === undefined || value === null) return { value: value === null ? "" : undefined };
  if (typeof value !== "string") return { error: "key must be a string" };
  const key = value.trim();
  if (!key) return { value: "" };
  if (key.length > 512) return { error: "key must be 512 characters or fewer" };
  for (const character of key) {
    const code = character.codePointAt(0);
    if (code < 0x21 || code > 0x7e) return { error: "key must contain only printable characters without whitespace" };
  }
  return { value: key };
}

export function validateOwnerRef(value) {
  return normalizeString(value, "ownerRef", { required: true });
}

export function validatePolicy(input, { partial = false } = {}) {
  if (!input || typeof input !== "object" || Array.isArray(input)) return { data: {}, error: "Request body must be an object" };
  const data = {};
  const owner = validateOwnerRef(input.ownerRef);
  if (owner.error) return { data, error: owner.error };
  data.ownerRef = owner.value;

  for (const field of STRING_FIELDS) {
    if (input[field] === undefined) continue;
    const result = normalizeString(input[field], field, { required: !partial && field === "name" });
    if (result.error) return { data, error: result.error };
    data[field] = result.value;
  }
  if (!partial && !data.name) return { data, error: "name is required" };

  if (input.isActive !== undefined) {
    if (typeof input.isActive !== "boolean") return { data, error: "isActive must be a boolean" };
    data.isActive = input.isActive;
  }
  if (input.key !== undefined) {
    const result = normalizeCustomKey(input.key);
    if (result.error) return { data, error: result.error };
    if (result.value) data.key = result.value;
  }

  for (const field of NUMBER_FIELDS) {
    if (input[field] === undefined) continue;
    const result = normalizeNumber(input[field], field, {
      positive: field === "rateLimitWindowSeconds" || field === "budgetResetHours",
      integer: field !== "budgetUsd",
    });
    if (result.error) return { data, error: result.error };
    data[field] = result.value;
  }

  if (input.expiresAt !== undefined) {
    const result = normalizeDate(input.expiresAt, "expiresAt", { future: true, isoOnly: true });
    if (result.error) return { data, error: result.error };
    data.expiresAt = result.value;
  }
  if (input.budgetWindowStartedAt !== undefined) {
    const result = normalizeDate(input.budgetWindowStartedAt, "budgetWindowStartedAt");
    if (result.error) return { data, error: result.error };
    data.budgetWindowStartedAt = result.value;
  }
  if (input.billingMode !== undefined) {
    if (typeof input.billingMode !== "string" || !["credits", "byok"].includes(input.billingMode.trim())) return { data, error: "billingMode must be credits or byok" };
    data.billingMode = input.billingMode.trim();
  }

  if (input.metadata !== undefined) {
    if (input.metadata === null || typeof input.metadata !== "object" || Array.isArray(input.metadata)) return { data, error: "metadata must be an object" };
    const serialized = JSON.stringify(input.metadata);
    if (serialized.length > 8192) return { data, error: "metadata must be 8192 characters or fewer" };
    data.metadata = input.metadata;
  }

  for (const field of LIST_FIELDS) {
    if (input[field] === undefined) continue;
    const result = normalizeList(input[field], field);
    if (result.error) return { data, error: result.error };
    if (result.value !== undefined) data[field] = result.value;
  }
  for (const field of ["allowedIPs", "blockedIPs"]) {
    if (data[field] && data[field].split(",").some((item) => !isValidIpRule(item))) return { data, error: `${field} contains an invalid IP or CIDR` };
  }
  const modelConflict = data.allowedModels && data.blockedModels && data.allowedModels.split(",").some((item) => data.blockedModels.split(",").includes(item));
  if (modelConflict) return { data, error: "allowedModels and blockedModels must not overlap" };
  const ipConflict = data.allowedIPs && data.blockedIPs && data.allowedIPs.split(",").some((item) => data.blockedIPs.split(",").includes(item));
  if (ipConflict) return { data, error: "allowedIPs and blockedIPs must not overlap" };

  return { data };
}

export function validateListConflicts(policy) {
  const allowedModels = policy.allowedModels || "";
  const blockedModels = policy.blockedModels || "";
  if (allowedModels.split(",").filter(Boolean).some((item) => blockedModels.split(",").includes(item))) return "allowedModels and blockedModels must not overlap";
  const allowedIPs = policy.allowedIPs || "";
  const blockedIPs = policy.blockedIPs || "";
  if (allowedIPs.split(",").filter(Boolean).some((item) => blockedIPs.split(",").includes(item))) return "allowedIPs and blockedIPs must not overlap";
  return null;
}

export function serializeAdminKey(key, { includeSecret = false } = {}) {
  if (!key) return null;
  const serialized = { ...key };
  delete serialized.byokCredentialsCiphertext;
  if (!includeSecret) delete serialized.key;
  return serialized;
}
