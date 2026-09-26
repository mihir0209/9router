import { OPENAI_BLOCK } from "../schema/index.js";

// Collapse an OpenAI content-part array: an all-text array becomes a plain
// newline-joined string, otherwise the array is returned as-is. Many
// OpenAI-compatible upstreams only accept string content on messages, so a
// text-only payload has to be flattened rather than passed through as parts.
// Blocks carrying cache_control are left alone — the array form is the only
// way to express that, so callers opting into it keep their structure.
export function collapseTextParts(parts) {
  if (parts.length === 0) return parts;
  if (!parts.every((part) => part?.type === OPENAI_BLOCK.TEXT)) return parts;
  if (parts.some((part) => part.cache_control != null)) return parts;
  return parts
    .map((part) => (typeof part.text === "string" ? part.text : ""))
    .filter(Boolean)
    .join("\n");
}
