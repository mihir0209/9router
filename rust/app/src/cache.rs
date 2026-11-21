use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::Response;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

use crate::translator::{
    ChatCompletionRequest, ClaudeMessageRequest,
};

/// Normalizes arbitrary whitespace (leading, trailing, consecutive spaces/tabs/newlines)
/// into a single space-separated token sequence.
pub fn normalize_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ============================================================================
// Canonical Request Representation
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CanonicalRequest {
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    pub messages: Vec<CanonicalMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<CanonicalTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CanonicalMessage {
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<CanonicalToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct CanonicalToolCall {
    pub id: String,
    pub tool_type: String,
    pub function_name: String,
    pub function_arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct CanonicalTool {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<String>,
}

impl CanonicalRequest {
    pub fn from_chat_request(req: &ChatCompletionRequest) -> Self {
        let (_, clean_model) = crate::proxy::strip_provider_prefix(&req.model);
        let model = clean_model.trim().to_lowercase();

        // Extract system instructions from messages with role "system" or "developer"
        let system_parts: Vec<String> = req
            .messages
            .iter()
            .filter(|m| {
                m.role.eq_ignore_ascii_case("system") || m.role.eq_ignore_ascii_case("developer")
            })
            .filter_map(|m| m.content.as_ref())
            .map(|c| normalize_whitespace(&c.as_text()))
            .filter(|s| !s.is_empty())
            .collect();
        let system = if system_parts.is_empty() {
            None
        } else {
            Some(system_parts.join(" "))
        };

        let messages: Vec<CanonicalMessage> = req
            .messages
            .iter()
            .filter(|m| {
                !m.role.eq_ignore_ascii_case("system") && !m.role.eq_ignore_ascii_case("developer")
            })
            .map(|m| {
                let content = m
                    .content
                    .as_ref()
                    .map(|c| normalize_whitespace(&c.as_text()))
                    .unwrap_or_default();
                let tool_calls = m.tool_calls.as_ref().map(|tcs| {
                    let mut list: Vec<CanonicalToolCall> = tcs
                        .iter()
                        .map(|tc| CanonicalToolCall {
                            id: tc.id.clone(),
                            tool_type: tc.tool_type.clone(),
                            function_name: tc.function.name.clone(),
                            function_arguments: normalize_whitespace(&tc.function.arguments),
                        })
                        .collect();
                    list.sort();
                    list
                });
                CanonicalMessage {
                    role: m.role.trim().to_lowercase(),
                    content,
                    name: m.name.clone(),
                    tool_calls,
                    tool_call_id: m.tool_call_id.clone(),
                }
            })
            .collect();

        let tools = req.tools.as_ref().map(|ts| {
            let mut list: Vec<CanonicalTool> = ts
                .iter()
                .map(|t| CanonicalTool {
                    name: t.function.name.trim().to_string(),
                    description: t.function.description.as_deref().map(normalize_whitespace),
                    parameters: t.function.parameters.as_ref().map(|p| p.to_string()),
                })
                .collect();
            list.sort();
            list
        });

        let temperature = req.temperature.map(|t| format!("{:.4}", t));

        Self {
            model,
            system,
            messages,
            tools,
            temperature,
        }
    }

    pub fn from_claude_request(req: &ClaudeMessageRequest) -> Self {
        let (_, clean_model) = crate::proxy::strip_provider_prefix(&req.model);
        let model = clean_model.trim().to_lowercase();

        let system = req.system.as_ref().map(|s| normalize_whitespace(&s.as_text()));

        let messages: Vec<CanonicalMessage> = req
            .messages
            .iter()
            .map(|m| CanonicalMessage {
                role: m.role.trim().to_lowercase(),
                content: normalize_whitespace(&m.content.as_text()),
                name: None,
                tool_calls: None,
                tool_call_id: None,
            })
            .collect();

        let tools = req.tools.as_ref().map(|ts| {
            let mut list: Vec<CanonicalTool> = ts
                .iter()
                .map(|t| CanonicalTool {
                    name: t.name.trim().to_string(),
                    description: t.description.as_deref().map(normalize_whitespace),
                    parameters: Some(t.input_schema.to_string()),
                })
                .collect();
            list.sort();
            list
        });

        let temperature = req.temperature.map(|t| format!("{:.4}", t));

        Self {
            model,
            system,
            messages,
            tools,
            temperature,
        }
    }
}

/// Compute SHA-256 hash of canonical request representation
pub fn compute_hash(canonical: &CanonicalRequest) -> String {
    let serialized = serde_json::to_vec(canonical).unwrap_or_default();
    let digest = ring::digest::digest(&ring::digest::SHA256, &serialized);
    digest
        .as_ref()
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect()
}

// ============================================================================
// Cached Response Model & Replay
// ============================================================================

#[derive(Debug, Clone)]
pub struct CachedResponse {
    pub key: String,
    pub created_at: Instant,
    pub expires_at: Instant,
    pub last_accessed: Instant,
    pub access_count: u64,
    pub json_body: Option<Bytes>,
    pub stream_chunks: Option<Vec<Bytes>>,
}

impl CachedResponse {
    pub fn to_response(&self, is_stream: bool) -> Response {
        if is_stream {
            self.to_stream_response()
        } else {
            self.to_json_response()
        }
    }

    pub fn to_json_response(&self) -> Response {
        let body_bytes = if let Some(ref json) = self.json_body {
            json.clone()
        } else if let Some(ref chunks) = self.stream_chunks {
            Bytes::from(reconstruct_json_from_chunks(chunks))
        } else {
            Bytes::from_static(b"{\"choices\":[]}")
        };

        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/json")
            .header("x-9router-cache", "HIT")
            .body(Body::from(body_bytes))
            .unwrap()
    }

    pub fn to_stream_response(&self) -> Response {
        let chunks: Vec<Bytes> = if let Some(ref chunks) = self.stream_chunks {
            chunks.clone()
        } else if let Some(ref json) = self.json_body {
            synthesize_chunks_from_json(json)
        } else {
            vec![Bytes::from_static(b"data: [DONE]\n\n")]
        };

        let stream = tokio_stream::iter(chunks.into_iter().map(Ok::<_, std::convert::Infallible>));

        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .header(header::CACHE_CONTROL, "no-cache")
            .header(header::CONNECTION, "keep-alive")
            .header("x-9router-cache", "HIT")
            .body(Body::from_stream(stream))
            .unwrap()
    }
}

pub fn reconstruct_json_from_chunks(chunks: &[Bytes]) -> Vec<u8> {
    let mut accumulated_content = String::new();
    let mut model = "default".to_string();
    let mut id = "chatcmpl-cached".to_string();
    let mut created = chrono::Utc::now().timestamp();

    for chunk in chunks {
        let text = String::from_utf8_lossy(chunk);
        for line in text.lines() {
            let line = line.trim();
            if let Some(data) = line.strip_prefix("data:") {
                let data = data.trim();
                if data == "[DONE]" || data.is_empty() {
                    continue;
                }
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(data) {
                    if let Some(m) = val.get("model").and_then(|v| v.as_str()) {
                        model = m.to_string();
                    }
                    if let Some(i) = val.get("id").and_then(|v| v.as_str()) {
                        id = i.to_string();
                    }
                    if let Some(c) = val.get("created").and_then(|v| v.as_i64()) {
                        created = c;
                    }

                    // OpenAI delta content
                    if let Some(content) = val
                        .get("choices")
                        .and_then(|c| c.as_array())
                        .and_then(|a| a.first())
                        .and_then(|first| first.get("delta"))
                        .and_then(|delta| delta.get("content"))
                        .and_then(|c| c.as_str())
                    {
                        accumulated_content.push_str(content);
                    }

                    // Claude text delta
                    if let Some(delta) = val.get("delta") {
                        if let Some(text) = delta.get("text").and_then(|t| t.as_str()) {
                            accumulated_content.push_str(text);
                        }
                    }
                }
            }
        }
    }

    let resp = serde_json::json!({
        "id": id,
        "object": "chat.completion",
        "created": created,
        "model": model,
        "choices": [
            {
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": accumulated_content
                },
                "finish_reason": "stop"
            }
        ],
        "usage": {
            "prompt_tokens": 0,
            "completion_tokens": 0,
            "total_tokens": 0
        }
    });

    serde_json::to_vec(&resp).unwrap_or_default()
}

pub fn synthesize_chunks_from_json(json_bytes: &[u8]) -> Vec<Bytes> {
    let val: serde_json::Value = serde_json::from_slice(json_bytes).unwrap_or_default();
    let id = val.get("id").and_then(|v| v.as_str()).unwrap_or("chatcmpl-cached");
    let model = val.get("model").and_then(|v| v.as_str()).unwrap_or("default");
    let created = val
        .get("created")
        .and_then(|v| v.as_i64())
        .unwrap_or_else(|| chrono::Utc::now().timestamp());

    let content = val
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|m| m.get("message"))
        .and_then(|msg| msg.get("content"))
        .and_then(|c| c.as_str())
        .or_else(|| {
            val.get("content")
                .and_then(|c| c.as_array())
                .and_then(|a| a.first())
                .and_then(|b| b.get("text"))
                .and_then(|t| t.as_str())
        })
        .unwrap_or_default();

    let chunk1 = serde_json::json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "choices": [
            {
                "index": 0,
                "delta": {
                    "role": "assistant",
                    "content": content
                },
                "finish_reason": null
            }
        ]
    });

    let chunk2 = serde_json::json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "choices": [
            {
                "index": 0,
                "delta": {},
                "finish_reason": "stop"
            }
        ]
    });

    vec![
        Bytes::from(format!("data: {}\n\n", serde_json::to_string(&chunk1).unwrap())),
        Bytes::from(format!("data: {}\n\n", serde_json::to_string(&chunk2).unwrap())),
        Bytes::from("data: [DONE]\n\n"),
    ]
}

// ============================================================================
// Thread-Safe LRU Token Cache
// ============================================================================

struct TokenCacheInner {
    entries: HashMap<String, CachedResponse>,
    access_counter: u64,
}

#[derive(Clone)]
pub struct TokenCache {
    inner: Arc<RwLock<TokenCacheInner>>,
    max_entries: usize,
    default_ttl: Duration,
}

impl Default for TokenCache {
    fn default() -> Self {
        Self::new(10_000, Duration::from_secs(300))
    }
}

impl TokenCache {
    pub fn new(max_entries: usize, default_ttl: Duration) -> Self {
        Self {
            inner: Arc::new(RwLock::new(TokenCacheInner {
                entries: HashMap::new(),
                access_counter: 0,
            })),
            max_entries: max_entries.max(1),
            default_ttl,
        }
    }

    pub fn compute_chat_hash(&self, req: &ChatCompletionRequest) -> String {
        let canonical = CanonicalRequest::from_chat_request(req);
        compute_hash(&canonical)
    }

    pub fn compute_claude_hash(&self, req: &ClaudeMessageRequest) -> String {
        let canonical = CanonicalRequest::from_claude_request(req);
        compute_hash(&canonical)
    }

    pub fn get(&self, key: &str) -> Option<CachedResponse> {
        let mut guard = self.inner.write().ok()?;
        let now = Instant::now();
        guard.access_counter += 1;
        let counter = guard.access_counter;

        if let Some(entry) = guard.entries.get_mut(key) {
            if now >= entry.expires_at {
                guard.entries.remove(key);
                return None;
            }
            entry.access_count = counter;
            entry.last_accessed = now;
            Some(entry.clone())
        } else {
            None
        }
    }

    pub fn set_json(&self, key: &str, body: Bytes) {
        self.insert_entry(key, Some(body), None, self.default_ttl);
    }

    pub fn set_stream(&self, key: &str, chunks: Vec<Bytes>) {
        self.insert_entry(key, None, Some(chunks), self.default_ttl);
    }

    pub fn insert_with_ttl(
        &self,
        key: &str,
        json: Option<Bytes>,
        stream_chunks: Option<Vec<Bytes>>,
        ttl: Duration,
    ) {
        self.insert_entry(key, json, stream_chunks, ttl);
    }

    fn insert_entry(
        &self,
        key: &str,
        json: Option<Bytes>,
        stream_chunks: Option<Vec<Bytes>>,
        ttl: Duration,
    ) {
        let Ok(mut guard) = self.inner.write() else {
            return;
        };
        let now = Instant::now();
        guard.access_counter += 1;
        let counter = guard.access_counter;

        // 1. If key exists, update entry
        if let Some(entry) = guard.entries.get_mut(key) {
            entry.access_count = counter;
            entry.last_accessed = now;
            entry.created_at = now;
            entry.expires_at = now + ttl;
            if json.is_some() {
                entry.json_body = json;
            }
            if stream_chunks.is_some() {
                entry.stream_chunks = stream_chunks;
            }
            return;
        }

        // 2. If full, evict expired entries first
        if guard.entries.len() >= self.max_entries {
            guard.entries.retain(|_, v| now < v.expires_at);
        }

        // 3. If still full, evict least recently used (lowest access_count)
        if guard.entries.len() >= self.max_entries {
            if let Some(lru_key) = guard
                .entries
                .iter()
                .min_by_key(|(_, v)| v.access_count)
                .map(|(k, _)| k.clone())
            {
                guard.entries.remove(&lru_key);
            }
        }

        guard.entries.insert(
            key.to_string(),
            CachedResponse {
                key: key.to_string(),
                created_at: now,
                expires_at: now + ttl,
                last_accessed: now,
                access_count: counter,
                json_body: json,
                stream_chunks,
            },
        );
    }

    pub fn len(&self) -> usize {
        self.inner.read().map(|g| g.entries.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn clear(&self) {
        if let Ok(mut guard) = self.inner.write() {
            guard.entries.clear();
        }
    }

    /// Wraps an upstream response: sets x-9router-cache: MISS on the response,
    /// and asynchronously caches the non-streaming JSON or streaming SSE chunks upon completion.
    pub async fn wrap_and_cache(
        &self,
        cache_key: String,
        resp: Response,
        is_stream: bool,
    ) -> Response {
        let (mut parts, body) = resp.into_parts();
        parts
            .headers
            .insert("x-9router-cache", HeaderValue::from_static("MISS"));

        if is_stream {
            let cache = self.clone();
            let mut stream = body.into_data_stream();
            let recorded_stream = async_stream::stream! {
                let mut collected = Vec::new();
                while let Some(item) = stream.next().await {
                    match item {
                        Ok(chunk) => {
                            collected.push(chunk.clone());
                            yield Ok::<_, axum::Error>(chunk);
                        }
                        Err(err) => {
                            yield Err(err);
                            return;
                        }
                    }
                }
                if !collected.is_empty() {
                    cache.set_stream(&cache_key, collected);
                }
            };
            Response::from_parts(parts, Body::from_stream(recorded_stream))
        } else {
            match axum::body::to_bytes(body, usize::MAX).await {
                Ok(bytes) => {
                    if parts.status.is_success() {
                        self.set_json(&cache_key, bytes.clone());
                    }
                    Response::from_parts(parts, Body::from(bytes))
                }
                Err(_) => Response::from_parts(parts, Body::empty()),
            }
        }
    }
}

// ============================================================================
// Unit Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::translator::{
        ChatMessage, ChatTool, ChatFunction, ClaudeMessage, ClaudeContent,
    };
    use std::collections::HashSet;

    #[test]
    fn test_canonical_hash_whitespace_normalization() {
        // Test 1: Messages with varying whitespace, tabs, and newlines
        let req1 = ChatCompletionRequest {
            model: "gpt-4o".to_string(),
            messages: vec![ChatMessage::user("  Hello,   world!  \n\t  ")],
            temperature: Some(0.7),
            ..Default::default()
        };

        let req2 = ChatCompletionRequest {
            model: "gpt-4o".to_string(),
            messages: vec![ChatMessage::user("Hello, world!")],
            temperature: Some(0.7000),
            ..Default::default()
        };

        let cache = TokenCache::default();
        let hash1 = cache.compute_chat_hash(&req1);
        let hash2 = cache.compute_chat_hash(&req2);
        assert_eq!(hash1, hash2, "Whitespace normalized user message should have identical hash");

        // Test 2: System prompt whitespace normalization
        let req_sys1 = ChatCompletionRequest {
            model: "claude-3-5".to_string(),
            messages: vec![
                ChatMessage::system("   You are a helpful    assistant.\n\n   Be concise.   "),
                ChatMessage::user("ping"),
            ],
            ..Default::default()
        };

        let req_sys2 = ChatCompletionRequest {
            model: "claude-3-5".to_string(),
            messages: vec![
                ChatMessage::system("You are a helpful assistant. Be concise."),
                ChatMessage::user("ping"),
            ],
            ..Default::default()
        };

        assert_eq!(
            cache.compute_chat_hash(&req_sys1),
            cache.compute_chat_hash(&req_sys2),
            "System prompt whitespace normalization should produce identical hash"
        );

        // Test 3: Claude message whitespace normalization
        let claude1 = ClaudeMessageRequest {
            model: "claude-3-sonnet".to_string(),
            messages: vec![ClaudeMessage {
                role: "user".to_string(),
                content: ClaudeContent::Single("  Calculate   2   +   2  \n".to_string()),
            }],
            max_tokens: 100,
            system: None,
            metadata: None,
            stop_sequences: None,
            stream: None,
            temperature: Some(0.5),
            top_p: None,
            top_k: None,
            tools: None,
            tool_choice: None,
        };

        let claude2 = ClaudeMessageRequest {
            model: "claude-3-sonnet".to_string(),
            messages: vec![ClaudeMessage {
                role: "user".to_string(),
                content: ClaudeContent::Single("Calculate 2 + 2".to_string()),
            }],
            max_tokens: 100,
            system: None,
            metadata: None,
            stop_sequences: None,
            stream: None,
            temperature: Some(0.5000),
            top_p: None,
            top_k: None,
            tools: None,
            tool_choice: None,
        };

        assert_eq!(
            cache.compute_claude_hash(&claude1),
            cache.compute_claude_hash(&claude2)
        );
    }

    #[test]
    fn test_canonical_hash_collision_resistance() {
        let cache = TokenCache::default();
        let base_req = ChatCompletionRequest {
            model: "gpt-4o".to_string(),
            messages: vec![ChatMessage::user("What is 2+2?")],
            temperature: Some(0.7),
            ..Default::default()
        };

        let base_hash = cache.compute_chat_hash(&base_req);

        // 1. Different model
        let mut req_model = base_req.clone();
        req_model.model = "gpt-4o-mini".to_string();
        let h_model = cache.compute_chat_hash(&req_model);

        // 2. Different prompt content
        let mut req_content = base_req.clone();
        req_content.messages = vec![ChatMessage::user("What is 2+3?")];
        let h_content = cache.compute_chat_hash(&req_content);

        // 3. Different role
        let mut req_role = base_req.clone();
        req_role.messages = vec![ChatMessage::assistant("What is 2+2?")];
        let h_role = cache.compute_chat_hash(&req_role);

        // 4. Added system message
        let mut req_system = base_req.clone();
        req_system.messages.insert(0, ChatMessage::system("You are a math tutor"));
        let h_system = cache.compute_chat_hash(&req_system);

        // 5. Different temperature
        let mut req_temp = base_req.clone();
        req_temp.temperature = Some(0.8);
        let h_temp = cache.compute_chat_hash(&req_temp);

        // 6. No temperature
        let mut req_no_temp = base_req.clone();
        req_no_temp.temperature = None;
        let h_no_temp = cache.compute_chat_hash(&req_no_temp);

        // 7. Added tool
        let mut req_tools = base_req.clone();
        req_tools.tools = Some(vec![ChatTool {
            tool_type: "function".to_string(),
            function: ChatFunction {
                name: "calculator".to_string(),
                description: Some("Performs math calculations".to_string()),
                parameters: Some(serde_json::json!({"type": "object"})),
            },
        }]);
        let h_tools = cache.compute_chat_hash(&req_tools);

        let mut hashes = HashSet::new();
        hashes.insert(base_hash);
        hashes.insert(h_model);
        hashes.insert(h_content);
        hashes.insert(h_role);
        hashes.insert(h_system);
        hashes.insert(h_temp);
        hashes.insert(h_no_temp);
        hashes.insert(h_tools);

        assert_eq!(
            hashes.len(),
            8,
            "All distinct request variations must produce strictly unique canonical hashes"
        );
    }

    #[test]
    fn test_cache_hit_miss_lifecycle() {
        let cache = TokenCache::new(50, Duration::from_secs(60));
        let key = "sample-sha256-key";

        // Initial check: MISS
        assert!(cache.get(key).is_none(), "Initial lookup must be a MISS");

        // Set JSON response
        let sample_json = Bytes::from_static(b"{\"id\":\"cmpl-1\",\"choices\":[{\"message\":{\"role\":\"assistant\",\"content\":\"42\"}}]}");
        cache.set_json(key, sample_json.clone());

        // Second check: HIT
        let cached = cache.get(key).expect("Second lookup must be a HIT");
        assert_eq!(cached.key, key);

        // Validate response generation (<1ms)
        let resp = cached.to_response(false);
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get("x-9router-cache").unwrap(),
            "HIT"
        );
        assert_eq!(
            resp.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/json"
        );
    }

    #[tokio::test]
    async fn test_ttl_expiration() {
        // Fast TTL of 30 milliseconds
        let cache = TokenCache::new(10, Duration::from_millis(30));
        let key = "fast-expiring-key";

        cache.set_json(key, Bytes::from_static(b"{\"ok\":true}"));

        // Immediately available
        assert!(cache.get(key).is_some());

        // Wait past TTL
        tokio::time::sleep(Duration::from_millis(45)).await;

        // Expired -> returns None and evicts
        assert!(cache.get(key).is_none(), "Entry must expire after TTL");
        assert_eq!(cache.len(), 0, "Expired entry must be cleaned up");
    }

    #[tokio::test]
    async fn test_stream_replay() {
        let cache = TokenCache::new(10, Duration::from_secs(60));
        let key = "streaming-key";

        let chunks = vec![
            Bytes::from_static(b"data: {\"id\":\"cmpl-s\",\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n\n"),
            Bytes::from_static(b"data: {\"id\":\"cmpl-s\",\"choices\":[{\"delta\":{\"content\":\" world!\"}}]}\n\n"),
            Bytes::from_static(b"data: [DONE]\n\n"),
        ];

        cache.set_stream(key, chunks.clone());

        let cached = cache.get(key).expect("Stream entry must exist");

        // 1. Replay as stream
        let stream_resp = cached.to_response(true);
        assert_eq!(stream_resp.status(), StatusCode::OK);
        assert_eq!(
            stream_resp.headers().get("x-9router-cache").unwrap(),
            "HIT"
        );
        assert_eq!(
            stream_resp.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/event-stream"
        );

        let body_bytes = axum::body::to_bytes(stream_resp.into_body(), usize::MAX)
            .await
            .expect("Reading stream body should succeed");
        let stream_text = String::from_utf8(body_bytes.to_vec()).unwrap();
        assert!(stream_text.contains("Hello"));
        assert!(stream_text.contains(" world!"));
        assert!(stream_text.contains("[DONE]"));

        // 2. Replay as non-stream (automatic synthesis)
        let json_resp = cached.to_response(false);
        assert_eq!(json_resp.status(), StatusCode::OK);
        assert_eq!(
            json_resp.headers().get("x-9router-cache").unwrap(),
            "HIT"
        );
        assert_eq!(
            json_resp.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/json"
        );

        let json_bytes = axum::body::to_bytes(json_resp.into_body(), usize::MAX)
            .await
            .expect("Reading json body should succeed");
        let parsed: serde_json::Value = serde_json::from_slice(&json_bytes).unwrap();
        assert_eq!(
            parsed["choices"][0]["message"]["content"].as_str().unwrap(),
            "Hello world!"
        );
    }

    #[test]
    fn test_lru_capacity_eviction() {
        // Cache capacity of 2
        let cache = TokenCache::new(2, Duration::from_secs(60));

        cache.set_json("k1", Bytes::from_static(b"v1"));
        cache.set_json("k2", Bytes::from_static(b"v2"));
        assert_eq!(cache.len(), 2);

        // Access k1 to make it most recently used (k2 becomes least recently used)
        assert!(cache.get("k1").is_some());

        // Insert k3 -> k2 should be evicted
        cache.set_json("k3", Bytes::from_static(b"v3"));

        assert!(cache.get("k1").is_some(), "k1 was recently accessed, must remain");
        assert!(cache.get("k2").is_none(), "k2 was LRU, must be evicted");
        assert!(cache.get("k3").is_some(), "k3 was just inserted, must exist");
    }
}
