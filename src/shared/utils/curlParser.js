/**
 * Client-Side cURL & JSON Analysis Utility
 * Executed purely in the user's browser runtime.
 * Guarantees zero SSRF vulnerability and zero arbitrary code execution on backend servers.
 */

export function parseCurlInBrowser(curl) {
  if (!curl || typeof curl !== "string") {
    return { method: "POST", url: "", headers: {}, body: null, isStreaming: false };
  }

  let method = "POST";
  let url = "";
  const headers = {};
  let bodyStr = "";

  // Tokenize preserving quotes
  const tokens = [];
  let current = "";
  let inSingle = false;
  let inDouble = false;
  let escaped = false;

  for (let i = 0; i < curl.length; i++) {
    const c = curl[i];
    if (escaped) {
      current += c;
      escaped = false;
      continue;
    }
    if (c === "\\") {
      escaped = true;
      continue;
    }
    if (c === "'" && !inDouble) {
      inSingle = !inSingle;
      continue;
    }
    if (c === '"' && !inSingle) {
      inDouble = !inDouble;
      continue;
    }
    if (/\s/.test(c) && !inSingle && !inDouble) {
      if (current) {
        tokens.push(current);
        current = "";
      }
      continue;
    }
    current += c;
  }
  if (current) tokens.push(current);

  for (let i = 0; i < tokens.length; i++) {
    const token = tokens[i];
    if (token === "-X" || token === "--request") {
      if (i + 1 < tokens.length) method = tokens[++i].toUpperCase();
    } else if (token === "-H" || token === "--header") {
      if (i + 1 < tokens.length) {
        const headerLine = tokens[++i];
        const colonIdx = headerLine.indexOf(":");
        if (colonIdx > 0) {
          const k = headerLine.slice(0, colonIdx).trim().toLowerCase();
          const v = headerLine.slice(colonIdx + 1).trim();
          headers[k] = v;
        }
      }
    } else if (
      token === "-d" ||
      token === "--data" ||
      token === "--data-raw" ||
      token === "--data-binary"
    ) {
      if (i + 1 < tokens.length) {
        bodyStr = tokens[++i];
      }
    } else if (
      !token.startsWith("-") &&
      (token.startsWith("http://") || token.startsWith("https://"))
    ) {
      url = token;
    }
  }

  let parsedBody = null;
  let isStreaming = false;
  if (bodyStr) {
    try {
      parsedBody = JSON.parse(bodyStr);
      if (parsedBody && typeof parsedBody === "object") {
        if (parsedBody.stream === true || parsedBody.streaming === true) {
          isStreaming = true;
        }
      }
    } catch {
      if (bodyStr.includes('"stream": true') || bodyStr.includes('"stream":true')) {
        isStreaming = true;
      }
    }
  }

  if (url && (url.includes("stream=true") || url.includes("stream=1"))) {
    isStreaming = true;
  }
  if (headers["accept"] && headers["accept"].includes("text/event-stream")) {
    isStreaming = true;
  }

  return { method, url, headers, body: parsedBody, isStreaming, rawBody: bodyStr };
}

export function collectJsonPaths(val, prefix = "", out = [], depth = 0) {
  if (depth > 8 || !val) return out;

  if (typeof val === "string") {
    const trimmed = val.trim();
    if ((trimmed.startsWith("{") && trimmed.endsWith("}")) || (trimmed.startsWith("[") && trimmed.endsWith("]"))) {
      try {
        const inner = JSON.parse(trimmed);
        collectJsonPaths(inner, prefix, out, depth + 1);
        return out;
      } catch {
        // Not valid JSON
      }
    }
  }

  if (Array.isArray(val)) {
    for (let i = 0; i < Math.min(val.length, 3); i++) {
      const path = prefix ? `${prefix}[${i}]` : `[${i}]`;
      collectJsonPaths(val[i], path, out, depth + 1);
    }
    return out;
  }

  if (typeof val === "object" && val !== null) {
    for (const [k, v] of Object.entries(val)) {
      const path = prefix ? `${prefix}.${k}` : k;
      if (typeof v === "string" || typeof v === "number" || typeof v === "boolean") {
        out.push({
          path,
          type: typeof v,
          sample: String(v).slice(0, 100),
        });
      } else {
        collectJsonPaths(v, path, out, depth + 1);
      }
    }
  }

  return out;
}

export function extractValueByPath(root, path) {
  if (!root || !path) return undefined;
  let current = root;

  if (typeof current === "string") {
    try {
      current = JSON.parse(current);
    } catch {
      return undefined;
    }
  }

  const parts = path.split(".");
  for (let part of parts) {
    part = part.trim();
    if (!part) continue;

    if (typeof current === "string") {
      try {
        current = JSON.parse(current);
      } catch {
        return undefined;
      }
    }

    if (part.includes("[")) {
      const openIdx = part.indexOf("[");
      const key = part.slice(0, openIdx);
      if (key) {
        if (!current || typeof current !== "object") return undefined;
        current = current[key];
      }
      const bracketMatches = [...part.matchAll(/\[(\d+)\]/g)];
      for (const m of bracketMatches) {
        const idx = parseInt(m[1], 10);
        if (!Array.isArray(current) || idx >= current.length) return undefined;
        current = current[idx];
      }
    } else {
      if (!current || typeof current !== "object") return undefined;
      current = current[part];
    }
  }

  return current;
}
