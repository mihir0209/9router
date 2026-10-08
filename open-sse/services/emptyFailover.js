/**
 * Empty-stream failover signal (upstream #4364, adapted).
 *
 * A provider can abort a generation yet still return HTTP 200 and stream a
 * terminal frame with no content (e.g. Gemini MALFORMED_FUNCTION_CALL, mapped
 * by the translator to finish_reason "stop"). The combo's HTTP-status check
 * would log "succeeded" and deliver an empty answer with no failover.
 *
 * The signal is STRUCTURAL — "the stream ended and no content frame was ever
 * released" — not a finish-reason enum, so future abort reasons and streams
 * that simply die are covered with no vocabulary to keep in sync. The
 * finish reason is used only inversely: a content_filter finish is a
 * legitimately empty response and must NOT fail over.
 *
 * Content test deliberately excludes:
 * - delta.role — 9router emits {role:"assistant"} on the first frame
 *   regardless of content; counting it would defeat the check.
 * - reasoning_content / delta.reasoning — a think-then-abort produces
 *   reasoning only, which is an unusable turn and must still fall through.
 */
import { parseSSELine } from "../utils/streamHelpers.js";

const NO_RETRY_FINISH = new Set(["content_filter"]);

export function isNoRetryFinish(reason) {
  return typeof reason === "string" && NO_RETRY_FINISH.has(reason.toLowerCase().trim());
}

export function deltaHasContent(delta) {
  if (!delta || typeof delta !== "object") return false;
  const c = delta.content;
  if (typeof c === "string" && c.length > 0) return true;
  if (Array.isArray(c) && c.length > 0) return true;
  if (Array.isArray(delta.tool_calls) && delta.tool_calls.length > 0) return true;
  // Responses-style / non-chat deltas occasionally use output_text.
  if (typeof delta.output_text === "string" && delta.output_text.length > 0) return true;
  return false;
}

/**
 * Inspect one complete SSE line. Returns { content, finishReason }.
 * Non-data lines and "[DONE]" carry nothing.
 */
export function inspectSSELine(line) {
  let parsed = null;
  try {
    parsed = parseSSELine(String(line).trimStart());
  } catch {
    return { content: false, finishReason: null };
  }
  if (!parsed || parsed.done) return { content: false, finishReason: null };
  // Upstream error payloads are handled by the error probe, not here.
  if (parsed.error) return { content: false, finishReason: null };
  let content = false;
  let finishReason = null;
  for (const choice of parsed.choices || []) {
    if (deltaHasContent(choice.delta || choice.message || {})) content = true;
    if (typeof choice.finish_reason === "string" && choice.finish_reason) finishReason = choice.finish_reason;
  }
  return { content, finishReason };
}

/**
 * Scan freshly arrived buffer text, honouring a pending fragment from the
 * previous chunk. Returns { sawContent, finishReason, pending } merged over
 * the incoming state.
 */
export function scanChunk(text, state = { sawContent: false, finishReason: null, pending: "" }) {
  const merged = `${state.pending || ""}${text || ""}`;
  const lines = merged.split("\n");
  const pending = lines.pop() || "";
  let sawContent = Boolean(state.sawContent);
  let finishReason = state.finishReason || null;
  for (const line of lines) {
    if (!line.trim()) continue;
    const hit = inspectSSELine(line);
    if (hit.content) sawContent = true;
    if (hit.finishReason) finishReason = hit.finishReason;
    if (sawContent) break;
  }
  return { sawContent, finishReason, pending };
}
