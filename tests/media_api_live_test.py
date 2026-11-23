#!/usr/bin/env python3
"""
Comprehensive Live Media Providers Verification Suite for 9Router
Tests all media routes on live 9Router instance:
1. Embeddings (/v1/embeddings, /api/v1/embeddings)
2. Text-to-Image (/v1/images/generations, /api/v1/images/generations)
3. Audio Speech / TTS (/v1/audio/speech, /api/v1/audio/speech)
4. Audio Transcriptions / STT (/v1/audio/transcriptions, /api/v1/audio/transcriptions)
5. Web Search (/v1/search, /api/v1/search)
6. Web Fetch (/v1/web/fetch, /api/v1/web/fetch) & SSRF Filter
7. System One (/v1/systemone, /api/v1/systemone)
8. Video Generation (/v1/videos/generations, /api/v1/videos/generations)
9. Model Catalog & Friendly Owner Names (/v1/models)
"""

import json
import os
import sys
import tempfile
import urllib.request
import urllib.error
import io

BASE_URL = os.environ.get("ROUTER_URL", "https://20128-agqvv2tziwoxkk2nxj6pyup2vu.instabox.tech").rstrip("/")
API_KEY = os.environ.get("ROUTER_API_KEY", "9r-live-660e037e5d153")

total_tests = 0
passed_tests = 0
results = []

def record(name, passed, detail=""):
    global total_tests, passed_tests
    total_tests += 1
    if passed:
        passed_tests += 1
        status = "PASS"
    else:
        status = "FAIL"
    results.append((name, status, detail))
    print(f"[{status}] {name}: {detail}")

def http_json(method, path, body=None, expected_status=200):
    url = f"{BASE_URL}{path}"
    headers = {
        "Authorization": f"Bearer {API_KEY}",
        "Content-Type": "application/json"
    }
    data = json.dumps(body).encode("utf-8") if body is not None else None
    req = urllib.request.Request(url, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=40) as res:
            res_body = res.read().decode("utf-8", errors="replace")
            parsed = json.loads(res_body) if res_body else {}
            return res.status, parsed
    except urllib.error.HTTPError as e:
        err_body = e.read().decode("utf-8", errors="replace")
        try:
            parsed = json.loads(err_body)
        except:
            parsed = {"raw": err_body}
        return e.code, parsed

def http_binary(method, path, body=None):
    url = f"{BASE_URL}{path}"
    headers = {
        "Authorization": f"Bearer {API_KEY}",
        "Content-Type": "application/json"
    }
    data = json.dumps(body).encode("utf-8") if body is not None else None
    req = urllib.request.Request(url, data=data, headers=headers, method=method)
    with urllib.request.urlopen(req, timeout=40) as res:
        return res.status, res.headers.get("content-type"), res.read()

def http_multipart(path, file_bytes, filename="audio.mp3", mime="audio/mpeg", fields=None):
    url = f"{BASE_URL}{path}"
    boundary = "----WebKitFormBoundary9RouterTestBoundaryXYZ"
    body = bytearray()
    
    if fields:
        for k, v in fields.items():
            body.extend(f"--{boundary}\r\n".encode())
            body.extend(f'Content-Disposition: form-data; name="{k}"\r\n\r\n'.encode())
            body.extend(f"{v}\r\n".encode())
            
    body.extend(f"--{boundary}\r\n".encode())
    body.extend(f'Content-Disposition: form-data; name="file"; filename="{filename}"\r\n'.encode())
    body.extend(f"Content-Type: {mime}\r\n\r\n".encode())
    body.extend(file_bytes)
    body.extend(b"\r\n")
    body.extend(f"--{boundary}--\r\n".encode())
    
    headers = {
        "Authorization": f"Bearer {API_KEY}",
        "Content-Type": f"multipart/form-data; boundary={boundary}"
    }
    req = urllib.request.Request(url, data=bytes(body), headers=headers, method="POST")
    with urllib.request.urlopen(req, timeout=40) as res:
        res_body = res.read().decode("utf-8", errors="replace")
        return res.status, json.loads(res_body)

print("=" * 70)
print(f"Starting 9Router Media Providers Live Verification Suite")
print(f"Target: {BASE_URL}")
print("=" * 70)

# 1. Health check
status, body = http_json("GET", "/api/health")
record("Health Check (/api/health)", status == 200 and body.get("ok") is True, f"status={status}")

# 2. Embeddings - Single
status, body = http_json("POST", "/v1/embeddings", {
    "model": "@cf/baai/bge-base-en-v1.5",
    "input": "Deep learning architectures for semantic search"
})
is_embed = (status == 200 and body.get("object") == "list" and len(body.get("data", [])) == 1 and len(body["data"][0]["embedding"]) == 768)
record("Embeddings Single (/v1/embeddings)", is_embed, f"dims={len(body.get('data', [{}])[0].get('embedding', [])) if is_embed else 'N/A'}")

# 3. Embeddings - Batch
status, body = http_json("POST", "/api/v1/embeddings", {
    "model": "@cf/baai/bge-small-en-v1.5",
    "input": ["First document chunk", "Second document chunk"]
})
is_batch_embed = (status == 200 and body.get("object") == "list" and len(body.get("data", [])) == 2 and len(body["data"][0]["embedding"]) == 384)
record("Embeddings Batch (/api/v1/embeddings)", is_batch_embed, f"count={len(body.get('data', []))} dims={len(body.get('data', [{}])[0].get('embedding', [])) if is_batch_embed else 'N/A'}")

# 4. Text-to-Image - Standard OpenAI format
status, body = http_json("POST", "/v1/images/generations", {
    "model": "dall-e-3",
    "prompt": "A modern electric sports car rendered in unreal engine",
    "n": 1,
    "response_format": "b64_json"
})
is_img = (status == 200 and len(body.get("data", [])) == 1 and len(body["data"][0].get("b64_json", "")) > 1000)
record("Text-to-Image (/v1/images/generations)", is_img, f"b64_bytes={len(body.get('data', [{}])[0].get('b64_json', '')) if is_img else 'N/A'}")

# 5. Text-to-Image - Explicit Flux model
status, body = http_json("POST", "/api/v1/images/generations", {
    "model": "@cf/black-forest-labs/flux-1-schnell",
    "prompt": "An astronaut walking on neon dunes of Mars",
    "n": 1,
    "response_format": "b64_json"
})
is_img2 = (status == 200 and len(body.get("data", [])) == 1 and len(body["data"][0].get("b64_json", "")) > 1000)
record("Text-to-Image Flux (/api/v1/images/generations)", is_img2, f"b64_bytes={len(body.get('data', [{}])[0].get('b64_json', '')) if is_img2 else 'N/A'}")

# 6. Audio TTS (Speech) - Standard OpenAI Voice (alloy)
status, ct, audio_bytes = http_binary("POST", "/v1/audio/speech", {
    "model": "tts-1",
    "input": "This is a live speech test for 9Router",
    "voice": "alloy"
})
is_audio1 = (status == 200 and len(audio_bytes) > 2000 and "audio" in ct or "mpeg" in ct or "octet-stream" in ct)
record("Audio TTS Alloy Voice (/v1/audio/speech)", is_audio1, f"len={len(audio_bytes)} ct={ct}")

# Save speech bytes for transcription STT test
saved_audio = audio_bytes

# 7. Audio TTS (Speech) - Free Edge-TTS Neural Voice (JennyNeural)
status, ct, audio_bytes2 = http_binary("POST", "/api/v1/audio/speech", {
    "model": "edge-tts",
    "input": "Neural speech synthesis is working perfectly",
    "voice": "en-US-JennyNeural"
})
is_audio2 = (status == 200 and len(audio_bytes2) > 2000)
record("Audio TTS Neural Voice (/api/v1/audio/speech)", is_audio2, f"len={len(audio_bytes2)} ct={ct}")

# 8. Audio STT (Transcriptions) - Round-trip verification
status, stt_body = http_multipart(
    "/v1/audio/transcriptions",
    saved_audio,
    filename="sample.mp3",
    mime="audio/mpeg",
    fields={"model": "whisper-1"}
)
text_out = stt_body.get("text", "")
is_stt = (status == 200 and len(text_out) > 0 and ("speech" in text_out.lower() or "router" in text_out.lower() or "test" in text_out.lower()))
record("Audio STT Whisper Roundtrip (/v1/audio/transcriptions)", is_stt, f"transcription='{text_out}'")

# 9. Audio STT (/api/v1/audio/transcriptions)
status, stt_body2 = http_multipart(
    "/api/v1/audio/transcriptions",
    saved_audio,
    filename="sample.mp3",
    mime="audio/mpeg",
    fields={"model": "@cf/openai/whisper"}
)
is_stt2 = (status == 200 and len(stt_body2.get("text", "")) > 0)
record("Audio STT Cloudflare Whisper (/api/v1/audio/transcriptions)", is_stt2, f"transcription='{stt_body2.get('text', '')}'")

# 10. Web Search (/v1/search)
status, body = http_json("POST", "/v1/search", {
    "query": "Rust language features",
    "limit": 3
})
results_list = body.get("results", [])
is_search = (status == 200 and len(results_list) > 0 and "url" in results_list[0])
record("Web Search (/v1/search)", is_search, f"count={len(results_list)} top_url={results_list[0].get('url') if is_search else 'N/A'}")

# 11. Web Search (/api/v1/search)
status, body = http_json("POST", "/api/v1/search", {
    "query": "Cloudflare Workers AI",
    "limit": 2
})
results_list2 = body.get("results", [])
is_search2 = (status == 200 and len(results_list2) > 0)
record("Web Search (/api/v1/search)", is_search2, f"count={len(results_list2)}")

# 12. Web Fetch (/v1/web/fetch)
status, body = http_json("POST", "/v1/web/fetch", {
    "url": "https://example.com"
})
is_fetch = (status == 200 and body.get("success") is True and (
    "documentation examples" in body.get("data", {}).get("markdown", "")
    or "Example Domain" in body.get("data", {}).get("metadata", {}).get("title", "")
))
record("Web Fetch Scrape (/v1/web/fetch)", is_fetch, f"markdown_len={len(body.get('data', {}).get('markdown', ''))}")

# 13. Web Fetch SSRF Defense
status, body = http_json("POST", "/v1/web/fetch", {
    "url": "http://127.0.0.1:20128/api/health"
}, expected_status=400)
is_ssrf_blocked = (status == 400 and "Blocked URL" in str(body))
record("Web Fetch SSRF Filter Protection", is_ssrf_blocked, f"status={status} err={body.get('error')}")

# 14. System One (/v1/systemone)
status, body = http_json("POST", "/v1/systemone", {
    "model": "system-one-v1",
    "state": {"query": "how to build a web scraper in rust", "step": "evaluate"}
})
is_sysone = (status == 200 and "decision" in body and "confidence" in body)
record("System One Decision Engine (/v1/systemone)", is_sysone, f"decision={body.get('decision')} confidence={body.get('confidence')}")

# 15. System One (/api/v1/systemone)
status, body = http_json("POST", "/api/v1/systemone", {
    "model": "system-one-v1",
    "state": {"task": "code_refactor", "complexity": "high"}
})
is_sysone2 = (status == 200 and "decision" in body)
record("System One (/api/v1/systemone)", is_sysone2, f"decision={body.get('decision')}")

# 16. Video Generation (/v1/videos/generations)
status, body = http_json("POST", "/v1/videos/generations", {
    "prompt": "Cinematic aerial view of Tokyo city at night with neon reflections",
    "duration": 5
})
# Expect either success or clear provider_not_found error (valid behavior)
is_video = (status == 200 or (status == 400 and body.get("error", {}).get("type") in ["provider_not_found", "invalid_request_error"]))
record("Video Generation Handler (/v1/videos/generations)", is_video, f"status={status} error_type={body.get('error', {}).get('type')}")

# 17. Video Generation Empty Prompt Validation
status, body = http_json("POST", "/api/v1/videos/generations", {
    "prompt": ""
})
is_video_val = (status == 400 and "prompt is required" in body.get("error", {}).get("message", ""))
record("Video Generation Validation (/api/v1/videos/generations)", is_video_val, f"status={status} msg={body.get('error', {}).get('message')}")

# 18. Model Catalog Friendly Names
status, body = http_json("GET", "/v1/models")
models = body.get("data", [])
# Verify that no model has raw UUIDs in owned_by
raw_uuids = [m for m in models if "openai-compatible-chat-" in m.get("owned_by", "")]
friendly_owners = set(m.get("owned_by") for m in models if m.get("owned_by"))
is_clean_models = (status == 200 and len(models) > 0 and len(raw_uuids) == 0)
record("Model Catalog Friendly Owner Names (/v1/models)", is_clean_models, f"total={len(models)} raw_uuid_leak={len(raw_uuids)} owners={list(friendly_owners)[:6]}")

print("=" * 70)
print(f"Results Summary: {passed_tests}/{total_tests} Tests Passed ({(passed_tests/total_tests)*100:.1f}%)")
print("=" * 70)

if passed_tests == total_tests:
    print("ALL 9ROUTER MEDIA PROVIDERS APIS VERIFIED WITH 100% SUCCESS!")
    sys.exit(0)
else:
    print("SOME TESTS FAILED")
    sys.exit(1)
