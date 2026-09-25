export function credentialsErrorResponse(credentials) {
  if (credentials?.constructor?.name !== "ByokCredentialsError") return null;
  return new Response(JSON.stringify({
    error: { message: credentials.message, type: "authentication_error", code: credentials.code },
  }), { status: 401, headers: { "Content-Type": "application/json", "Access-Control-Allow-Origin": "*" } });
}

export function byokUpstreamErrorResponse(credentials, status) {
  if (!credentials?.isByok || (status !== 401 && status !== 403)) return null;
  return credentialsErrorResponse({ constructor: { name: "ByokCredentialsError" }, message: "The provider rejected the BYOK credential", code: "user_credentials_invalid" });
}
