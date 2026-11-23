use std::collections::HashMap;
use std::time::Instant;

use axum::{
    extract::State,
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::db::{ApiKey, ProviderConnection, ProviderNode, RequestDetail, UsageRecord};
use crate::proxy::UpstreamConnection;
use crate::routes::AppState;

/// Input struct for OpenAI-compatible embeddings requests (/v1/embeddings and /api/v1/embeddings).
/// Accepts { model: String, input: serde_json::Value, user?: String, encoding_format?: String }
/// where input can be a single string or an array of strings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingRequest {
    pub model: String,
    pub input: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoding_format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dimensions: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_type: Option<String>,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// An individual embedding item in the response data array.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EmbeddingItem {
    pub object: String,
    pub index: usize,
    pub embedding: serde_json::Value,
}

/// Token usage statistics for the embeddings request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct EmbeddingUsage {
    pub prompt_tokens: usize,
    pub total_tokens: usize,
}

/// Standard OpenAI embeddings response structure:
/// { "object": "list", "data": [{ "object": "embedding", "index": i, "embedding": vec }], "model": model, "usage": { "prompt_tokens": pt, "total_tokens": pt } }
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EmbeddingResponse {
    pub object: String,
    pub data: Vec<EmbeddingItem>,
    pub model: String,
    pub usage: EmbeddingUsage,
}

/// Upstream provider resolution result for an embeddings request.
#[derive(Debug, Clone, PartialEq)]
pub enum EmbeddingTarget {
    Cloudflare {
        account_id: String,
        model: String,
        endpoint_url: String,
        api_key: Option<String>,
    },
    OpenAiCompatible {
        provider: String,
        model: String,
        endpoint_url: String,
        api_key: Option<String>,
        custom_headers: Option<HeaderMap>,
    },
}

/// Extracts text strings from input JSON value:
/// - Single string: vec![str]
/// - Array of strings: vec![str1, str2, ...]
/// Rejects non-string types, empty arrays, or non-string array elements.
pub fn extract_input_texts(val: &serde_json::Value) -> Result<Vec<String>, String> {
    match val {
        serde_json::Value::String(s) => Ok(vec![s.clone()]),
        serde_json::Value::Array(arr) => {
            if arr.is_empty() {
                return Err("input array must not be empty".to_string());
            }
            let mut texts = Vec::with_capacity(arr.len());
            for (i, item) in arr.iter().enumerate() {
                match item {
                    serde_json::Value::String(s) => texts.push(s.clone()),
                    _ => return Err(format!("input array item at index {} must be a string", i)),
                }
            }
            Ok(texts)
        }
        _ => Err("input must be a string or array of strings".to_string()),
    }
}

/// Estimates prompt tokens from input text strings (~4 characters per token).
pub fn estimate_prompt_tokens(texts: &[String]) -> usize {
    texts.iter().map(|s| (s.len() / 4).max(1)).sum()
}

/// Formats base URL into OpenAI embeddings endpoint without duplicate /v1 or /embeddings suffixes.
pub fn build_embeddings_url(base_url: &str) -> String {
    let trimmed = base_url.trim().trim_end_matches('/');
    if trimmed.ends_with("/embeddings") {
        return trimmed.to_string();
    }
    if trimmed.ends_with("/v1") || trimmed.ends_with("/v2") || trimmed.ends_with("/compat") {
        return format!("{}/embeddings", trimmed);
    }
    format!("{}/v1/embeddings", trimmed)
}

/// Normalizes Cloudflare model name to ensure standard `@cf/` format.
pub fn normalize_cloudflare_model(raw_model: &str) -> String {
    let trimmed = raw_model.trim();
    if let Some(stripped) = trimmed.strip_prefix("cf/@cf/") {
        format!("@cf/{}", stripped)
    } else if let Some(stripped) = trimmed.strip_prefix("cf/") {
        if stripped.starts_with('@') {
            stripped.to_string()
        } else {
            format!("@cf/{}", stripped)
        }
    } else if let Some(stripped) = trimmed.strip_prefix("cfr/@cf/") {
        format!("@cf/{}", stripped)
    } else if let Some(stripped) = trimmed.strip_prefix("cfr/") {
        if stripped.starts_with('@') {
            stripped.to_string()
        } else {
            format!("@cf/{}", stripped)
        }
    } else if let Some(stripped) = trimmed.strip_prefix("cloudflare-ai/@cf/") {
        format!("@cf/{}", stripped)
    } else if let Some(stripped) = trimmed.strip_prefix("cloudflare-ai/") {
        if stripped.starts_with('@') {
            stripped.to_string()
        } else {
            format!("@cf/{}", stripped)
        }
    } else if trimmed.starts_with("@cf/") {
        trimmed.to_string()
    } else {
        format!("@cf/{}", trimmed)
    }
}

/// Resolves provider target from provider nodes and connections:
/// 1. Node prefix matching (custom provider nodes)
/// 2. Cloudflare AI models (starts with @cf/, cf/, cfr/, cloudflare-ai/ or provider is cloudflare-ai)
/// 3. Standard provider prefix (e.g. azure/, ollama/, openai/)
/// 4. Connection defaultModel match
/// 5. OpenAI fallback
pub fn resolve_embedding_target(
    model: &str,
    provider_nodes: &[ProviderNode],
    all_connections: &[ProviderConnection],
) -> EmbeddingTarget {
    let active_conns: Vec<_> = all_connections.iter().filter(|c| c.is_active).collect();

    // 1. Check custom provider nodes (URL prefix routing)
    for node in provider_nodes {
        let mut matched_prefix: Option<String> = None;
        if !node.prefix.is_empty() {
            let prefix_slash = format!("{}/", node.prefix);
            if model.starts_with(&prefix_slash) || model == node.prefix {
                matched_prefix = Some(prefix_slash);
            }
        }
        if matched_prefix.is_none() {
            if let Some(ref name) = node.name {
                let name_slash = format!("{}/", name);
                if model.to_lowercase().starts_with(&name_slash.to_lowercase()) {
                    matched_prefix = Some(model[..name_slash.len()].to_string());
                }
            }
        }
        if matched_prefix.is_none() {
            let id_slash = format!("{}/", node.id);
            if model.starts_with(&id_slash) {
                matched_prefix = Some(id_slash);
            }
        }

        if let Some(prefix_slash) = matched_prefix {
            let upstream_model = if model.len() > prefix_slash.len() {
                model[prefix_slash.len()..].to_string()
            } else {
                model.to_string()
            };

            let is_cf_node = node.prefix == "cf"
                || node.prefix == "cfr"
                || node.name.as_deref() == Some("cloudflare-ai")
                || node.id == "cloudflare-ai";

            if is_cf_node {
                let cf_conn = active_conns.iter().find(|c| c.provider == "cloudflare-ai" || c.provider == "cf" || c.provider == node.id);
                let account_id = cf_conn
                    .and_then(|c| {
                        c.data.extra.get("providerSpecificData")
                            .and_then(|psd| psd.get("accountId"))
                            .or_else(|| c.data.extra.get("accountId"))
                            .and_then(|aid| aid.as_str())
                    })
                    .or_else(|| node.data.get("accountId").and_then(|aid| aid.as_str()))
                    .unwrap_or("1da22c19548115accf1b98ab7c562b2c")
                    .to_string();

                let cf_model = normalize_cloudflare_model(&upstream_model);
                let base_url = if !node.base_url.is_empty() {
                    let b = node.base_url.replace("{accountId}", &account_id).replace("{model}", &cf_model);
                    if b.ends_with("/ai/run") {
                        format!("{}/{}", b, cf_model)
                    } else if b.contains("/ai/run/") {
                        b
                    } else {
                        format!("{}/{}", b.trim_end_matches('/'), cf_model)
                    }
                } else {
                    format!("https://api.cloudflare.com/client/v4/accounts/{}/ai/run/{}", account_id, cf_model)
                };

                let api_key = cf_conn.and_then(|c| c.data.api_key.clone())
                    .or_else(|| node.data.get("apiKey").and_then(|v| v.as_str()).map(|s| s.to_string()));

                return EmbeddingTarget::Cloudflare {
                    account_id,
                    model: cf_model,
                    endpoint_url: base_url,
                    api_key,
                };
            }

            let mut api_key = node.data.get("apiKey").and_then(|v| v.as_str()).map(ToString::to_string);
            let mut custom_headers = None;

            if let Some(pc) = active_conns.iter().find(|c| c.provider == node.id || c.provider == node.prefix || node.name.as_ref() == Some(&c.provider)) {
                if api_key.is_none() {
                    api_key = pc.data.api_key.clone();
                }
                if let Some(hdrs) = &pc.data.headers {
                    let mut hmap = HeaderMap::new();
                    for (k, v) in hdrs {
                        if let (Ok(kn), Ok(kv)) = (
                            header::HeaderName::from_bytes(k.as_bytes()),
                            HeaderValue::from_str(v),
                        ) {
                            hmap.insert(kn, kv);
                        }
                    }
                    custom_headers = Some(hmap);
                }
            }

            return EmbeddingTarget::OpenAiCompatible {
                provider: node.name.clone().unwrap_or_else(|| node.prefix.clone()),
                model: upstream_model,
                endpoint_url: build_embeddings_url(&node.base_url),
                api_key,
                custom_headers,
            };
        }
    }

    // 2. Cloudflare AI models: @cf/ or cf/ or cfr/ or cloudflare-ai/
    let is_cloudflare = model.starts_with("@cf/")
        || model.starts_with("cf/")
        || model.starts_with("cfr/")
        || model.starts_with("cloudflare-ai/");

    if is_cloudflare {
        let cf_conn = active_conns.iter().find(|c| c.provider == "cloudflare-ai" || c.provider == "cf");
        let env_aid = std::env::var("CLOUDFLARE_ACCOUNT_ID").ok();
        let account_id = cf_conn
            .and_then(|c| {
                c.data.extra.get("providerSpecificData")
                    .and_then(|psd| psd.get("accountId"))
                    .or_else(|| c.data.extra.get("accountId"))
                    .and_then(|aid| aid.as_str())
                    .map(ToString::to_string)
            })
            .or(env_aid)
            .unwrap_or_else(|| "1da22c19548115accf1b98ab7c562b2c".to_string());

        let cf_model = normalize_cloudflare_model(model);
        let endpoint_url = if let Some(cf_c) = cf_conn {
            if let Some(b) = &cf_c.data.base_url {
                let replaced = b.replace("{accountId}", &account_id).replace("{model}", &cf_model);
                if replaced.ends_with("/ai/run") {
                    format!("{}/{}", replaced, cf_model)
                } else if replaced.contains("/ai/run/") {
                    replaced
                } else {
                    format!("{}/{}", replaced.trim_end_matches('/'), cf_model)
                }
            } else {
                format!("https://api.cloudflare.com/client/v4/accounts/{}/ai/run/{}", account_id, cf_model)
            }
        } else {
            format!("https://api.cloudflare.com/client/v4/accounts/{}/ai/run/{}", account_id, cf_model)
        };

        let api_key = cf_conn.and_then(|c| c.data.api_key.clone())
            .or_else(|| std::env::var("CLOUDFLARE_API_TOKEN").or_else(|_| std::env::var("CLOUDFLARE_API_KEY")).ok());

        return EmbeddingTarget::Cloudflare {
            account_id,
            model: cf_model,
            endpoint_url,
            api_key,
        };
    }

    // 3. Provider prefix routing (e.g. "azure/text-embedding-ada-002", "ollama/all-minilm")
    if let Some((p_prefix, m_part)) = model.split_once('/') {
        let conn_opt = active_conns.iter().find(|c| c.provider.eq_ignore_ascii_case(p_prefix));
        let provider = p_prefix.to_string();

        let (base_url, api_key, custom_headers) = if let Some(pc) = conn_opt {
            let mut b_url = pc.data.base_url.clone();
            let mut hmap = None;

            if provider.eq_ignore_ascii_case("azure") {
                if b_url.is_none() {
                    b_url = pc.data.extra.get("providerSpecificData")
                        .and_then(|psd| psd.get("azureEndpoint"))
                        .or_else(|| pc.data.extra.get("azureEndpoint"))
                        .and_then(|v| v.as_str())
                        .map(ToString::to_string);
                }
                if let Some(key) = &pc.data.api_key {
                    let mut m = HeaderMap::new();
                    if let Ok(val) = HeaderValue::from_str(key) {
                        m.insert(header::HeaderName::from_static("api-key"), val);
                    }
                    hmap = Some(m);
                }
            }

            if (provider.eq_ignore_ascii_case("ollama") || provider.eq_ignore_ascii_case("ollama-local")) && b_url.is_none() {
                b_url = Some("http://localhost:11434".to_string());
            }

            if let Some(hdrs) = &pc.data.headers {
                let mut m = hmap.unwrap_or_default();
                for (k, v) in hdrs {
                    if let (Ok(kn), Ok(kv)) = (
                        header::HeaderName::from_bytes(k.as_bytes()),
                        HeaderValue::from_str(v),
                    ) {
                        m.insert(kn, kv);
                    }
                }
                hmap = Some(m);
            }
            (b_url.unwrap_or_else(|| "https://api.openai.com".to_string()), pc.data.api_key.clone(), hmap)
        } else if provider.eq_ignore_ascii_case("ollama") || provider.eq_ignore_ascii_case("ollama-local") {
            ("http://localhost:11434".to_string(), None, None)
        } else {
            ("https://api.openai.com".to_string(), None, None)
        };

        return EmbeddingTarget::OpenAiCompatible {
            provider,
            model: m_part.to_string(),
            endpoint_url: build_embeddings_url(&base_url),
            api_key,
            custom_headers,
        };
    }

    // 4. Match by defaultModel
    if let Some(pc) = active_conns.iter().find(|c| c.data.default_model.as_deref() == Some(model)) {
        let base_url = pc.data.base_url.clone().unwrap_or_else(|| "https://api.openai.com".to_string());
        return EmbeddingTarget::OpenAiCompatible {
            provider: pc.provider.clone(),
            model: model.to_string(),
            endpoint_url: build_embeddings_url(&base_url),
            api_key: pc.data.api_key.clone(),
            custom_headers: None,
        };
    }

    // 5. Default OpenAI
    let openai_conn = active_conns.iter().find(|c| c.provider.eq_ignore_ascii_case("openai"));
    let (base_url, api_key) = if let Some(pc) = openai_conn {
        (
            pc.data.base_url.clone().unwrap_or_else(|| "https://api.openai.com".to_string()),
            pc.data.api_key.clone(),
        )
    } else {
        (
            "https://api.openai.com".to_string(),
            std::env::var("OPENAI_API_KEY").ok(),
        )
    };

    EmbeddingTarget::OpenAiCompatible {
        provider: "openai".to_string(),
        model: model.to_string(),
        endpoint_url: build_embeddings_url(&base_url),
        api_key,
        custom_headers: None,
    }
}

/// Formats a list of embedding vectors into EmbeddingItem list, encoding as base64 if requested.
pub fn format_embedding_items(
    embeddings: &[Vec<f64>],
    encoding_format: Option<&str>,
) -> Vec<EmbeddingItem> {
    let is_base64 = encoding_format
        .map(|s| s.eq_ignore_ascii_case("base64"))
        .unwrap_or(false);

    embeddings
        .iter()
        .enumerate()
        .map(|(idx, emb)| {
            let embedding_val = if is_base64 {
                use base64::Engine;
                let mut bytes = Vec::with_capacity(emb.len() * 4);
                for &f in emb {
                    bytes.extend_from_slice(&(f as f32).to_le_bytes());
                }
                serde_json::Value::String(base64::engine::general_purpose::STANDARD.encode(&bytes))
            } else {
                serde_json::Value::Array(
                    emb.iter().map(|&n| serde_json::json!(n)).collect(),
                )
            };

            EmbeddingItem {
                object: "embedding".to_string(),
                index: idx,
                embedding: embedding_val,
            }
        })
        .collect()
}

/// Parses and formats Cloudflare Workers AI response:
/// Cloudflare returns { result: { data: [[f64, ...], ...] } } or { result: { data: [f64, ...] } }
pub fn parse_cloudflare_embeddings(
    val: &serde_json::Value,
    model: &str,
    input_texts: &[String],
    encoding_format: Option<&str>,
) -> Result<EmbeddingResponse, String> {
    if let Some(errors) = val.get("errors").and_then(|v| v.as_array()) {
        if !errors.is_empty() {
            let msg = errors
                .iter()
                .filter_map(|e| e.get("message").and_then(|m| m.as_str()))
                .collect::<Vec<_>>()
                .join(", ");
            if !msg.is_empty() {
                return Err(format!("Cloudflare AI error: {}", msg));
            }
        }
    }

    let result = val
        .get("result")
        .ok_or_else(|| "Missing 'result' in Cloudflare response".to_string())?;
    let data_val = result
        .get("data")
        .ok_or_else(|| "Missing 'data' in Cloudflare result".to_string())?;

    let embeddings: Vec<Vec<f64>> = match data_val {
        serde_json::Value::Array(outer) => {
            if outer.is_empty() {
                Vec::new()
            } else if outer[0].is_array() {
                let mut list = Vec::with_capacity(outer.len());
                for sub in outer {
                    if let serde_json::Value::Array(vec_arr) = sub {
                        let vec: Vec<f64> = vec_arr.iter().filter_map(|n| n.as_f64()).collect();
                        list.push(vec);
                    }
                }
                list
            } else if outer[0].is_number() {
                let vec: Vec<f64> = outer.iter().filter_map(|n| n.as_f64()).collect();
                vec![vec]
            } else {
                return Err("Unexpected format in Cloudflare data array".to_string());
            }
        }
        _ => return Err("Cloudflare 'data' must be an array".to_string()),
    };

    let items = format_embedding_items(&embeddings, encoding_format);
    let prompt_tokens = estimate_prompt_tokens(input_texts);

    Ok(EmbeddingResponse {
        object: "list".to_string(),
        data: items,
        model: model.to_string(),
        usage: EmbeddingUsage {
            prompt_tokens,
            total_tokens: prompt_tokens,
        },
    })
}

/// Normalizes upstream response (OpenAI, Cloudflare, Ollama, Gemini) into standard OpenAI embeddings format.
pub fn normalize_openai_embeddings_response(
    val: &serde_json::Value,
    model: &str,
    input_texts: &[String],
    encoding_format: Option<&str>,
) -> Result<EmbeddingResponse, String> {
    // 1. Standard OpenAI response: { object: "list", data: [{ object: "embedding", index: 0, embedding: [...] }] }
    if val.get("object").and_then(|v| v.as_str()) == Some("list") {
        if let Some(arr) = val.get("data").and_then(|v| v.as_array()) {
            if arr.iter().all(|item| item.is_object() && item.get("embedding").is_some()) {
                let prompt_tokens = val
                    .get("usage")
                    .and_then(|u| u.get("prompt_tokens").or_else(|| u.get("input_tokens")))
                    .and_then(|t| t.as_u64())
                    .map(|t| t as usize)
                    .unwrap_or_else(|| estimate_prompt_tokens(input_texts));
                let total_tokens = val
                    .get("usage")
                    .and_then(|u| u.get("total_tokens"))
                    .and_then(|t| t.as_u64())
                    .map(|t| t as usize)
                    .unwrap_or(prompt_tokens);

                let is_base64 = encoding_format
                    .map(|s| s.eq_ignore_ascii_case("base64"))
                    .unwrap_or(false);

                let items: Vec<EmbeddingItem> = arr
                    .iter()
                    .enumerate()
                    .map(|(i, item)| {
                        let idx = item
                            .get("index")
                            .and_then(|v| v.as_u64())
                            .map(|v| v as usize)
                            .unwrap_or(i);
                        let mut emb = item.get("embedding").cloned().unwrap_or(serde_json::Value::Array(vec![]));

                        if is_base64 && emb.is_array() {
                            if let Some(arr) = emb.as_array() {
                                use base64::Engine;
                                let floats: Vec<f64> = arr.iter().filter_map(|n| n.as_f64()).collect();
                                let mut bytes = Vec::with_capacity(floats.len() * 4);
                                for &f in &floats {
                                    bytes.extend_from_slice(&(f as f32).to_le_bytes());
                                }
                                emb = serde_json::Value::String(
                                    base64::engine::general_purpose::STANDARD.encode(&bytes),
                                );
                            }
                        }

                        EmbeddingItem {
                            object: "embedding".to_string(),
                            index: idx,
                            embedding: emb,
                        }
                    })
                    .collect();

                let resp_model = val.get("model").and_then(|v| v.as_str()).unwrap_or(model);

                return Ok(EmbeddingResponse {
                    object: "list".to_string(),
                    data: items,
                    model: resp_model.to_string(),
                    usage: EmbeddingUsage {
                        prompt_tokens,
                        total_tokens,
                    },
                });
            }
        }
    }

    // 2. Cloudflare response
    if val.get("result").is_some() {
        return parse_cloudflare_embeddings(val, model, input_texts, encoding_format);
    }

    // 3. Alternative array format: { "embeddings": [[...], ...] }
    if let Some(arr) = val.get("embeddings").and_then(|v| v.as_array()) {
        let mut list = Vec::new();
        for item in arr {
            if let Some(sub) = item.as_array() {
                let vec: Vec<f64> = sub.iter().filter_map(|n| n.as_f64()).collect();
                list.push(vec);
            } else if let Some(obj) = item.as_object() {
                if let Some(vals) = obj.get("values").and_then(|v| v.as_array()) {
                    let vec: Vec<f64> = vals.iter().filter_map(|n| n.as_f64()).collect();
                    list.push(vec);
                }
            }
        }
        let items = format_embedding_items(&list, encoding_format);
        let pt = estimate_prompt_tokens(input_texts);
        return Ok(EmbeddingResponse {
            object: "list".to_string(),
            data: items,
            model: model.to_string(),
            usage: EmbeddingUsage {
                prompt_tokens: pt,
                total_tokens: pt,
            },
        });
    }

    // 4. Single embedding: { "embedding": [...] } (e.g. Ollama /api/embeddings)
    if let Some(emb_val) = val.get("embedding") {
        let vec: Vec<f64> = match emb_val {
            serde_json::Value::Array(a) => a.iter().filter_map(|n| n.as_f64()).collect(),
            serde_json::Value::Object(o) => o
                .get("values")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|n| n.as_f64()).collect())
                .unwrap_or_default(),
            _ => vec![],
        };
        let items = format_embedding_items(&[vec], encoding_format);
        let pt = estimate_prompt_tokens(input_texts);
        return Ok(EmbeddingResponse {
            object: "list".to_string(),
            data: items,
            model: model.to_string(),
            usage: EmbeddingUsage {
                prompt_tokens: pt,
                total_tokens: pt,
            },
        });
    }

    // 5. Raw data array: { "data": [[...], [...]] }
    if let Some(arr) = val.get("data").and_then(|v| v.as_array()) {
        if arr.iter().all(|x| x.is_array()) {
            let mut list = Vec::new();
            for sub in arr {
                if let Some(vec_arr) = sub.as_array() {
                    let vec: Vec<f64> = vec_arr.iter().filter_map(|n| n.as_f64()).collect();
                    list.push(vec);
                }
            }
            let items = format_embedding_items(&list, encoding_format);
            let pt = estimate_prompt_tokens(input_texts);
            return Ok(EmbeddingResponse {
                object: "list".to_string(),
                data: items,
                model: model.to_string(),
                usage: EmbeddingUsage {
                    prompt_tokens: pt,
                    total_tokens: pt,
                },
            });
        }
    }

    Err(format!("Unrecognized embeddings response structure from upstream: {}", val))
}

/// Core execution logic for embeddings request.
pub async fn handle_embeddings_inner(
    state: AppState,
    _headers: HeaderMap,
    body: axum::body::Bytes,
    api_key: ApiKey,
) -> Response {
    let start_time = Instant::now();

    // 1. Parse JSON body
    let req: EmbeddingRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                [(header::CONTENT_TYPE, "application/json")],
                Json(json!({
                    "error": {
                        "message": format!("Invalid JSON request body: {}", e),
                        "type": "invalid_request_error",
                        "param": null,
                        "code": "invalid_json"
                    }
                })),
            )
                .into_response();
        }
    };

    // 2. Validate model
    if req.model.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            [(header::CONTENT_TYPE, "application/json")],
            Json(json!({
                "error": {
                    "message": "Missing required field: model",
                    "type": "invalid_request_error",
                    "param": "model",
                    "code": "missing_required_field"
                }
            })),
        )
            .into_response();
    }

    // 3. Validate input
    let input_texts = match extract_input_texts(&req.input) {
        Ok(texts) => texts,
        Err(msg) => {
            return (
                StatusCode::BAD_REQUEST,
                [(header::CONTENT_TYPE, "application/json")],
                Json(json!({
                    "error": {
                        "message": msg,
                        "type": "invalid_request_error",
                        "param": "input",
                        "code": "invalid_value"
                    }
                })),
            )
                .into_response();
        }
    };

    // 4. Model and endpoint authorization
    let (_, clean_model) = crate::proxy::strip_provider_prefix(&req.model);
    if let Err(e) = state.db.check_model_allowed(&api_key, &clean_model) {
        return (
            StatusCode::FORBIDDEN,
            [(header::CONTENT_TYPE, "application/json")],
            Json(json!({
                "error": {
                    "message": e.to_string(),
                    "type": "permission_denied",
                    "param": "model",
                    "code": "model_not_allowed"
                }
            })),
        )
            .into_response();
    }

    if !api_key.allowed_endpoints.is_empty() {
        let allowed = api_key
            .allowed_endpoints
            .split(',')
            .any(|ep| {
                let ep_t = ep.trim();
                ep_t == "*" || ep_t == "embeddings" || ep_t == "/v1/embeddings" || ep_t == "/api/v1/embeddings"
            });
        if !allowed {
            return (
                StatusCode::FORBIDDEN,
                [(header::CONTENT_TYPE, "application/json")],
                Json(json!({
                    "error": {
                        "message": "Access to embeddings endpoint is not permitted for this API key",
                        "type": "permission_denied",
                        "param": null,
                        "code": "endpoint_not_allowed"
                    }
                })),
            )
                .into_response();
        }
    }

    // 5. Provider resolution
    let all_conns = state.db.load_provider_connections().unwrap_or_default();
    let provider_nodes = state.db.load_provider_nodes().unwrap_or_default();
    let target = resolve_embedding_target(&req.model, &provider_nodes, &all_conns);

    // 6. Upstream dispatch
    let (url, payload, conn, provider_name) = match target {
        EmbeddingTarget::Cloudflare {
            account_id: _,
            model: _,
            endpoint_url,
            api_key: cf_key,
        } => {
            let conn = UpstreamConnection {
                provider: "cloudflare-ai".to_string(),
                api_key: cf_key,
                ..Default::default()
            };
            let payload = json!({ "text": req.input });
            (endpoint_url, payload, conn, "cloudflare-ai".to_string())
        }
        EmbeddingTarget::OpenAiCompatible {
            provider,
            model: target_model,
            endpoint_url,
            api_key: prov_key,
            custom_headers,
        } => {
            let mut fwd = json!({
                "model": target_model,
                "input": req.input,
            });
            if let Some(enc) = &req.encoding_format {
                fwd["encoding_format"] = json!(enc);
            }
            if let Some(dim) = req.dimensions {
                fwd["dimensions"] = json!(dim);
            }
            if let Some(u) = &req.user {
                fwd["user"] = json!(u);
            }
            if let Some(it) = &req.input_type {
                fwd["input_type"] = json!(it);
            }
            let conn = UpstreamConnection {
                provider: provider.clone(),
                api_key: prov_key,
                custom_headers,
                ..Default::default()
            };
            (endpoint_url, fwd, conn, provider)
        }
    };

    let upstream_res = state
        .proxy
        .forward_custom_request(&url, &payload, &conn)
        .await;

    let resp = match upstream_res {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                [(header::CONTENT_TYPE, "application/json")],
                Json(json!({
                    "error": {
                        "message": format!("Upstream request failed: {}", e),
                        "type": "upstream_error",
                        "param": null,
                        "code": "bad_gateway"
                    }
                })),
            )
                .into_response();
        }
    };

    let status = resp.status();
    let resp_bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                [(header::CONTENT_TYPE, "application/json")],
                Json(json!({
                    "error": {
                        "message": format!("Failed to read upstream response body: {}", e),
                        "type": "upstream_error",
                        "param": null,
                        "code": "bad_gateway"
                    }
                })),
            )
                .into_response();
        }
    };

    if !status.is_success() {
        let err_json: serde_json::Value = serde_json::from_slice(&resp_bytes)
            .unwrap_or_else(|_| json!({ "error": String::from_utf8_lossy(&resp_bytes) }));
        let err_msg = err_json
            .get("error")
            .and_then(|e| e.get("message").or(Some(e)))
            .and_then(|m| m.as_str())
            .or_else(|| {
                err_json.get("errors")
                    .and_then(|e| e.as_array())
                    .and_then(|arr| arr.first())
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
            })
            .unwrap_or("Upstream error occurred");

        return (
            status,
            [(header::CONTENT_TYPE, "application/json")],
            Json(json!({
                "error": {
                    "message": format!("[{}] {}", provider_name, err_msg),
                    "type": "upstream_error",
                    "param": null,
                    "code": status.as_u16()
                }
            })),
        )
            .into_response();
    }

    let parsed_val: serde_json::Value = match serde_json::from_slice(&resp_bytes) {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                [(header::CONTENT_TYPE, "application/json")],
                Json(json!({
                    "error": {
                        "message": format!("Invalid JSON response from upstream: {}", e),
                        "type": "upstream_error",
                        "param": null,
                        "code": "invalid_upstream_json"
                    }
                })),
            )
                .into_response();
        }
    };

    let normalized = match normalize_openai_embeddings_response(
        &parsed_val,
        &req.model,
        &input_texts,
        req.encoding_format.as_deref(),
    ) {
        Ok(n) => n,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                [(header::CONTENT_TYPE, "application/json")],
                Json(json!({
                    "error": {
                        "message": format!("Failed to normalize upstream response: {}", e),
                        "type": "upstream_error",
                        "param": null,
                        "code": "normalization_failed"
                    }
                })),
            )
                .into_response();
        }
    };

    // 7. Write-behind usage logging
    let usage_record = UsageRecord {
        timestamp: chrono::Utc::now().to_rfc3339(),
        provider: Some(provider_name.clone()),
        model: Some(req.model.clone()),
        connection_id: None,
        api_key: Some(api_key.key.clone()),
        api_key_id: Some(api_key.id.clone()),
        endpoint: Some("/v1/embeddings".to_string()),
        prompt_tokens: normalized.usage.prompt_tokens as i64,
        completion_tokens: 0,
        cached_tokens: 0,
        cost: 0.0,
        status: "ok".to_string(),
        tokens: Some(serde_json::to_string(&normalized.usage).unwrap_or_default()),
        meta: None,
    };
    state.db.queue_usage(usage_record);

    if crate::routes::resolve_should_record_request_details(&state.config) {
        let detail = RequestDetail {
            provider: Some(provider_name),
            model: Some(req.model.clone()),
            endpoint: Some("/v1/embeddings".to_string()),
            latency_ms: Some(start_time.elapsed().as_millis() as u64),
            tokens: Some(json!({
                "prompt_tokens": normalized.usage.prompt_tokens,
                "total_tokens": normalized.usage.total_tokens,
            })),
            ..Default::default()
        };
        state.db.record_request_detail(detail);
    }

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
        ],
        Json(normalized),
    )
        .into_response()
}

/// Handler for /v1/embeddings and /api/v1/embeddings routes.
pub async fn embeddings_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let api_key = match crate::routes::authenticate(&headers, &state.db) {
        Ok(k) => k,
        Err(auth_err) => {
            if crate::auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db).is_ok() {
                ApiKey {
                    id: "dashboard-session".to_string(),
                    key: "dashboard-session".to_string(),
                    name: Some("Dashboard Internal Session".to_string()),
                    created_at: chrono::Utc::now().to_rfc3339(),
                    ..Default::default()
                }
            } else {
                return auth_err;
            }
        }
    };

    handle_embeddings_inner(state, headers, body, api_key).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{ConnectionData, ProviderConnection, ProviderNode};

    #[test]
    fn test_extract_input_texts_single_string() {
        let val = json!("The quick brown fox jumps over the lazy dog");
        let texts = extract_input_texts(&val).expect("should extract single string");
        assert_eq!(texts.len(), 1);
        assert_eq!(texts[0], "The quick brown fox jumps over the lazy dog");
    }

    #[test]
    fn test_extract_input_texts_batch_array() {
        let val = json!(["First text item", "Second text item", "Third text item"]);
        let texts = extract_input_texts(&val).expect("should extract array of strings");
        assert_eq!(texts.len(), 3);
        assert_eq!(texts[0], "First text item");
        assert_eq!(texts[1], "Second text item");
        assert_eq!(texts[2], "Third text item");
    }

    #[test]
    fn test_extract_input_texts_error_cases() {
        // Empty array
        let empty_arr = json!([]);
        assert!(extract_input_texts(&empty_arr).is_err());

        // Array with non-string elements
        let mixed_arr = json!(["Valid text", 12345, true]);
        let err = extract_input_texts(&mixed_arr).unwrap_err();
        assert!(err.contains("must be a string"));

        // Invalid types: number, bool, object, null
        assert!(extract_input_texts(&json!(42)).is_err());
        assert!(extract_input_texts(&json!(true)).is_err());
        assert!(extract_input_texts(&json!(null)).is_err());
        assert!(extract_input_texts(&json!({"text": "invalid"})).is_err());
    }

    #[test]
    fn test_build_embeddings_url_normalization() {
        assert_eq!(
            build_embeddings_url("http://localhost:11434"),
            "http://localhost:11434/v1/embeddings"
        );
        assert_eq!(
            build_embeddings_url("http://localhost:11434/v1"),
            "http://localhost:11434/v1/embeddings"
        );
        assert_eq!(
            build_embeddings_url("http://localhost:11434/v1/embeddings"),
            "http://localhost:11434/v1/embeddings"
        );
        assert_eq!(
            build_embeddings_url("https://api.openai.com"),
            "https://api.openai.com/v1/embeddings"
        );
        assert_eq!(
            build_embeddings_url("https://api.openai.com/v1"),
            "https://api.openai.com/v1/embeddings"
        );
        assert_eq!(
            build_embeddings_url("https://my-server.internal:8000/compat"),
            "https://my-server.internal:8000/compat/embeddings"
        );
    }

    #[test]
    fn test_normalize_cloudflare_model() {
        assert_eq!(
            normalize_cloudflare_model("@cf/baai/bge-small-en-v1.5"),
            "@cf/baai/bge-small-en-v1.5"
        );
        assert_eq!(
            normalize_cloudflare_model("cf/@cf/baai/bge-small-en-v1.5"),
            "@cf/baai/bge-small-en-v1.5"
        );
        assert_eq!(
            normalize_cloudflare_model("cf/baai/bge-small-en-v1.5"),
            "@cf/baai/bge-small-en-v1.5"
        );
        assert_eq!(
            normalize_cloudflare_model("cfr/@cf/baai/bge-base-en-v1.5"),
            "@cf/baai/bge-base-en-v1.5"
        );
        assert_eq!(
            normalize_cloudflare_model("cloudflare-ai/@cf/baai/bge-large-en-v1.5"),
            "@cf/baai/bge-large-en-v1.5"
        );
    }

    #[test]
    fn test_cloudflare_target_resolution_with_account_and_connection() {
        let mut conn_data = ConnectionData::default();
        conn_data.api_key = Some("cf-test-token-123".to_string());
        conn_data.extra.insert(
            "providerSpecificData".to_string(),
            json!({ "accountId": "acc-xyz-987" }),
        );

        let conns = vec![ProviderConnection {
            id: "conn-cf-1".to_string(),
            provider: "cloudflare-ai".to_string(),
            auth_type: "apikey".to_string(),
            name: Some("Cloudflare Workers AI".to_string()),
            email: None,
            priority: Some(10),
            is_active: true,
            data: conn_data,
            raw_data: "{}".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        }];

        // Test with @cf/ model
        let target = resolve_embedding_target("@cf/baai/bge-small-en-v1.5", &[], &conns);
        match target {
            EmbeddingTarget::Cloudflare {
                account_id,
                model,
                endpoint_url,
                api_key,
            } => {
                assert_eq!(account_id, "acc-xyz-987");
                assert_eq!(model, "@cf/baai/bge-small-en-v1.5");
                assert_eq!(
                    endpoint_url,
                    "https://api.cloudflare.com/client/v4/accounts/acc-xyz-987/ai/run/@cf/baai/bge-small-en-v1.5"
                );
                assert_eq!(api_key.as_deref(), Some("cf-test-token-123"));
            }
            _ => panic!("Expected Cloudflare target"),
        }

        // Test with cf/ prefix
        let target2 = resolve_embedding_target("cf/baai/bge-base-en-v1.5", &[], &conns);
        match target2 {
            EmbeddingTarget::Cloudflare {
                account_id,
                model,
                endpoint_url,
                api_key,
            } => {
                assert_eq!(account_id, "acc-xyz-987");
                assert_eq!(model, "@cf/baai/bge-base-en-v1.5");
                assert_eq!(
                    endpoint_url,
                    "https://api.cloudflare.com/client/v4/accounts/acc-xyz-987/ai/run/@cf/baai/bge-base-en-v1.5"
                );
                assert_eq!(api_key.as_deref(), Some("cf-test-token-123"));
            }
            _ => panic!("Expected Cloudflare target"),
        }
    }

    #[test]
    fn test_parse_cloudflare_embeddings_single_and_batch() {
        let input_single = vec!["Hello world".to_string()];
        let cf_single_resp = json!({
            "result": {
                "shape": [1, 3],
                "data": [[0.1, 0.2, 0.3]]
            },
            "success": true,
            "errors": [],
            "messages": []
        });

        let parsed_single = parse_cloudflare_embeddings(
            &cf_single_resp,
            "@cf/baai/bge-small-en-v1.5",
            &input_single,
            None,
        )
        .expect("should parse single embedding");

        assert_eq!(parsed_single.object, "list");
        assert_eq!(parsed_single.model, "@cf/baai/bge-small-en-v1.5");
        assert_eq!(parsed_single.data.len(), 1);
        assert_eq!(parsed_single.data[0].index, 0);
        assert_eq!(parsed_single.data[0].object, "embedding");
        assert_eq!(parsed_single.data[0].embedding, json!([0.1, 0.2, 0.3]));
        assert!(parsed_single.usage.prompt_tokens > 0);

        // Test batch array
        let input_batch = vec!["Sentence one".to_string(), "Sentence two".to_string()];
        let cf_batch_resp = json!({
            "result": {
                "shape": [2, 3],
                "data": [
                    [0.11, 0.22, 0.33],
                    [0.44, 0.55, 0.66]
                ]
            },
            "success": true
        });

        let parsed_batch = parse_cloudflare_embeddings(
            &cf_batch_resp,
            "@cf/baai/bge-base-en-v1.5",
            &input_batch,
            None,
        )
        .expect("should parse batch embeddings");

        assert_eq!(parsed_batch.data.len(), 2);
        assert_eq!(parsed_batch.data[0].index, 0);
        assert_eq!(parsed_batch.data[0].embedding, json!([0.11, 0.22, 0.33]));
        assert_eq!(parsed_batch.data[1].index, 1);
        assert_eq!(parsed_batch.data[1].embedding, json!([0.44, 0.55, 0.66]));
    }

    #[test]
    fn test_parse_cloudflare_embeddings_error_response() {
        let cf_err = json!({
            "success": false,
            "errors": [
                { "code": 7001, "message": "Model not found or access denied" }
            ],
            "messages": [],
            "result": null
        });

        let err = parse_cloudflare_embeddings(
            &cf_err,
            "@cf/baai/bge-small-en-v1.5",
            &["test".to_string()],
            None,
        )
        .unwrap_err();

        assert!(err.contains("Cloudflare AI error"));
        assert!(err.contains("Model not found"));
    }

    #[test]
    fn test_normalize_openai_embeddings_response_standard_passthrough() {
        let input_texts = vec!["Input sentence".to_string()];
        let standard_resp = json!({
            "object": "list",
            "data": [
                {
                    "object": "embedding",
                    "index": 0,
                    "embedding": [0.01, 0.02, 0.03]
                }
            ],
            "model": "text-embedding-3-small",
            "usage": {
                "prompt_tokens": 4,
                "total_tokens": 4
            }
        });

        let normalized = normalize_openai_embeddings_response(
            &standard_resp,
            "text-embedding-3-small",
            &input_texts,
            None,
        )
        .expect("should parse standard openai response");

        assert_eq!(normalized.object, "list");
        assert_eq!(normalized.model, "text-embedding-3-small");
        assert_eq!(normalized.data.len(), 1);
        assert_eq!(normalized.data[0].index, 0);
        assert_eq!(normalized.data[0].embedding, json!([0.01, 0.02, 0.03]));
        assert_eq!(normalized.usage.prompt_tokens, 4);
    }

    #[test]
    fn test_normalize_openai_embeddings_response_non_standard_formats() {
        let input_texts = vec!["Hello".to_string(), "World".to_string()];

        // 1. Raw array under "embeddings" (e.g. Cohere or custom servers)
        let raw_embeddings = json!({
            "embeddings": [
                [0.1, 0.2],
                [0.3, 0.4]
            ]
        });
        let norm1 = normalize_openai_embeddings_response(
            &raw_embeddings,
            "custom-embed-model",
            &input_texts,
            None,
        )
        .expect("should normalize raw embeddings array");
        assert_eq!(norm1.object, "list");
        assert_eq!(norm1.data.len(), 2);
        assert_eq!(norm1.data[0].index, 0);
        assert_eq!(norm1.data[0].embedding, json!([0.1, 0.2]));
        assert_eq!(norm1.data[1].index, 1);
        assert_eq!(norm1.data[1].embedding, json!([0.3, 0.4]));

        // 2. Single "embedding" key (e.g. Ollama /api/embeddings)
        let single_ollama = json!({
            "embedding": [0.77, 0.88, 0.99]
        });
        let norm2 = normalize_openai_embeddings_response(
            &single_ollama,
            "all-minilm",
            &["Single input".to_string()],
            None,
        )
        .expect("should normalize single embedding");
        assert_eq!(norm2.data.len(), 1);
        assert_eq!(norm2.data[0].embedding, json!([0.77, 0.88, 0.99]));

        // 3. Raw 2D array under "data"
        let raw_data = json!({
            "data": [
                [0.55, 0.66]
            ]
        });
        let norm3 = normalize_openai_embeddings_response(
            &raw_data,
            "raw-model",
            &["Test".to_string()],
            None,
        )
        .expect("should normalize raw data array");
        assert_eq!(norm3.data.len(), 1);
        assert_eq!(norm3.data[0].embedding, json!([0.55, 0.66]));
    }

    #[test]
    fn test_format_embedding_items_base64_encoding() {
        use base64::Engine;

        let embeddings = vec![vec![1.0f64, -2.5f64, 3.14f64]];
        let items = format_embedding_items(&embeddings, Some("base64"));
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].index, 0);

        let b64_str = items[0].embedding.as_str().expect("must be base64 string");
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(b64_str)
            .expect("should decode base64");
        assert_eq!(decoded.len(), 12); // 3 * 4 bytes for 32-bit floats

        let f0 = f32::from_le_bytes(decoded[0..4].try_into().unwrap());
        let f1 = f32::from_le_bytes(decoded[4..8].try_into().unwrap());
        let f2 = f32::from_le_bytes(decoded[8..12].try_into().unwrap());

        assert!((f0 - 1.0).abs() < 1e-6);
        assert!((f1 - -2.5).abs() < 1e-6);
        assert!((f2 - 3.14).abs() < 1e-4);
    }

    #[test]
    fn test_provider_resolution_nodes_and_azure() {
        let nodes = vec![ProviderNode {
            id: "node-vllm-embed".to_string(),
            node_type: Some("custom".to_string()),
            name: Some("Local vLLM".to_string()),
            prefix: "vllm".to_string(),
            base_url: "http://10.0.0.5:8000/v1".to_string(),
            api_type: Some("openai".to_string()),
            data: json!({ "apiKey": "vllm-secret-key" }),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        }];

        let mut azure_conn_data = ConnectionData::default();
        azure_conn_data.api_key = Some("azure-key-abc".to_string());
        azure_conn_data.extra.insert(
            "azureEndpoint".to_string(),
            json!("https://my-azure-resource.openai.azure.com"),
        );

        let conns = vec![ProviderConnection {
            id: "conn-azure".to_string(),
            provider: "azure".to_string(),
            auth_type: "apikey".to_string(),
            name: Some("Azure OpenAI".to_string()),
            email: None,
            priority: Some(1),
            is_active: true,
            data: azure_conn_data,
            raw_data: "{}".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        }];

        // Test node prefix resolution
        let target_node = resolve_embedding_target("vllm/bge-large-en", &nodes, &conns);
        match target_node {
            EmbeddingTarget::OpenAiCompatible {
                provider,
                model,
                endpoint_url,
                api_key,
                ..
            } => {
                assert_eq!(provider, "Local vLLM");
                assert_eq!(model, "bge-large-en");
                assert_eq!(endpoint_url, "http://10.0.0.5:8000/v1/embeddings");
                assert_eq!(api_key.as_deref(), Some("vllm-secret-key"));
            }
            _ => panic!("Expected OpenAiCompatible node target"),
        }

        // Test Azure provider resolution
        let target_azure = resolve_embedding_target("azure/text-embedding-ada-002", &nodes, &conns);
        match target_azure {
            EmbeddingTarget::OpenAiCompatible {
                provider,
                model,
                endpoint_url,
                api_key,
                custom_headers,
            } => {
                assert_eq!(provider, "azure");
                assert_eq!(model, "text-embedding-ada-002");
                assert_eq!(
                    endpoint_url,
                    "https://my-azure-resource.openai.azure.com/v1/embeddings"
                );
                assert_eq!(api_key.as_deref(), Some("azure-key-abc"));
                let hdrs = custom_headers.expect("Azure should set custom api-key header");
                assert_eq!(hdrs.get("api-key").unwrap(), "azure-key-abc");
            }
            _ => panic!("Expected OpenAiCompatible Azure target"),
        }
    }

    #[test]
    fn test_embedding_request_json_deserialization() {
        let raw = r#"{
            "model": "text-embedding-3-small",
            "input": ["First sentence", "Second sentence"],
            "user": "test-user-42",
            "encoding_format": "float",
            "dimensions": 1536
        }"#;

        let req: EmbeddingRequest = serde_json::from_str(raw).expect("should deserialize EmbeddingRequest");
        assert_eq!(req.model, "text-embedding-3-small");
        assert_eq!(req.user.as_deref(), Some("test-user-42"));
        assert_eq!(req.encoding_format.as_deref(), Some("float"));
        assert_eq!(req.dimensions, Some(1536));

        let texts = extract_input_texts(&req.input).expect("extract texts");
        assert_eq!(texts.len(), 2);
    }
}
