import json
import urllib.request
import urllib.error
import sys
import time

BASE_URL = "http://127.0.0.1:20128"
ADMIN_TOKEN = "test-admin-secret-xyz"
TENANT = f"tenant-test-{int(time.time())}"

passed = 0
failed = 0

def test(name, condition, detail=""):
    global passed, failed
    if condition:
        print(f"  [PASS] {name}")
        passed += 1
    else:
        print(f"  [FAIL] {name} - {detail}")
        failed += 1

def req(path, method="GET", body=None, headers=None):
    url = f"{BASE_URL}{path}"
    h = headers.copy() if headers else {}
    data = None
    if body is not None:
        if isinstance(body, (dict, list)):
            data = json.dumps(body).encode("utf-8")
            h["Content-Type"] = "application/json"
        elif isinstance(body, str):
            data = body.encode("utf-8")
        elif isinstance(body, bytes):
            data = body
    
    r = urllib.request.Request(url, data=data, headers=h, method=method)
    try:
        with urllib.request.urlopen(r) as resp:
            resp_body = resp.read().decode("utf-8")
            return resp.status, resp_body, resp.headers
    except urllib.error.HTTPError as e:
        resp_body = e.read().decode("utf-8")
        return e.code, resp_body, e.headers

print("==========================================================")
print("           9ROUTER ADMIN API VERIFICATION SUITE           ")
print("==========================================================")
print(f"Target: {BASE_URL} | Tenant: {TENANT}")
print("")

auth_header = {"Authorization": f"Bearer {ADMIN_TOKEN}"}

# 1. Unauthorized access
print("=== TEST 1: Unauthorized Access Prevention ===")
s, b, _ = req("/api/admin/gateway")
test("GET /api/admin/gateway without token returns 401", s == 401, f"status={s}")

s, b, _ = req("/api/admin/gateway", headers={"Authorization": "Bearer bad-token"})
test("GET /api/admin/gateway with wrong token returns 401", s == 401, f"status={s}")

s, b, _ = req("/api/admin/keys")
test("GET /api/admin/keys without token returns 401", s == 401, f"status={s}")

s, b, _ = req("/api/admin/audit")
test("GET /api/admin/audit without token returns 401", s == 401, f"status={s}")

s, b, _ = req("/api/observability/metrics")
test("GET /api/observability/metrics without token returns 401", s == 401, f"status={s}")

s, b, _ = req("/api/key-actions/debit", method="POST", body={"keyId": "k", "requestId": "r"})
test("POST /api/key-actions/debit without token returns 401", s == 401, f"status={s}")

# 2. Gateway Configuration
print("\n=== TEST 2: Gateway Configuration (GET & PUT) ===")
s, b, _ = req("/api/admin/gateway", headers=auth_header)
test("GET /api/admin/gateway returns 200", s == 200, f"status={s}")
data = json.loads(b)
test("GET response contains gatewayControl", "gatewayControl" in data, f"keys={list(data.keys())}")

gw_update = {
    "disabled": False,
    "reason": "maintenance test run",
    "drain": False,
    "blockedProviders": ["test-provider-x"],
    "blockedModels": ["test-model-y"]
}
s, b, _ = req("/api/admin/gateway", method="PUT", body=gw_update, headers=auth_header)
test("PUT /api/admin/gateway returns 200", s == 200, f"status={s}")
data = json.loads(b)
ctrl = data.get("gatewayControl", {})
test("PUT /api/admin/gateway updates reason", ctrl.get("reason") == "maintenance test run", f"reason={ctrl.get('reason')}")
test("PUT /api/admin/gateway updates blockedProviders", "test-provider-x" in ctrl.get("blockedProviders", []), f"ctrl={ctrl}")
test("PUT /api/admin/gateway updates blockedModels", "test-model-y" in ctrl.get("blockedModels", []), f"ctrl={ctrl}")

# Reset gatewayControl to clean state
req("/api/admin/gateway", method="PUT", body={"disabled": False, "reason": "", "drain": False, "blockedProviders": [], "blockedModels": []}, headers=auth_header)

# 3. Tenant API Key Provisioning
print("\n=== TEST 3: Tenant API Key Provisioning (POST /api/admin/keys) ===")
s, b, _ = req("/api/admin/keys", method="POST", body={"name": "Key Missing Owner"}, headers=auth_header)
test("POST /api/admin/keys without ownerRef returns 400", s == 400, f"status={s}")

s, b, _ = req("/api/admin/keys", method="POST", body={"ownerRef": TENANT}, headers=auth_header)
test("POST /api/admin/keys without name returns 400", s == 400, f"status={s}")

s, b, _ = req("/api/admin/keys", method="POST", body={"name": "Key BYOK", "ownerRef": TENANT, "billingMode": "byok"}, headers=auth_header)
test("POST /api/admin/keys with billingMode=byok returns 400", s == 400, f"status={s}")

key_payload = {
    "name": "Production Tenant Key",
    "ownerRef": TENANT,
    "rateLimitRequests": 120,
    "rateLimitWindowSeconds": 60,
    "budgetUsd": 100.0,
    "billingMode": "credits",
    "metadata": {"environment": "production", "dept": "engineering"}
}
s, b, _ = req("/api/admin/keys", method="POST", body=key_payload, headers=auth_header)
test("POST /api/admin/keys with valid payload returns 201", s == 201, f"status={s}, body={b}")
created_key = json.loads(b)
key_id = created_key.get("id")
key_secret = created_key.get("key")
test("Key ID is generated", bool(key_id), f"id={key_id}")
test("Plaintext key secret starts with sk-", key_secret.startswith("sk-") if key_secret else False, f"key={key_secret}")
test("Key ownerRef matches tenant", created_key.get("ownerRef") == TENANT, f"ownerRef={created_key.get('ownerRef')}")
test("Key rateLimitRequests is 120", created_key.get("rateLimitRequests") == 120, f"rl={created_key.get('rateLimitRequests')}")
test("Key budgetUsd is 100.0", created_key.get("budgetUsd") == 100.0, f"budget={created_key.get('budgetUsd')}")

# 4. Scoped Tenant Key Listing
print("\n=== TEST 4: Scoped Tenant Key Listing (GET /api/admin/keys) ===")
s, b, _ = req("/api/admin/keys", headers=auth_header)
test("GET /api/admin/keys without ?ownerRef returns 400", s == 400, f"status={s}")

s, b, _ = req(f"/api/admin/keys?ownerRef={TENANT}", headers=auth_header)
test("GET /api/admin/keys?ownerRef= returns 200", s == 200, f"status={s}")
list_data = json.loads(b)
keys_list = list_data.get("keys", [])
test("Key found in scoped list", len(keys_list) == 1 and keys_list[0].get("id") == key_id, f"keys_count={len(keys_list)}")
test("GET /api/admin/keys omits raw secret", "key" not in keys_list[0] if keys_list else False, f"keys={keys_list}")

# 5. Key Detail & Policy Update
print("\n=== TEST 5: Key Detail & Policy Update (GET & PUT /api/admin/keys/{id}) ===")
# Missing ownerRef on GET -> 400
s, b, _ = req(f"/api/admin/keys/{key_id}", headers=auth_header)
test("GET /api/admin/keys/{id} without ownerRef returns 400", s == 400, f"status={s}")

# Mismatched ownerRef on GET -> 404
s, b, _ = req(f"/api/admin/keys/{key_id}?ownerRef=other-tenant", headers=auth_header)
test("GET /api/admin/keys/{id} with mismatched ownerRef returns 404", s == 404, f"status={s}")

# Valid GET
s, b, _ = req(f"/api/admin/keys/{key_id}?ownerRef={TENANT}", headers=auth_header)
test("GET /api/admin/keys/{id}?ownerRef= returns 200", s == 200, f"status={s}")
key_detail = json.loads(b)
test("Key detail id matches", key_detail.get("id") == key_id, f"id={key_detail.get('id')}")
test("Key detail omits secret", "key" not in key_detail, f"keys={list(key_detail.keys())}")

# Update policy
update_payload = {
    "name": "Updated Tenant Key",
    "rateLimitRequests": 250,
    "budgetUsd": 150.0
}
s, b, _ = req(f"/api/admin/keys/{key_id}?ownerRef={TENANT}", method="PUT", body=update_payload, headers=auth_header)
test("PUT /api/admin/keys/{id} returns 200", s == 200, f"status={s}, body={b}")
updated_key = json.loads(b)
test("Key name updated", updated_key.get("name") == "Updated Tenant Key", f"name={updated_key.get('name')}")
test("Key rateLimitRequests updated to 250", updated_key.get("rateLimitRequests") == 250, f"rateLimit={updated_key.get('rateLimitRequests')}")
test("Key budgetUsd updated to 150.0", updated_key.get("budgetUsd") == 150.0, f"budgetUsd={updated_key.get('budgetUsd')}")

# 6. Key Secret Rotation
print("\n=== TEST 6: Key Secret Rotation (POST /api/admin/keys/{id}/rotate) ===")
# Missing ownerRef -> 400
s, b, _ = req(f"/api/admin/keys/{key_id}/rotate", method="POST", body={}, headers=auth_header)
test("POST /api/admin/keys/{id}/rotate without ownerRef returns 400", s == 400, f"status={s}")

# Valid rotate
rotate_payload = {"ownerRef": TENANT}
s, b, _ = req(f"/api/admin/keys/{key_id}/rotate", method="POST", body=rotate_payload, headers=auth_header)
test("POST /api/admin/keys/{id}/rotate returns 200", s == 200, f"status={s}, body={b}")
rotated_data = json.loads(b)
new_secret = rotated_data.get("key")
new_version = rotated_data.get("regenerateVersion")
test("Rotated key secret is new sk- value", bool(new_secret) and new_secret.startswith("sk-") and new_secret != key_secret, f"new_secret={new_secret}")
test("Regenerate version incremented to 1", new_version == 1, f"version={new_version}")

# 7. Budget Debit Actions
print("\n=== TEST 7: Budget Debit Actions (/api/key-actions/debit) ===")
# Missing keyId or requestId -> 400
s, b, _ = req("/api/key-actions/debit", method="POST", body={"reserveUsd": 10.0}, headers=auth_header)
test("POST /api/key-actions/debit without keyId/requestId returns 400", s == 400, f"status={s}")

# Valid reserve debit
s, b, _ = req("/api/key-actions/debit", method="POST", body={"keyId": key_id, "requestId": "req-verify-001", "reserveUsd": 25.5}, headers=auth_header)
test("POST /api/key-actions/debit returns 200", s == 200, f"status={s}, body={b}")
debit_data = json.loads(b).get("budget", {})
test("Budget remaining is 124.5", debit_data.get("remaining") == 124.5, f"remaining={debit_data.get('remaining')}")
test("Budget reserved is True", debit_data.get("reserved") is True, f"reserved={debit_data.get('reserved')}")

# Exceeding budget returns 402
s, b, _ = req("/api/key-actions/debit", method="POST", body={"keyId": key_id, "requestId": "req-verify-002", "reserveUsd": 500.0}, headers=auth_header)
test("Exceeding budget returns 402 Payment Required", s == 402, f"status={s}, body={b}")
exceed_body = json.loads(b)
test("402 body contains error message", "exceeds" in exceed_body.get("error", "").lower(), f"error={exceed_body.get('error')}")
test("402 body contains budget state with reserved=False", exceed_body.get("budget", {}).get("reserved") is False, f"budget={exceed_body.get('budget')}")

# 8. Audit Log Querying
print("\n=== TEST 8: Audit Log Querying (GET /api/admin/audit) ===")
s, b, _ = req("/api/admin/audit", headers=auth_header)
test("GET /api/admin/audit without ownerRef returns 400", s == 400, f"status={s}")

s, b, _ = req(f"/api/admin/audit?ownerRef={TENANT}", headers=auth_header)
test("GET /api/admin/audit?ownerRef= returns 200", s == 200, f"status={s}")
audit_events = json.loads(b).get("events", [])
test("Audit log recorded lifecycle events (count >= 3)", len(audit_events) >= 3, f"count={len(audit_events)}")
actions = [e.get("action") for e in audit_events]
test("Audit actions include api_key.created", "api_key.created" in actions, f"actions={actions}")
test("Audit actions include api_key.updated", "api_key.updated" in actions, f"actions={actions}")
test("Audit actions include api_key.rotated", "api_key.rotated" in actions, f"actions={actions}")

# 9. Observability Metrics & Alerts
print("\n=== TEST 9: Observability Metrics & Alerts ===")
s, b, _ = req("/api/observability/metrics", headers=auth_header)
test("GET /api/observability/metrics returns 200", s == 200, f"status={s}")
metrics_obj = json.loads(b)
test("Metrics contains counters and latency", "counters" in metrics_obj and "latency" in metrics_obj, f"keys={list(metrics_obj.keys())}")
test("Metrics contains alerts list", "alerts" in metrics_obj, f"keys={list(metrics_obj.keys())}")

s, b, h = req("/api/observability/metrics?format=prometheus", headers=auth_header)
test("GET /api/observability/metrics?format=prometheus returns 200", s == 200, f"status={s}")
content_type = h.get("Content-Type", "")
test("Prometheus metrics Content-Type matches text/plain", "text/plain" in content_type, f"content_type={content_type}")
test("Prometheus output contains gateway_latency_average_ms", "gateway_latency_average_ms" in b, f"output_sample={b[:100]}")
test("Prometheus output contains gateway_alerts", "gateway_alerts" in b, f"output_sample={b[:100]}")

s, b, _ = req(f"/api/observability/alerts?tenant={TENANT}", headers=auth_header)
test("GET /api/observability/alerts?tenant= returns 200", s == 200, f"status={s}")
alerts_obj = json.loads(b)
test("Alerts response contains alerts array", "alerts" in alerts_obj, f"keys={list(alerts_obj.keys())}")

# 10. Database Export with Admin Token
print("\n=== TEST 10: Database Export with Admin Token (/api/settings/database) ===")
s, b, _ = req("/api/settings/database", headers=auth_header)
test("GET /api/settings/database returns 200", s == 200, f"status={s}")
db_data = json.loads(b)
test("Database export contains settings and apiKeys", "settings" in db_data and "apiKeys" in db_data, f"keys={list(db_data.keys())}")

# 11. Tenant Key Deletion
print("\n=== TEST 11: Tenant Key Deletion (DELETE /api/admin/keys/{id}) ===")
# Missing ownerRef on DELETE -> 400
s, b, _ = req(f"/api/admin/keys/{key_id}", method="DELETE", headers=auth_header)
test("DELETE /api/admin/keys/{id} without ownerRef returns 400", s == 400, f"status={s}")

# Valid DELETE
s, b, _ = req(f"/api/admin/keys/{key_id}?ownerRef={TENANT}", method="DELETE", headers=auth_header)
test("DELETE /api/admin/keys/{id}?ownerRef= returns 200", s == 200, f"status={s}")
del_data = json.loads(b)
test("Delete message is Key deleted successfully", del_data.get("message") == "Key deleted successfully", f"msg={del_data.get('message')}")

s, b, _ = req(f"/api/admin/keys?ownerRef={TENANT}", headers=auth_header)
remaining = len(json.loads(b).get("keys", []))
test("No keys remain for tenant after deletion", remaining == 0, f"remaining={remaining}")

# Check audit log for deletion
s, b, _ = req(f"/api/admin/audit?ownerRef={TENANT}", headers=auth_header)
events_after = json.loads(b).get("events", [])
del_actions = [e.get("action") for e in events_after]
test("Audit log includes api_key.deleted", "api_key.deleted" in del_actions, f"actions={del_actions}")

print("\n==========================================================")
print(f"TEST RESULTS: PASS={passed}, FAIL={failed}")
print("==========================================================")
if failed > 0:
    sys.exit(1)
