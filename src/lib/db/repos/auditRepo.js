import { getAdapter } from "../driver.js";
import { parseJson, stringifyJson } from "../helpers/jsonCol.js";

const MAX_BEFORE_BYTES = 16_000;
const MAX_AFTER_BYTES = 16_000;

function truncate(value) {
  if (value == null) return null;
  const serialized = stringifyJson(value);
  if (serialized.length <= Math.max(MAX_BEFORE_BYTES, MAX_AFTER_BYTES)) return value;
  return { truncated: true, preview: serialized.slice(0, Math.max(MAX_BEFORE_BYTES, MAX_AFTER_BYTES)) };
}

function rowToAudit(row) {
  if (!row) return null;
  return {
    id: row.id,
    timestamp: row.timestamp,
    actor: row.actor,
    action: row.action,
    resourceType: row.resourceType,
    resourceId: row.resourceId,
    ownerRef: row.ownerRef,
    requestId: row.requestId,
    sourceIp: row.sourceIp,
    outcome: row.outcome,
    before: parseJson(row.before, null),
    after: parseJson(row.after, null),
    metadata: parseJson(row.metadata, {}),
  };
}

export async function appendAuditEvent(event) {
  const db = await getAdapter();
  const id = event.id || crypto.randomUUID();
  const row = {
    id,
    timestamp: event.timestamp || new Date().toISOString(),
    actor: String(event.actor || "admin-token").slice(0, 200),
    action: String(event.action || "unknown").slice(0, 120),
    resourceType: String(event.resourceType || "unknown").slice(0, 80),
    resourceId: event.resourceId == null ? null : String(event.resourceId).slice(0, 200),
    ownerRef: event.ownerRef == null ? null : String(event.ownerRef).slice(0, 200),
    requestId: event.requestId == null ? null : String(event.requestId).slice(0, 200),
    sourceIp: event.sourceIp == null ? null : String(event.sourceIp).slice(0, 100),
    outcome: String(event.outcome || "success").slice(0, 40),
    before: truncate(event.before),
    after: truncate(event.after),
    metadata: truncate(event.metadata || {}),
  };
  db.run(
    `INSERT INTO auditLog(id, timestamp, actor, action, resourceType, resourceId, ownerRef, requestId, sourceIp, outcome, before, after, metadata) VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
    [row.id, row.timestamp, row.actor, row.action, row.resourceType, row.resourceId, row.ownerRef, row.requestId, row.sourceIp, row.outcome, row.before == null ? null : stringifyJson(row.before), row.after == null ? null : stringifyJson(row.after), stringifyJson(row.metadata)]
  );
  return row;
}

export async function listAuditEvents({ ownerRef, action, limit = 100 } = {}) {
  const db = await getAdapter();
  const conditions = [];
  const params = [];
  if (ownerRef) {
    conditions.push("ownerRef = ?");
    params.push(ownerRef);
  }
  if (action) {
    conditions.push("action = ?");
    params.push(action);
  }
  const where = conditions.length ? `WHERE ${conditions.join(" AND ")}` : "";
  const bounded = Math.max(1, Math.min(Number(limit) || 100, 500));
  return db.all(`SELECT * FROM auditLog ${where} ORDER BY timestamp DESC LIMIT ${bounded}`, params).map(rowToAudit);
}
