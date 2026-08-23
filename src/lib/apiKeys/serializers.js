export function serializeApiKey(key, { includeSecret = false } = {}) {
  if (!key) return null;
  const serialized = { ...key };
  delete serialized.byokCredentialsCiphertext;
  if (!includeSecret) {
    // If key has ownerRef (WebClaw workspace key contract), omit the secret
    if (key.ownerRef) {
      delete serialized.key;
    }
  }
  return serialized;
}

export function serializeApiKeys(keys, options) {
  return keys.map((key) => serializeApiKey(key, options));
}
