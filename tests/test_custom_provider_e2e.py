#!/usr/bin/env python3
"""
End-to-End Verification Script for 9Router Custom Provider Adapter.
Tests all 4 execution modes against running 9Router on port 20127:
1. OpenAI Non-Streaming (/v1/chat/completions)
2. OpenAI Streaming (/v1/chat/completions?stream=true)
3. Anthropic Non-Streaming (/v1/messages)
4. Anthropic Streaming (/v1/messages?stream=true)
"""

import sys
import json
import urllib.request
import time

API_KEY = "sk-e201c0e43f532a74-x2aq57-a38075d0"
BASE_URL = "http://127.0.0.1:20127"
MODEL = "worst/k2-horizon:free"

def test_openai_non_streaming():
    print("[TEST 1/4] OpenAI Non-Streaming...")
    t0 = time.time()
    req = urllib.request.Request(
        f"{BASE_URL}/v1/chat/completions",
        data=json.dumps({
            "model": MODEL,
            "messages": [{"role": "user", "content": "Reply with 'Test 1 Passed' in 3 words"}],
            "stream": False
        }).encode("utf-8"),
        headers={
            "Authorization": f"Bearer {API_KEY}",
            "Content-Type": "application/json"
        }
    )
    with urllib.request.urlopen(req, timeout=35) as resp:
        assert resp.status == 200, f"Expected 200, got {resp.status}"
        data = json.loads(resp.read().decode("utf-8"))
        content = data["choices"][0]["message"]["content"]
        elapsed = time.time() - t0
        print(f"  -> HTTP {resp.status} in {elapsed:.2f}s | Output: {content.strip()}")
        assert len(content) > 0

def test_openai_streaming():
    print("[TEST 2/4] OpenAI Streaming...")
    t0 = time.time()
    req = urllib.request.Request(
        f"{BASE_URL}/v1/chat/completions",
        data=json.dumps({
            "model": MODEL,
            "messages": [{"role": "user", "content": "Count from 1 to 3"}],
            "stream": True
        }).encode("utf-8"),
        headers={
            "Authorization": f"Bearer {API_KEY}",
            "Content-Type": "application/json"
        }
    )
    tokens = []
    has_done = False
    with urllib.request.urlopen(req, timeout=35) as resp:
        assert resp.status == 200
        for line in resp:
            line_str = line.decode("utf-8").strip()
            if line_str.startswith("data: "):
                payload = line_str[6:]
                if payload == "[DONE]":
                    has_done = True
                    break
                try:
                    c = json.loads(payload)
                    delta = c["choices"][0]["delta"].get("content", "")
                    if delta:
                        tokens.append(delta)
                except Exception:
                    pass
    elapsed = time.time() - t0
    full_text = "".join(tokens).strip()
    print(f"  -> HTTP 200 in {elapsed:.2f}s | Streamed: '{full_text}' | [DONE] received: {has_done}")
    assert has_done

def test_anthropic_non_streaming():
    print("[TEST 3/4] Anthropic Non-Streaming...")
    t0 = time.time()
    req = urllib.request.Request(
        f"{BASE_URL}/v1/messages",
        data=json.dumps({
            "model": MODEL,
            "messages": [{"role": "user", "content": "Reply with 'Test 3 Passed' in 3 words"}],
            "max_tokens": 100,
            "stream": False
        }).encode("utf-8"),
        headers={
            "Authorization": f"Bearer {API_KEY}",
            "anthropic-version": "2023-06-01",
            "Content-Type": "application/json"
        }
    )
    with urllib.request.urlopen(req, timeout=35) as resp:
        assert resp.status == 200
        data = json.loads(resp.read().decode("utf-8"))
        assert data["type"] == "message"
        content = data["content"][0]["text"]
        elapsed = time.time() - t0
        print(f"  -> HTTP {resp.status} in {elapsed:.2f}s | Output: {content.strip()}")
        assert len(content) > 0

def test_anthropic_streaming():
    print("[TEST 4/4] Anthropic Streaming...")
    t0 = time.time()
    req = urllib.request.Request(
        f"{BASE_URL}/v1/messages",
        data=json.dumps({
            "model": MODEL,
            "messages": [{"role": "user", "content": "Count from 1 to 3"}],
            "max_tokens": 100,
            "stream": True
        }).encode("utf-8"),
        headers={
            "Authorization": f"Bearer {API_KEY}",
            "anthropic-version": "2023-06-01",
            "Content-Type": "application/json"
        }
    )
    events = []
    tokens = []
    with urllib.request.urlopen(req, timeout=35) as resp:
        assert resp.status == 200
        current_event = None
        for line in resp:
            line_str = line.decode("utf-8").strip()
            if line_str.startswith("event: "):
                current_event = line_str[7:]
                events.append(current_event)
            elif line_str.startswith("data: ") and current_event == "content_block_delta":
                try:
                    d = json.loads(line_str[6:])
                    tokens.append(d["delta"]["text"])
                except Exception:
                    pass
    elapsed = time.time() - t0
    full_text = "".join(tokens).strip()
    print(f"  -> HTTP 200 in {elapsed:.2f}s | Events: {events} | Text: '{full_text}'")
    assert "message_start" in events
    assert "content_block_delta" in events
    assert "message_stop" in events

def main():
    print("=" * 60)
    print("9Router Custom Provider Adapter Test Suite")
    print("=" * 60)
    test_openai_non_streaming()
    test_openai_streaming()
    test_anthropic_non_streaming()
    test_anthropic_streaming()
    print("=" * 60)
    print("ALL 4 TESTS PASSED CONVINCINGLY!")
    print("=" * 60)

if __name__ == "__main__":
    main()
