export function serializeApiKey(key, { includeSecret = false } = {}) {
  if (!key) return null;
  const serialized = { ...key };
  delete serialized.key;
  delete serialized.byokCredentialsCiphertext;
  if (includeSecret) serialized.key = key.key;
  return serialized;
}

export function serializeApiKeys(keys, options) {
  return keys.map((key) => serializeApiKey(key, options));
}
