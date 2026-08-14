import { createCipheriv, createDecipheriv, randomBytes } from "node:crypto";

export const BYOK_ENVELOPE_VERSION = 1;
const ALGORITHM = "aes-256-gcm";

function getEncryptionKey() {
  const encoded = process.env.BYOK_ENC_KEY;
  if (!encoded) throw new Error("BYOK_ENC_KEY is not configured");

  const key = Buffer.from(encoded, "base64");
  if (key.length !== 32 || key.toString("base64").replace(/=+$/, "") !== encoded.replace(/=+$/, "")) {
    throw new Error("BYOK_ENC_KEY must be a base64-encoded 32-byte key");
  }
  return key;
}

function associatedData(provider, version) {
  return Buffer.from(`9router-byok:${version}:${provider}`, "utf8");
}

/**
 * Encrypt an upstream API key into a JSON envelope:
 * { v: 1, alg: "A256GCM", iv: "<base64>", ciphertext: "<base64>", tag: "<base64>" }.
 * The separately stored provider and version are authenticated as AES-GCM AAD.
 */
export function encryptByokCredential(apiKey, provider, version = BYOK_ENVELOPE_VERSION) {
  if (typeof apiKey !== "string" || !apiKey.trim()) throw new Error("apiKey is required");
  if (typeof provider !== "string" || !provider.trim()) throw new Error("provider is required");
  if (version !== BYOK_ENVELOPE_VERSION) throw new Error(`Unsupported BYOK envelope version: ${version}`);

  const iv = randomBytes(12);
  const cipher = createCipheriv(ALGORITHM, getEncryptionKey(), iv);
  cipher.setAAD(associatedData(provider, version));
  const ciphertext = Buffer.concat([cipher.update(apiKey, "utf8"), cipher.final()]);

  return JSON.stringify({
    v: version,
    alg: "A256GCM",
    iv: iv.toString("base64"),
    ciphertext: ciphertext.toString("base64"),
    tag: cipher.getAuthTag().toString("base64"),
  });
}

export function decryptByokCredential(envelopeValue, provider, version = BYOK_ENVELOPE_VERSION) {
  if (version !== BYOK_ENVELOPE_VERSION) throw new Error(`Unsupported BYOK envelope version: ${version}`);
  const envelope = typeof envelopeValue === "string" ? JSON.parse(envelopeValue) : envelopeValue;
  if (!envelope || envelope.v !== version || envelope.alg !== "A256GCM") {
    throw new Error("Invalid BYOK credential envelope");
  }

  const decipher = createDecipheriv(ALGORITHM, getEncryptionKey(), Buffer.from(envelope.iv, "base64"));
  decipher.setAAD(associatedData(provider, version));
  decipher.setAuthTag(Buffer.from(envelope.tag, "base64"));
  return Buffer.concat([
    decipher.update(Buffer.from(envelope.ciphertext, "base64")),
    decipher.final(),
  ]).toString("utf8");
}
