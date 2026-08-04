export function stripRequestUser(body, enabled) {
  if (!enabled || !body || typeof body !== "object" || Array.isArray(body)) return body;
  if (!Object.prototype.hasOwnProperty.call(body, "user")) return body;

  const sanitized = { ...body };
  delete sanitized.user;
  return sanitized;
}
