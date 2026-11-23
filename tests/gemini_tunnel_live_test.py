#!/usr/bin/env python3
"""
Automated live verification for Gemini API (/v1beta/models) and Tunnel Service (/api/tunnel/*)
on 9Router native Rust deployment.
"""

import sys
import json
import urllib.request
import urllib.error

BASE_URL = "https://20128-agqvv2tziwoxkk2nxj6pyup2vu.instabox.tech"
API_KEY = "9r-live-660e037e5d153"

passed = 0
failed = 0

def run_test(name, fn):
    global passed, failed
    print(f"[*] Testing {name}...", end=" ", flush=True)
    try:
        fn()
        print("PASSED")
        passed += 1
    except Exception as e:
        print(f"FAILED: {e}")
        failed += 1

def request(path, method="GET", body=None, headers=None, expect_status=200):
    url = f"{BASE_URL}{path}"
    req_headers = {"User-Agent": "9Router-Test/1.0"}
    if headers:
        req_headers.update(headers)
    
    data = None
    if body is not None:
        data = json.dumps(body).encode("utf-8")
        req_headers["Content-Type"] = "application/json"
    
    req = urllib.request.Request(url, data=data, headers=req_headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            resp_body = resp.read().decode("utf-8")
            status = resp.status
            return status, resp_body, resp.headers
    except urllib.error.HTTPError as e:
        resp_body = e.read().decode("utf-8")
        return e.code, resp_body, e.headers

# Test 1: GET /v1beta/models with Bearer auth
def test_gemini_models_bearer():
    status, body, _ = request("/v1beta/models", headers={"Authorization": f"Bearer {API_KEY}"})
    assert status == 200, f"Expected 200, got {status}: {body}"
    data = json.loads(body)
    assert "models" in data, f"Missing 'models' in response: {body[:200]}"
    assert len(data["models"]) > 0, "Empty models array"
    names = [m["name"] for m in data["models"]]
    assert any("gemini" in n for n in names), f"Expected gemini models, got {names[:5]}"

# Test 2: GET /v1beta/models with ?key= query parameter
def test_gemini_models_query_key():
    status, body, _ = request(f"/v1beta/models?key={API_KEY}")
    assert status == 200, f"Expected 200, got {status}: {body}"
    data = json.loads(body)
    assert "models" in data, f"Missing 'models' in response: {body[:200]}"

# Test 3: GET /v1beta/models with x-goog-api-key header
def test_gemini_models_goog_header():
    status, body, _ = request("/v1beta/models", headers={"x-goog-api-key": API_KEY})
    assert status == 200, f"Expected 200, got {status}: {body}"
    data = json.loads(body)
    assert "models" in data, f"Missing 'models' in response: {body[:200]}"

# Test 4: GET /v1beta/models/gemini-2.5-flash
def test_gemini_model_get():
    status, body, _ = request("/v1beta/models/gemini-2.5-flash", headers={"Authorization": f"Bearer {API_KEY}"})
    assert status == 200, f"Expected 200, got {status}: {body}"
    data = json.loads(body)
    assert data.get("name") == "models/gemini-2.5-flash", f"Unexpected name: {data}"
    assert "supportedGenerationMethods" in data

# Test 5: POST /v1beta/models/h/Qwen3.8-Flash-Next:generateContent
def test_gemini_generate_content_qwen():
    payload = {
        "contents": [
            {
                "role": "user",
                "parts": [{"text": "Reply with 'Hello 9Router' only."}]
            }
        ],
        "generationConfig": {
            "maxOutputTokens": 32,
            "temperature": 0.2
        }
    }
    status, body, _ = request(
        "/v1beta/models/h/Qwen3.8-Flash-Next:generateContent",
        method="POST",
        body=payload,
        headers={"Authorization": f"Bearer {API_KEY}"}
    )
    assert status == 200, f"Expected 200, got {status}: {body}"
    data = json.loads(body)
    assert "candidates" in data, f"Missing 'candidates': {body}"
    cand = data["candidates"][0]
    assert cand.get("content", {}).get("role") == "model"
    assert len(cand.get("content", {}).get("parts", [])) > 0
    text = cand["content"]["parts"][0].get("text", "")
    assert len(text) > 0, "Empty response text"
    assert "usageMetadata" in data, f"Missing usageMetadata: {body}"

# Test 6: POST /v1beta/models/h/hy4:generateContent
def test_gemini_generate_content_hy4():
    payload = {
        "contents": [
            {
                "role": "user",
                "parts": [{"text": "Say 'OK' and nothing else."}]
            }
        ],
        "generationConfig": {
            "maxOutputTokens": 16,
            "temperature": 0.1
        }
    }
    status, body, _ = request(
        "/v1beta/models/h/hy4:generateContent",
        method="POST",
        body=payload,
        headers={"x-goog-api-key": API_KEY}
    )
    assert status == 200, f"Expected 200, got {status}: {body}"
    data = json.loads(body)
    assert "candidates" in data, f"Missing 'candidates': {body}"
    cand = data["candidates"][0]
    assert len(cand.get("content", {}).get("parts", [])) > 0
    assert "usageMetadata" in data

# Test 7: POST /v1beta/models/h/Qwen3.8-Flash-Next:streamGenerateContent (SSE streaming)
def test_gemini_stream_generate_content():
    payload = {
        "contents": [
            {
                "role": "user",
                "parts": [{"text": "Count from 1 to 3."}]
            }
        ]
    }
    status, body, headers = request(
        "/v1beta/models/h/Qwen3.8-Flash-Next:streamGenerateContent",
        method="POST",
        body=payload,
        headers={"Authorization": f"Bearer {API_KEY}"}
    )
    assert status == 200, f"Expected 200, got {status}: {body}"
    ct = headers.get("Content-Type", "")
    assert "text/event-stream" in ct, f"Expected text/event-stream, got {ct}"
    assert "data: " in body, f"Expected SSE data lines: {body[:200]}"
    # Verify chunks are valid JSON with candidates
    found_candidate = False
    for line in body.splitlines():
        if line.startswith("data: "):
            chunk_str = line[6:].strip()
            if chunk_str and chunk_str != "[DONE]":
                parsed = json.loads(chunk_str)
                if "candidates" in parsed:
                    found_candidate = True
                    break
    assert found_candidate, f"No valid candidates in SSE stream: {body[:300]}"

# Test 8: GET /api/tunnel/status
def test_tunnel_status():
    status, body, _ = request("/api/tunnel/status")
    assert status == 200, f"Expected 200, got {status}: {body}"
    data = json.loads(body)
    assert "tunnel" in data, f"Missing 'tunnel' key: {body}"
    assert "tailscale" in data, f"Missing 'tailscale' key: {body}"
    assert "running" in data["tunnel"]

# Test 9: GET /api/tunnel/tailscale-check
def test_tailscale_check():
    status, body, _ = request("/api/tunnel/tailscale-check")
    assert status == 200, f"Expected 200, got {status}: {body}"
    data = json.loads(body)
    assert "installed" in data, f"Missing 'installed': {body}"
    assert "platform" in data, f"Missing 'platform': {body}"

# Test 10: POST /api/tunnel/tailscale-disable
def test_tailscale_disable():
    status, body, _ = request("/api/tunnel/tailscale-disable", method="POST")
    assert status == 200, f"Expected 200, got {status}: {body}"
    data = json.loads(body)
    assert data.get("success") is True

# Test 11: GET /v1/models cleanly formatted with no raw provider UUID leaks
def test_models_clean_names():
    status, body, _ = request("/v1/models", headers={"Authorization": f"Bearer {API_KEY}"})
    assert status == 200, f"Expected 200, got {status}: {body}"
    data = json.loads(body)
    for m in data.get("data", []):
        owned_by = m.get("owned_by", "")
        assert not owned_by.startswith("openai-compatible-chat-fa357"), \
            f"Model {m.get('id')} leaked raw provider UUID in owned_by: {owned_by}"

if __name__ == "__main__":
    print("==================================================")
    print("🚀 9Router Native Gemini & Tunnel Live Verification")
    print(f"   Target: {BASE_URL}")
    print("==================================================")

    run_test("GET /v1beta/models (Bearer auth)", test_gemini_models_bearer)
    run_test("GET /v1beta/models (?key= query param)", test_gemini_models_query_key)
    run_test("GET /v1beta/models (x-goog-api-key header)", test_gemini_models_goog_header)
    run_test("GET /v1beta/models/gemini-2.5-flash", test_gemini_model_get)
    run_test("POST /v1beta/models/h/Qwen3.8-Flash-Next:generateContent", test_gemini_generate_content_qwen)
    run_test("POST /v1beta/models/h/hy4:generateContent", test_gemini_generate_content_hy4)
    run_test("POST /v1beta/models/h/Qwen3.8-Flash-Next:streamGenerateContent (SSE)", test_gemini_stream_generate_content)
    run_test("GET /api/tunnel/status", test_tunnel_status)
    run_test("GET /api/tunnel/tailscale-check", test_tailscale_check)
    run_test("POST /api/tunnel/tailscale-disable", test_tailscale_disable)
    run_test("GET /v1/models clean owned_by & no UUID leaks", test_models_clean_names)

    print("==================================================")
    print(f"Results: {passed} passed, {failed} failed")
    print("==================================================")
    sys.exit(0 if failed == 0 else 1)
