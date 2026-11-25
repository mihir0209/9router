use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use async_stream::stream;
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, NaiveDate, SecondsFormat, Utc};
use dashmap::DashMap;
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth;
use crate::db::{ApiKey, Combo, Database, ProviderConnection, ProviderNode};
use crate::proxy_pools::generate_uuid;
use crate::responses::{normalize_responses_request, responses_to_chat_request};
use crate::routes::{chat_completions_handler, options_handler, AppState};
use crate::token_refresh::{extract_refresh_token, refresh_oauth_token};
use crate::translator::{
    claude_to_openai_request, openai_to_claude_request, ChatCompletionRequest, ClaudeMessageRequest,
};

// ============================================================================
// Embedded Static Provider & Model Registry (145 providers, 1309 static models)
// ============================================================================

const PROVIDER_REGISTRY_JSON: &str = r##"[{"id":"alicode-intl","name":"Alibaba Coding","alias":"alicode-intl","uiAlias":"alicode-intl","aliases":[],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"qwen3.5-plus","name":"Qwen3.5 Plus"},{"id":"kimi-k2.5","name":"Kimi K2.5"},{"id":"glm-5","name":"GLM 5"},{"id":"MiniMax-M2.5","name":"MiniMax M2.5"},{"id":"qwen3-coder-next","name":"Qwen3 Coder Next"},{"id":"qwen3-coder-plus","name":"Qwen3 Coder Plus"},{"id":"glm-4.7","name":"GLM 4.7"}]},{"id":"alicode","name":"Alibaba","alias":"alicode","uiAlias":"alicode","aliases":[],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"qwen3.5-plus","name":"Qwen3.5 Plus"},{"id":"kimi-k2.5","name":"Kimi K2.5"},{"id":"glm-5","name":"GLM 5"},{"id":"MiniMax-M2.5","name":"MiniMax M2.5"},{"id":"qwen3-max-2026-01-23","name":"Qwen3 Max"},{"id":"qwen3-coder-next","name":"Qwen3 Coder Next"},{"id":"qwen3-coder-plus","name":"Qwen3 Coder Plus"},{"id":"glm-4.7","name":"GLM 4.7"}]},{"id":"anthropic","name":"Anthropic","alias":"anthropic","uiAlias":"anthropic","aliases":[],"serviceKinds":["llm","imageToText"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"claude-sonnet-4-20250514","name":"Claude Sonnet 4"},{"id":"claude-opus-4-20250514","name":"Claude Opus 4"},{"id":"claude-3-5-sonnet-20241022","name":"Claude 3.5 Sonnet"}]},{"id":"antigravity","name":"Antigravity","alias":"ag","uiAlias":"ag","aliases":[],"serviceKinds":["llm","image","webSearch"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"gemini-4-carbon-high","name":"Gemini 4 Carbon (High)"},{"id":"gemini-4-carbon-medium","name":"Gemini 4 Carbon (Medium)"},{"id":"gemini-4-carbon-low","name":"Gemini 4 Carbon (Low)"},{"id":"gemini-4-carbon","name":"Gemini 4 Carbon"},{"id":"gemini-4-argon-high","name":"Gemini 4 Argon (High)"},{"id":"gemini-4-argon-medium","name":"Gemini 4 Argon (Medium)"},{"id":"gemini-4-argon-low","name":"Gemini 4 Argon (Low)"},{"id":"gemini-3.8-flash-high","name":"Gemini 3.8 Flash (High)"},{"id":"gemini-3.8-flash-medium","name":"Gemini 3.8 Flash (Medium)"},{"id":"gemini-3.8-flash-low","name":"Gemini 3.8 Flash (Low)"},{"id":"gemini-3.8-flash","name":"Gemini 3.8 Flash"},{"id":"gemini-3.7-flash-high","name":"Gemini 3.7 Flash (High)"},{"id":"gemini-3.7-flash-medium","name":"Gemini 3.7 Flash (Medium)"},{"id":"gemini-3.7-flash-low","name":"Gemini 3.7 Flash (Low)"},{"id":"gemini-3.6-flash-high","name":"Gemini 3.6 Flash (High)"},{"id":"gemini-3.6-flash-medium","name":"Gemini 3.6 Flash (Medium)"},{"id":"gemini-3.6-flash-low","name":"Gemini 3.6 Flash (Low)"},{"id":"gemini-3.1-pro-high","name":"Gemini 3.1 Pro (High)"},{"id":"gemini-pro-agent","name":"Gemini 3.1 Pro (High)"},{"id":"gemini-3.1-pro-low","name":"Gemini 3.1 Pro (Low)"},{"id":"claude-sonnet-5-5-high","name":"Claude Sonnet 5.5 (High)"},{"id":"claude-sonnet-5-5-medium","name":"Claude Sonnet 5.5 (Medium)"},{"id":"claude-sonnet-5-5-low","name":"Claude Sonnet 5.5 (Low)"},{"id":"claude-sonnet-5-5","name":"Claude Sonnet 5.5 (Thinking)"},{"id":"claude-opus-5-5-high","name":"Claude Opus 5.5 (High)"},{"id":"claude-opus-5-5-medium","name":"Claude Opus 5.5 (Medium)"},{"id":"claude-opus-5-5-low","name":"Claude Opus 5.5 (Low)"},{"id":"claude-opus-5-5","name":"Claude Opus 5.5 (Thinking)"},{"id":"claude-sonnet-4-6","name":"Claude Sonnet 4.6 (Thinking)"},{"id":"claude-opus-4-6-thinking","name":"Claude Opus 4.6 (Thinking)"},{"id":"gpt-oss-120b-medium","name":"GPT-OSS 120B (Medium)"},{"id":"gemini-3.1-flash-image","name":"Gemini 3.1 Flash (Image)","kind":"image","capabilities":["textToImage"]}]},{"id":"assemblyai","name":"AssemblyAI","alias":"assemblyai","uiAlias":"aai","aliases":["aai"],"serviceKinds":["stt"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"universal-3-pro","name":"Universal 3 Pro","kind":"stt","params":["language"]},{"id":"universal-2","name":"Universal 2","kind":"stt","params":["language"]},{"id":"best","name":"Best (Nano + Universal)","kind":"stt"},{"id":"nano","name":"Nano (Fast)","kind":"stt"}]},{"id":"aws-polly","name":"AWS Polly","alias":"polly","uiAlias":"polly","aliases":[],"serviceKinds":["tts"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[]},{"id":"azure","name":"Azure OpenAI","alias":"azure","uiAlias":"azure","aliases":[],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[]},{"id":"black-forest-labs","name":"Black Forest Labs","alias":"black-forest-labs","uiAlias":"bfl","aliases":["bfl"],"serviceKinds":["image"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"flux-pro-1.1","name":"FLUX Pro 1.1","kind":"image","params":["n","size"]},{"id":"flux-pro-1.1-ultra","name":"FLUX Pro 1.1 Ultra","kind":"image","params":["size"]},{"id":"flux-pro","name":"FLUX Pro","kind":"image","params":["n","size"]},{"id":"flux-dev","name":"FLUX Dev","kind":"image","params":["n","size"]},{"id":"flux-kontext-pro","name":"FLUX Kontext Pro (Edit)","kind":"image","params":["size"],"capabilities":["edit"]},{"id":"flux-kontext-max","name":"FLUX Kontext Max (Edit)","kind":"image","params":["size"],"capabilities":["edit"]}]},{"id":"blackbox","name":"Blackbox AI","alias":"blackbox","uiAlias":"bb","aliases":["bb"],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"claude-fable-5","name":"Claude Fable 5"},{"id":"claude-opus-4.8","name":"Claude Opus 4.8"},{"id":"claude-sonnet-4.6","name":"Claude Sonnet 4.6"},{"id":"gpt-5.5","name":"GPT-5.5"},{"id":"gpt-5.4-pro","name":"GPT-5.4 Pro"},{"id":"gpt-5.4","name":"GPT-5.4"},{"id":"gpt-5.3-codex","name":"GPT-5.3 Codex"},{"id":"gpt-5.4-nano","name":"GPT-5.4 Nano"},{"id":"deepseek-v4-flash","name":"DeepSeek V4 Flash"},{"id":"grok-4.3","name":"Grok 4.3"}]},{"id":"brave-search","name":"Brave Search","alias":"brave","uiAlias":"brave","aliases":[],"serviceKinds":["webSearch"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"searchConfig":{"searchTypes":["web","news"],"maxMaxResults":20,"requiredOptions":null},"models":[]},{"id":"byteplus","name":"BytePlus ModelArk","alias":"byteplus","uiAlias":"bpm","aliases":["bpm"],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"seed-2-0-pro-260328","name":"Seed 2.0 Pro"},{"id":"seed-2-0-code-preview-260328","name":"Seed 2.0 Code Preview"},{"id":"seed-2-0-mini-260215","name":"Seed 2.0 Mini"},{"id":"seed-2-0-lite-260228","name":"Seed 2.0 Lite"},{"id":"kimi-k2-thinking-251104","name":"Kimi K2 Thinking"},{"id":"glm-4-7-251222","name":"GLM 4.7"},{"id":"gpt-oss-120b-250805","name":"GPT-OSS-120B"}]},{"id":"cartesia","name":"Cartesia","alias":"cartesia","uiAlias":"cartesia","aliases":[],"serviceKinds":["tts"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[]},{"id":"cerebras","name":"Cerebras","alias":"cerebras","uiAlias":"cerebras","aliases":[],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"gpt-oss-120b","name":"GPT OSS 120B"},{"id":"zai-glm-4.7","name":"ZAI GLM 4.7"},{"id":"llama-3.3-70b","name":"Llama 3.3 70B"},{"id":"llama-4-scout-17b-16e-instruct","name":"Llama 4 Scout"},{"id":"qwen-3-235b-a22b-instruct-2507","name":"Qwen3 235B A22B"},{"id":"qwen-3-32b","name":"Qwen3 32B"}]},{"id":"chutes","name":"Chutes AI","alias":"chutes","uiAlias":"ch","aliases":["ch"],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[]},{"id":"claude","name":"Claude Code","alias":"cc","uiAlias":"cc","aliases":[],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"claude-fable-5-2","name":"Claude Fable 5.2"},{"id":"claude-opus-5-5","name":"Claude Opus 5.5"},{"id":"claude-opus-5","name":"Claude Opus 5"},{"id":"claude-fable-5","name":"Claude Fable 5"},{"id":"claude-sonnet-5-5","name":"Claude Sonnet 5.5"},{"id":"claude-sonnet-5","name":"Claude Sonnet 5"},{"id":"claude-haiku-5-5","name":"Claude Haiku 5.5"},{"id":"claude-haiku-4-5-20251001","name":"Claude 4.5 Haiku"}]},{"id":"cline","name":"Cline","alias":"cl","uiAlias":"cl","aliases":[],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"anthropic/claude-opus-4.7","name":"Claude Opus 4.7"},{"id":"anthropic/claude-sonnet-4.6","name":"Claude Sonnet 4.6"},{"id":"anthropic/claude-opus-4.6","name":"Claude Opus 4.6"},{"id":"openai/gpt-5.3-codex","name":"GPT-5.3 Codex"},{"id":"openai/gpt-5.4","name":"GPT-5.4"},{"id":"google/gemini-3.1-pro-preview","name":"Gemini 3.1 Pro Preview"},{"id":"google/gemini-3.1-flash-lite-preview","name":"Gemini 3.1 Flash Lite Preview"},{"id":"kwaipilot/kat-coder-pro","name":"KAT Coder Pro"}]},{"id":"clinepass","name":"ClinePass","alias":"clinepass","uiAlias":"clinepass","aliases":[],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"cline-pass/glm-5.2","name":"GLM-5.2 (ClinePass)"},{"id":"cline-pass/kimi-k2.7-code","name":"Kimi K2.7 Code (ClinePass)"},{"id":"cline-pass/kimi-k2.6","name":"Kimi K2.6 (ClinePass)"},{"id":"cline-pass/deepseek-v4-pro","name":"DeepSeek V4 Pro (ClinePass)"},{"id":"cline-pass/deepseek-v4-flash","name":"DeepSeek V4 Flash (ClinePass)"},{"id":"cline-pass/mimo-v2.5","name":"MiMo-V2.5 (ClinePass)"},{"id":"cline-pass/mimo-v2.5-pro","name":"MiMo-V2.5-Pro (ClinePass)"},{"id":"cline-pass/minimax-m3","name":"MiniMax M3 (ClinePass)"},{"id":"cline-pass/qwen3.7-max","name":"Qwen3.7 Max (ClinePass)"},{"id":"cline-pass/qwen3.7-plus","name":"Qwen3.7 Plus (ClinePass)"}]},{"id":"cloudflare-ai","name":"Cloudflare","alias":"cloudflare-ai","uiAlias":"cf","aliases":["cf"],"serviceKinds":["llm","image","systemone"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"@cf/cloudflare/clef-flash","name":"Clef Flash","kind":"systemone"},{"id":"@cf/meta/llama-3.2-1b-instruct","name":"Llama 3.2 1B Instruct"},{"id":"@cf/meta/llama-3.2-3b-instruct","name":"Llama 3.2 3B Instruct"},{"id":"@cf/meta/llama-3.1-8b-instruct-fp8-fast","name":"Llama 3.1 8B Instruct FP8 Fast"},{"id":"@cf/meta/llama-3.1-8b-instruct-awq","name":"Llama 3.1 8B Instruct AWQ"},{"id":"@cf/mistralai/mistral-small-3.1-24b-instruct","name":"Mistral Small 3.1 24B Instruct"},{"id":"@cf/meta/llama-3.1-70b-instruct-fp8-fast","name":"Llama 3.1 70B Instruct FP8 Fast"},{"id":"@cf/meta/llama-3.3-70b-instruct-fp8-fast","name":"Llama 3.3 70B Instruct FP8 Fast"},{"id":"@cf/deepseek-ai/deepseek-r1-distill-qwen-32b","name":"DeepSeek R1 Distill Qwen 32B"},{"id":"@cf/moonshotai/kimi-k2.5","name":"Kimi K2.5"},{"id":"@cf/moonshotai/kimi-k2.6","name":"Kimi K2.6"},{"id":"@cf/zai-org/glm-4.7-flash","name":"GLM 4.7 Flash"},{"id":"@cf/qwen/qwq-32b","name":"QwQ 32B"},{"id":"@cf/qwen/qwen2.5-coder-32b-instruct","name":"Qwen 2.5 Coder 32B Instruct"},{"id":"@cf/black-forest-labs/flux-2-klein-9b","name":"FLUX.2 Klein 9B","kind":"image","params":["size"]},{"id":"@cf/black-forest-labs/flux-2-klein-4b","name":"FLUX.2 Klein 4B","kind":"image","params":["size"]},{"id":"@cf/black-forest-labs/flux-2-dev","name":"FLUX.2 Dev","kind":"image","params":["size"]},{"id":"@cf/leonardo/lucid-origin","name":"Lucid Origin","kind":"image","params":["size"]},{"id":"@cf/leonardo/phoenix-1.0","name":"Phoenix 1.0","kind":"image","params":["size"]},{"id":"@cf/black-forest-labs/flux-1-schnell","name":"FLUX.1 Schnell","kind":"image","params":["size"]},{"id":"@cf/bytedance/stable-diffusion-xl-lightning","name":"SDXL Lightning","kind":"image","params":["size"]},{"id":"@cf/lykon/dreamshaper-8-lcm","name":"DreamShaper 8 LCM","kind":"image","params":["size"]},{"id":"@cf/runwayml/stable-diffusion-v1-5-img2img","name":"Stable Diffusion v1.5 Img2Img","kind":"image","params":["size"],"capabilities":["edit"]},{"id":"@cf/runwayml/stable-diffusion-v1-5-inpainting","name":"Stable Diffusion v1.5 Inpainting","kind":"image","params":["size"],"capabilities":["edit","mask"]},{"id":"@cf/stabilityai/stable-diffusion-xl-base-1.0","name":"SDXL Base 1.0","kind":"image","params":["size"]}]},{"id":"codebuddy-cn","name":"CodeBuddy CN","alias":"cbcn","uiAlias":"cbcn","aliases":[],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"glm-5.2","name":"GLM-5.2"},{"id":"glm-5.1","name":"GLM-5.1"},{"id":"minimax-m3","name":"MiniMax-M3"},{"id":"hy3","name":"Hy3"},{"id":"hy4-preview","name":"Hy4-Preview"},{"id":"glm-5.3","name":"GLM-5.3"},{"id":"glm-5.3-flash","name":"GLM-5.3-Flash"},{"id":"kimi-k3-1","name":"Kimi-K3"},{"id":"kimi-k2.8-preview","name":"Kimi-K2.8-Preview"},{"id":"deepseek-v4-pro","name":"DeepSeek-V4-Pro"},{"id":"deepseek-v4.1-flash","name":"DeepSeek-V4.1-Flash"}]},{"id":"codex","name":"OpenAI Codex","alias":"cx","uiAlias":"cx","aliases":[],"serviceKinds":["llm","image","stt"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"gpt-6.1-sol","name":"GPT 6.1 Sol"},{"id":"gpt-6.1-terra","name":"GPT 6.1 Terra"},{"id":"gpt-6-astra","name":"GPT 6.0 Astra"},{"id":"gpt-6-astra[1m]","name":"GPT 6.0 Astra (extended context)"},{"id":"gpt-6-sol","name":"GPT 6.0 Sol"},{"id":"gpt-6-sol[1m]","name":"GPT 6.0 Sol (extended context)"},{"id":"gpt-6-luna","name":"GPT 6.0 Luna"},{"id":"gpt-6-luna[1m]","name":"GPT 6.0 Luna (extended context)"},{"id":"gpt-5.6-sol","name":"GPT 5.6 Sol"},{"id":"gpt-5.6-sol[1m]","name":"GPT 5.6 Sol (extended context)"},{"id":"gpt-5.6-sol-review","name":"GPT 5.6 Sol Review"},{"id":"gpt-5.6-terra","name":"GPT 5.6 Terra"},{"id":"gpt-5.6-terra[1m]","name":"GPT 5.6 Terra (extended context)"},{"id":"gpt-5.6-terra-review","name":"GPT 5.6 Terra Review"},{"id":"gpt-5.6-luna","name":"GPT 5.6 Luna"},{"id":"gpt-5.6-luna[1m]","name":"GPT 5.6 Luna (extended context)"},{"id":"gpt-5.6-luna-review","name":"GPT 5.6 Luna Review"},{"id":"gpt-daybreak-blue-latest","name":"GPT Daybreak Blue"},{"id":"gpt-reserve","name":"GPT Reserve"},{"id":"codex-auto-review","name":"Codex Auto Review"},{"id":"gpt-image-2.5","name":"GPT Image 2.5","kind":"image","params":["size","quality","background","image_detail","output_format"],"capabilities":["text2img","edit","multiImage"]},{"id":"gpt-image-2.5-flare","name":"GPT Image 2.5 Flare","kind":"image","params":["size","quality","background","image_detail","output_format"],"capabilities":["text2img","edit","multiImage"]},{"id":"gpt-image-2.5-sunburst","name":"GPT Image 2.5 Sunburst","kind":"image","params":["size","quality","background","image_detail","output_format"],"capabilities":["text2img","edit","multiImage"]},{"id":"gpt-image-2.6-flare","name":"GPT Image 2.6 Flare","kind":"image","params":["size","quality","background","image_detail","output_format"],"capabilities":["text2img","edit","multiImage"]},{"id":"gpt-image-2.6-sunburst","name":"GPT Image 2.6 Sunburst","kind":"image","params":["size","quality","background","image_detail","output_format"],"capabilities":["text2img","edit","multiImage"]},{"id":"gpt-image-2","name":"GPT Image 2","kind":"image","params":["size","quality","background","image_detail","output_format"],"capabilities":["text2img","edit","multiImage"]},{"id":"gpt-image-1.5","name":"GPT Image 1.5","kind":"image","params":["size","quality","background","image_detail","output_format"],"capabilities":["text2img","edit","multiImage"]},{"id":"gpt-6-sol-image","name":"GPT 6.0 Sol Image","kind":"image","params":["size","quality","background","image_detail","output_format"],"capabilities":["text2img","edit"]},{"id":"transcribe","name":"Codex Transcription","kind":"stt","params":["response_format"]}]},{"id":"cohere","name":"Cohere","alias":"cohere","uiAlias":"cohere","aliases":[],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"command-r-plus-08-2024","name":"Command R+ (Aug 2024)"},{"id":"command-r-08-2024","name":"Command R (Aug 2024)"},{"id":"command-a-03-2025","name":"Command A (Mar 2025)"}]},{"id":"comfyui","name":"ComfyUI","alias":"comfyui","uiAlias":"comfyui","aliases":[],"serviceKinds":["image"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"flux-dev","name":"FLUX Dev","kind":"image","params":["n","size"]},{"id":"sdxl","name":"SDXL","kind":"image","params":["n","size"]}]},{"id":"commandcode","name":"Command Code","alias":"commandcode","uiAlias":"cmc","aliases":["cmc"],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":true,"hasFetch":false,"models":[{"id":"deepseek/deepseek-v4-pro","name":"DeepSeek V4 Pro"},{"id":"deepseek/deepseek-v4-flash","name":"DeepSeek V4 Flash"},{"id":"moonshotai/Kimi-K3.1-Pro","name":"Kimi K3.1 Pro"},{"id":"moonshotai/Kimi-K2.7-Code","name":"Kimi K2.7 Code"},{"id":"moonshotai/Kimi-K2.7-Code-Highspeed","name":"Kimi K2.7 Code HighSpeed"},{"id":"moonshotai/Kimi-K2.6","name":"Kimi K2.6"},{"id":"moonshotai/Kimi-K2.5","name":"Kimi K2.5"},{"id":"zai-org/GLM-5.2","name":"GLM 5.2"},{"id":"zai-org/GLM-5.2-Fast","name":"GLM 5.2 Fast"},{"id":"zai-org/GLM-5.1","name":"GLM 5.1"},{"id":"zai-org/GLM-5","name":"GLM 5"},{"id":"MiniMaxAI/MiniMax-M3","name":"MiniMax M3"},{"id":"MiniMaxAI/MiniMax-M2.7","name":"MiniMax M2.7"},{"id":"MiniMaxAI/MiniMax-M2.5","name":"MiniMax M2.5"},{"id":"xiaomi/mimo-v2.5-pro","name":"MiMo V2.5 Pro"},{"id":"xiaomi/mimo-v2.5","name":"MiMo V2.5"},{"id":"Qwen/Qwen3.6-Max-Preview","name":"Qwen 3.6 Max Preview"},{"id":"Qwen/Qwen3.6-Plus","name":"Qwen 3.6 Plus"},{"id":"Qwen/Qwen3.7-Max","name":"Qwen 3.7 Max"},{"id":"Qwen/Qwen3.7-Plus","name":"Qwen 3.7 Plus"},{"id":"stepfun/Step-3.7-Flash","name":"Step 3.7 Flash"},{"id":"stepfun/Step-3.5-Flash","name":"Step 3.5 Flash"},{"id":"nvidia/nemotron-3-ultra-550b-a55b","name":"Nemotron 3 Ultra"},{"id":"google/gemini-4-carbon","name":"Gemini 4 Carbon"},{"id":"claude-fable-5-2","name":"Claude Fable 5.2"}]},{"id":"coqui","name":"Coqui TTS","alias":"coqui","uiAlias":"coqui","aliases":[],"serviceKinds":["tts"],"noAuth":true,"passthroughModels":false,"hasFetch":false,"models":[]},{"id":"cursor","name":"Cursor IDE","alias":"cu","uiAlias":"cu","aliases":[],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"default","name":"Auto (Server Picks)"},{"id":"claude-4.5-opus-high-thinking","name":"Claude 4.5 Opus High Thinking"},{"id":"claude-4.5-opus-high","name":"Claude 4.5 Opus High"},{"id":"claude-4.5-sonnet-thinking","name":"Claude 4.5 Sonnet Thinking"},{"id":"claude-4.5-sonnet","name":"Claude 4.5 Sonnet"},{"id":"claude-4.5-haiku","name":"Claude 4.5 Haiku"},{"id":"claude-4.5-opus","name":"Claude 4.5 Opus"},{"id":"gpt-5.2-codex","name":"GPT 5.2 Codex"},{"id":"claude-4.6-opus-max","name":"Claude 4.6 Opus Max"},{"id":"claude-4.6-sonnet-medium-thinking","name":"Claude 4.6 Sonnet Medium Thinking"},{"id":"kimi-k2.5","name":"Kimi K2.5"},{"id":"gemini-3-flash-preview","name":"Gemini 3 Flash Preview"},{"id":"gpt-5.2","name":"GPT 5.2"},{"id":"gpt-5.3-codex","name":"GPT 5.3 Codex"}]},{"id":"deepgram","name":"Deepgram","alias":"deepgram","uiAlias":"dg","aliases":["dg"],"serviceKinds":["stt"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"nova-3","name":"Nova 3","kind":"stt","params":["language"]},{"id":"nova-2","name":"Nova 2","kind":"stt","params":["language"]},{"id":"whisper-large","name":"Whisper Large","kind":"stt","params":["language"]},{"id":"nova","name":"Nova","kind":"stt"}]},{"id":"deepseek","name":"DeepSeek","alias":"deepseek","uiAlias":"ds","aliases":["ds"],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"deepseek-v4-pro","name":"DeepSeek V4 Pro"},{"id":"deepseek-v4-pro-max","name":"DeepSeek V4 Pro Max"},{"id":"deepseek-v4-pro-none","name":"DeepSeek V4 Pro No Thinking"},{"id":"deepseek-v4.1-flash","name":"DeepSeek V4.1 Flash"},{"id":"deepseek-v4-flash","name":"DeepSeek V4 Flash"},{"id":"deepseek-v4-flash-vision-exp","name":"DeepSeek V4 Flash Vision (Exp)"},{"id":"deepseek-chat","name":"DeepSeek V3.2 Chat"},{"id":"deepseek-reasoner","name":"DeepSeek V3.2 Reasoner"}]},{"id":"edge-tts","name":"Edge TTS","alias":"edge-tts","uiAlias":"edge-tts","aliases":[],"serviceKinds":["tts"],"noAuth":true,"passthroughModels":false,"hasFetch":false,"models":[]},{"id":"elevenlabs","name":"ElevenLabs","alias":"el","uiAlias":"el","aliases":[],"serviceKinds":["tts","stt"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"scribe_v1","name":"Scribe v1","kind":"stt","params":["language","response_format","timestamps_granularity","tag_audio_events","diarize","num_speakers"]},{"id":"scribe_v2","name":"Scribe v2","kind":"stt","params":["language","response_format","timestamps_granularity","tag_audio_events","diarize","num_speakers"]}]},{"id":"exa","name":"Exa","alias":"exa","uiAlias":"exa","aliases":[],"serviceKinds":["webSearch","webFetch"],"noAuth":false,"passthroughModels":false,"hasFetch":true,"searchConfig":{"searchTypes":["web","news"],"maxMaxResults":100,"requiredOptions":null},"models":[]},{"id":"fal-ai","name":"Fal.ai","alias":"fal-ai","uiAlias":"fal","aliases":["fal"],"serviceKinds":["image"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"fal-ai/flux/schnell","name":"FLUX Schnell","kind":"image","params":["n","size"]},{"id":"fal-ai/flux/dev","name":"FLUX Dev","kind":"image","params":["n","size"]},{"id":"fal-ai/flux-pro/v1.1","name":"FLUX Pro v1.1","kind":"image","params":["n","size"]},{"id":"fal-ai/flux-pro/v1.1-ultra","name":"FLUX Pro v1.1 Ultra","kind":"image","params":["n","size"]},{"id":"fal-ai/recraft-v3","name":"Recraft V3","kind":"image","params":["n","size","style"]},{"id":"fal-ai/ideogram/v2","name":"Ideogram V2","kind":"image","params":["n","size","style"]},{"id":"fal-ai/stable-diffusion-v35-large","name":"SD 3.5 Large","kind":"image","params":["n","size"]}]},{"id":"featherless","name":"Featherless","alias":"featherless","uiAlias":"fl","aliases":["fl"],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"deepseek-ai/DeepSeek-V4-Pro","name":"DeepSeek V4 Pro"},{"id":"deepseek-ai/DeepSeek-V4-Flash","name":"DeepSeek V4 Flash"},{"id":"zai-org/GLM-5.2","name":"GLM 5.2"},{"id":"zai-org/GLM-5.1","name":"GLM 5.1"},{"id":"moonshotai/Kimi-K2.7-Code","name":"Kimi K2.7 Code"},{"id":"moonshotai/Kimi-K2.6","name":"Kimi K2.6"},{"id":"moonshotai/Kimi-K2.5","name":"Kimi K2.5"}]},{"id":"firecrawl","name":"Firecrawl","alias":"firecrawl","uiAlias":"firecrawl","aliases":[],"serviceKinds":["webFetch"],"noAuth":false,"passthroughModels":false,"hasFetch":true,"models":[]},{"id":"fireworks","name":"Fireworks AI","alias":"fireworks","uiAlias":"fireworks","aliases":[],"serviceKinds":["llm","embedding"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"accounts/fireworks/models/deepseek-v3p1","name":"DeepSeek V3.1"},{"id":"accounts/fireworks/models/llama-v3p3-70b-instruct","name":"Llama 3.3 70B"},{"id":"accounts/fireworks/models/qwen3-235b-a22b","name":"Qwen3 235B"},{"id":"nomic-ai/nomic-embed-text-v1.5","name":"Nomic Embed Text v1.5","kind":"embedding"}]},{"id":"gemini-cli","name":"Gemini CLI","alias":"gc","uiAlias":"gc","aliases":[],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"gemini-3.1-pro-preview","name":"Gemini 3.1 Pro Preview"},{"id":"gemini-3-pro-preview","name":"Gemini 3 Pro Preview"},{"id":"gemini-3-flash-preview","name":"Gemini 3 Flash Preview"},{"id":"gemini-3.1-flash-lite-preview","name":"Gemini 3.1 Flash Lite Preview"},{"id":"gemini-2.5-pro","name":"Gemini 2.5 Pro"},{"id":"gemini-2.5-flash","name":"Gemini 2.5 Flash"},{"id":"gemini-2.5-flash-lite","name":"Gemini 2.5 Flash Lite"}]},{"id":"gemini","name":"Gemini","alias":"gemini","uiAlias":"gemini","aliases":[],"serviceKinds":["llm","embedding","image","imageToText","webSearch","tts","stt"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"gemini-4-argon","name":"Gemini 4 Argon"},{"id":"gemini-4-carbon","name":"Gemini 4 Carbon"},{"id":"gemini-3.8-flash","name":"Gemini 3.8 Flash"},{"id":"gemini-3.7-flash","name":"Gemini 3.7 Flash"},{"id":"gemini-3.6-flash","name":"Gemini 3.6 Flash"},{"id":"gemini-3.5-flash-lite","name":"Gemini 3.5 Flash Lite"},{"id":"gemini-3.1-pro-preview","name":"Gemini 3.1 Pro Preview"},{"id":"gemini-3.1-flash-lite-preview","name":"Gemini 3.1 Flash Lite Preview"},{"id":"gemini-3-flash-preview","name":"Gemini 3 Flash Preview"},{"id":"gemini-2.5-pro","name":"Gemini 2.5 Pro"},{"id":"gemini-2.5-flash","name":"Gemini 2.5 Flash"},{"id":"gemini-2.5-flash-lite","name":"Gemini 2.5 Flash Lite"},{"id":"gemma-4-31b-it","name":"Gemma 4 31B IT"},{"id":"gemini-embedding-2-preview","name":"Gemini Embedding 2 Preview","kind":"embedding"},{"id":"gemini-embedding-001","name":"Gemini Embedding 001","kind":"embedding"},{"id":"text-embedding-005","name":"Text Embedding 005","kind":"embedding"},{"id":"text-embedding-004","name":"Text Embedding 004 (Legacy)","kind":"embedding"},{"id":"gemini-3.1-flash-image-preview","name":"Gemini 3.1 Flash Image (Nano Banana 2)","kind":"image","params":[]},{"id":"gemini-3-pro-image-preview","name":"Gemini 3 Pro Image (Nano Banana Pro)","kind":"image","params":[]},{"id":"gemini-2.5-flash-image","name":"Gemini 2.5 Flash Image (Nano Banana)","kind":"image","params":[]},{"id":"gemini-2.5-pro","name":"Gemini 2.5 Pro (Best)","kind":"stt","params":["language","prompt"]},{"id":"gemini-2.5-flash","name":"Gemini 2.5 Flash","kind":"stt","params":["language","prompt"]},{"id":"gemini-2.5-flash-lite","name":"Gemini 2.5 Flash Lite (Cheapest)","kind":"stt","params":["language","prompt"]},{"id":"gemini-2.0-flash","name":"Gemini 2.0 Flash","kind":"stt","params":["language","prompt"]},{"id":"gemini-2.5-flash-native-audio-preview-09-17","name":"Gemini Live Transcription (Realtime)","kind":"stt","params":["language","prompt","system_instruction","setup_timeout_ms","turn_timeout_ms"]},{"id":"gemini-3.1-flash-tts-preview","name":"Gemini 3.1 Flash TTS","kind":"tts"},{"id":"gemini-2.5-flash-preview-tts","name":"Gemini 2.5 Flash TTS","kind":"tts"},{"id":"gemini-2.5-pro-preview-tts","name":"Gemini 2.5 Pro TTS","kind":"tts"},{"id":"embedding-001","name":"Embedding 001","kind":"embedding","dimensions":768}]},{"id":"github","name":"GitHub Copilot","alias":"gh","uiAlias":"gh","aliases":[],"serviceKinds":["llm","embedding"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"gpt-5-mini","name":"GPT-5 Mini"},{"id":"gpt-5.3-codex","name":"GPT-5.3 Codex"},{"id":"gpt-5.4","name":"GPT-5.4"},{"id":"gpt-5.4-mini","name":"GPT-5.4 Mini"},{"id":"gpt-5.5","name":"GPT-5.5"},{"id":"gpt-5.6-luna","name":"GPT-5.6 Luna"},{"id":"gpt-5.6-sol","name":"GPT-5.6 Sol"},{"id":"gpt-5.6-terra","name":"GPT-5.6 Terra"},{"id":"gpt-6-astra","name":"GPT-6 Astra"},{"id":"gpt-6-luna","name":"GPT-6 Luna"},{"id":"gpt-6-sol","name":"GPT-6 Sol"},{"id":"gpt-6.1-sol","name":"GPT-6.1 Sol"},{"id":"claude-fable-5","name":"Claude Fable 5"},{"id":"claude-haiku-4.5","name":"Claude Haiku 4.5"},{"id":"claude-haiku-5.5","name":"Claude Haiku 5.5"},{"id":"claude-opus-4.8","name":"Claude Opus 4.8"},{"id":"claude-opus-4.8-fast","name":"Claude Opus 4.8 (Fast)"},{"id":"claude-opus-5","name":"Claude Opus 5"},{"id":"claude-opus-5.5","name":"Claude Opus 5.5"},{"id":"claude-sonnet-4.6","name":"Claude Sonnet 4.6"},{"id":"claude-sonnet-5","name":"Claude Sonnet 5"},{"id":"claude-sonnet-5.5","name":"Claude Sonnet 5.5"},{"id":"gemini-3.7-flash","name":"Gemini 3.7 Flash"},{"id":"gemini-3.8-flash","name":"Gemini 3.8 Flash"},{"id":"gemini-4-argon","name":"Gemini 4 Argon"},{"id":"kimi-k3","name":"Kimi K3"},{"id":"mai-code-1.1-flash","name":"MAI-Code-1.1-Flash"},{"id":"text-embedding-3-small","name":"Text Embedding 3 Small (GitHub)","kind":"embedding"},{"id":"text-embedding-ada-002","name":"Text Embedding Ada 002 (GitHub)","kind":"embedding"}]},{"id":"gitlab","name":"GitLab Duo","alias":"gitlab","uiAlias":"gitlab","aliases":[],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[]},{"id":"glm-cn","name":"GLM (China)","alias":"glm-cn","uiAlias":"glm-cn","aliases":[],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"glm-5.3","name":"GLM 5.3"},{"id":"glm-5.3-flash","name":"GLM 5.3 Flash (Vision)"},{"id":"glm-5.2","name":"GLM 5.2"},{"id":"glm-5.1","name":"GLM 5.1"},{"id":"glm-5-turbo","name":"GLM 5 Turbo"},{"id":"glm-5","name":"GLM 5"},{"id":"glm-4.7","name":"GLM-4.7"},{"id":"glm-4.6v","name":"GLM 4.6V (Vision)"},{"id":"glm-4.6","name":"GLM-4.6"},{"id":"glm-4.5-air","name":"GLM-4.5-Air"}]},{"id":"glm","name":"Zai GLM Coding","alias":"glm","uiAlias":"glm","aliases":[],"serviceKinds":["llm","webSearch"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"searchConfig":{"searchTypes":["web"],"maxMaxResults":50,"requiredOptions":null},"models":[{"id":"glm-5.3","name":"GLM 5.3"},{"id":"glm-5.3-flash","name":"GLM 5.3 Flash (Vision)"},{"id":"glm-5.2","name":"GLM 5.2"},{"id":"glm-5.1","name":"GLM 5.1"},{"id":"glm-5-turbo","name":"GLM 5 Turbo"},{"id":"glm-5","name":"GLM 5"},{"id":"glm-4.7","name":"GLM 4.7"},{"id":"glm-4.6v","name":"GLM 4.6V (Vision)"}]},{"id":"google-pse","name":"Google PSE","alias":"gpse","uiAlias":"gpse","aliases":[],"serviceKinds":["webSearch"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"searchConfig":{"searchTypes":["web","news"],"maxMaxResults":10,"requiredOptions":null},"models":[]},{"id":"google-tts","name":"Google TTS","alias":"google-tts","uiAlias":"google-tts","aliases":[],"serviceKinds":["tts"],"noAuth":true,"passthroughModels":false,"hasFetch":false,"models":[]},{"id":"grok-cli","name":"Grok CLI (Grok Build)","alias":"gcli","uiAlias":"gcli","aliases":["grok-build","gb"],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"grok-build","name":"Grok Build"},{"id":"grok-4.7","name":"Grok 4.7"},{"id":"grok-4.5","name":"Grok 4.5"},{"id":"grok-4.5-high","name":"Grok 4.5 (High)"},{"id":"grok-4.5-medium","name":"Grok 4.5 (Medium)"},{"id":"grok-4.5-low","name":"Grok 4.5 (Low)"}]},{"id":"grok-web","name":"Grok Web (Subscription)","alias":"grok-web","uiAlias":"gw","aliases":["gw"],"serviceKinds":["image","video"],"noAuth":false,"passthroughModels":true,"hasFetch":false,"models":[{"id":"grok-image","name":"Grok Imagine Image","kind":"image","params":["n","size"]},{"id":"grok-video","name":"Grok Imagine Video","kind":"video","params":["seconds","size"]},{"id":"grok-3","name":"Grok 3"},{"id":"grok-3-mini","name":"Grok 3 Mini (Thinking)"},{"id":"grok-3-thinking","name":"Grok 3 Thinking"},{"id":"grok-4","name":"Grok 4"},{"id":"grok-4-mini","name":"Grok 4 Mini (Thinking)"},{"id":"grok-4-thinking","name":"Grok 4 Thinking"},{"id":"grok-4-heavy","name":"Grok 4 Heavy (SuperGrok)"},{"id":"grok-4.1-mini","name":"Grok 4.1 Mini (Thinking)"},{"id":"grok-4.1-fast","name":"Grok 4.1 Fast"},{"id":"grok-4.1-expert","name":"Grok 4.1 Expert"},{"id":"grok-4.1-thinking","name":"Grok 4.1 Thinking"},{"id":"grok-4.2","name":"Grok 4.2 (4.20 Beta)"}]},{"id":"groq","name":"Groq","alias":"groq","uiAlias":"groq","aliases":[],"serviceKinds":["llm","imageToText","stt"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"llama-3.3-70b-versatile","name":"Llama 3.3 70B"},{"id":"meta-llama/llama-4-maverick-17b-128e-instruct","name":"Llama 4 Maverick"},{"id":"qwen/qwen3-32b","name":"Qwen3 32B"},{"id":"openai/gpt-oss-120b","name":"GPT-OSS 120B"},{"id":"whisper-large-v3","name":"Whisper Large v3","kind":"stt","params":["language","response_format","temperature","prompt"]},{"id":"whisper-large-v3-turbo","name":"Whisper Large v3 Turbo","kind":"stt","params":["language","response_format","temperature","prompt"]},{"id":"distil-whisper-large-v3-en","name":"Distil Whisper Large v3 EN","kind":"stt","params":["language","response_format","temperature","prompt"]}]},{"id":"huggingface","name":"HuggingFace","alias":"huggingface","uiAlias":"hf","aliases":["hf"],"serviceKinds":["image","stt"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"black-forest-labs/FLUX.1-schnell","name":"FLUX.1 Schnell","kind":"image","params":[]},{"id":"black-forest-labs/FLUX.1-dev","name":"FLUX.1 Dev","kind":"image","params":[]},{"id":"black-forest-labs/FLUX.1-Krea-dev","name":"FLUX.1 Krea","kind":"image","params":[]},{"id":"black-forest-labs/FLUX.1-Kontext-dev","name":"FLUX.1 Kontext","kind":"image","params":[],"capabilities":["edit"]},{"id":"black-forest-labs/FLUX.2-dev","name":"FLUX.2 Dev","kind":"image","params":[],"capabilities":["edit"]},{"id":"black-forest-labs/FLUX.2-klein-9B","name":"FLUX.2 Klein 9B","kind":"image","params":[],"capabilities":["edit"]},{"id":"black-forest-labs/FLUX.2-klein-4B","name":"FLUX.2 Klein 4B","kind":"image","params":[],"capabilities":["edit"]},{"id":"black-forest-labs/FLUX.2-klein-base-9B","name":"FLUX.2 Klein Base 9B","kind":"image","params":[],"capabilities":["edit"]},{"id":"black-forest-labs/FLUX.2-klein-base-4B","name":"FLUX.2 Klein Base 4B","kind":"image","params":[],"capabilities":["edit"]},{"id":"stabilityai/stable-diffusion-xl-base-1.0","name":"SDXL Base 1.0","kind":"image","params":[]},{"id":"stabilityai/stable-diffusion-3.5-large","name":"Stable Diffusion 3.5 Large","kind":"image","params":[]},{"id":"stabilityai/stable-diffusion-3.5-large-turbo","name":"Stable Diffusion 3.5 Large Turbo","kind":"image","params":[]},{"id":"Qwen/Qwen-Image","name":"Qwen Image","kind":"image","params":[]},{"id":"Qwen/Qwen-Image-2512","name":"Qwen Image 2512","kind":"image","params":[]},{"id":"Qwen/Qwen-Image-Edit","name":"Qwen Image Edit","kind":"image","params":[],"capabilities":["edit"]},{"id":"Qwen/Qwen-Image-Edit-2509","name":"Qwen Image Edit 2509","kind":"image","params":[],"capabilities":["edit"]},{"id":"Qwen/Qwen-Image-Edit-2511","name":"Qwen Image Edit 2511","kind":"image","params":[],"capabilities":["edit"]},{"id":"ideogram-ai/ideogram-4-fp8","name":"Ideogram 4","kind":"image","params":[]},{"id":"tencent/HunyuanImage-3.0","name":"HunyuanImage 3.0","kind":"image","params":[]},{"id":"Tongyi-MAI/Z-Image-Turbo","name":"Z-Image Turbo","kind":"image","params":[]},{"id":"krea/Krea-2-Turbo","name":"Krea 2 Turbo","kind":"image","params":[]},{"id":"HiDream-ai/HiDream-I1-Fast","name":"HiDream I1 Fast","kind":"image","params":[]},{"id":"playgroundai/playground-v2.5-1024px-aesthetic","name":"Playground v2.5","kind":"image","params":[]},{"id":"openai/whisper-large-v3","name":"Whisper Large v3 (HF)","kind":"stt","params":[]},{"id":"openai/whisper-large-v3-turbo","name":"Whisper Large v3 Turbo (HF)","kind":"stt","params":[]}]},{"id":"hyperbolic","name":"Hyperbolic","alias":"hyperbolic","uiAlias":"hyp","aliases":["hyp"],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"Qwen/QwQ-32B","name":"QwQ 32B"},{"id":"deepseek-ai/DeepSeek-R1","name":"DeepSeek R1"},{"id":"deepseek-ai/DeepSeek-V3","name":"DeepSeek V3"},{"id":"meta-llama/Llama-3.3-70B-Instruct","name":"Llama 3.3 70B"},{"id":"meta-llama/Llama-3.2-3B-Instruct","name":"Llama 3.2 3B"},{"id":"Qwen/Qwen2.5-72B-Instruct","name":"Qwen 2.5 72B"},{"id":"Qwen/Qwen2.5-Coder-32B-Instruct","name":"Qwen 2.5 Coder 32B"},{"id":"NousResearch/Hermes-3-Llama-3.1-70B","name":"Hermes 3 70B"}]},{"id":"iflow","name":"iFlow AI","alias":"if","uiAlias":"if","aliases":[],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"qwen3-coder-plus","name":"Qwen3 Coder Plus"},{"id":"qwen3-max","name":"Qwen3 Max"},{"id":"qwen3-vl-plus","name":"Qwen3 VL Plus"},{"id":"qwen3-max-preview","name":"Qwen3 Max Preview"},{"id":"qwen3-235b","name":"Qwen3 235B A22B"},{"id":"qwen3-235b-a22b-instruct","name":"Qwen3 235B A22B Instruct"},{"id":"qwen3-235b-a22b-thinking-2507","name":"Qwen3 235B A22B Thinking"},{"id":"qwen3-32b","name":"Qwen3 32B"},{"id":"kimi-k2","name":"Kimi K2"},{"id":"deepseek-v3.2","name":"DeepSeek V3.2 Exp"},{"id":"deepseek-v3.1","name":"DeepSeek V3.1 Terminus"},{"id":"deepseek-v3","name":"DeepSeek V3 671B"},{"id":"deepseek-r1","name":"DeepSeek R1"},{"id":"glm-4.7","name":"GLM 4.7"},{"id":"iflow-rome-30ba3b","name":"iFlow ROME"}]},{"id":"inworld","name":"Inworld TTS","alias":"inworld","uiAlias":"inworld","aliases":[],"serviceKinds":["tts"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[]},{"id":"jina-ai","name":"Jina AI","alias":"jina","uiAlias":"jina","aliases":[],"serviceKinds":["embedding"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[]},{"id":"jina-reader","name":"Jina Reader","alias":"jina-reader","uiAlias":"jina-reader","aliases":[],"serviceKinds":["webFetch"],"noAuth":false,"passthroughModels":false,"hasFetch":true,"models":[]},{"id":"kilocode","name":"Kilo Code","alias":"kc","uiAlias":"kc","aliases":[],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":true,"hasFetch":false,"models":[{"id":"anthropic/claude-sonnet-4-20250514","name":"Claude Sonnet 4"},{"id":"anthropic/claude-opus-4-20250514","name":"Claude Opus 4"},{"id":"google/gemini-2.5-pro","name":"Gemini 2.5 Pro"},{"id":"google/gemini-2.5-flash","name":"Gemini 2.5 Flash"},{"id":"openai/gpt-4.1","name":"GPT-4.1"},{"id":"openai/o3","name":"o3"},{"id":"deepseek/deepseek-chat","name":"DeepSeek Chat"},{"id":"deepseek/deepseek-reasoner","name":"DeepSeek Reasoner"}]},{"id":"kimchi","name":"Kimchi","alias":"kimchi","uiAlias":"kimchi","aliases":[],"serviceKinds":["llm","imageToText"],"noAuth":false,"passthroughModels":true,"hasFetch":false,"models":[{"id":"minimax-m3","name":"MiniMax-M3"},{"id":"kimi-k2.7","name":"Kimi-K2.7"},{"id":"kimi-k2.6","name":"Kimi-K2.6"},{"id":"kimi-k2.5","name":"Kimi-K2.5"},{"id":"nemotron-3-ultra-fp4","name":"Nemotron 3 Ultra FP4"},{"id":"minimax-m2.7","name":"MiniMax-M2.7"},{"id":"claude-opus-4-6","name":"Claude Opus 4.6"},{"id":"claude-sonnet-4-6","name":"Claude Sonnet 4.6"}]},{"id":"kimi","name":"Kimi","alias":"kimi","uiAlias":"kimi","aliases":["kimi-coding","kmc"],"serviceKinds":["llm","webSearch"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"kimi-k3","name":"Kimi K3"},{"id":"k3","name":"Kimi K3 (Code)"},{"id":"kimi-for-coding","name":"Kimi for Coding"},{"id":"kimi-for-coding-highspeed","name":"Kimi for Coding Highspeed"},{"id":"kimi-k2.7-code","name":"Kimi K2.7 Code"},{"id":"kimi-k2.7-code-highspeed","name":"Kimi K2.7 Code Highspeed"},{"id":"kimi-k2.6","name":"Kimi K2.6"},{"id":"kimi-k2.5","name":"Kimi K2.5"},{"id":"kimi-k2.5-thinking","name":"Kimi K2.5 Thinking"},{"id":"kimi-latest","name":"Kimi Latest"}]},{"id":"kiro","name":"Kiro AI","alias":"kr","uiAlias":"kr","aliases":[],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"claude-opus-5.5","name":"Claude Opus 5.5"},{"id":"claude-opus-5.5-thinking","name":"Claude Opus 5.5 (Thinking)"},{"id":"claude-opus-5.5-agentic","name":"Claude Opus 5.5 (Agentic)"},{"id":"claude-opus-5.5-thinking-agentic","name":"Claude Opus 5.5 (Thinking + Agentic)"},{"id":"claude-opus-5","name":"Claude Opus 5"},{"id":"claude-opus-5-thinking","name":"Claude Opus 5 (Thinking)"},{"id":"claude-opus-5-agentic","name":"Claude Opus 5 (Agentic)"},{"id":"claude-opus-5-thinking-agentic","name":"Claude Opus 5 (Thinking + Agentic)"},{"id":"claude-opus-4.8","name":"Claude Opus 4.8"},{"id":"claude-opus-4.8-thinking","name":"Claude Opus 4.8 (Thinking)"},{"id":"claude-opus-4.8-agentic","name":"Claude Opus 4.8 (Agentic)"},{"id":"claude-opus-4.8-thinking-agentic","name":"Claude Opus 4.8 (Thinking + Agentic)"},{"id":"claude-opus-4.7","name":"Claude Opus 4.7"},{"id":"claude-opus-4.7-thinking","name":"Claude Opus 4.7 (Thinking)"},{"id":"claude-opus-4.7-agentic","name":"Claude Opus 4.7 (Agentic)"},{"id":"claude-opus-4.7-thinking-agentic","name":"Claude Opus 4.7 (Thinking + Agentic)"},{"id":"claude-opus-4.5","name":"Claude Opus 4.5"},{"id":"claude-opus-4.5-thinking","name":"Claude Opus 4.5 (Thinking)"},{"id":"claude-opus-4.5-agentic","name":"Claude Opus 4.5 (Agentic)"},{"id":"claude-opus-4.5-thinking-agentic","name":"Claude Opus 4.5 (Thinking + Agentic)"},{"id":"claude-sonnet-5","name":"Claude Sonnet 5"},{"id":"claude-sonnet-4.5","name":"Claude Sonnet 4.5"},{"id":"claude-haiku-4.5","name":"Claude Haiku 4.5"},{"id":"deepseek-3.2","name":"DeepSeek 3.2"},{"id":"qwen3-coder-next","name":"Qwen3 Coder Next"},{"id":"glm-5","name":"GLM 5"},{"id":"MiniMax-M2.5","name":"MiniMax M2.5"},{"id":"gpt-5.6-sol","name":"GPT 5.6 Sol"},{"id":"gpt-5.6-terra","name":"GPT 5.6 Terra"},{"id":"gpt-5.6-luna","name":"GPT 5.6 Luna"},{"id":"claude-sonnet-5-thinking","name":"Claude Sonnet 5 (Thinking)"},{"id":"claude-sonnet-4.5-thinking","name":"Claude Sonnet 4.5 (Thinking)"},{"id":"claude-haiku-4.5-thinking","name":"Claude Haiku 4.5 (Thinking)"},{"id":"gpt-5.6-sol-thinking","name":"GPT 5.6 Sol (Thinking)"},{"id":"gpt-5.6-terra-thinking","name":"GPT 5.6 Terra (Thinking)"},{"id":"gpt-5.6-luna-thinking","name":"GPT 5.6 Luna (Thinking)"},{"id":"claude-sonnet-5-agentic","name":"Claude Sonnet 5 (Agentic)"},{"id":"claude-sonnet-4.5-agentic","name":"Claude Sonnet 4.5 (Agentic)"},{"id":"claude-haiku-4.5-agentic","name":"Claude Haiku 4.5 (Agentic)"},{"id":"gpt-5.6-sol-agentic","name":"GPT 5.6 Sol (Agentic)"},{"id":"gpt-5.6-terra-agentic","name":"GPT 5.6 Terra (Agentic)"},{"id":"gpt-5.6-luna-agentic","name":"GPT 5.6 Luna (Agentic)"},{"id":"claude-sonnet-5-thinking-agentic","name":"Claude Sonnet 5 (Thinking + Agentic)"},{"id":"claude-sonnet-4.5-thinking-agentic","name":"Claude Sonnet 4.5 (Thinking + Agentic)"},{"id":"claude-haiku-4.5-thinking-agentic","name":"Claude Haiku 4.5 (Thinking + Agentic)"},{"id":"gpt-5.6-sol-thinking-agentic","name":"GPT 5.6 Sol (Thinking + Agentic)"},{"id":"gpt-5.6-terra-thinking-agentic","name":"GPT 5.6 Terra (Thinking + Agentic)"},{"id":"gpt-5.6-luna-thinking-agentic","name":"GPT 5.6 Luna (Thinking + Agentic)"}]},{"id":"linkup","name":"Linkup","alias":"linkup","uiAlias":"linkup","aliases":[],"serviceKinds":["webSearch"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"searchConfig":{"searchTypes":["web"],"maxMaxResults":50,"requiredOptions":null},"models":[]},{"id":"local-device","name":"Local Device","alias":"local-device","uiAlias":"local-device","aliases":[],"serviceKinds":["tts"],"noAuth":true,"passthroughModels":false,"hasFetch":false,"models":[]},{"id":"mimo-free","name":"MiMo Code Free","alias":"mmf","uiAlias":"mmf","aliases":[],"serviceKinds":["llm"],"noAuth":true,"passthroughModels":true,"hasFetch":false,"models":[{"id":"mimo-auto","name":"MiMo Auto"}]},{"id":"minimax-cn","name":"Minimax (China)","alias":"minimax-cn","uiAlias":"minimax-cn","aliases":[],"serviceKinds":["llm","tts"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"MiniMax-M3","name":"MiniMax M3"},{"id":"MiniMax-M2.7","name":"MiniMax M2.7"},{"id":"MiniMax-M2.5","name":"MiniMax M2.5"},{"id":"MiniMax-M2.1","name":"MiniMax M2.1"},{"id":"speech-2.8-hd","name":"Speech 2.8 HD","kind":"tts"},{"id":"speech-2.8-turbo","name":"Speech 2.8 Turbo","kind":"tts"},{"id":"speech-2.6-hd","name":"Speech 2.6 HD","kind":"tts"},{"id":"speech-2.6-turbo","name":"Speech 2.6 Turbo","kind":"tts"},{"id":"speech-02-hd","name":"Speech 02 HD","kind":"tts"},{"id":"speech-02-turbo","name":"Speech 02 Turbo","kind":"tts"},{"id":"speech-01-hd","name":"Speech 01 HD","kind":"tts"},{"id":"speech-01-turbo","name":"Speech 01 Turbo","kind":"tts"}]},{"id":"minimax","name":"Minimax Coding","alias":"minimax","uiAlias":"minimax","aliases":[],"serviceKinds":["llm","image","imageToText","webSearch","tts"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"MiniMax-M3","name":"MiniMax M3"},{"id":"MiniMax-M2.7","name":"MiniMax M2.7"},{"id":"MiniMax-M2.5","name":"MiniMax M2.5"},{"id":"MiniMax-M2.1","name":"MiniMax M2.1"},{"id":"minimax-image-01","name":"MiniMax Image 01","kind":"image","params":["n","size","response_format"]},{"id":"speech-2.8-hd","name":"Speech 2.8 HD","kind":"tts"},{"id":"speech-2.8-turbo","name":"Speech 2.8 Turbo","kind":"tts"},{"id":"speech-2.6-hd","name":"Speech 2.6 HD","kind":"tts"},{"id":"speech-2.6-turbo","name":"Speech 2.6 Turbo","kind":"tts"},{"id":"speech-02-hd","name":"Speech 02 HD","kind":"tts"},{"id":"speech-02-turbo","name":"Speech 02 Turbo","kind":"tts"},{"id":"speech-01-hd","name":"Speech 01 HD","kind":"tts"},{"id":"speech-01-turbo","name":"Speech 01 Turbo","kind":"tts"}]},{"id":"mistral","name":"Mistral","alias":"mistral","uiAlias":"mistral","aliases":[],"serviceKinds":["llm","imageToText","embedding"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"mistral-large-latest","name":"Mistral Large 3"},{"id":"codestral-latest","name":"Codestral"},{"id":"mistral-medium-latest","name":"Mistral Medium 3"},{"id":"mistral-embed","name":"Mistral Embed","kind":"embedding"}]},{"id":"mmf","name":"MMF","alias":"mmf","uiAlias":"mmf","aliases":[],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"mimo-auto","name":"MiMo Auto"}]},{"id":"nanobanana","name":"NanoBanana API","alias":"nanobanana","uiAlias":"nb","aliases":["nb"],"serviceKinds":["image"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"nanobanana-flash","name":"NanoBanana Flash","kind":"image","params":["n","size"]},{"id":"nanobanana-pro","name":"NanoBanana Pro","kind":"image","params":["n","size"]}]},{"id":"nebius","name":"Nebius AI","alias":"nebius","uiAlias":"nebius","aliases":[],"serviceKinds":["llm","embedding"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"meta-llama/Llama-3.3-70B-Instruct","name":"Llama 3.3 70B Instruct"},{"id":"Qwen/Qwen3-Embedding-8B","name":"Qwen3 Embedding 8B","kind":"embedding"}]},{"id":"nvidia","name":"NVIDIA NIM","alias":"nvidia","uiAlias":"nvidia","aliases":[],"serviceKinds":["llm","tts","embedding"],"noAuth":false,"passthroughModels":true,"hasFetch":false,"models":[{"id":"moonshotai/kimi-k3","name":"Kimi K3"},{"id":"z-ai/glm-5.3","name":"GLM 5.3"},{"id":"z-ai/glm-5.3-flash","name":"GLM 5.3 Flash"},{"id":"thinkingmachines/inkling","name":"Inkling"},{"id":"deepseek-ai/deepseek-v4-pro-0813","name":"DeepSeek V4 Pro 0813"},{"id":"deepseek-ai/deepseek-v4-flash-0731","name":"DeepSeek V4 Flash 0731"},{"id":"nvidia/nemotron-3.5-lightning-30b-a3b","name":"Nemotron 3.5 Lightning"},{"id":"nvidia/nemotron-3.5-bolt-e4b","name":"Nemotron 3.5 Bolt"},{"id":"nvidia/nemotron-3-super-120b-a12b","name":"Nemotron 3 Super 120B"},{"id":"nvidia/nemotron-3-nano-omni-30b-a3b-reasoning","name":"Nemotron 3 Nano Omni"},{"id":"nvidia/nemotron-3-ultra-550b-a55b","name":"Nemotron 3 Ultra"},{"id":"nvidia/llama-3.3-nemotron-super-49b-v1.5","name":"Llama 3.3 Nemotron Super 49B v1.5"},{"id":"nvidia/llama-3.1-nemotron-ultra-253b-v1","name":"Llama 3.1 Nemotron Ultra 253B"},{"id":"nvidia/nemotron-nano-12b-v2-vl","name":"Nemotron Nano 12B v2 VL"},{"id":"nvidia/nvidia-nemotron-nano-9b-v2","name":"Nemotron Nano 9B v2"},{"id":"nvidia/nemotron-3-nano-30b-a3b","name":"Nemotron 3 Nano 30B"},{"id":"meta/llama-guard-4-12b","name":"Llama Guard 4 12B"},{"id":"nvidia/riva-translate-4b-instruct-v2","name":"Riva Translate 4B v2"},{"id":"meta/llama-3.3-70b-instruct","name":"Llama 3.3 70B Instruct"},{"id":"qwen/qwen3.5-397b-a17b","name":"Qwen 3.5 397B"},{"id":"qwen/qwen3.5-122b-a10b","name":"Qwen 3.5 122B"},{"id":"qwen/qwen3-coder-480b-a35b-instruct","name":"Qwen 3 Coder 480B"},{"id":"mistralai/mistral-medium-3.5-128b","name":"Mistral Medium 3.5 128B"},{"id":"mistralai/mistral-small-4-119b-2603","name":"Mistral Small 4 119B"},{"id":"stepfun-ai/step-3.7-flash","name":"Step 3.7 Flash"},{"id":"stepfun-ai/step-3.5-flash","name":"Step 3.5 Flash"},{"id":"poolside/laguna-xs-2.1","name":"Laguna XS 2.1"},{"id":"openai/gpt-oss-20b","name":"GPT-OSS 20B"},{"id":"meta/muse-glimmer-30b","name":"Muse Glimmer 30B"},{"id":"google/gemma-4-31b-it","name":"Gemma 4 31B"},{"id":"microsoft/phi-4-mini-instruct","name":"Phi 4 Mini"},{"id":"minimaxai/minimax-m3","name":"MiniMax M3"},{"id":"minimaxai/minimax-m2.7","name":"MiniMax M2.7"},{"id":"z-ai/glm-5.2","name":"GLM 5.2"},{"id":"deepseek-ai/deepseek-v4-pro","name":"DeepSeek V4 Pro"},{"id":"deepseek-ai/deepseek-v4-flash","name":"DeepSeek V4 Flash"},{"id":"moonshotai/kimi-k2.6","name":"Kimi K2.6"},{"id":"nvidia/nv-embedqa-e5-v5","name":"NV EmbedQA E5 v5","kind":"embedding"},{"id":"nvidia/parakeet-ctc-1.1b-asr","name":"Parakeet CTC 1.1B","kind":"stt","params":["language"]},{"id":"fastpitch","name":"FastPitch","kind":"tts"},{"id":"tacotron2","name":"Tacotron2","kind":"tts"}]},{"id":"ollama-local","name":"Ollama Local","alias":"ollama-local","uiAlias":"ollama-local","aliases":[],"serviceKinds":["llm","systemone","tts","embedding"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"clef","name":"Clef","kind":"systemone"},{"id":"clef-flash","name":"Clef Flash","kind":"systemone"},{"id":"phonon:latest","name":"Phonon (Latest)","kind":"tts","params":["voice","response_format"]},{"id":"phonon","name":"Phonon","kind":"tts","params":["voice","response_format"]},{"id":"phonon:multilingual","name":"Phonon Multilingual","kind":"tts","params":["voice","response_format"]},{"id":"phonon:40m-multilingual","name":"Phonon 40M Multilingual","kind":"tts","params":["voice","response_format"]},{"id":"phonon:en","name":"Phonon English","kind":"tts","params":["voice","response_format"]},{"id":"phonon:31m-en","name":"Phonon 31M English","kind":"tts","params":["voice","response_format"]},{"id":"phonon:40m-en","name":"Phonon 40M English","kind":"tts","params":["voice","response_format"]},{"id":"phonon:de","name":"Phonon German","kind":"tts","params":["voice","response_format"]},{"id":"phonon:40m-de","name":"Phonon 40M German","kind":"tts","params":["voice","response_format"]},{"id":"phonon:es","name":"Phonon Spanish","kind":"tts","params":["voice","response_format"]},{"id":"phonon:40m-es","name":"Phonon 40M Spanish","kind":"tts","params":["voice","response_format"]},{"id":"phonon:fr","name":"Phonon French","kind":"tts","params":["voice","response_format"]},{"id":"phonon:40m-fr","name":"Phonon 40M French","kind":"tts","params":["voice","response_format"]},{"id":"phonon:pt","name":"Phonon Portuguese","kind":"tts","params":["voice","response_format"]},{"id":"phonon:40m-pt","name":"Phonon 40M Portuguese","kind":"tts","params":["voice","response_format"]},{"id":"nomic-embed-text","name":"Nomic Embed Text","kind":"embedding","dimensions":768},{"id":"nomic-embed-text:latest","name":"Nomic Embed Text (Latest)","kind":"embedding","dimensions":768},{"id":"nomic-embed-text:v1.5","name":"Nomic Embed Text v1.5","kind":"embedding","dimensions":768},{"id":"nomic-embed-text:137m-v1.5-fp16","name":"Nomic Embed Text 137M v1.5 (FP16)","kind":"embedding","dimensions":768},{"id":"nomic-embed-text-v2-moe","name":"Nomic Embed Text v2 MoE","kind":"embedding","dimensions":768},{"id":"nomic-embed-text-v2-moe:latest","name":"Nomic Embed Text v2 MoE (Latest)","kind":"embedding","dimensions":768},{"id":"qwen3-embedding","name":"Qwen3 Embedding","kind":"embedding","dimensions":4096},{"id":"qwen3-embedding:latest","name":"Qwen3 Embedding (Latest)","kind":"embedding","dimensions":4096},{"id":"qwen3-embedding:8b","name":"Qwen3 Embedding 8B","kind":"embedding","dimensions":4096},{"id":"qwen3-embedding:8b-q4_K_M","name":"Qwen3 Embedding 8B (Q4_K_M)","kind":"embedding","dimensions":4096},{"id":"qwen3-embedding:8b-q8_0","name":"Qwen3 Embedding 8B (Q8_0)","kind":"embedding","dimensions":4096},{"id":"qwen3-embedding:8b-fp16","name":"Qwen3 Embedding 8B (FP16)","kind":"embedding","dimensions":4096},{"id":"qwen3-embedding:4b","name":"Qwen3 Embedding 4B","kind":"embedding","dimensions":2560},{"id":"qwen3-embedding:4b-q4_K_M","name":"Qwen3 Embedding 4B (Q4_K_M)","kind":"embedding","dimensions":2560},{"id":"qwen3-embedding:4b-q8_0","name":"Qwen3 Embedding 4B (Q8_0)","kind":"embedding","dimensions":2560},{"id":"qwen3-embedding:4b-fp16","name":"Qwen3 Embedding 4B (FP16)","kind":"embedding","dimensions":2560},{"id":"qwen3-embedding:0.6b","name":"Qwen3 Embedding 0.6B","kind":"embedding","dimensions":1024},{"id":"qwen3-embedding:0.6b-q8_0","name":"Qwen3 Embedding 0.6B (Q8_0)","kind":"embedding","dimensions":1024},{"id":"qwen3-embedding:0.6b-fp16","name":"Qwen3 Embedding 0.6B (FP16)","kind":"embedding","dimensions":1024},{"id":"bge-m3","name":"BGE M3","kind":"embedding","dimensions":1024},{"id":"bge-m3:latest","name":"BGE M3 (Latest)","kind":"embedding","dimensions":1024},{"id":"bge-m3:567m","name":"BGE M3 567M","kind":"embedding","dimensions":1024},{"id":"bge-m3:567m-fp16","name":"BGE M3 567M (FP16)","kind":"embedding","dimensions":1024},{"id":"bge-large","name":"BGE Large","kind":"embedding","dimensions":1024},{"id":"bge-large:latest","name":"BGE Large (Latest)","kind":"embedding","dimensions":1024},{"id":"bge-large:335m","name":"BGE Large 335M","kind":"embedding","dimensions":1024},{"id":"bge-large:335m-en-v1.5-fp16","name":"BGE Large 335M EN v1.5 (FP16)","kind":"embedding","dimensions":1024},{"id":"mxbai-embed-large","name":"mxbai Embed Large","kind":"embedding","dimensions":1024},{"id":"mxbai-embed-large:latest","name":"mxbai Embed Large (Latest)","kind":"embedding","dimensions":1024},{"id":"mxbai-embed-large:335m","name":"mxbai Embed Large 335M","kind":"embedding","dimensions":1024},{"id":"mxbai-embed-large:v1","name":"mxbai Embed Large v1","kind":"embedding","dimensions":1024},{"id":"mxbai-embed-large:335m-v1-fp16","name":"mxbai Embed Large 335M v1 (FP16)","kind":"embedding","dimensions":1024},{"id":"embeddinggemma","name":"EmbeddingGemma","kind":"embedding","dimensions":768},{"id":"embeddinggemma:latest","name":"EmbeddingGemma (Latest)","kind":"embedding","dimensions":768},{"id":"embeddinggemma:300m","name":"EmbeddingGemma 300M","kind":"embedding","dimensions":768},{"id":"embeddinggemma:300m-bf16","name":"EmbeddingGemma 300M (BF16)","kind":"embedding","dimensions":768},{"id":"embeddinggemma:300m-qat-q4_0","name":"EmbeddingGemma 300M (QAT Q4_0)","kind":"embedding","dimensions":768},{"id":"embeddinggemma:300m-qat-q8_0","name":"EmbeddingGemma 300M (QAT Q8_0)","kind":"embedding","dimensions":768},{"id":"embeddinggemma-2","name":"EmbeddingGemma 2","kind":"embedding","dimensions":768},{"id":"embeddinggemma-2:latest","name":"EmbeddingGemma 2 (Latest)","kind":"embedding","dimensions":768},{"id":"embeddinggemma-2:740m","name":"EmbeddingGemma 2 740M","kind":"embedding","dimensions":768},{"id":"embeddinggemma-2:740m-bf16","name":"EmbeddingGemma 2 740M (BF16)","kind":"embedding","dimensions":768},{"id":"embeddinggemma-2:740m-q8_0","name":"EmbeddingGemma 2 740M (Q8_0)","kind":"embedding","dimensions":768},{"id":"embeddinggemma-2:570m","name":"EmbeddingGemma 2 570M","kind":"embedding","dimensions":768},{"id":"embeddinggemma-2:570m-bf16-text-audio","name":"EmbeddingGemma 2 570M Audio (BF16)","kind":"embedding","dimensions":768},{"id":"embeddinggemma-2:570m-q8_0-text-audio","name":"EmbeddingGemma 2 570M Audio (Q8_0)","kind":"embedding","dimensions":768},{"id":"embeddinggemma-2:440m","name":"EmbeddingGemma 2 440M","kind":"embedding","dimensions":768},{"id":"embeddinggemma-2:440m-bf16-text-vision","name":"EmbeddingGemma 2 440M Vision (BF16)","kind":"embedding","dimensions":768},{"id":"embeddinggemma-2:440m-q8_0-text-vision","name":"EmbeddingGemma 2 440M Vision (Q8_0)","kind":"embedding","dimensions":768},{"id":"embeddinggemma-2:270m","name":"EmbeddingGemma 2 270M","kind":"embedding","dimensions":768},{"id":"embeddinggemma-2:270m-bf16-text","name":"EmbeddingGemma 2 270M Text (BF16)","kind":"embedding","dimensions":768},{"id":"embeddinggemma-2:270m-q8_0-text","name":"EmbeddingGemma 2 270M Text (Q8_0)","kind":"embedding","dimensions":768},{"id":"all-minilm","name":"All-MiniLM","kind":"embedding","dimensions":384},{"id":"all-minilm:latest","name":"All-MiniLM (Latest)","kind":"embedding","dimensions":384},{"id":"all-minilm:33m","name":"All-MiniLM 33M (L12)","kind":"embedding","dimensions":384},{"id":"all-minilm:22m","name":"All-MiniLM 22M (L6)","kind":"embedding","dimensions":384},{"id":"all-minilm:l12-v2","name":"All-MiniLM L12 v2","kind":"embedding","dimensions":384},{"id":"all-minilm:l6-v2","name":"All-MiniLM L6 v2","kind":"embedding","dimensions":384},{"id":"all-minilm:33m-l12-v2-fp16","name":"All-MiniLM 33M L12 v2 (FP16)","kind":"embedding","dimensions":384},{"id":"all-minilm:22m-l6-v2-fp16","name":"All-MiniLM 22M L6 v2 (FP16)","kind":"embedding","dimensions":384},{"id":"snowflake-arctic-embed","name":"Snowflake Arctic Embed","kind":"embedding","dimensions":1024},{"id":"snowflake-arctic-embed:latest","name":"Snowflake Arctic Embed (Latest)","kind":"embedding","dimensions":1024},{"id":"snowflake-arctic-embed:335m","name":"Snowflake Arctic Embed 335M (L)","kind":"embedding","dimensions":1024},{"id":"snowflake-arctic-embed:137m","name":"Snowflake Arctic Embed 137M (M-Long)","kind":"embedding","dimensions":768},{"id":"snowflake-arctic-embed:110m","name":"Snowflake Arctic Embed 110M (M)","kind":"embedding","dimensions":768},{"id":"snowflake-arctic-embed:33m","name":"Snowflake Arctic Embed 33M (S)","kind":"embedding","dimensions":384},{"id":"snowflake-arctic-embed:22m","name":"Snowflake Arctic Embed 22M (XS)","kind":"embedding","dimensions":384},{"id":"snowflake-arctic-embed:l","name":"Snowflake Arctic Embed L","kind":"embedding","dimensions":1024},{"id":"snowflake-arctic-embed:m","name":"Snowflake Arctic Embed M","kind":"embedding","dimensions":768},{"id":"snowflake-arctic-embed:m-long","name":"Snowflake Arctic Embed M-Long","kind":"embedding","dimensions":768},{"id":"snowflake-arctic-embed:s","name":"Snowflake Arctic Embed S","kind":"embedding","dimensions":384},{"id":"snowflake-arctic-embed:xs","name":"Snowflake Arctic Embed XS","kind":"embedding","dimensions":384},{"id":"snowflake-arctic-embed:335m-l-fp16","name":"Snowflake Arctic Embed 335M L (FP16)","kind":"embedding","dimensions":1024},{"id":"snowflake-arctic-embed:137m-m-long-fp16","name":"Snowflake Arctic Embed 137M M-Long (FP16)","kind":"embedding","dimensions":768},{"id":"snowflake-arctic-embed:110m-m-fp16","name":"Snowflake Arctic Embed 110M M (FP16)","kind":"embedding","dimensions":768},{"id":"snowflake-arctic-embed:33m-s-fp16","name":"Snowflake Arctic Embed 33M S (FP16)","kind":"embedding","dimensions":384},{"id":"snowflake-arctic-embed:22m-xs-fp16","name":"Snowflake Arctic Embed 22M XS (FP16)","kind":"embedding","dimensions":384},{"id":"snowflake-arctic-embed2","name":"Snowflake Arctic Embed 2","kind":"embedding","dimensions":1024},{"id":"snowflake-arctic-embed2:latest","name":"Snowflake Arctic Embed 2 (Latest)","kind":"embedding","dimensions":1024},{"id":"snowflake-arctic-embed2:568m","name":"Snowflake Arctic Embed 2 568M","kind":"embedding","dimensions":1024},{"id":"snowflake-arctic-embed2:568m-l-fp16","name":"Snowflake Arctic Embed 2 568M L (FP16)","kind":"embedding","dimensions":1024},{"id":"granite-embedding","name":"Granite Embedding","kind":"embedding","dimensions":384},{"id":"granite-embedding:latest","name":"Granite Embedding (Latest)","kind":"embedding","dimensions":384},{"id":"granite-embedding:278m","name":"Granite Embedding 278M (Multilingual)","kind":"embedding","dimensions":768},{"id":"granite-embedding:30m","name":"Granite Embedding 30M (EN)","kind":"embedding","dimensions":384},{"id":"granite-embedding:30m-en","name":"Granite Embedding 30M EN","kind":"embedding","dimensions":384},{"id":"granite-embedding:278m-fp16","name":"Granite Embedding 278M (FP16)","kind":"embedding","dimensions":768},{"id":"granite-embedding:30m-en-fp16","name":"Granite Embedding 30M EN (FP16)","kind":"embedding","dimensions":384},{"id":"paraphrase-multilingual","name":"Paraphrase Multilingual","kind":"embedding","dimensions":768},{"id":"paraphrase-multilingual:latest","name":"Paraphrase Multilingual (Latest)","kind":"embedding","dimensions":768},{"id":"paraphrase-multilingual:278m","name":"Paraphrase Multilingual 278M","kind":"embedding","dimensions":768},{"id":"paraphrase-multilingual:278m-mpnet-base-v2-fp16","name":"Paraphrase Multilingual 278M (FP16)","kind":"embedding","dimensions":768},{"id":"laya","name":"Laya","kind":"embedding","dimensions":1024},{"id":"laya:latest","name":"Laya (Latest)","kind":"embedding","dimensions":1024},{"id":"laya:421m-english-bf16","name":"Laya 421M English (BF16)","kind":"embedding","dimensions":1024},{"id":"laya:322m-multilingual-bf16","name":"Laya 322M Multilingual (BF16)","kind":"embedding","dimensions":1024},{"id":"laya:421m-typed-decisions-bf16","name":"Laya 421M Decisions (BF16)","kind":"embedding","dimensions":1024}]},{"id":"ollama","name":"Ollama Cloud","alias":"ollama","uiAlias":"ollama","aliases":[],"serviceKinds":["llm","webFetch","systemone"],"noAuth":false,"passthroughModels":false,"hasFetch":true,"models":[{"id":"gpt-oss:120b","name":"GPT OSS 120B"},{"id":"kimi-k2.5","name":"Kimi K2.5"},{"id":"glm-5","name":"GLM 5"},{"id":"minimax-m2.5","name":"MiniMax M2.5"},{"id":"glm-4.7-flash","name":"GLM 4.7 Flash"},{"id":"qwen3.5","name":"Qwen3.5"},{"id":"minimax-m3","name":"MiniMax M3"},{"id":"deepseek-v4.1-flash:cloud","name":"DeepSeek V4.1 Flash"},{"id":"clef","name":"Clef","kind":"systemone"},{"id":"clef-flash","name":"Clef Flash","kind":"systemone"},{"id":"clef:cloud","name":"Clef (Cloud)","kind":"systemone"}]},{"id":"ollama-search","name":"Ollama Search","alias":"ollama-search","uiAlias":"ollama-search","aliases":[],"serviceKinds":["webSearch"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"searchConfig":{"searchTypes":["web"],"maxMaxResults":10,"requiredOptions":null},"models":[]},{"id":"qoder-cn","name":"Qoder CN","alias":"qdcn","uiAlias":"qdcn","aliases":[],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"ultimate","name":"Ultimate"},{"id":"auto","name":"Auto"},{"id":"performance","name":"Performance"},{"id":"efficient","name":"Efficient"},{"id":"lite","name":"Lite"},{"id":"qmodel_38max","name":"Qwen3.8-Max"},{"id":"qmodel_latest","name":"Qwen3.7-Max"},{"id":"qmodel","name":"Qwen3.7-Plus"},{"id":"qfmodel","name":"Qwen3.8-Flash"},{"id":"kmodel_latest","name":"Kimi-K3"},{"id":"kmodel","name":"Kimi-K2.7-Code"},{"id":"gmodel","name":"GLM-5.3"},{"id":"gfmodel","name":"GLM-5.3-Flash"},{"id":"dmodel","name":"DeepSeek-V4-Pro"},{"id":"dfmodel","name":"DeepSeek-V4-Flash"},{"id":"mmodel","name":"MiniMax-M3"}]},{"id":"openai","name":"OpenAI","alias":"openai","uiAlias":"openai","aliases":[],"serviceKinds":["llm","embedding","tts","stt","image","imageToText","webSearch"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"gpt-5.5","name":"GPT-5.5"},{"id":"gpt-5.4","name":"GPT-5.4"},{"id":"gpt-5.4-mini","name":"GPT-5.4 Mini"},{"id":"gpt-5.4-nano","name":"GPT-5.4 Nano"},{"id":"gpt-5.2","name":"GPT-5.2"},{"id":"gpt-5.1","name":"GPT-5.1"},{"id":"gpt-5","name":"GPT-5"},{"id":"gpt-5-mini","name":"GPT-5 Mini"},{"id":"gpt-5-nano","name":"GPT-5 Nano"},{"id":"gpt-4o","name":"GPT-4o"},{"id":"gpt-4o-mini","name":"GPT-4o Mini"},{"id":"gpt-4-turbo","name":"GPT-4 Turbo"},{"id":"gpt-4.1","name":"GPT-4.1"},{"id":"gpt-4.1-mini","name":"GPT-4.1 Mini"},{"id":"gpt-4.1-nano","name":"GPT-4.1 Nano"},{"id":"o3","name":"O3"},{"id":"o3-mini","name":"O3 Mini"},{"id":"o3-pro","name":"O3 Pro"},{"id":"o4-mini","name":"O4 Mini"},{"id":"o1","name":"O1"},{"id":"o1-mini","name":"O1 Mini"},{"id":"text-embedding-3-large","name":"Text Embedding 3 Large","kind":"embedding"},{"id":"text-embedding-3-small","name":"Text Embedding 3 Small","kind":"embedding"},{"id":"text-embedding-ada-002","name":"Text Embedding Ada 002","kind":"embedding"},{"id":"tts-1","name":"TTS-1","kind":"tts"},{"id":"tts-1-hd","name":"TTS-1 HD","kind":"tts"},{"id":"gpt-4o-mini-tts","name":"GPT-4o Mini TTS","kind":"tts"},{"id":"whisper-1","name":"Whisper 1","kind":"stt","params":["language","response_format","temperature","prompt"]},{"id":"gpt-4o-transcribe","name":"GPT-4o Transcribe","kind":"stt","params":["language","response_format","temperature","prompt"]},{"id":"gpt-4o-mini-transcribe","name":"GPT-4o Mini Transcribe","kind":"stt","params":["language","response_format","temperature","prompt"]},{"id":"gpt-image-2.5","name":"GPT Image 2.5","kind":"image","params":["n","size","quality","response_format"]},{"id":"gpt-image-2.5-flare","name":"GPT Image 2.5 Flare","kind":"image","params":["n","size","quality","response_format"]},{"id":"gpt-image-2.5-sunburst","name":"GPT Image 2.5 Sunburst","kind":"image","params":["n","size","quality","response_format"]},{"id":"gpt-image-1","name":"GPT Image 1","kind":"image","params":["n","size","quality","response_format"]},{"id":"dall-e-3","name":"DALL-E 3","kind":"image","params":["size","quality","style","response_format"]},{"id":"dall-e-2","name":"DALL-E 2","kind":"image","params":["n","size","response_format"]}]},{"id":"opencode-go","name":"OpenCode Go","alias":"opencode-go","uiAlias":"ocg","aliases":["ocg"],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":true,"hasFetch":false,"models":[{"id":"deepseek-flash","name":"DeepSeek Flash"},{"id":"glm-5.3-flash","name":"GLM 5.3 Flash (Vision)"},{"id":"glm-5.3","name":"GLM 5.3"},{"id":"glm-5.2","name":"GLM 5.2"},{"id":"glm-5.1","name":"GLM 5.1"},{"id":"glm-5","name":"GLM 5"},{"id":"kimi-k2.7-code","name":"Kimi K2.7 Code"},{"id":"kimi-k2.6","name":"Kimi K2.6"},{"id":"kimi-k2.5","name":"Kimi K2.5"},{"id":"kimi-k3","name":"Kimi K3"},{"id":"kimi-k3.1-pro","name":"Kimi K3.1 Pro"},{"id":"deepseek-v4-pro","name":"DeepSeek V4 Pro"},{"id":"deepseek-v4-flash","name":"DeepSeek V4 Flash"},{"id":"deepseek-v4-flash-vision-exp","name":"DeepSeek V4 Flash Vision (Exp)"},{"id":"deepseek-v4.1-flash","name":"DeepSeek V4.1 Flash"},{"id":"longcat-2.0","name":"LongCat 2.0"},{"id":"mimo-v2.6-flash","name":"MiMo V2.6 Flash"},{"id":"mimo-v2.6-pro","name":"MiMo V2.6 Pro"},{"id":"mimo-v2.5","name":"MiMo V2.5"},{"id":"mimo-v2.5-pro","name":"MiMo V2.5 Pro"},{"id":"mimo-v2-pro","name":"MiMo V2 Pro"},{"id":"mimo-v2-omni","name":"MiMo V2 Omni"},{"id":"minimax-m3","name":"MiniMax M3"},{"id":"minimax-m2.7","name":"MiniMax M2.7"},{"id":"minimax-m2.5","name":"MiniMax M2.5"},{"id":"space-bunny-free","name":"Space Bunny Free"},{"id":"qwen3.8-max","name":"Qwen 3.8 Max"},{"id":"qwen3.8-flash","name":"Qwen 3.8 Flash"},{"id":"qwen3.7-max","name":"Qwen 3.7 Max"},{"id":"qwen3.7-plus","name":"Qwen 3.7 Plus"},{"id":"qwen3.6-plus","name":"Qwen 3.6 Plus"},{"id":"qwen3.5-plus","name":"Qwen 3.5 Plus"},{"id":"hy4-preview","name":"Hy4 Preview"},{"id":"hy3","name":"Hy3"},{"id":"hy3-preview","name":"Hy3 Preview"},{"id":"omen-alpha","name":"Omen Alpha"},{"id":"grok-4.7","name":"Grok 4.7"},{"id":"grok-4.6","name":"Grok 4.6"},{"id":"grok-4.5","name":"Grok 4.5"},{"id":"gpt-5.6-luna","name":"GPT 5.6 Luna"},{"id":"gpt-6-luna","name":"GPT 6 Luna"},{"id":"muse-spark-1.2-contributor","name":"Muse Spark 1.2 Contributor"},{"id":"muse-spark-1.3-contributor","name":"Muse Spark 1.3 Contributor"},{"id":"claude-haiku-5-5","name":"Claude Haiku 5.5"}]},{"id":"opencode-zen","name":"OpenCode Zen","alias":"ocz","uiAlias":"ocz","aliases":["opencode-zen"],"serviceKinds":["llm","systemone"],"noAuth":false,"passthroughModels":true,"hasFetch":false,"models":[{"id":"claude-fable-5","name":"Claude Fable 5"},{"id":"claude-opus-5","name":"Claude Opus 5"},{"id":"claude-opus-4-8","name":"Claude Opus 4.8"},{"id":"claude-opus-4-7","name":"Claude Opus 4.7"},{"id":"claude-opus-4-6","name":"Claude Opus 4.6"},{"id":"claude-opus-4-5","name":"Claude Opus 4.5"},{"id":"claude-sonnet-5","name":"Claude Sonnet 5"},{"id":"claude-sonnet-4-6","name":"Claude Sonnet 4.6"},{"id":"claude-sonnet-4-5","name":"Claude Sonnet 4.5"},{"id":"claude-sonnet-4","name":"Claude Sonnet 4"},{"id":"claude-haiku-4-5","name":"Claude Haiku 4.5"},{"id":"gemini-3.6-flash","name":"Gemini 3.6 Flash"},{"id":"gemini-3.8-flash","name":"Gemini 3.8 Flash"},{"id":"gemini-3.7-flash","name":"Gemini 3.7 Flash"},{"id":"gemini-3.5-flash-lite","name":"Gemini 3.5 Flash Lite"},{"id":"gemini-3.5-flash","name":"Gemini 3.5 Flash"},{"id":"gemini-3.1-pro","name":"Gemini 3.1 Pro"},{"id":"gemini-3-flash","name":"Gemini 3 Flash"},{"id":"gpt-6-astra","name":"GPT 6 Astra"},{"id":"gpt-5.6-sol","name":"GPT 5.6 Sol"},{"id":"gpt-5.6-terra","name":"GPT 5.6 Terra"},{"id":"gpt-5.6-luna","name":"GPT 5.6 Luna"},{"id":"gpt-5.5","name":"GPT 5.5"},{"id":"gpt-5.5-pro","name":"GPT 5.5 Pro"},{"id":"gpt-5.4","name":"GPT 5.4"},{"id":"gpt-5.4-pro","name":"GPT 5.4 Pro"},{"id":"gpt-5.4-mini","name":"GPT 5.4 Mini"},{"id":"gpt-5.4-nano","name":"GPT 5.4 Nano"},{"id":"gpt-5.3-codex-spark","name":"GPT 5.3 Codex Spark"},{"id":"gpt-5.3-codex","name":"GPT 5.3 Codex"},{"id":"gpt-5.2","name":"GPT 5.2"},{"id":"gpt-5.2-codex","name":"GPT 5.2 Codex"},{"id":"gpt-5.1","name":"GPT 5.1"},{"id":"gpt-5.1-codex-max","name":"GPT 5.1 Codex Max"},{"id":"gpt-5.1-codex","name":"GPT 5.1 Codex"},{"id":"gpt-5.1-codex-mini","name":"GPT 5.1 Codex Mini"},{"id":"gpt-5","name":"GPT 5"},{"id":"gpt-5-codex","name":"GPT 5 Codex"},{"id":"gpt-5-nano","name":"GPT 5 Nano"},{"id":"grok-build-0.1","name":"Grok Build 0.1"},{"id":"grok-4.6","name":"Grok 4.6"},{"id":"grok-4.5","name":"Grok 4.5"},{"id":"muse-spark-1.3","name":"Muse Spark 1.3"},{"id":"muse-spark-1.2","name":"Muse Spark 1.2"},{"id":"qwen3.6-plus","name":"Qwen 3.6 Plus"},{"id":"qwen3.5-plus","name":"Qwen 3.5 Plus"},{"id":"deepseek-v4-pro","name":"DeepSeek V4 Pro"},{"id":"deepseek-v4-flash","name":"DeepSeek V4 Flash"},{"id":"deepseek-v4-flash-vision-exp","name":"DeepSeek V4 Flash Vision Exp"},{"id":"glm-5.3-flash","name":"GLM 5.3 Flash (Vision)"},{"id":"glm-5.3","name":"GLM 5.3"},{"id":"glm-5.2","name":"GLM 5.2"},{"id":"glm-5.1","name":"GLM 5.1"},{"id":"glm-5","name":"GLM 5"},{"id":"minimax-m3","name":"MiniMax M3"},{"id":"minimax-m2.7","name":"MiniMax M2.7"},{"id":"minimax-m2.5","name":"MiniMax M2.5"},{"id":"kimi-k3","name":"Kimi K3"},{"id":"kimi-k2.7-code","name":"Kimi K2.7 Code"},{"id":"kimi-k2.6","name":"Kimi K2.6"},{"id":"kimi-k2.5","name":"Kimi K2.5"},{"id":"big-pickle","name":"Big Pickle"},{"id":"union-alpha","name":"Union Alpha"},{"id":"deepseek-v4-flash-free","name":"DeepSeek V4 Flash Free"},{"id":"mimo-v2.6-flash-free","name":"MiMo V2.6 Flash Free"},{"id":"mimo-v2.5-free","name":"MiMo V2.5 Free"},{"id":"ling-3.0-flash-fin-free","name":"Ling 3.0 Flash Fin Free"},{"id":"nemotron-3-ultra-free","name":"Nemotron 3 Ultra Free"},{"id":"nemotron-3.5-lightning-free","name":"Nemotron 3.5 Lightning Free"},{"id":"muse-spark-1.3-contributor-free","name":"Muse Spark 1.3 Contributor Free"},{"id":"muse-spark-1.2-contributor-free","name":"Muse Spark 1.2 Contributor Free"},{"id":"jev-1.13","name":"Jev 1.13","kind":"systemone"},{"id":"jev-1.13-free","name":"Jev 1.13 Free","kind":"systemone"}]},{"id":"opencode","name":"OpenCode Free","alias":"oc","uiAlias":"oc","aliases":[],"serviceKinds":["llm","systemone"],"noAuth":true,"passthroughModels":true,"hasFetch":false,"models":[{"id":"muse-spark-1.2-contributor-free","name":"Muse Spark 1.2 Contributor Free"},{"id":"muse-spark-1.3-contributor-free","name":"Muse Spark 1.3 Contributor Free"},{"id":"union-alpha","name":"Union Alpha Free"},{"id":"jev-1.13-free","name":"Jev 1.13 Free","kind":"systemone"}]},{"id":"openrouter","name":"OpenRouter","alias":"openrouter","uiAlias":"openrouter","aliases":["or"],"serviceKinds":["llm","embedding","tts","imageToText","video","systemone"],"noAuth":false,"passthroughModels":true,"hasFetch":false,"models":[{"id":"auto","name":"Auto (Best for prompt)","kind":"llm"},{"id":"liquid/lfm-2.5-2.6b:free","name":"Liquid LFM 2.5 2.6B (Free)","kind":"llm"},{"id":"google/gemma-4-31b-it:free","name":"Google Gemma 4 31B IT (Free)","kind":"llm"},{"id":"cohere/north-mini-code:free","name":"Cohere North Mini Code (Free)","kind":"llm"},{"id":"openai/text-embedding-3-large","name":"OpenAI Text Embedding 3 Large","kind":"embedding"},{"id":"openai/text-embedding-3-small","name":"OpenAI Text Embedding 3 Small","kind":"embedding"},{"id":"openai/text-embedding-ada-002","name":"OpenAI Text Embedding Ada 002","kind":"embedding"},{"id":"qwen/qwen3-embedding-8b","name":"Qwen3 Embedding 8B","kind":"embedding"},{"id":"perplexity/pplx-embed-v1-4b","name":"Perplexity Embed V1 4B","kind":"embedding"},{"id":"perplexity/pplx-embed-v1-0.6b","name":"Perplexity Embed V1 0.6B","kind":"embedding"},{"id":"nvidia/llama-nemotron-embed-vl-1b-v2:free","name":"NVIDIA Nemotron Embed VL 1B V2 (Free)","kind":"embedding"},{"id":"openai/gpt-4o-mini-tts","name":"GPT-4o Mini TTS","kind":"tts"},{"id":"openai/tts-1-hd","name":"TTS-1 HD","kind":"tts"},{"id":"openai/tts-1","name":"TTS-1","kind":"tts"},{"id":"openai/dall-e-3","name":"DALL-E 3 (via OpenRouter)","kind":"image","params":["size","quality","style","response_format"]},{"id":"openai/gpt-image-1","name":"GPT Image 1 (via OpenRouter)","kind":"image","params":["n","size","quality","response_format"]},{"id":"google/imagen-3.0-generate-002","name":"Imagen 3 (via OpenRouter)","kind":"image","params":["n","size"]},{"id":"black-forest-labs/FLUX.1-schnell","name":"FLUX.1 Schnell (via OpenRouter)","kind":"image","params":["n","size"]},{"id":"google/veo-3.1","name":"Veo 3.1 (via OpenRouter)","kind":"video","params":["duration","aspect_ratio","resolution"]},{"id":"openai/sora-2-pro","name":"Sora 2 Pro (via OpenRouter)","kind":"video","params":["duration","aspect_ratio","resolution"]},{"id":"bytedance/seedance-2.0","name":"Seedance 2.0 (via OpenRouter)","kind":"video","params":["duration","aspect_ratio","resolution"]},{"id":"typesafe/jev-1.13","name":"Jev 1.13","kind":"systemone"}]},{"id":"perplexity-web","name":"Perplexity Web (Pro/Max)","alias":"perplexity-web","uiAlias":"pw","aliases":["pw"],"serviceKinds":[],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"pplx-auto","name":"Perplexity Auto (Free)"},{"id":"pplx-sonar","name":"Perplexity Sonar"},{"id":"pplx-gpt","name":"GPT-5.4 (via Perplexity)"},{"id":"pplx-gemini","name":"Gemini 3.1 Pro (via Perplexity)"},{"id":"pplx-sonnet","name":"Claude Sonnet 4.6 (via Perplexity)"},{"id":"pplx-opus","name":"Claude Opus 4.6 (via Perplexity)"},{"id":"pplx-nemotron","name":"Nemotron 3 Super (via Perplexity)"}]},{"id":"perplexity","name":"Perplexity","alias":"perplexity","uiAlias":"pplx","aliases":["pplx"],"serviceKinds":["llm","webSearch"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"sonar-pro","name":"Sonar Pro"},{"id":"sonar","name":"Sonar"}]},{"id":"perplexity-agent","name":"Perplexity Agent","alias":"perplexity-agent","uiAlias":"pa","aliases":["pplx-agent","pplx-responses"],"serviceKinds":["llm","webSearch"],"noAuth":false,"passthroughModels":true,"hasFetch":false,"models":[{"id":"perplexity/sonar","name":"Perplexity Sonar"},{"id":"openai/gpt-5.5","name":"GPT-5.5"},{"id":"openai/gpt-5.4","name":"GPT-5.4"},{"id":"openai/gpt-5.4-mini","name":"GPT-5.4 Mini"},{"id":"anthropic/claude-sonnet-4-6","name":"Claude Sonnet 4.6"},{"id":"anthropic/claude-opus-4-8","name":"Claude Opus 4.8"},{"id":"google/gemini-3.1-pro-preview","name":"Gemini 3.1 Pro"},{"id":"xai/grok-4.20-reasoning","name":"Grok 4.20 Reasoning"},{"id":"perplexity/glm-5.2","name":"GLM 5.2"},{"id":"perplexity/kimi-k2.7-code","name":"Kimi K2.7 Code"},{"id":"nvidia/nemotron-3-super-120b-a12b","name":"Nemotron 3 Super 120B"}]},{"id":"playht","name":"PlayHT","alias":"playht","uiAlias":"playht","aliases":[],"serviceKinds":["tts"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[]},{"id":"qoder","name":"Qoder","alias":"qd","uiAlias":"qd","aliases":[],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"ultimate","name":"Ultimate"},{"id":"auto","name":"Auto"},{"id":"performance","name":"Performance"},{"id":"efficient","name":"Efficient"},{"id":"lite","name":"Lite"},{"id":"qmodel_38max","name":"Qwen3.8-Max"},{"id":"qmodel_latest","name":"Qwen3.7-Max"},{"id":"qmodel","name":"Qwen3.7-Plus"},{"id":"qfmodel","name":"Qwen3.8-Flash"},{"id":"kmodel_latest","name":"Kimi-K3"},{"id":"kmodel","name":"Kimi-K2.7-Code"},{"id":"gmodel","name":"GLM-5.3"},{"id":"gfmodel","name":"GLM-5.3-Flash"},{"id":"dmodel","name":"DeepSeek-V4-Pro"},{"id":"dfmodel","name":"DeepSeek-V4-Flash"},{"id":"mmodel","name":"MiniMax-M3"},{"id":"smodel","name":"Sonus"},{"id":"cmodel","name":"Cantus"}]},{"id":"recraft","name":"Recraft","alias":"recraft","uiAlias":"recraft","aliases":[],"serviceKinds":["image"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"recraftv3","name":"Recraft V3","kind":"image","params":["n","size","style"]},{"id":"recraftv2","name":"Recraft V2","kind":"image","params":["n","size","style"]}]},{"id":"runwayml","name":"Runway ML","alias":"runwayml","uiAlias":"runway","aliases":["runway"],"serviceKinds":["image"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"gen4_image","name":"Gen-4 Image","kind":"image","params":["size"]},{"id":"gen4_image_turbo","name":"Gen-4 Image Turbo","kind":"image","params":["size"]},{"id":"gen4_turbo","name":"Gen-4 Turbo","kind":"video","params":[]},{"id":"gen3a_turbo","name":"Gen-3 Alpha Turbo","kind":"video","params":[]}]},{"id":"sdwebui","name":"SD WebUI","alias":"sdwebui","uiAlias":"sdwebui","aliases":[],"serviceKinds":["image"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"stable-diffusion-v1-5","name":"Stable Diffusion v1.5","kind":"image","params":["n","size"]},{"id":"sdxl-base-1.0","name":"SDXL Base 1.0","kind":"image","params":["n","size"]}]},{"id":"searchapi","name":"SearchAPI","alias":"searchapi","uiAlias":"searchapi","aliases":[],"serviceKinds":["webSearch"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"searchConfig":{"searchTypes":["web","news"],"maxMaxResults":100,"requiredOptions":null},"models":[]},{"id":"searxng","name":"SearXNG","alias":"searxng","uiAlias":"searxng","aliases":[],"serviceKinds":["webSearch"],"noAuth":true,"passthroughModels":false,"hasFetch":false,"searchConfig":{"searchTypes":["web","news"],"maxMaxResults":50,"requiredOptions":null},"models":[]},{"id":"serper","name":"Serper","alias":"serper","uiAlias":"serper","aliases":[],"serviceKinds":["webSearch"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"searchConfig":{"searchTypes":["web","news"],"maxMaxResults":100,"requiredOptions":null},"models":[]},{"id":"siliconflow","name":"SiliconFlow","alias":"siliconflow","uiAlias":"siliconflow","aliases":[],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"deepseek-ai/DeepSeek-V4-Pro","name":"DeepSeek V4 Pro"},{"id":"deepseek-ai/DeepSeek-V4-Flash","name":"DeepSeek V4 Flash"},{"id":"deepseek-ai/DeepSeek-V3.2","name":"DeepSeek V3.2"},{"id":"deepseek-ai/DeepSeek-V3.2-Exp","name":"DeepSeek V3.2 Exp"},{"id":"deepseek-ai/DeepSeek-V3.1","name":"DeepSeek V3.1"},{"id":"deepseek-ai/DeepSeek-V3.1-Terminus","name":"DeepSeek V3.1 Terminus"},{"id":"deepseek-ai/DeepSeek-R1","name":"DeepSeek R1"},{"id":"Qwen/Qwen3.5-397B-A17B","name":"Qwen 3.5 397B A17B"},{"id":"Qwen/Qwen3.5-122B-A10B","name":"Qwen 3.5 122B A10B"},{"id":"zai-org/GLM-5.1","name":"GLM 5.1"},{"id":"zai-org/GLM-5","name":"GLM 5"},{"id":"moonshotai/Kimi-K2.6","name":"Kimi K2.6"},{"id":"moonshotai/Kimi-K2.5","name":"Kimi K2.5"},{"id":"openai/gpt-oss-120b","name":"GPT OSS 120B"},{"id":"MiniMaxAI/MiniMax-M2.5","name":"MiniMax M2.5"},{"id":"inclusionAI/Ling-flash-2.0","name":"Ling Flash 2.0"}]},{"id":"sensenova","name":"SenseNova","alias":"sensenova","uiAlias":"sensenova","aliases":[],"serviceKinds":["llm","image","imageToText"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"sensenova-6.8-flash","name":"SenseNova 6.8 Flash"},{"id":"sensenova-6.8-flash-lite","name":"SenseNova 6.8 Flash Lite"},{"id":"sensenova-u1.5-lite","name":"SenseNova U1.5 Lite","kind":"image","params":["n","size","output_format","response_format","watermark","prompt_extend"],"capabilities":["text2img","edit"]},{"id":"sensenova-u1.5-fast","name":"SenseNova U1.5 Fast","kind":"image","params":["n","size","output_format","response_format","watermark","prompt_extend"],"capabilities":["text2img"]},{"id":"deepseek-flash","name":"DeepSeek V4.1 Flash"},{"id":"deepseek-v4-flash","name":"DeepSeek V4 Flash"},{"id":"glm-5.3","name":"GLM-5.3"},{"id":"glm-5.2","name":"GLM-5.2"},{"id":"kimi-k3","name":"Kimi K3"}]},{"id":"stability-ai","name":"Stability AI","alias":"stability-ai","uiAlias":"stability","aliases":["stability"],"serviceKinds":["image"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"stable-image-ultra","name":"Stable Image Ultra","kind":"image","params":["size"]},{"id":"stable-image-core","name":"Stable Image Core","kind":"image","params":["size","style"]},{"id":"sd3.5-large","name":"Stable Diffusion 3.5 Large","kind":"image","params":["size"]},{"id":"sd3.5-large-turbo","name":"Stable Diffusion 3.5 Large Turbo","kind":"image","params":["size"]},{"id":"sd3.5-medium","name":"Stable Diffusion 3.5 Medium","kind":"image","params":["size"]}]},{"id":"tavily","name":"Tavily","alias":"tavily","uiAlias":"tavily","aliases":[],"serviceKinds":["webSearch","webFetch"],"noAuth":false,"passthroughModels":false,"hasFetch":true,"searchConfig":{"searchTypes":["web","news"],"maxMaxResults":20,"requiredOptions":null},"models":[]},{"id":"together","name":"Together AI","alias":"together","uiAlias":"together","aliases":[],"serviceKinds":["llm","embedding"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"meta-llama/Llama-3.3-70B-Instruct-Turbo","name":"Llama 3.3 70B Turbo"},{"id":"deepseek-ai/DeepSeek-R1","name":"DeepSeek R1"},{"id":"Qwen/Qwen3-235B-A22B","name":"Qwen3 235B"},{"id":"meta-llama/Llama-4-Maverick-17B-128E-Instruct-FP8","name":"Llama 4 Maverick"},{"id":"BAAI/bge-large-en-v1.5","name":"BGE Large EN v1.5","kind":"embedding"},{"id":"togethercomputer/m2-bert-80M-8k-retrieval","name":"M2 BERT 80M 8K","kind":"embedding"}]},{"id":"topaz","name":"Topaz","alias":"topaz","uiAlias":"topaz","aliases":[],"serviceKinds":["image"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[]},{"id":"tortoise","name":"Tortoise TTS","alias":"tortoise","uiAlias":"tortoise","aliases":[],"serviceKinds":["tts"],"noAuth":true,"passthroughModels":false,"hasFetch":false,"models":[]},{"id":"venice","name":"Venice AI","alias":"venice","uiAlias":"venice","aliases":["vn"],"serviceKinds":["llm","embedding","image"],"noAuth":false,"passthroughModels":true,"hasFetch":false,"models":[{"id":"venice-uncensored-1-2","name":"Venice Uncensored 1.2"},{"id":"zai-org-glm-5","name":"GLM-5"},{"id":"qwen3-235b-a22b-instruct-2507","name":"Qwen3 235B A22B Instruct"},{"id":"qwen3-coder-480b-a35b-instruct-turbo","name":"Qwen3 Coder 480B A35B Turbo"},{"id":"qwen3-vl-235b-a22b","name":"Qwen3 VL 235B A22B"},{"id":"deepseek-v4-pro","name":"DeepSeek V4 Pro"},{"id":"llama-3.3-70b","name":"Llama 3.3 70B"},{"id":"hermes-3-llama-3.1-405b","name":"Hermes 3 Llama 3.1 405B"},{"id":"mistral-small-3-2-24b-instruct","name":"Mistral Small 3.2 24B"},{"id":"text-embedding-3-large","name":"Text Embedding 3 Large","kind":"embedding"},{"id":"text-embedding-bge-m3","name":"BGE-M3 Embedding","kind":"embedding"},{"id":"text-embedding-qwen3-8b","name":"Qwen3 8B Embedding","kind":"embedding"},{"id":"venice-sd35","name":"Venice SD3.5","kind":"image","params":["n","size"]},{"id":"flux-2-pro","name":"FLUX.2 Pro","kind":"image","params":["n","size"]},{"id":"gpt-image-2","name":"GPT Image 2 (via Venice)","kind":"image","params":["n","size","quality"]}]},{"id":"vercel-ai-gateway","name":"Vercel AI Gateway","alias":"vercel-ai-gateway","uiAlias":"vercel","aliases":["vercel"],"serviceKinds":["llm","embedding","image","imageToText","webSearch"],"noAuth":false,"passthroughModels":true,"hasFetch":false,"models":[]},{"id":"vertex-partner","name":"Vertex Partner","alias":"vertex-partner","uiAlias":"vxp","aliases":["vxp"],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"deepseek-ai/deepseek-v3.2-maas","name":"DeepSeek V3.2 (Vertex)"},{"id":"qwen/qwen3-next-80b-a3b-thinking-maas","name":"Qwen3 Next 80B Thinking (Vertex)"},{"id":"qwen/qwen3-next-80b-a3b-instruct-maas","name":"Qwen3 Next 80B Instruct (Vertex)"},{"id":"zai-org/glm-5-maas","name":"GLM-5 (Vertex)"}]},{"id":"vertex","name":"Vertex AI","alias":"vertex","uiAlias":"vx","aliases":["vx"],"serviceKinds":["llm","imageToText","video"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"gemini-4-argon","name":"Gemini 4 Argon"},{"id":"gemini-4-carbon","name":"Gemini 4 Carbon"},{"id":"gemini-3.1-pro-preview","name":"Gemini 3.1 Pro Preview"},{"id":"gemini-3.1-flash-lite-preview","name":"Gemini 3.1 Flash Lite Preview"},{"id":"gemini-3-flash-preview","name":"Gemini 3 Flash Preview"},{"id":"gemini-2.5-flash","name":"Gemini 2.5 Flash"},{"id":"veo-3.1-generate-preview","name":"Veo 3.1 (Preview)","kind":"video","params":["duration","aspect_ratio","resolution","negative_prompt","seed","storage_uri","generate_audio"]},{"id":"veo-3.1-fast-generate-preview","name":"Veo 3.1 Fast (Preview)","kind":"video","params":["duration","aspect_ratio","resolution","negative_prompt","seed","storage_uri","generate_audio"]},{"id":"veo-3.0-generate-001","name":"Veo 3","kind":"video","params":["duration","aspect_ratio","resolution","negative_prompt","seed","storage_uri","generate_audio"]},{"id":"veo-2.0-generate-001","name":"Veo 2","kind":"video","params":["duration","aspect_ratio","negative_prompt","seed","storage_uri"]}]},{"id":"volcengine-ark","name":"Volcengine Ark","alias":"volcengine-ark","uiAlias":"ark","aliases":["ark"],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"Doubao-Seed-2.0-Code","name":"Doubao-Seed-2.0-Code"},{"id":"Doubao-Seed-2.0-pro","name":"Doubao-Seed-2.0-pro"},{"id":"Doubao-Seed-2.0-lite","name":"Doubao-Seed-2.0-lite"},{"id":"Doubao-Seed-Code","name":"Doubao-Seed-Code"},{"id":"DeepSeek-V4-Flash","name":"DeepSeek-V4-Flash"},{"id":"DeepSeek-V4-Pro","name":"DeepSeek-V4-Pro"},{"id":"GLM-5.1","name":"GLM-5.1"},{"id":"MiniMax-M2.7","name":"MiniMax-M2.7"},{"id":"Kimi-K2.6","name":"Kimi-K2.6"}]},{"id":"voyage-ai","name":"Voyage AI","alias":"voyage-ai","uiAlias":"voyage","aliases":[],"serviceKinds":["embedding"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"voyage-3-large","name":"Voyage 3 Large","kind":"embedding"},{"id":"voyage-3.5","name":"Voyage 3.5","kind":"embedding"},{"id":"voyage-3.5-lite","name":"Voyage 3.5 Lite","kind":"embedding"},{"id":"voyage-code-3","name":"Voyage Code 3","kind":"embedding"},{"id":"voyage-finance-2","name":"Voyage Finance 2","kind":"embedding"},{"id":"voyage-law-2","name":"Voyage Law 2","kind":"embedding"},{"id":"voyage-multilingual-2","name":"Voyage Multilingual 2","kind":"embedding"}]},{"id":"xai","name":"xAI (Grok)","alias":"xai","uiAlias":"xai","aliases":[],"serviceKinds":["llm","imageToText","webSearch","image","video"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"grok-4.7","name":"Grok 4.7"},{"id":"grok-4.6","name":"Grok 4.6"},{"id":"grok-4.5","name":"Grok 4.5"},{"id":"grok-4","name":"Grok 4"},{"id":"grok-4-fast-reasoning","name":"Grok 4 Fast Reasoning"},{"id":"grok-code-fast-1","name":"Grok Code Fast"},{"id":"grok-3","name":"Grok 3"},{"id":"grok-2-image-1212","name":"Grok 2 Image","kind":"image","params":["n","response_format"]},{"id":"grok-imagine-video","name":"Grok Imagine Video","kind":"video","params":["duration","aspect_ratio","resolution"]}]},{"id":"xiaomi-mimo","name":"Xiaomi MiMo","alias":"xiaomi-mimo","uiAlias":"mimo","aliases":["mimo","mimo-desktop","xmd"],"serviceKinds":["llm","tts"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"mimo-v2.6-pro","name":"MiMo V2.6 Pro"},{"id":"mimo-v2.6-flash","name":"MiMo V2.6 Flash"},{"id":"mimo-v2.6-pro-ultraspeed","name":"MiMo V2.6 Pro UltraSpeed"},{"id":"mimo-v2.5-pro","name":"MiMo V2.5 Pro"},{"id":"mimo-v2.5","name":"MiMo V2.5"},{"id":"mimo-v2-omni","name":"MiMo V2 Omni"},{"id":"mimo-v2-flash","name":"MiMo V2 Flash"},{"id":"mimo-v2.5-tts","name":"MiMo V2.5 TTS","kind":"tts"}]},{"id":"xiaomi-tokenplan","name":"Xiaomi MiMo (Token Plan)","alias":"xiaomi-tokenplan","uiAlias":"xmtp","aliases":["xmtp"],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"mimo-v2.6-pro","name":"MiMo V2.6 Pro"},{"id":"mimo-v2.6-flash","name":"MiMo V2.6 Flash"},{"id":"mimo-v2.5-asr","name":"MiMo V2.5 ASR","kind":"stt","params":["language"]},{"id":"mimo-v2.5-tts","name":"MiMo V2.5 TTS","kind":"tts"},{"id":"mimo-v2.5-tts-voiceclone","name":"MiMo V2.5 TTS Voice Clone","kind":"tts"},{"id":"mimo-v2.5-tts-voicedesign","name":"MiMo V2.5 TTS Voice Design","kind":"tts"}]},{"id":"youcom","name":"You.com Search","alias":"youcom","uiAlias":"youcom","aliases":[],"serviceKinds":["webSearch"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"searchConfig":{"searchTypes":["web","news"],"maxMaxResults":100,"requiredOptions":null},"models":[]},{"id":"alims-intl","name":"Alibaba Studio","alias":"alims-intl","uiAlias":"alims-intl","aliases":[],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"qwen3.5-plus","name":"Qwen3.5 Plus"},{"id":"kimi-k2.5","name":"Kimi K2.5"},{"id":"glm-5","name":"GLM 5"},{"id":"MiniMax-M2.5","name":"MiniMax M2.5"},{"id":"qwen3-coder-next","name":"Qwen3 Coder Next"},{"id":"qwen3-coder-plus","name":"Qwen3 Coder Plus"},{"id":"glm-4.7","name":"GLM 4.7"}]},{"id":"codebuddy-intl","name":"CodeBuddy","alias":"cbai","uiAlias":"cbai","aliases":[],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"glm-5.2","name":"GLM-5.2"},{"id":"glm-5.1","name":"GLM-5.1"},{"id":"glm-5.0","name":"GLM-5.0"},{"id":"glm-5.0-turbo","name":"GLM-5.0-Turbo"},{"id":"glm-5v-turbo","name":"GLM-5v-Turbo"},{"id":"glm-4.7","name":"GLM-4.7"},{"id":"minimax-m3","name":"MiniMax-M3"},{"id":"minimax-m2.7","name":"MiniMax-M2.7"},{"id":"kimi-k2.7","name":"Kimi-K2.7-Code"},{"id":"kimi-k2.6","name":"Kimi-K2.6"},{"id":"kimi-k2.5","name":"Kimi-K2.5"},{"id":"hy3-preview","name":"Hy3 Preview"},{"id":"deepseek-v4-pro","name":"DeepSeek-V4-Pro"},{"id":"deepseek-v4.1-flash","name":"DeepSeek-V4.1-Flash"},{"id":"deepseek-v3-2-volc","name":"DeepSeek-V3.2"}]},{"id":"zed","name":"Zed","alias":"zd","uiAlias":"zd","aliases":[],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":true,"hasFetch":false,"models":[]},{"id":"api-airforce","name":"API.airforce","alias":"af","uiAlias":"af","aliases":["airforce"],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":true,"hasFetch":false,"models":[{"id":"gpt-oss-120b","name":"GPT-OSS 120B (Free)"},{"id":"gpt-oss-20b","name":"GPT-OSS 20B (Free)"},{"id":"kimi-k2.7-code","name":"Kimi K2.7 Code (Free)"}]},{"id":"baidu","name":"Baidu Qianfan","alias":"qianfan","uiAlias":"qianfan","aliases":["qianfan","ernie","baidu-qianfan"],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"deepseek-v4-pro","name":"DeepSeek V4 Pro"},{"id":"deepseek-v4-flash","name":"DeepSeek V4 Flash"},{"id":"glm-5.2","name":"GLM 5.2"},{"id":"glm-5.1","name":"GLM 5.1"},{"id":"kimi-k2.6","name":"Kimi K2.6"},{"id":"qwen3.5-397b-a17b","name":"Qwen 3.5 397B A17B"},{"id":"qwen3.5-27b","name":"Qwen 3.5 27B"}]},{"id":"bazaarlink","name":"Bazaarlink","alias":"bzl","uiAlias":"bzl","aliases":["bazaar-link"],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"auto:free","name":"Auto Free (Zero Cost)"},{"id":"claude-opus-4.7","name":"Claude Opus 4.7"},{"id":"claude-sonnet-4.6","name":"Claude Sonnet 4.6"},{"id":"claude-haiku-4.5","name":"Claude Haiku 4.5"},{"id":"gpt-5.5","name":"GPT-5.5"},{"id":"gpt-5.4","name":"GPT-5.4"},{"id":"gpt-5.4-mini","name":"GPT-5.4 Mini"},{"id":"gpt-5.4-nano","name":"GPT-5.4 Nano"},{"id":"grok-4.3","name":"Grok 4.3"},{"id":"grok-4.20","name":"Grok 4.20"},{"id":"gemini-3.1-pro-preview","name":"Gemini 3.1 Pro"},{"id":"gemini-3-flash-preview","name":"Gemini 3 Flash"},{"id":"gemini-3.1-flash-lite-preview","name":"Gemini 3.1 Flash Lite"},{"id":"kimi-k2.6","name":"Kimi K2.6"},{"id":"kimi-k2.5","name":"Kimi K2.5"},{"id":"glm-5.1","name":"GLM 5.1"},{"id":"glm-5","name":"GLM 5"},{"id":"mimo-v2.5-pro","name":"MiMo-V2.5-Pro"},{"id":"mimo-v2.5","name":"MiMo-V2.5"},{"id":"minimax-m3","name":"MiniMax M3"},{"id":"minimax-m2.7","name":"MiniMax M2.7"},{"id":"minimax-m2.5","name":"MiniMax M2.5"},{"id":"qwen3.6-plus","name":"Qwen 3.6 Plus"},{"id":"nemotron-3-super-120b-a12b","name":"Nemotron 3 Super"}]},{"id":"bluesminds","name":"BluesMinds","alias":"bm","uiAlias":"bm","aliases":["blue-sminds"],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"gpt-4.1","name":"GPT-4.1"},{"id":"gpt-4.1-mini","name":"GPT-4.1 Mini"},{"id":"gpt-4.1-nano","name":"GPT-4.1 Nano"},{"id":"claude-sonnet-4-5","name":"Claude Sonnet 4.5"},{"id":"claude-haiku-4-5","name":"Claude Haiku 4.5"},{"id":"gemini-2.0-flash","name":"Gemini 2.0 Flash"},{"id":"gemini-2.0-flash-exp","name":"Gemini 2.0 Flash (Exp)"},{"id":"qwen-turbo","name":"Qwen Turbo"},{"id":"kimi-k2","name":"Kimi K2"},{"id":"kimi-k2-thinking","name":"Kimi K2 Thinking"},{"id":"glm-4.7","name":"GLM 4.7"},{"id":"minimax-m2.5","name":"MiniMax M2.5"},{"id":"claude-opus-4-5","name":"Claude Opus 4.5 (VIP)"},{"id":"gemini-2.5-pro","name":"Gemini 2.5 Pro (VIP)"}]},{"id":"kilo-gateway","name":"Kilo Gateway","alias":"kgw","uiAlias":"kgw","aliases":["kilo-gateway","kilogateway"],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"kilo-auto/free","name":"Kilo Auto Free"},{"id":"nvidia/nemotron-3-super-120b-a12b:free","name":"Nemotron 3 Super 120B (Free)"},{"id":"nvidia/nemotron-3-ultra-550b-a55b:free","name":"Nemotron 3 Ultra 550B (Free)"},{"id":"kwaipilot/kat-coder-pro-v2.5:free","name":"Kat Coder Pro v2.5 (Free)"},{"id":"kilo-auto/frontier","name":"Kilo Auto Frontier"},{"id":"kilo-auto/balanced","name":"Kilo Auto Balanced"}]},{"id":"llm7","name":"LLM7","alias":"llm7","uiAlias":"llm7","aliases":["llm-7"],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":true,"hasFetch":false,"models":[{"id":"gpt-5.5","name":"GPT-5.5 (LLM7)"},{"id":"claude-opus-5","name":"Claude Opus 5 (LLM7)"},{"id":"deepseek-v4-flash","name":"DeepSeek V4 Flash (LLM7)"},{"id":"grok-4.5","name":"Grok 4.5 (LLM7)"},{"id":"kimi-k3","name":"Kimi K3 (LLM7)"}]},{"id":"sambanova","name":"SambaNova","alias":"samba","uiAlias":"samba","aliases":["sambanova-ai"],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"MiniMax-M2.7","name":"MiniMax M2.7"}]},{"id":"tencent","name":"Tencent Hunyuan","alias":"hunyuan","uiAlias":"hunyuan","aliases":["hunyuan","tencent-hunyuan"],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"hunyuan-turbos-latest","name":"Hunyuan TurboS Latest"},{"id":"hunyuan-t1-latest","name":"Hunyuan T1 Latest"}]},{"id":"morph","name":"Morph","alias":"morph","uiAlias":"morph","aliases":["morphllm"],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"morph-v3-large","name":"Morph v3 Large"},{"id":"morph-v3-fast","name":"Morph v3 Fast"},{"id":"morph-qwen35-397b","name":"Qwen 3.5 397B (Morph)"},{"id":"morph-minimax27-230b","name":"MiniMax M2.7 (Morph)"},{"id":"morph-qwen36-27b","name":"Qwen 3.6 27B (Morph)"},{"id":"morph-dsv4flash","name":"DeepSeek V4 Flash (Morph)"}]},{"id":"poolside","name":"Poolside","alias":"poolside","uiAlias":"ps","aliases":["ps"],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"poolside/laguna-s-2.1","name":"Laguna S 2.1"},{"id":"poolside/laguna-xs-2.1","name":"Laguna XS 2.1"}]},{"id":"tokenrouter","name":"TokenRouter","alias":"tokenrouter","uiAlias":"tokenrouter","aliases":["tr"],"serviceKinds":["llm","embedding","image"],"noAuth":false,"passthroughModels":true,"hasFetch":false,"models":[{"id":"anthropic/claude-haiku-4.5","name":"Claude Haiku 4.5"},{"id":"anthropic/claude-sonnet-4.6","name":"Claude Sonnet 4.6"},{"id":"anthropic/claude-opus-4.8","name":"Claude Opus 4.8"},{"id":"anthropic/claude-opus-4.8-fast","name":"Claude Opus 4.8 Fast"},{"id":"openai/gpt-5.4","name":"Gpt 5.4"},{"id":"openai/gpt-5.4-mini","name":"Gpt 5.4 Mini"},{"id":"openai/gpt-5.4-pro","name":"Gpt 5.4 Pro"},{"id":"openai/gpt-5.5","name":"Gpt 5.5"},{"id":"openai/gpt-5.6-sol","name":"Gpt 5.6 Sol"},{"id":"google/gemini-3.5-flash","name":"Gemini 3.5 Flash"},{"id":"google/gemini-3.6-flash","name":"Gemini 3.6 Flash"},{"id":"deepseek/deepseek-v4-flash","name":"Deepseek V4 Flash"},{"id":"deepseek/deepseek-v4-pro","name":"Deepseek V4 Pro"},{"id":"qwen/qwen3-coder-next","name":"Qwen3 Coder Next"},{"id":"qwen/qwen3.7-max","name":"Qwen3.7 Max"},{"id":"qwen/qwen3.8-max","name":"Qwen3.8 Max"},{"id":"moonshotai/kimi-k2.7-code","name":"Kimi K2.7 Code"},{"id":"moonshotai/kimi-k3-free","name":"Kimi K3 Free"},{"id":"z-ai/glm-5.3-free","name":"Glm 5.3 Free"},{"id":"z-ai/glm-5.2","name":"Glm 5.2"},{"id":"z-ai/glm-5-turbo","name":"Glm 5 Turbo"},{"id":"x-ai/grok-4.5","name":"Grok 4.5"}]},{"id":"selfhosted-stt","name":"Self-hosted STT","alias":"selfhosted-stt","uiAlias":"selfhosted-stt","aliases":[],"serviceKinds":["stt"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"whisper-1","name":"Whisper (self-hosted)","kind":"stt","params":["language","response_format","temperature","prompt"]}]},{"id":"selfhosted-tts","name":"Self-hosted TTS","alias":"selfhosted-tts","uiAlias":"selfhosted-tts","aliases":[],"serviceKinds":["tts"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"kokoro","name":"Kokoro (self-hosted)","kind":"tts","params":["voice","response_format","speed"]}]},{"id":"selfhosted-embedding","name":"Self-hosted Embedding","alias":"selfhosted-embedding","uiAlias":"selfhosted-embedding","aliases":[],"serviceKinds":["embedding"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"embedding","name":"Self-hosted embedding model","kind":"embedding"}]},{"id":"fish-audio","name":"Fish Audio","alias":"fish","uiAlias":"fish","aliases":[],"serviceKinds":["tts"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[]},{"id":"alitp-intl","name":"Alibaba Token Plan","alias":"alitp-intl","uiAlias":"alitp-intl","aliases":[],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"qwen3.8-max-preview","name":"Qwen3.8 Max Preview"},{"id":"qwen3.7-max","name":"Qwen3.7 Max"},{"id":"qwen3.7-plus","name":"Qwen3.7 Plus"},{"id":"qwen3.6-flash","name":"Qwen3.6 Flash"},{"id":"glm-5.2","name":"GLM 5.2"},{"id":"deepseek-v4-pro","name":"DeepSeek V4 Pro"}]},{"id":"xquik","name":"Xquik","alias":"xquik","uiAlias":"xquik","aliases":[],"serviceKinds":["webSearch"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"searchConfig":{"searchTypes":["x"],"maxMaxResults":100,"requiredOptions":null},"models":[]},{"id":"tokenharbor","name":"Token Harbor","alias":"tokenharbor","uiAlias":"tokenharbor","aliases":["th","thh"],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":true,"hasFetch":false,"models":[{"id":"claude-opus-5.5","name":"Claude Opus 5.5"},{"id":"claude-sonnet-5","name":"Claude Sonnet 5"},{"id":"gpt-6-astra","name":"GPT-6 Astra"},{"id":"gpt-6-sol","name":"GPT-6 Sol"},{"id":"deepseek-v4.1-flash:free","name":"DeepSeek V4.1 Flash (Free)"},{"id":"grok-4.7","name":"Grok 4.7"}]},{"id":"dahl","name":"Dahl Inference","alias":"dahl","uiAlias":"dahl","aliases":["dahl-inference"],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":true,"hasFetch":false,"models":[{"id":"zai-org/GLM-5.3-Flash","name":"GLM-5.3 Flash"},{"id":"deepseek-ai/DeepSeek-V4-Flash-0731","name":"DeepSeek V4 Flash 0731"},{"id":"MiniMaxAI/MiniMax-M2.7","name":"MiniMax M2.7"}]},{"id":"atria","name":"Atria Dawn","alias":"atria","uiAlias":"atria","aliases":["atria-asi"],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":true,"hasFetch":false,"models":[{"id":"Atria-Dawn-Preview","name":"Atria Dawn Preview"}]},{"id":"agnes","name":"Agnes AI","alias":"agnes","uiAlias":"agnes","aliases":["agnes-ai"],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":true,"hasFetch":false,"models":[{"id":"agnes-2.5-flash","name":"Agnes 2.5 Flash"},{"id":"agnes-2.5-pro","name":"Agnes 2.5 Pro"},{"id":"agnes-2.5-pro-beta","name":"Agnes 2.5 Pro Beta"},{"id":"agnes-3.0-flash","name":"Agnes 3.0 Flash"}]},{"id":"bai","name":"B.AI","alias":"bai","uiAlias":"bai","aliases":["b-ai"],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":true,"hasFetch":false,"models":[]},{"id":"tinyfish","name":"TinyFish","alias":"tinyfish","uiAlias":"tinyfish","aliases":[],"serviceKinds":["webSearch","webFetch"],"noAuth":false,"passthroughModels":false,"hasFetch":true,"searchConfig":{"searchTypes":["web","news","research_paper"],"maxMaxResults":10,"requiredOptions":null},"models":[]},{"id":"v1m","name":"v1m (System One)","alias":"v1m","uiAlias":"v1m","aliases":["systemone","jev"],"serviceKinds":["systemone"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"rev-latest","name":"v1m Rev Latest (Calibrated)","kind":"systemone"},{"id":"v1m-decision-engine","name":"v1m Decision Engine","kind":"systemone"}]},{"id":"muse","name":"Muse (Meta Model API)","alias":"muse","uiAlias":"muse","aliases":["muse-ai","meta-model-api","muse-code","muse-subscription"],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":true,"hasFetch":false,"models":[{"id":"muse-spark-1.3","name":"Muse Spark 1.3"},{"id":"muse-spark-1.2","name":"Muse Spark 1.2"},{"id":"muse-spark-1.1","name":"Muse Spark 1.1"},{"id":"muse-spark-1.3-contributor","name":"Muse Spark 1.3 Contributor"},{"id":"muse-spark-1.2-contributor","name":"Muse Spark 1.2 Contributor"}]},{"id":"minimax-code","name":"MiniMax Code","alias":"mm","uiAlias":"mm","aliases":[],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"MiniMax-M3.1-Flash-Preview","name":"MiniMax M3.1 Flash Preview"},{"id":"MiniMax-M3","name":"MiniMax M3"},{"id":"MiniMax-M2.7","name":"MiniMax M2.7"},{"id":"MiniMax-M2.7-highspeed","name":"MiniMax M2.7 Highspeed"}]},{"id":"minimax-code-global","name":"MiniMax Code (Global)","alias":"mmg","uiAlias":"mmg","aliases":[],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"MiniMax-M3.1-Flash-Preview","name":"MiniMax M3.1 Flash Preview"},{"id":"MiniMax-M3","name":"MiniMax M3"},{"id":"MiniMax-M2.7","name":"MiniMax M2.7"},{"id":"MiniMax-M2.7-highspeed","name":"MiniMax M2.7 Highspeed"}]},{"id":"bedrock","name":"AWS Bedrock","alias":"br","uiAlias":"br","aliases":["aws-bedrock"],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":true,"hasFetch":false,"models":[{"id":"us.anthropic.claude-opus-5","name":"Claude Opus 5"},{"id":"us.anthropic.claude-sonnet-5","name":"Claude Sonnet 5"},{"id":"us.anthropic.claude-fable-5","name":"Claude Fable 5"},{"id":"us.anthropic.claude-opus-4-8","name":"Claude Opus 4.8"},{"id":"us.anthropic.claude-opus-4-7","name":"Claude Opus 4.7"},{"id":"us.anthropic.claude-sonnet-4-6","name":"Claude Sonnet 4.6"},{"id":"us.anthropic.claude-opus-4-5-20251101-v1:0","name":"Claude Opus 4.5"},{"id":"us.anthropic.claude-sonnet-4-5-20250929-v1:0","name":"Claude Sonnet 4.5"},{"id":"us.anthropic.claude-opus-4-1-20250805-v1:0","name":"Claude Opus 4.1"},{"id":"us.anthropic.claude-sonnet-4-20250514-v1:0","name":"Claude Sonnet 4"},{"id":"us.anthropic.claude-3-haiku-20240307-v1:0","name":"Claude 3 Haiku"}]},{"id":"bedrock-xai","name":"AWS Bedrock (xAI)","alias":"brx","uiAlias":"brx","aliases":["bedrock-grok"],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":true,"hasFetch":false,"models":[{"id":"us.xai.grok-4.6","name":"Grok 4.6"},{"id":"global.xai.grok-4.6","name":"Grok 4.6 (global)"}]},{"id":"nous","name":"Nous Research","alias":"nous","uiAlias":"nous","aliases":["nous-portal","nous-research","ns"],"serviceKinds":["llm","imageToText"],"noAuth":false,"passthroughModels":true,"hasFetch":false,"models":[{"id":"meituan/longcat-2.0:free","name":"Meituan: LongCat 2.0"},{"id":"meituan/longcat-2.5-preview:free","name":"Meituan: LongCat 2.5 Preview"},{"id":"stepfun/step-5-preview:free","name":"StepFun: Step 5 Preview"},{"id":"upstage/solar-mini4:free","name":"Upstage: Solar Mini 4"},{"id":"stepfun/step-3.7-flash:free","name":"StepFun: Step 3.7 Flash"},{"id":"poolside/laguna-s-2.1:free","name":"Poolside: Laguna S 2.1"},{"id":"poolside/laguna-xs-2.1:free","name":"Poolside: Laguna XS 2.1"},{"id":"stealth/missingno","name":"MissingNo"}]},{"id":"deepseek-web","name":"DeepSeek Web","alias":"deepseek-web","uiAlias":"deepseek-web","aliases":["ds-web"],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"deepseek-v4-pro","name":"DeepSeek V4 Pro"},{"id":"deepseek-v4-pro-think","name":"DeepSeek V4 Pro Think"},{"id":"deepseek-v4-pro-search","name":"DeepSeek V4 Pro Search"},{"id":"deepseek-v4-pro-think-search","name":"DeepSeek V4 Pro Think+Search"},{"id":"deepseek-v4-flash","name":"DeepSeek V4 Flash"},{"id":"deepseek-v4-flash-think","name":"DeepSeek V4 Flash Think"},{"id":"deepseek-v4-flash-search","name":"DeepSeek V4 Flash Search"},{"id":"deepseek-v4-flash-think-search","name":"DeepSeek V4 Flash Think+Search"},{"id":"deepseek-chat","name":"DeepSeek Chat"},{"id":"deepseek-reasoner","name":"DeepSeek Reasoner"},{"id":"DeepSeek-R1","name":"DeepSeek R1"},{"id":"DeepSeek-R1-Search","name":"DeepSeek R1 Search"},{"id":"DeepSeek-V3.2","name":"DeepSeek V3.2"},{"id":"DeepSeek-Search","name":"DeepSeek Search"}]},{"id":"gemini-web","name":"Gemini Web","alias":"gemini-web","uiAlias":"gemini-web","aliases":["gweb"],"serviceKinds":["image"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"gemini-3.1-pro","name":"Gemini 3.1 Pro"},{"id":"gemini-3.7-flash","name":"Gemini 3.7 Flash"},{"id":"gemini-3.1-flash-lite","name":"Gemini 3.1 Flash-Lite"},{"id":"gemini-image","name":"Gemini Image (web)","kind":"image","params":["n","size"]}]},{"id":"muse-web","name":"Muse Web","alias":"muse-web","uiAlias":"muse-web","aliases":["mu"],"serviceKinds":["image","video"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"muse-chat","name":"Muse Chat"},{"id":"muse-image","name":"Muse Image","kind":"image","params":["n","size"]},{"id":"muse-video","name":"Muse Video","kind":"video","params":["size","seconds","image"]}]},{"id":"chatgpt-web","name":"ChatGPT Web","alias":"chatgpt-web","uiAlias":"chatgpt-web","aliases":["cg"],"serviceKinds":["image"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"gpt-5-6","name":"GPT-5.6 Sol — Instant"},{"id":"gpt-5-6-thinking","name":"GPT-5.6 Sol — Thinking"},{"id":"gpt-5-6-pro","name":"GPT-5.6 Sol — Pro"},{"id":"gpt-5.6-luna-free","name":"GPT-5.6 Luna — Free"},{"id":"gpt-5.6-luna-free-thinking","name":"GPT-5.6 Luna — Free Thinking"},{"id":"gpt-5-5-instant","name":"GPT-5.5 — Instant"},{"id":"gpt-5-5-thinking","name":"GPT-5.5 — Thinking"},{"id":"gpt-5-5-pro","name":"GPT-5.5 — Pro"},{"id":"gpt-image-2","name":"GPT Image 2 (web)","kind":"image","params":["n","size"]}]},{"id":"veoaifree-web","name":"Veo AI Free Web","alias":"veo-free","uiAlias":"veo-free","aliases":[],"serviceKinds":["image","video"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"veo","name":"VEO 3.1 (Free)","kind":"video","params":["seconds","size"]},{"id":"seedance","name":"Seedance (Free)","kind":"video","params":["seconds","size"]},{"id":"nano-banana","name":"Nano Banana (Free)","kind":"image","params":["n","size"]}]},{"id":"uc-web","name":"UC Web","alias":"uc","uiAlias":"uc","aliases":[],"serviceKinds":["image","video"],"noAuth":false,"passthroughModels":true,"hasFetch":false,"models":[{"id":"seedream-v4.5","name":"Seedream v4.5 (UC)","kind":"image","params":["n","size"]},{"id":"seedream-v5","name":"Seedream v5 (UC)","kind":"image","params":["n","size"]},{"id":"seedream-v5-flash","name":"Seedream v5 Flash (UC)","kind":"image","params":["n","size","resolution"]},{"id":"nano-banana-pro-ultra","name":"Nano Banana Pro Ultra (UC)","kind":"image","params":["n","size","resolution"]},{"id":"nano-banana-pro","name":"Nano Banana Pro (UC)","kind":"image","params":["n","size","resolution"]},{"id":"nano-banana","name":"Nano Banana (UC)","kind":"image","params":["n","size"]},{"id":"nano-banana-2","name":"Nano Banana 2 (UC)","kind":"image","params":["n","size","resolution"]},{"id":"gpt-image","name":"GPT Image 1.5 (UC)","kind":"image","params":["n","size","quality"]},{"id":"gpt-image-2","name":"GPT Image 2 (UC)","kind":"image","params":["n","size"]},{"id":"gpt-image-2.5-flare","name":"GPT Image 2.5 Flare (UC)","kind":"image","params":["n","size"]},{"id":"gpt-image-2.5-sunburst","name":"GPT Image 2.5 Sunburst (UC)","kind":"image","params":["n","size"]},{"id":"wan-2.6-image","name":"Wan 2.6 Image (UC)","kind":"image","params":["n","size"]},{"id":"wan-2.7-text-to-image","name":"Wan 2.7 (UC)","kind":"image","params":["n","size"]},{"id":"wan-2.7-text-to-image-pro","name":"Wan 2.7 Pro (UC)","kind":"image","params":["n","size"]},{"id":"prefect-pony-xl","name":"Prefect Pony XL (UC)","kind":"image","params":["n","size"]},{"id":"1.0","name":"Flux.1 (UC)","kind":"image","params":["n","size"]},{"id":"1.1","name":"Flux.1.1 (UC)","kind":"image","params":["n","size"]},{"id":"2.0","name":"Flux.2 (UC)","kind":"image","params":["n","size"]},{"id":"flux-2-max","name":"Flux 2 Max (UC)","kind":"image","params":["n","size"]},{"id":"SDXL","name":"SDXL Uncensored (UC)","kind":"image","params":["n","size"]},{"id":"realism","name":"Realism (UC)","kind":"image","params":["n","size"]},{"id":"realism-2","name":"Realism 2 (UC)","kind":"image","params":["n","size"]},{"id":"z-image-turbo","name":"Z-Image Turbo (UC)","kind":"image","params":["n","size"]},{"id":"default","name":"Video Standard (UC)","kind":"video","params":["seconds","size","resolution","image"]},{"id":"turbo","name":"Video Turbo (UC)","kind":"video","params":["seconds","size","image"]},{"id":"wan-2.2-uncensored","name":"Wan Ultra Uncensored (UC)","kind":"video","params":["seconds","size","image"]},{"id":"wan-2.2-uncensored-v2","name":"Wan Ultra Uncensored v2 (UC)","kind":"video","params":["seconds","size","image"]},{"id":"wan-2.6","name":"Wan 2.6 Video (UC)","kind":"video","params":["seconds","size","image"]},{"id":"wan-2.6-ultra","name":"Wan 2.6 Ultra Uncensored (UC)","kind":"video","params":["seconds","size","image"]},{"id":"wan-2.7","name":"Wan 2.7 Video (UC)","kind":"video","params":["seconds","size","image"]},{"id":"wan-2.7-pro","name":"Wan 2.7 Pro Video (UC)","kind":"video","params":["seconds","size","image"]},{"id":"wan-2.7-uncensored","name":"Wan 2.7 Uncensored (UC)","kind":"video","params":["seconds","size","image"]},{"id":"wan-2.7-uncensored-v2","name":"Wan 2.7 Uncensored v2 (UC)","kind":"video","params":["seconds","size","image"]},{"id":"wan-3.0","name":"Wan 3.0 (UC)","kind":"video","params":["seconds","size","image"]},{"id":"wan-3.0-uncensored","name":"Wan 3.0 Uncensored (UC)","kind":"video","params":["seconds","size","image"]},{"id":"wan-3.0-uncensored-pro","name":"Wan 3.0 Uncensored Pro (UC)","kind":"video","params":["seconds","size","image"]},{"id":"sora-2","name":"Sora 2 (UC)","kind":"video","params":["seconds","size","image"]},{"id":"sora-2-pro","name":"Sora 2 Pro (UC)","kind":"video","params":["seconds","size","image"]},{"id":"grok-imagine-video","name":"Grok Imagine Video (UC)","kind":"video","params":["seconds","size","image"]},{"id":"bytedance-seedance-2.0","name":"Seedance 2.0 (UC)","kind":"video","params":["seconds","size","image"]},{"id":"seedance-2.0-fast","name":"Seedance 2.0 Fast (UC)","kind":"video","params":["seconds","size","image"]},{"id":"seedance-2.0-uncensored","name":"Seedance 2.0 Uncensored (UC)","kind":"video","params":["seconds","size","image"]},{"id":"seedance-2.0-fast-uncensored","name":"Seedance 2.0 Fast Uncensored (UC)","kind":"video","params":["seconds","size","image"]},{"id":"seedance-2.0-mini-uncensored","name":"Seedance 2.0 Mini Uncensored (UC)","kind":"video","params":["seconds","size","image"]},{"id":"seedance-2.0-mini-turbo","name":"Seedance 2.0 Mini Turbo Uncensored (UC)","kind":"video","params":["seconds","size","image"]},{"id":"seedance-v1.5-pro-uncensored","name":"Seedance v1.5 Pro Uncensored (UC)","kind":"video","params":["seconds","size","image"]},{"id":"seedance-2.5-uncensored","name":"Seedance 2.5 Uncensored (UC)","kind":"video","params":["seconds","size","image"]},{"id":"seedance-2.5","name":"Seedance 2.5 (UC)","kind":"video","params":["seconds","size","image"]},{"id":"vidu-q3-uncensored","name":"Vidu Q3 Uncensored (UC)","kind":"video","params":["seconds","size","image"]},{"id":"av-gen-v2","name":"AV-Gen v2 with audio (UC)","kind":"video","params":["seconds","size","image"]},{"id":"cosmos-predict","name":"Cosmos Predict 2.5 (UC)","kind":"video","params":["seconds","size","image"]},{"id":"minimax-h3","name":"MiniMax H3 (UC)","kind":"video","params":["seconds","size","image"]},{"id":"happyhorse-1.0","name":"Happy Horse 1.0 (UC)","kind":"video","params":["seconds","size","image"]}]},{"id":"runcrate","name":"Runcrate","alias":"runcrate","uiAlias":"rc","aliases":["rc"],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"deepseek-ai/DeepSeek-V4.1-Flash","name":"DeepSeek V4.1 Flash"},{"id":"deepseek-ai/DeepSeek-V4.1-Flash-1017","name":"DeepSeek V4.1 Flash 1017"},{"id":"zai-org/GLM-5.3","name":"GLM 5.3"},{"id":"zai-org/GLM-5.3-Flash","name":"GLM 5.3 Flash"},{"id":"Qwen/Qwen3.8-Flash","name":"Qwen3.8 Flash"},{"id":"openai/gpt-oss-120b","name":"gpt-oss 120B"}]},{"id":"nace","name":"Nace Drex","alias":"nace","uiAlias":"nace","aliases":["drex"],"serviceKinds":["systemone"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"drex-max","name":"Drex Max","kind":"systemone"},{"id":"drex-v2.0","name":"Drex 2.0","kind":"systemone"}]},{"id":"sensenova-intl","name":"SenseNova (Intl)","alias":"sensenova-intl","uiAlias":"sensenova-intl","aliases":[],"serviceKinds":["llm","image","imageToText"],"noAuth":false,"passthroughModels":false,"hasFetch":false,"models":[{"id":"sensenova-6.8-flash","name":"SenseNova 6.8 Flash"},{"id":"sensenova-6.8-flash-lite","name":"SenseNova 6.8 Flash Lite"},{"id":"sensenova-6.7-flash-lite","name":"SenseNova 6.7 Flash Lite"},{"id":"sensenova-u1.5-lite","name":"SenseNova U1.5 Lite","kind":"image","params":["n","size","output_format","response_format","watermark","prompt_extend"],"capabilities":["text2img","edit"]},{"id":"sensenova-u1-fast","name":"SenseNova U1 Fast","kind":"image","params":["n","size","output_format","response_format","watermark","prompt_extend"],"capabilities":["text2img"]}]},{"id":"synthetic","name":"Synthetic","alias":"synthetic","uiAlias":"syn","aliases":["syn","synthetic-new"],"serviceKinds":["llm"],"noAuth":false,"passthroughModels":true,"hasFetch":false,"models":[{"id":"hf:zai-org/GLM-5.3","name":"GLM 5.3"},{"id":"hf:zai-org/GLM-5.3-Flash","name":"GLM 5.3 Flash"},{"id":"hf:zai-org/GLM-4.7-Flash","name":"GLM 4.7 Flash"},{"id":"hf:moonshotai/Kimi-K3","name":"Kimi K3"},{"id":"hf:deepseek-ai/DeepSeek-V4.1-Flash","name":"DeepSeek V4.1 Flash"},{"id":"hf:deepseek-ai/DeepSeek-V4.1-Flash-1017","name":"DeepSeek V4.1 Flash 1017"},{"id":"hf:Qwen/Qwen3.8-27B","name":"Qwen3.8 27B"},{"id":"hf:openai/gpt-oss-120b","name":"GPT-OSS 120B"},{"id":"hf:nvidia/NVIDIA-Nemotron-3-Super-120B-A12B-NVFP4","name":"Nemotron 3 Super 120B"}]}]"##;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RegistryModel {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    params: Option<Value>,
    #[serde(default)]
    capabilities: Option<Value>,
    #[serde(default)]
    options: Option<Value>,
    #[serde(default)]
    dimensions: Option<Value>,
    #[serde(default)]
    context_window: Option<u64>,
    #[serde(default)]
    max_output: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RegistrySearchConfig {
    #[serde(default)]
    search_types: Option<Value>,
    #[serde(default)]
    max_max_results: Option<Value>,
    #[serde(default)]
    required_options: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RegistryProvider {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    alias: String,
    #[serde(default)]
    ui_alias: String,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    service_kinds: Vec<String>,
    #[serde(default)]
    no_auth: bool,
    #[serde(default)]
    has_fetch: bool,
    #[serde(default)]
    search_config: Option<RegistrySearchConfig>,
    #[serde(default)]
    models: Vec<RegistryModel>,
}

struct RegistryIndex {
    providers: Vec<RegistryProvider>,
    by_id: HashMap<String, usize>,
    alias_to_id: HashMap<String, String>,
    static_kind_by_full_id: HashMap<String, String>,
}

static REGISTRY_INDEX: OnceLock<RegistryIndex> = OnceLock::new();

fn registry() -> &'static RegistryIndex {
    REGISTRY_INDEX.get_or_init(|| {
        let providers: Vec<RegistryProvider> =
            serde_json::from_str(PROVIDER_REGISTRY_JSON).unwrap_or_default();
        let mut by_id = HashMap::new();
        let mut alias_to_id = HashMap::new();
        let mut static_kind_by_full_id = HashMap::new();

        for (idx, p) in providers.iter().enumerate() {
            by_id.insert(p.id.clone(), idx);
            if !p.alias.is_empty() {
                alias_to_id.insert(p.alias.clone(), p.id.clone());
            }
            if !p.ui_alias.is_empty() {
                alias_to_id.insert(p.ui_alias.clone(), p.id.clone());
            }
            for a in &p.aliases {
                alias_to_id.insert(a.clone(), p.id.clone());
            }

            let mut keys = vec![p.id.as_str()];
            if !p.alias.is_empty() {
                keys.push(p.alias.as_str());
            }
            if !p.ui_alias.is_empty() {
                keys.push(p.ui_alias.as_str());
            }
            for m in &p.models {
                let kind = m
                    .kind
                    .as_deref()
                    .unwrap_or("llm")
                    .to_string();
                for k in &keys {
                    static_kind_by_full_id.insert(format!("{}/{}", k, m.id), kind.clone());
                }
            }
        }

        RegistryIndex {
            providers,
            by_id,
            alias_to_id,
            static_kind_by_full_id,
        }
    })
}

fn find_registry_provider(alias_or_id: &str) -> Option<&'static RegistryProvider> {
    let reg = registry();
    if let Some(&idx) = reg.by_id.get(alias_or_id) {
        return reg.providers.get(idx);
    }
    if let Some(pid) = reg.alias_to_id.get(alias_or_id) {
        if let Some(&idx) = reg.by_id.get(pid) {
            return reg.providers.get(idx);
        }
    }
    None
}

fn resolve_provider_id(alias_or_id: &str) -> String {
    let reg = registry();
    if reg.by_id.contains_key(alias_or_id) {
        return alias_or_id.to_string();
    }
    reg.alias_to_id
        .get(alias_or_id)
        .cloned()
        .unwrap_or_else(|| alias_or_id.to_string())
}

fn get_provider_alias(provider_id: &str) -> String {
    find_registry_provider(provider_id)
        .map(|p| {
            if !p.ui_alias.is_empty() {
                p.ui_alias.clone()
            } else if !p.alias.is_empty() {
                p.alias.clone()
            } else {
                p.id.clone()
            }
        })
        .unwrap_or_else(|| provider_id.to_string())
}

// ============================================================================
// Constants & Shared Helpers
// ============================================================================

const USAGE_APIKEY_PROVIDERS: &[&str] = &[
    "clinepass",
    "codebuddy-cn",
    "codebuddy-intl",
    "commandcode",
    "deepseek",
    "glm",
    "glm-cn",
    "groq",
    "kimi",
    "kiro",
    "minimax",
    "minimax-cn",
    "ollama",
    "opencode-go",
    "opencode-zen",
    "qoder",
    "qoder-cn",
    "synthetic",
    "vercel-ai-gateway",
    "xiaomi-mimo",
];

const AUTH_EXPIRED_PATTERNS: &[&str] = &[
    "expired",
    "authentication",
    "unauthorized",
    "401",
    "re-authorize",
];

const TRANSLATOR_ALLOWED_FILES: &[&str] = &[
    "1_req_client.json",
    "2_req_source.json",
    "3_req_openai.json",
    "4_req_target.json",
    "5_res_provider.txt",
    "6_res_openai.txt",
    "7_res_client.txt",
    "7_res_client.json",
];

const CLAUDE_CACHE_TTL: Duration = Duration::from_secs(300);
const CLAUDE_429_COOLDOWN: Duration = Duration::from_secs(180);

struct CachedClaudeUsage {
    result: Value,
    expires_at: Instant,
}

static CLAUDE_USAGE_CACHE: OnceLock<DashMap<String, CachedClaudeUsage>> = OnceLock::new();
static CLAUDE_429_COOLDOWN_MAP: OnceLock<DashMap<String, Instant>> = OnceLock::new();

fn claude_usage_cache() -> &'static DashMap<String, CachedClaudeUsage> {
    CLAUDE_USAGE_CACHE.get_or_init(DashMap::new)
}

fn claude_429_cooldown_map() -> &'static DashMap<String, Instant> {
    CLAUDE_429_COOLDOWN_MAP.get_or_init(DashMap::new)
}

fn json_response(status: StatusCode, body: Value) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        Json(body),
    )
        .into_response()
}

fn json_cors_response(status: StatusCode, body: Value) -> Response {
    (
        status,
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
        ],
        Json(body),
    )
        .into_response()
}

fn mask_api_key(key: Option<&str>) -> Option<String> {
    let k = key?.trim();
    if k.is_empty() {
        return None;
    }
    if k.len() > 11 {
        Some(format!("{}...{}", &k[..7], &k[k.len() - 4..]))
    } else if k.len() > 4 {
        Some(format!("{}...", &k[..4]))
    } else {
        Some("****".to_string())
    }
}

fn is_auth_expired_str(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    AUTH_EXPIRED_PATTERNS.iter().any(|p| lower.contains(p))
}

fn is_auth_expired_usage(val: &Value) -> bool {
    if let Some(msg) = val.get("message").and_then(|v| v.as_str()) {
        return is_auth_expired_str(msg);
    }
    false
}

fn parse_reset_time(val: Option<&Value>) -> Option<String> {
    let v = val?;
    if v.is_null() {
        return None;
    }
    if let Some(n) = v.as_i64() {
        if n <= 0 {
            return None;
        }
        let ms = if n < 1_000_000_000_000 { n * 1000 } else { n };
        return DateTime::from_timestamp_millis(ms)
            .map(|dt| dt.to_rfc3339_opts(SecondsFormat::Millis, true));
    }
    if let Some(f) = v.as_f64() {
        if !f.is_finite() || f <= 0.0 {
            return None;
        }
        let n = f as i64;
        let ms = if n < 1_000_000_000_000 { n * 1000 } else { n };
        return DateTime::from_timestamp_millis(ms)
            .map(|dt| dt.to_rfc3339_opts(SecondsFormat::Millis, true));
    }
    if let Some(s) = v.as_str() {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return None;
        }
        if trimmed.chars().all(|c| c.is_ascii_digit()) {
            if let Ok(n) = trimmed.parse::<i64>() {
                if n <= 0 {
                    return None;
                }
                let ms = if n < 1_000_000_000_000 { n * 1000 } else { n };
                return DateTime::from_timestamp_millis(ms)
                    .map(|dt| dt.to_rfc3339_opts(SecondsFormat::Millis, true));
            }
        }
        if let Ok(dt) = DateTime::parse_from_rfc3339(trimmed) {
            return Some(
                dt.with_timezone(&Utc)
                    .to_rfc3339_opts(SecondsFormat::Millis, true),
            );
        }
        if let Ok(d) = NaiveDate::parse_from_str(trimmed, "%Y-%m-%d") {
            if let Some(dt) = d.and_hms_opt(0, 0, 0) {
                return Some(
                    dt.and_utc()
                        .to_rfc3339_opts(SecondsFormat::Millis, true),
                );
            }
        }
    }
    None
}

fn to_finite_f64(val: Option<&Value>, fallback: f64) -> f64 {
    let Some(v) = val else {
        return fallback;
    };
    if let Some(f) = v.as_f64() {
        if f.is_finite() {
            return f;
        }
    }
    if let Some(s) = v.as_str() {
        if let Ok(f) = s.trim().parse::<f64>() {
            if f.is_finite() {
                return f;
            }
        }
    }
    fallback
}

fn conn_psd(conn: &ProviderConnection) -> Option<&serde_json::Map<String, Value>> {
    conn.data
        .extra
        .get("providerSpecificData")
        .and_then(|v| v.as_object())
}

fn conn_str<'a>(conn: &'a ProviderConnection, key: &str) -> Option<&'a str> {
    conn.data
        .extra
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            conn_psd(conn)
                .and_then(|p| p.get(key))
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
        })
}

fn conn_access_token(conn: &ProviderConnection) -> Option<String> {
    conn_str(conn, "accessToken").map(ToString::to_string)
}

fn conn_api_key(conn: &ProviderConnection) -> Option<String> {
    conn.data
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .or_else(|| conn_str(conn, "apiKey").map(ToString::to_string))
}

fn conn_needs_refresh(conn: &ProviderConnection) -> bool {
    let expires_str = conn_str(conn, "expiresAt").or_else(|| conn_str(conn, "tokenExpiresAt"));
    let Some(exp_raw) = expires_str else {
        return false;
    };
    if let Ok(dt) = DateTime::parse_from_rfc3339(exp_raw) {
        return dt.with_timezone(&Utc) <= Utc::now() + chrono::Duration::seconds(300);
    }
    if let Ok(ts) = exp_raw.parse::<i64>() {
        let ms = if ts < 1_000_000_000_000 { ts * 1000 } else { ts };
        if let Some(dt) = DateTime::from_timestamp_millis(ms) {
            return dt <= Utc::now() + chrono::Duration::seconds(300);
        }
    }
    false
}

fn build_usage_http_client(state: &AppState, conn: &ProviderConnection) -> reqwest::Client {
    let psd = conn_psd(conn);
    let enabled = psd
        .and_then(|p| p.get("connectionProxyEnabled"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let proxy_url = psd
        .and_then(|p| p.get("connectionProxyUrl"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());

    if enabled {
        if let Some(purl) = proxy_url {
            let no_proxy = psd
                .and_then(|p| p.get("connectionNoProxy"))
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty());
            return state.proxy.rotator().get_or_create_client(purl, no_proxy);
        }
    }

    state.proxy.client().clone()
}

async fn refresh_connection_if_needed(
    state: &AppState,
    conn: &ProviderConnection,
    force: bool,
) -> Result<ProviderConnection, String> {
    let latest = state
        .db
        .load_provider_connections()
        .ok()
        .and_then(|list| list.into_iter().find(|c| c.id == conn.id))
        .unwrap_or_else(|| conn.clone());

    if !force && !conn_needs_refresh(&latest) {
        return Ok(latest);
    }

    if extract_refresh_token(&latest).is_none() {
        if conn_access_token(&latest).is_some() && !force {
            return Ok(latest);
        }
        return Err("Failed to refresh credentials. Please re-authorize the connection.".to_string());
    }

    let client = build_usage_http_client(state, &latest);
    let res = refresh_oauth_token(&client, &latest, &state.db).await;
    if res.success {
        if let Ok(list) = state.db.load_provider_connections() {
            if let Some(updated) = list.into_iter().find(|c| c.id == latest.id) {
                return Ok(updated);
            }
        }
        return Ok(latest);
    }

    let err_msg = res.error.unwrap_or_else(|| "Credential refresh failed".to_string());
    let lower = err_msg.to_ascii_lowercase();
    if lower.contains("invalid_grant") || lower.contains("reused") || lower.contains("revoked") {
        return Err("Refresh token invalid or reused. Please re-authorize the connection.".to_string());
    }

    if !force && conn_access_token(&latest).is_some() {
        return Ok(latest);
    }

    Err(err_msg)
}

// ============================================================================
// 1. GET /api/usage/history
// ============================================================================

pub async fn usage_history_handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(resp) = auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let mut stats = match state.db.get_usage_statistics("all") {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("Error fetching usage stats: {}", e);
            return json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({ "error": "Failed to fetch usage stats" }),
            );
        }
    };

    enrich_usage_stats_from_sqlite(&state.db, &mut stats);
    json_response(StatusCode::OK, stats)
}

fn enrich_usage_stats_from_sqlite(db: &Database, stats: &mut Value) {
    let mut conn_names: HashMap<String, String> = HashMap::new();
    if let Ok(conns) = db.load_provider_connections() {
        for c in conns {
            let label = c
                .name
                .clone()
                .or(c.email.clone())
                .unwrap_or_else(|| c.id.clone());
            conn_names.insert(c.id, label);
        }
    }

    let mut node_names: HashMap<String, String> = HashMap::new();
    if let Ok(nodes) = db.load_provider_nodes() {
        for n in nodes {
            if let Some(name) = n.name {
                node_names.insert(n.id, name);
            }
        }
    }

    let mut api_key_names: HashMap<String, String> = HashMap::new();
    if let Ok(keys) = db.list_api_keys() {
        for k in keys {
            if let Some(name) = k.name {
                api_key_names.insert(k.key.trim().to_string(), name);
            }
        }
    }

    let Ok(guard) = db.reader() else {
        return;
    };

    // Update accountName in byAccount
    if let Some(by_acc) = stats.get_mut("byAccount").and_then(|v| v.as_object_mut()) {
        for (_k, entry) in by_acc.iter_mut() {
            if let Some(obj) = entry.as_object_mut() {
                if let Some(cid) = obj.get("connectionId").and_then(|v| v.as_str()) {
                    if let Some(name) = conn_names.get(cid) {
                        obj.insert("accountName".to_string(), json!(name));
                    } else if cid != "default" && cid.len() > 8 {
                        obj.insert(
                            "accountName".to_string(),
                            json!(format!("Account {}...", &cid[..8])),
                        );
                    }
                }
            }
        }
    }

    // Populate byApiKey
    let mut by_api_key = serde_json::Map::new();
    if let Ok(mut stmt) = guard.prepare(
        r#"
        SELECT
            COALESCE(apiKey, ''),
            COALESCE(model, ''),
            COALESCE(provider, 'unknown'),
            COUNT(*),
            COALESCE(SUM(promptTokens), 0),
            COALESCE(SUM(completionTokens), 0),
            COALESCE(SUM(cachedTokens), 0),
            COALESCE(SUM(cost), 0.0),
            MAX(timestamp)
        FROM usageHistory
        GROUP BY apiKey, model, provider
        "#,
    ) {
        if let Ok(rows) = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, i64>(6)?,
                r.get::<_, f64>(7)?,
                r.get::<_, Option<String>>(8)?,
            ))
        }) {
            for (raw_key, model, provider, reqs, pt, ct, cached, cost, last_used) in rows.flatten()
            {
                let prov_display = node_names
                    .get(&provider)
                    .cloned()
                    .unwrap_or_else(|| provider.clone());
                let trimmed = raw_key.trim();
                if trimmed.is_empty() {
                    let entry = by_api_key
                        .entry("local-no-key".to_string())
                        .or_insert_with(|| {
                            json!({
                                "requests": 0,
                                "promptTokens": 0,
                                "completionTokens": 0,
                                "cachedTokens": 0,
                                "cost": 0.0,
                                "rawModel": model,
                                "provider": prov_display,
                                "apiKeyMasked": Value::Null,
                                "keyName": "Local (No API Key)",
                                "apiKeyKey": "local-no-key",
                                "lastUsed": last_used,
                            })
                        });
                    if let Some(obj) = entry.as_object_mut() {
                        let prev_r = obj.get("requests").and_then(|v| v.as_i64()).unwrap_or(0);
                        let prev_p = obj.get("promptTokens").and_then(|v| v.as_i64()).unwrap_or(0);
                        let prev_c = obj.get("completionTokens").and_then(|v| v.as_i64()).unwrap_or(0);
                        let prev_ca = obj.get("cachedTokens").and_then(|v| v.as_i64()).unwrap_or(0);
                        let prev_cost = obj.get("cost").and_then(|v| v.as_f64()).unwrap_or(0.0);
                        obj.insert("requests".to_string(), json!(prev_r + reqs));
                        obj.insert("promptTokens".to_string(), json!(prev_p + pt));
                        obj.insert("completionTokens".to_string(), json!(prev_c + ct));
                        obj.insert("cachedTokens".to_string(), json!(prev_ca + cached));
                        obj.insert("cost".to_string(), json!(prev_cost + cost));
                    }
                } else {
                    let masked = mask_api_key(Some(trimmed));
                    let key_name = api_key_names
                        .get(trimmed)
                        .cloned()
                        .or_else(|| masked.clone())
                        .unwrap_or_else(|| format!("{}...", &trimmed[..trimmed.len().min(8)]));
                    let ak_key = format!("{}|{}|{}", raw_key, model, provider);
                    by_api_key.insert(
                        ak_key,
                        json!({
                            "requests": reqs,
                            "promptTokens": pt,
                            "completionTokens": ct,
                            "cachedTokens": cached,
                            "cost": cost,
                            "rawModel": model,
                            "provider": prov_display,
                            "apiKeyMasked": masked,
                            "keyName": key_name,
                            "apiKeyKey": masked,
                            "lastUsed": last_used,
                        }),
                    );
                }
            }
        }
    }

    // Populate byEndpoint
    let mut by_endpoint = serde_json::Map::new();
    if let Ok(mut stmt) = guard.prepare(
        r#"
        SELECT
            COALESCE(endpoint, 'Unknown'),
            COALESCE(model, ''),
            COALESCE(provider, 'unknown'),
            COUNT(*),
            COALESCE(SUM(promptTokens), 0),
            COALESCE(SUM(completionTokens), 0),
            COALESCE(SUM(cachedTokens), 0),
            COALESCE(SUM(cost), 0.0),
            MAX(timestamp)
        FROM usageHistory
        GROUP BY endpoint, model, provider
        "#,
    ) {
        if let Ok(rows) = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, i64>(6)?,
                r.get::<_, f64>(7)?,
                r.get::<_, Option<String>>(8)?,
            ))
        }) {
            for (ep, model, provider, reqs, pt, ct, cached, cost, last_used) in rows.flatten() {
                let prov_display = node_names
                    .get(&provider)
                    .cloned()
                    .unwrap_or_else(|| provider.clone());
                let ep_key = format!("{}|{}|{}", ep, model, provider);
                by_endpoint.insert(
                    ep_key,
                    json!({
                        "requests": reqs,
                        "promptTokens": pt,
                        "completionTokens": ct,
                        "cachedTokens": cached,
                        "cost": cost,
                        "endpoint": ep,
                        "rawModel": model,
                        "provider": prov_display,
                        "lastUsed": last_used,
                    }),
                );
            }
        }
    }

    // Populate last10Minutes
    let now = Utc::now();
    let now_ms = now.timestamp_millis();
    let cur_min_ms = (now_ms / 60_000) * 60_000;
    let ten_min_ago_ms = cur_min_ms - 9 * 60_000;
    let mut buckets = vec![(0i64, 0i64, 0i64, 0.0f64); 10];
    if let Some(cutoff_dt) = DateTime::from_timestamp_millis(ten_min_ago_ms) {
        let cutoff_iso = cutoff_dt.to_rfc3339();
        if let Ok(mut stmt) = guard.prepare(
            "SELECT timestamp, promptTokens, completionTokens, cost FROM usageHistory WHERE timestamp >= ?",
        ) {
            if let Ok(rows) = stmt.query_map([&cutoff_iso], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, f64>(3)?,
                ))
            }) {
                for (ts, pt, ct, cost) in rows.flatten() {
                    if let Ok(dt) = DateTime::parse_from_rfc3339(&ts) {
                        let ms = dt.timestamp_millis();
                        let min_start = (ms / 60_000) * 60_000;
                        let diff = (min_start - ten_min_ago_ms) / 60_000;
                        if (0..10).contains(&diff) {
                            let idx = diff as usize;
                            buckets[idx].0 += 1;
                            buckets[idx].1 += pt;
                            buckets[idx].2 += ct;
                            buckets[idx].3 += cost;
                        }
                    }
                }
            }
        }
    }
    let last_10_val: Vec<Value> = buckets
        .into_iter()
        .map(|(r, p, c, cost)| {
            json!({
                "requests": r,
                "promptTokens": p,
                "completionTokens": c,
                "cost": cost,
            })
        })
        .collect();

    if let Some(obj) = stats.as_object_mut() {
        obj.insert("byApiKey".to_string(), Value::Object(by_api_key));
        obj.insert("byEndpoint".to_string(), Value::Object(by_endpoint));
        obj.insert("last10Minutes".to_string(), Value::Array(last_10_val));
    }
}

// ============================================================================
// 2. GET /api/usage/api-keys/{id}
// ============================================================================

#[derive(Debug, Deserialize, Default)]
pub struct ApiKeyUsageQuery {
    pub from: Option<String>,
    pub to: Option<String>,
}

fn is_date_only(s: &str) -> bool {
    let bytes = s.as_bytes();
    bytes.len() == 10
        && bytes[0..4].iter().all(u8::is_ascii_digit)
        && bytes[4] == b'-'
        && bytes[5..7].iter().all(u8::is_ascii_digit)
        && bytes[7] == b'-'
        && bytes[8..10].iter().all(u8::is_ascii_digit)
}

fn parse_boundary(value: Option<&str>, end_of_day: bool) -> Result<Option<String>, String> {
    let Some(raw) = value else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

    if is_date_only(trimmed) {
        let date = NaiveDate::parse_from_str(trimmed, "%Y-%m-%d")
            .map_err(|_| "Invalid date range".to_string())?;
        let dt = if end_of_day {
            date.and_hms_milli_opt(23, 59, 59, 999)
        } else {
            date.and_hms_milli_opt(0, 0, 0, 0)
        }
        .ok_or_else(|| "Invalid date range".to_string())?;
        return Ok(Some(
            dt.and_utc().to_rfc3339_opts(SecondsFormat::Millis, true),
        ));
    }

    if let Ok(dt) = DateTime::parse_from_rfc3339(trimmed) {
        return Ok(Some(
            dt.with_timezone(&Utc)
                .to_rfc3339_opts(SecondsFormat::Millis, true),
        ));
    }

    Err("Invalid date range".to_string())
}

pub async fn usage_api_key_by_id_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<ApiKeyUsageQuery>,
) -> Response {
    if let Err(resp) = auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let key = match state.db.get_api_key_by_id(&id) {
        Ok(Some(k)) => k,
        Ok(None) => {
            return json_response(StatusCode::NOT_FOUND, json!({ "error": "Key not found" }));
        }
        Err(e) => {
            return json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({ "error": format!("Failed to fetch API key usage: {}", e) }),
            );
        }
    };

    let from = match parse_boundary(query.from.as_deref(), false) {
        Ok(v) => v,
        Err(msg) => return json_response(StatusCode::BAD_REQUEST, json!({ "error": msg })),
    };
    let to = match parse_boundary(query.to.as_deref(), true) {
        Ok(v) => v,
        Err(msg) => return json_response(StatusCode::BAD_REQUEST, json!({ "error": msg })),
    };

    if let (Some(f), Some(t)) = (&from, &to) {
        if f > t {
            return json_response(
                StatusCode::BAD_REQUEST,
                json!({ "error": "from must be before or equal to to" }),
            );
        }
    }

    match query_api_key_daily_usage(&state.db, &key.key, from.as_deref(), to.as_deref()) {
        Ok(rows) => json_response(
            StatusCode::OK,
            json!({
                "keyId": id,
                "from": from,
                "to": to,
                "rows": rows,
            }),
        ),
        Err(e) => json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({ "error": format!("Failed to fetch API key usage: {}", e) }),
        ),
    }
}

fn query_api_key_daily_usage(
    db: &Database,
    api_key_secret: &str,
    from: Option<&str>,
    to: Option<&str>,
) -> anyhow::Result<Vec<Value>> {
    let guard = db.reader()?;

    let mut conditions = vec!["apiKey = ?".to_string()];
    let mut params: Vec<String> = vec![api_key_secret.to_string()];
    if let Some(f) = from {
        conditions.push("timestamp >= ?".to_string());
        params.push(f.to_string());
    }
    if let Some(t) = to {
        conditions.push("timestamp <= ?".to_string());
        params.push(t.to_string());
    }

    let sql = format!(
        "SELECT substr(timestamp, 1, 10) AS date,
                COUNT(*) AS requests,
                COALESCE(SUM(promptTokens), 0) AS promptTokens,
                COALESCE(SUM(completionTokens), 0) AS completionTokens,
                COALESCE(SUM(cost), 0.0) AS cost
           FROM usageHistory
          WHERE {}
          GROUP BY substr(timestamp, 1, 10)
          ORDER BY date ASC",
        conditions.join(" AND ")
    );

    let mut out = Vec::new();
    if let Ok(mut stmt) = guard.prepare(&sql) {
        let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, f64>(4)?,
            ))
        })?;
        for row in rows.flatten() {
            out.push(json!({
                "day": row.0,
                "requests": row.1,
                "tokensIn": row.2,
                "tokensOut": row.3,
                "costUsd": row.4,
            }));
        }
    }

    if !out.is_empty() {
        return Ok(out);
    }

    // Fallback to usageDaily rollups when history is empty / buffered
    let from_day = from.map(|s| &s[..s.len().min(10)]);
    let to_day = to.map(|s| &s[..s.len().min(10)]);
    if let Ok(mut stmt) = guard.prepare("SELECT dateKey, data FROM usageDaily ORDER BY dateKey ASC")
    {
        if let Ok(rows) = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        {
            for (date_key, data_str) in rows.flatten() {
                if let Some(fd) = from_day {
                    if date_key.as_str() < fd {
                        continue;
                    }
                }
                if let Some(td) = to_day {
                    if date_key.as_str() > td {
                        continue;
                    }
                }
                if let Ok(parsed) = serde_json::from_str::<Value>(&data_str) {
                    if let Some(by_ak) = parsed.get("byApiKey").and_then(|v| v.as_object()) {
                        let mut requests = 0i64;
                        let mut tokens_in = 0i64;
                        let mut tokens_out = 0i64;
                        let mut cost_usd = 0.0f64;
                        for ak in by_ak.values() {
                            if ak.get("apiKey").and_then(|v| v.as_str()) == Some(api_key_secret) {
                                requests +=
                                    ak.get("requests").and_then(|v| v.as_i64()).unwrap_or(0);
                                tokens_in += ak
                                    .get("promptTokens")
                                    .and_then(|v| v.as_i64())
                                    .unwrap_or(0);
                                tokens_out += ak
                                    .get("completionTokens")
                                    .and_then(|v| v.as_i64())
                                    .unwrap_or(0);
                                cost_usd += ak.get("cost").and_then(|v| v.as_f64()).unwrap_or(0.0);
                            }
                        }
                        if requests > 0 || tokens_in > 0 || tokens_out > 0 {
                            out.push(json!({
                                "day": date_key,
                                "requests": requests,
                                "tokensIn": tokens_in,
                                "tokensOut": tokens_out,
                                "costUsd": cost_usd,
                            }));
                        }
                    }
                }
            }
        }
    }

    Ok(out)
}

// ============================================================================
// 3. GET /api/usage/{connectionId}
// ============================================================================

#[derive(Debug, Deserialize, Default)]
pub struct ConnectionUsageQuery {
    pub force: Option<String>,
}

pub async fn usage_connection_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(connection_id): Path<String>,
    Query(query): Query<ConnectionUsageQuery>,
) -> Response {
    if let Err(resp) = auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let force = query.force.as_deref() == Some("1");
    let mut connection = match state
        .db
        .load_provider_connections()
        .ok()
        .and_then(|list| list.into_iter().find(|c| c.id == connection_id))
    {
        Some(c) => c,
        None => {
            return json_response(
                StatusCode::NOT_FOUND,
                json!({ "error": "Connection not found" }),
            );
        }
    };

    let is_oauth = connection.auth_type == "oauth";
    let is_apikey_auth = connection.auth_type == "apikey" || connection.auth_type == "api_key";
    let is_apikey_eligible = is_apikey_auth && USAGE_APIKEY_PROVIDERS.contains(&connection.provider.as_str());
    let is_lease = connection.auth_type == "mcode-lease";

    if !is_oauth && !is_apikey_eligible && !is_lease {
        return json_response(
            StatusCode::OK,
            json!({ "message": "Usage not available for this connection" }),
        );
    }

    if is_oauth {
        match refresh_connection_if_needed(&state, &connection, false).await {
            Ok(updated) => connection = updated,
            Err(err) => {
                return json_response(
                    StatusCode::UNAUTHORIZED,
                    json!({ "error": format!("Credential refresh failed: {}", err) }),
                );
            }
        }
    }

    let mut usage = match fetch_usage_for_provider(&state, &connection, force).await {
        Ok(u) => u,
        Err(err) => {
            tracing::warn!("[Usage] {}: {}", connection.provider, err);
            return json_response(StatusCode::INTERNAL_SERVER_ERROR, json!({ "error": err }));
        }
    };

    if is_oauth && is_auth_expired_usage(&usage) && extract_refresh_token(&connection).is_some() {
        if let Ok(updated) = refresh_connection_if_needed(&state, &connection, true).await {
            connection = updated;
            if let Ok(retried) = fetch_usage_for_provider(&state, &connection, force).await {
                usage = retried;
            }
        }
    }

    json_response(StatusCode::OK, usage)
}

async fn fetch_usage_for_provider(
    state: &AppState,
    conn: &ProviderConnection,
    force: bool,
) -> Result<Value, String> {
    let client = build_usage_http_client(state, conn);
    let access_token = conn_access_token(conn);
    let api_key = conn_api_key(conn);

    match conn.provider.as_str() {
        "claude" => fetch_claude_usage(&client, access_token.as_deref(), force).await,
        "codex" => fetch_codex_usage(&client, access_token.as_deref()).await,
        "github" => fetch_github_usage(&client, access_token.as_deref()).await,
        "gemini-cli" => fetch_gemini_cli_usage(&client, conn, access_token.as_deref()).await,
        "antigravity" => fetch_antigravity_usage(&client, access_token.as_deref()).await,
        "kiro" => fetch_kiro_usage(&client, conn, access_token.as_deref()).await,
        "glm" | "glm-cn" => {
            let tok = api_key.as_deref().or(access_token.as_deref());
            fetch_glm_usage(&client, &conn.provider, tok).await
        }
        "minimax" | "minimax-cn" => {
            fetch_minimax_usage(&client, &conn.provider, api_key.as_deref()).await
        }
        "deepseek" => fetch_deepseek_usage(&client, api_key.as_deref()).await,
        "groq" => fetch_groq_usage(&client, api_key.as_deref()).await,
        "kimi" => fetch_kimi_usage(&client, conn, access_token.as_deref(), api_key.as_deref()).await,
        "opencode-go" | "opencode-zen" => {
            fetch_opencode_usage(&client, &conn.provider, api_key.as_deref()).await
        }
        "ollama" => fetch_ollama_usage(&client, api_key.as_deref()).await,
        "vercel-ai-gateway" => fetch_vercel_ai_gateway_usage(&client, api_key.as_deref()).await,
        "clinepass" => {
            fetch_clinepass_usage(&client, access_token.as_deref(), api_key.as_deref()).await
        }
        "codebuddy-cn" | "codebuddy-intl" => {
            let tok = access_token.as_deref().or(api_key.as_deref());
            fetch_codebuddy_usage(&client, &conn.provider, tok).await
        }
        "qoder" | "qoder-cn" => {
            let tok = access_token.as_deref().or(api_key.as_deref());
            fetch_qoder_usage(&client, &conn.provider, tok).await
        }
        "synthetic" => fetch_synthetic_usage(&client, api_key.as_deref()).await,
        "zed" => fetch_zed_usage(&client, conn, access_token.as_deref()).await,
        "cursor" => fetch_cursor_usage(&client, conn, access_token.as_deref()).await,
        "grok-cli" => fetch_grok_cli_usage(&client, conn, access_token.as_deref()).await,
        "commandcode" => fetch_commandcode_usage(&client, api_key.as_deref()).await,
        "xiaomi-mimo" => {
            let tok = access_token.as_deref().or(api_key.as_deref());
            fetch_xiaomi_mimo_usage(&client, tok).await
        }
        "iflow" => Ok(json!({ "message": "iFlow connected. Usage tracked per request." })),
        "minimax-code" | "minimax-code-global" => Ok(json!({
            "message": format!("{} connected. Account credits are tracked via desktop lease or session.", conn.provider),
            "quotas": {}
        })),
        other => Ok(json!({ "message": format!("Usage API not implemented for {}", other) })),
    }
}

// ---- Provider usage fetchers & parsers ----

pub fn parse_claude_reset_grants(block: Option<&Value>) -> Value {
    let Some(obj) = block.and_then(|v| v.as_object()) else {
        return Value::Null;
    };
    if obj.get("eligible").and_then(|v| v.as_bool()) != Some(true) {
        return Value::Null;
    }
    let Some(raw_grants) = obj.get("grants").and_then(|v| v.as_array()) else {
        return Value::Null;
    };

    let active_grants: Vec<&Value> = raw_grants
        .iter()
        .filter(|g| {
            g.get("id").and_then(|v| v.as_str()).is_some()
                && g.get("paused").and_then(|v| v.as_bool()) != Some(true)
                && to_finite_f64(g.get("resets_left"), 0.0) > 0.0
        })
        .collect();

    let next_id = obj.get("next_grant_id").and_then(|v| v.as_str());
    let next = next_id
        .and_then(|nid| {
            active_grants
                .iter()
                .find(|g| g.get("id").and_then(|v| v.as_str()) == Some(nid))
                .copied()
        })
        .or_else(|| active_grants.first().copied());

    let available_count: f64 = active_grants
        .iter()
        .map(|g| to_finite_f64(g.get("resets_left"), 0.0))
        .sum();

    let mapped_grants: Vec<Value> = raw_grants
        .iter()
        .filter_map(|g| {
            let id = g.get("id").and_then(|v| v.as_str())?;
            Some(json!({
                "id": id,
                "label": g.get("label").and_then(|v| v.as_str()).unwrap_or(""),
                "resetsLeft": to_finite_f64(g.get("resets_left"), 0.0),
                "resetsTotal": to_finite_f64(g.get("resets_total"), 0.0),
                "startsAt": g.get("starts_at").cloned().unwrap_or(Value::Null),
                "endsAt": g.get("ends_at").cloned().unwrap_or(Value::Null),
                "clears": g.get("clears").and_then(|v| v.as_array()).cloned().unwrap_or_default(),
                "paused": g.get("paused").and_then(|v| v.as_bool()) == Some(true),
                "usableNow": g.get("usable_now").and_then(|v| v.as_bool()) == Some(true),
                "useRequiresLimit": g.get("use_requires_limit").and_then(|v| v.as_bool()) != Some(false),
            }))
        })
        .collect();

    json!({
        "availableCount": available_count,
        "nextGrantId": next.and_then(|g| g.get("id")).cloned().unwrap_or(Value::Null),
        "expiresAt": next.and_then(|g| g.get("ends_at")).cloned().unwrap_or(Value::Null),
        "clears": next.and_then(|g| g.get("clears")).and_then(|v| v.as_array()).cloned().unwrap_or_default(),
        "cooldownUntil": obj.get("cooldown_until").cloned().unwrap_or(Value::Null),
        "weeklyResetsAt": obj.get("weekly_resets_at").cloned().unwrap_or(Value::Null),
        "grants": mapped_grants,
    })
}

pub fn parse_claude_usage_payload(data: &Value) -> Value {
    let mut quotas = serde_json::Map::new();
    let create_quota = |used: f64, resets_at: Option<&Value>| {
        let remaining = (100.0 - used).max(0.0);
        json!({
            "used": used,
            "total": 100,
            "remaining": remaining,
            "remainingPercentage": remaining,
            "resetAt": parse_reset_time(resets_at),
            "unlimited": false,
        })
    };

    if let Some(w) = data.get("five_hour").and_then(|v| v.as_object()) {
        if let Some(u) = w.get("utilization").and_then(|v| v.as_f64()) {
            quotas.insert(
                "session (5h)".to_string(),
                create_quota(u, w.get("resets_at")),
            );
        }
    }
    if let Some(w) = data.get("seven_day").and_then(|v| v.as_object()) {
        if let Some(u) = w.get("utilization").and_then(|v| v.as_f64()) {
            quotas.insert(
                "weekly (7d)".to_string(),
                create_quota(u, w.get("resets_at")),
            );
        }
    }
    if let Some(obj) = data.as_object() {
        for (k, v) in obj {
            if let Some(model_name) = k.strip_prefix("seven_day_") {
                if let Some(u) = v.get("utilization").and_then(|x| x.as_f64()) {
                    quotas.insert(
                        format!("weekly {} (7d)", model_name),
                        create_quota(u, v.get("resets_at")),
                    );
                }
            }
        }
    }
    if let Some(limits) = data.get("limits").and_then(|v| v.as_array()) {
        for limit in limits {
            if limit.get("kind").and_then(|v| v.as_str()) != Some("weekly_scoped") {
                continue;
            }
            let model_name = limit
                .get("scope")
                .and_then(|s| s.get("model"))
                .and_then(|m| m.get("display_name"))
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_ascii_lowercase())
                .unwrap_or_default();
            if model_name.is_empty() {
                continue;
            }
            if let Some(pct) = limit.get("percent").and_then(|v| v.as_f64()) {
                quotas.insert(
                    format!("weekly {} (7d)", model_name),
                    create_quota(pct.clamp(0.0, 100.0), limit.get("resets_at")),
                );
            }
        }
    }

    json!({
        "plan": "Claude Code",
        "extraUsage": data.get("extra_usage").cloned().unwrap_or(Value::Null),
        "resetCredits": parse_claude_reset_grants(data.get("cedar_ember")),
        "quotas": Value::Object(quotas),
    })
}

async fn fetch_claude_usage(
    client: &reqwest::Client,
    access_token: Option<&str>,
    force: bool,
) -> Result<Value, String> {
    let Some(token) = access_token.filter(|s| !s.is_empty()) else {
        return Ok(json!({ "message": "Claude connected. Access token not available." }));
    };

    if !force {
        if let Some(hit) = claude_usage_cache().get(token) {
            if hit.expires_at > Instant::now() {
                return Ok(hit.result.clone());
            }
        }
    }

    let stale = if !force {
        claude_usage_cache()
            .get(token)
            .map(|h| h.result.clone())
    } else {
        None
    };

    let on_cooldown = claude_429_cooldown_map()
        .get(token)
        .map(|until| Instant::now() < *until)
        .unwrap_or(false);

    if !on_cooldown {
        let resp = client
            .get("https://api.anthropic.com/api/oauth/usage?cedar_ember=1")
            .header(header::AUTHORIZATION, format!("Bearer {}", token))
            .header("anthropic-beta", "oauth-2025-04-20")
            .header("anthropic-version", "2023-06-01")
            .header(header::USER_AGENT, "claude-cli/2.1.322 (external, cli)")
            .timeout(Duration::from_secs(12))
            .send()
            .await;

        if let Ok(r) = resp {
            if r.status().is_success() {
                if let Ok(data) = r.json::<Value>().await {
                    let parsed = parse_claude_usage_payload(&data);
                    claude_usage_cache().insert(
                        token.to_string(),
                        CachedClaudeUsage {
                            result: parsed.clone(),
                            expires_at: Instant::now() + CLAUDE_CACHE_TTL,
                        },
                    );
                    return Ok(parsed);
                }
            } else if r.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
                claude_429_cooldown_map()
                    .insert(token.to_string(), Instant::now() + CLAUDE_429_COOLDOWN);
            }
        }
    }

    // Legacy fallback
    let settings_res = client
        .get("https://api.anthropic.com/v1/settings")
        .header(header::AUTHORIZATION, format!("Bearer {}", token))
        .header("anthropic-version", "2023-06-01")
        .timeout(Duration::from_secs(10))
        .send()
        .await;

    if let Ok(sr) = settings_res {
        if sr.status().is_success() {
            if let Ok(settings) = sr.json::<Value>().await {
                let plan = settings
                    .get("plan")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Unknown");
                let org_name = settings.get("organization_name").cloned().unwrap_or(Value::Null);
                if let Some(org_id) = settings.get("organization_id").and_then(|v| v.as_str()) {
                    let u_url = format!(
                        "https://api.anthropic.com/v1/organizations/{}/usage",
                        org_id
                    );
                    if let Ok(ur) = client
                        .get(&u_url)
                        .header(header::AUTHORIZATION, format!("Bearer {}", token))
                        .header("anthropic-version", "2023-06-01")
                        .timeout(Duration::from_secs(10))
                        .send()
                        .await
                    {
                        if ur.status().is_success() {
                            if let Ok(usage) = ur.json::<Value>().await {
                                let res = json!({
                                    "plan": plan,
                                    "organization": org_name,
                                    "quotas": usage,
                                });
                                claude_usage_cache().insert(
                                    token.to_string(),
                                    CachedClaudeUsage {
                                        result: res.clone(),
                                        expires_at: Instant::now() + CLAUDE_CACHE_TTL,
                                    },
                                );
                                return Ok(res);
                            }
                        }
                    }
                }
                return Ok(stale.unwrap_or_else(|| {
                    json!({
                        "plan": plan,
                        "organization": org_name,
                        "message": "Claude connected. Usage details require admin access.",
                    })
                }));
            }
        }
    }

    Ok(stale.unwrap_or_else(|| {
        json!({ "message": "Claude connected. Usage API requires admin permissions." })
    }))
}

pub fn parse_codex_usage_payload(data: &Value) -> Value {
    let format_window = |w: &Value| {
        let used = to_finite_f64(
            w.get("used_percent").or_else(|| w.get("percent_used")),
            0.0,
        )
        .clamp(0.0, 100.0);
        let reset = w
            .get("reset_at")
            .or_else(|| w.get("resets_at"))
            .or_else(|| w.get("resetAt"));
        json!({
            "used": used,
            "total": 100,
            "remaining": (100.0 - used).max(0.0),
            "resetAt": parse_reset_time(reset),
            "unlimited": false,
        })
    };

    let append_windows = |quotas: &mut serde_json::Map<String, Value>, prefix: &str, snap: Option<&Value>| {
        let Some(snap) = snap else { return };
        let rl = snap
            .get("rate_limit")
            .filter(|v| v.is_object())
            .unwrap_or(snap);
        if let Some(primary) = rl
            .get("primary_window")
            .or_else(|| rl.get("primary"))
            .or_else(|| snap.get("primary_window"))
            .or_else(|| snap.get("primary"))
        {
            let key = if prefix.is_empty() {
                "session".to_string()
            } else {
                format!("{}_session", prefix)
            };
            quotas.insert(key, format_window(primary));
        }
        if let Some(secondary) = rl
            .get("secondary_window")
            .or_else(|| rl.get("secondary"))
            .or_else(|| snap.get("secondary_window"))
            .or_else(|| snap.get("secondary"))
        {
            let key = if prefix.is_empty() {
                "weekly".to_string()
            } else {
                format!("{}_weekly", prefix)
            };
            quotas.insert(key, format_window(secondary));
        }
    };

    let normal_rl = data
        .get("rate_limit")
        .or_else(|| data.get("rate_limits"))
        .or_else(|| {
            data.get("rate_limits_by_limit_id")
                .and_then(|m| m.get("codex"))
        });

    let review_rl = data
        .get("code_review_rate_limit")
        .or_else(|| data.get("review_rate_limit"))
        .or_else(|| {
            data.get("rate_limits_by_limit_id").and_then(|m| {
                m.get("code_review")
                    .or_else(|| m.get("codex_review"))
                    .or_else(|| m.get("review"))
            })
        })
        .or_else(|| {
            data.get("additional_rate_limits")
                .and_then(|a| a.as_array())
                .and_then(|arr| {
                    arr.iter().find(|e| {
                        let id = e
                            .get("limit_name")
                            .or_else(|| e.get("metered_feature"))
                            .or_else(|| e.get("id"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_ascii_lowercase();
                        id.contains("review")
                    })
                })
        });

    let spark_rl = data
        .get("spark_rate_limit")
        .or_else(|| data.get("gpt_5_3_codex_spark_rate_limit"))
        .or_else(|| {
            data.get("rate_limits_by_limit_id").and_then(|m| {
                m.get("gpt-5.3-codex-spark")
                    .or_else(|| m.get("gpt_5_3_codex_spark"))
                    .or_else(|| m.get("spark"))
            })
        })
        .or_else(|| {
            data.get("additional_rate_limits")
                .and_then(|a| a.as_array())
                .and_then(|arr| {
                    arr.iter().find(|e| {
                        let id = e
                            .get("limit_name")
                            .or_else(|| e.get("metered_feature"))
                            .or_else(|| e.get("id"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_ascii_lowercase();
                        id.contains("spark")
                    })
                })
        });

    let mut quotas = serde_json::Map::new();
    append_windows(&mut quotas, "", normal_rl);
    append_windows(&mut quotas, "review", review_rl);
    append_windows(&mut quotas, "spark", spark_rl);

    let limit_reached = |snap: Option<&Value>| {
        snap.and_then(|s| s.get("rate_limit").unwrap_or(s).get("limit_reached"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    };

    let available_reset_credits = to_finite_f64(
        data.get("rate_limit_reset_credits")
            .and_then(|r| r.get("available_count")),
        0.0,
    )
    .max(0.0);

    let plan = data
        .get("plan_type")
        .or_else(|| data.get("summary").and_then(|s| s.get("plan")))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    json!({
        "plan": plan,
        "limitReached": limit_reached(normal_rl),
        "reviewLimitReached": limit_reached(review_rl),
        "sparkLimitReached": limit_reached(spark_rl),
        "resetCredits": { "availableCount": available_reset_credits },
        "quotas": Value::Object(quotas),
    })
}

async fn fetch_codex_usage(
    client: &reqwest::Client,
    access_token: Option<&str>,
) -> Result<Value, String> {
    let Some(token) = access_token.filter(|s| !s.is_empty()) else {
        return Err("Failed to fetch Codex usage: No access token available.".to_string());
    };

    let resp = client
        .get("https://chatgpt.com/backend-api/wham/usage")
        .header(header::AUTHORIZATION, format!("Bearer {}", token))
        .header(header::ACCEPT, "application/json")
        .timeout(Duration::from_secs(12))
        .send()
        .await
        .map_err(|e| format!("Failed to fetch Codex usage: {}", e))?;

    if !resp.status().is_success() {
        return Ok(json!({
            "message": format!("Codex connected. Usage API temporarily unavailable ({}).", resp.status().as_u16())
        }));
    }

    let data: Value = resp
        .json()
        .await
        .map_err(|e| format!("Failed to fetch Codex usage: {}", e))?;
    Ok(parse_codex_usage_payload(&data))
}

async fn fetch_github_usage(
    client: &reqwest::Client,
    access_token: Option<&str>,
) -> Result<Value, String> {
    let Some(token) = access_token.filter(|s| !s.is_empty()) else {
        return Err("Failed to fetch GitHub usage: No GitHub access token available. Please re-authorize the connection.".to_string());
    };

    let resp = client
        .get("https://api.github.com/copilot_internal/user")
        .header(header::AUTHORIZATION, format!("token {}", token))
        .header(header::ACCEPT, "application/json")
        .header("X-GitHub-Api-Version", "2025-04-01")
        .header(header::USER_AGENT, "GitHubCopilotChat/0.38.0")
        .header("Editor-Version", "vscode/1.100.0")
        .header("Editor-Plugin-Version", "copilot-chat/0.26.7")
        .timeout(Duration::from_secs(12))
        .send()
        .await
        .map_err(|e| format!("Failed to fetch GitHub usage: {}", e))?;

    if !resp.status().is_success() {
        let err = resp.text().await.unwrap_or_default();
        return Err(format!("Failed to fetch GitHub usage: GitHub API error: {}", err));
    }

    let data: Value = resp
        .json()
        .await
        .map_err(|e| format!("Failed to fetch GitHub usage: {}", e))?;

    if let Some(snaps) = data.get("quota_snapshots").and_then(|v| v.as_object()) {
        let reset_at = parse_reset_time(data.get("quota_reset_date"));
        let fmt_snap = |q: Option<&Value>| {
            let Some(obj) = q.and_then(|v| v.as_object()) else {
                return json!({ "used": 0, "total": 0, "unlimited": true, "resetAt": reset_at });
            };
            let entitlement = obj.get("entitlement").and_then(|v| v.as_i64()).unwrap_or(0);
            let remaining = obj.get("remaining").and_then(|v| v.as_i64()).unwrap_or(0);
            let unlimited = obj.get("unlimited").and_then(|v| v.as_bool()).unwrap_or(false);
            json!({
                "used": entitlement - remaining,
                "total": entitlement,
                "remaining": remaining,
                "unlimited": unlimited,
                "resetAt": reset_at,
            })
        };
        return Ok(json!({
            "plan": data.get("copilot_plan").cloned().unwrap_or(Value::Null),
            "resetDate": data.get("quota_reset_date").cloned().unwrap_or(Value::Null),
            "quotas": {
                "chat": fmt_snap(snaps.get("chat")),
                "completions": fmt_snap(snaps.get("completions")),
                "premium_interactions": fmt_snap(snaps.get("premium_interactions")),
            }
        }));
    }

    if data.get("monthly_quotas").is_some() || data.get("limited_user_quotas").is_some() {
        let reset_at = parse_reset_time(data.get("limited_user_reset_date"));
        let mq = data.get("monthly_quotas");
        let uq = data.get("limited_user_quotas");
        return Ok(json!({
            "plan": data.get("copilot_plan").or_else(|| data.get("access_type_sku")).cloned().unwrap_or(Value::Null),
            "resetDate": data.get("limited_user_reset_date").cloned().unwrap_or(Value::Null),
            "quotas": {
                "chat": {
                    "used": uq.and_then(|v| v.get("chat")).and_then(|v| v.as_i64()).unwrap_or(0),
                    "total": mq.and_then(|v| v.get("chat")).and_then(|v| v.as_i64()).unwrap_or(0),
                    "unlimited": false,
                    "resetAt": reset_at,
                },
                "completions": {
                    "used": uq.and_then(|v| v.get("completions")).and_then(|v| v.as_i64()).unwrap_or(0),
                    "total": mq.and_then(|v| v.get("completions")).and_then(|v| v.as_i64()).unwrap_or(0),
                    "unlimited": false,
                    "resetAt": reset_at,
                }
            }
        }));
    }

    Ok(json!({ "message": "GitHub Copilot connected. Unable to parse quota data." }))
}

async fn fetch_gemini_cli_usage(
    client: &reqwest::Client,
    conn: &ProviderConnection,
    access_token: Option<&str>,
) -> Result<Value, String> {
    let Some(token) = access_token.filter(|s| !s.is_empty()) else {
        return Ok(json!({ "plan": "Free", "message": "Gemini CLI access token not available." }));
    };

    let mut project_id = conn_str(conn, "projectId").map(ToString::to_string);
    let mut plan = "Free".to_string();

    if project_id.is_none() {
        if let Ok(res) = client
            .post("https://cloudcode-pa.googleapis.com/v1internal:loadCodeAssist")
            .header(header::AUTHORIZATION, format!("Bearer {}", token))
            .json(&json!({ "metadata": { "ideType": "IDE_UNSPECIFIED", "platform": "PLATFORM_UNSPECIFIED", "pluginType": "GEMINI" } }))
            .timeout(Duration::from_secs(10))
            .send()
            .await
        {
            if res.status().is_success() {
                if let Ok(info) = res.json::<Value>().await {
                    if let Some(p) = info.get("cloudaicompanionProject").and_then(|v| v.as_str()) {
                        if !p.trim().is_empty() {
                            project_id = Some(p.trim().to_string());
                        }
                    } else if let Some(p) = info
                        .get("cloudaicompanionProject")
                        .and_then(|v| v.get("id"))
                        .and_then(|v| v.as_str())
                    {
                        if !p.trim().is_empty() {
                            project_id = Some(p.trim().to_string());
                        }
                    }
                    if let Some(tier) = info
                        .get("currentTier")
                        .and_then(|t| t.get("name"))
                        .and_then(|v| v.as_str())
                    {
                        plan = tier.to_string();
                    }
                }
            }
        }
    }

    let Some(pid) = project_id else {
        return Ok(json!({
            "plan": plan,
            "message": "Gemini CLI project ID not available. Reconnect Gemini CLI, or configure a Google Cloud project with Gemini Code Assist access before checking quota."
        }));
    };

    let resp = match client
        .post("https://cloudcode-pa.googleapis.com/v1internal:retrieveUserQuota")
        .header(header::AUTHORIZATION, format!("Bearer {}", token))
        .json(&json!({ "project": pid }))
        .timeout(Duration::from_secs(10))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return Ok(json!({ "message": format!("Gemini CLI error: {}", e) })),
    };

    if !resp.status().is_success() {
        return Ok(json!({
            "plan": plan,
            "message": format!("Gemini CLI quota error ({}).", resp.status().as_u16())
        }));
    }

    let data: Value = resp.json().await.unwrap_or_default();
    let mut quotas = serde_json::Map::new();
    if let Some(buckets) = data.get("buckets").and_then(|v| v.as_array()) {
        for b in buckets {
            let Some(model_id) = b.get("modelId").and_then(|v| v.as_str()) else {
                continue;
            };
            if b.get("remainingFraction").is_none() || b.get("remainingFraction") == Some(&Value::Null) {
                continue;
            }
            let frac = to_finite_f64(b.get("remainingFraction"), 0.0);
            let total = 1000i64;
            let remaining = (total as f64 * frac).round() as i64;
            let used = (total - remaining).max(0);
            quotas.insert(
                model_id.to_string(),
                json!({
                    "used": used,
                    "total": total,
                    "resetAt": parse_reset_time(b.get("resetTime")),
                    "remainingPercentage": frac * 100.0,
                    "unlimited": false,
                }),
            );
        }
    }

    Ok(json!({ "plan": plan, "quotas": Value::Object(quotas) }))
}

async fn fetch_antigravity_usage(
    client: &reqwest::Client,
    access_token: Option<&str>,
) -> Result<Value, String> {
    let Some(token) = access_token.filter(|s| !s.is_empty()) else {
        return Ok(json!({ "message": "Antigravity access token not available.", "quotas": {} }));
    };

    let resp = match client
        .post("https://daily-cloudcode-pa.googleapis.com/v1internal:fetchAvailableModels")
        .header(header::AUTHORIZATION, format!("Bearer {}", token))
        .header(header::USER_AGENT, "antigravity/hub/2.24.0 darwin/arm64")
        .header("X-Client-Name", "antigravity")
        .json(&json!({}))
        .timeout(Duration::from_secs(10))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return Ok(json!({ "message": format!("Antigravity quota error: {}", e), "quotas": {} })),
    };

    if resp.status() == reqwest::StatusCode::FORBIDDEN {
        return Ok(json!({
            "message": "Antigravity quota API access forbidden. Chat may still work.",
            "quotas": {}
        }));
    }
    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Ok(json!({
            "message": "Antigravity quota API authentication expired. Chat may still work.",
            "quotas": {}
        }));
    }
    if !resp.status().is_success() {
        return Ok(json!({
            "message": format!("Antigravity quota API returned {}.", resp.status().as_u16()),
            "quotas": {}
        }));
    }

    let data: Value = resp.json().await.unwrap_or_default();
    let mut quotas = serde_json::Map::new();
    if let Some(models) = data.get("models").and_then(|v| v.as_object()) {
        for (model_id, info) in models {
            if let Some(qi) = info.get("quotaInfo") {
                let frac = to_finite_f64(qi.get("remainingFraction"), 1.0);
                let total = 1000i64;
                let remaining = (total as f64 * frac).round() as i64;
                let used = (total - remaining).max(0);
                quotas.insert(
                    model_id.clone(),
                    json!({
                        "used": used,
                        "total": total,
                        "remainingPercentage": frac * 100.0,
                        "resetAt": parse_reset_time(qi.get("resetTime")),
                        "unlimited": false,
                    }),
                );
            }
        }
    }

    Ok(json!({ "plan": "Antigravity", "quotas": Value::Object(quotas) }))
}

async fn fetch_kiro_usage(
    client: &reqwest::Client,
    conn: &ProviderConnection,
    access_token: Option<&str>,
) -> Result<Value, String> {
    let Some(token) = access_token.filter(|s| !s.is_empty()) else {
        return Ok(json!({ "message": "Kiro access token not available.", "quotas": {} }));
    };

    let url = "https://codewhisperer.us-east-1.amazonaws.com/getUsageLimits?isEmailRequired=true&origin=AI_EDITOR&resourceType=AGENTIC_REQUEST";
    let resp = match client
        .get(url)
        .header(header::AUTHORIZATION, format!("Bearer {}", token))
        .header(header::ACCEPT, "application/json")
        .header("x-amz-user-agent", "aws-sdk-js/1.0.0 KiroIDE")
        .header(header::USER_AGENT, "aws-sdk-js/1.0.0 KiroIDE")
        .timeout(Duration::from_secs(10))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return Ok(json!({ "message": format!("Kiro usage error: {}", e), "quotas": {} })),
    };

    if !resp.status().is_success() {
        let _ = conn;
        return Ok(json!({
            "message": format!("Kiro usage API returned {}.", resp.status().as_u16()),
            "quotas": {}
        }));
    }

    let data: Value = resp.json().await.unwrap_or_default();
    let reset_at = parse_reset_time(data.get("nextDateReset").or_else(|| data.get("resetDate")));
    let mut quotas = serde_json::Map::new();
    if let Some(list) = data.get("usageBreakdownList").and_then(|v| v.as_array()) {
        for item in list {
            let name = item
                .get("displayName")
                .or_else(|| item.get("resourceType"))
                .and_then(|v| v.as_str())
                .unwrap_or("Credits");
            let used = to_finite_f64(item.get("currentUsageWithPrecision").or_else(|| item.get("currentUsage")), 0.0);
            let total = to_finite_f64(item.get("usageLimitWithPrecision").or_else(|| item.get("usageLimit")), 0.0);
            let remaining = (total - used).max(0.0);
            let rem_pct = if total > 0.0 { (remaining / total) * 100.0 } else { 0.0 };
            quotas.insert(
                name.to_string(),
                json!({
                    "used": used,
                    "total": total,
                    "remainingPercentage": rem_pct,
                    "resetAt": reset_at,
                    "unlimited": false,
                }),
            );
        }
    }

    let plan = data
        .get("subscriptionInfo")
        .and_then(|s| s.get("subscriptionTitle"))
        .and_then(|v| v.as_str())
        .unwrap_or("Kiro");
    Ok(json!({ "plan": plan, "quotas": Value::Object(quotas) }))
}

pub fn parse_glm_quota_response(payload: &Value) -> Value {
    let data = payload.get("data").and_then(|v| v.as_object());
    let limits = data
        .and_then(|d| d.get("limits"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut quotas = serde_json::Map::new();
    for limit in limits {
        let Some(ltype) = limit.get("type").and_then(|v| v.as_str()) else {
            continue;
        };
        if ltype != "TOKENS_LIMIT" && ltype != "CREDIT_LIMIT" {
            continue;
        }
        let used_pct = to_finite_f64(limit.get("percentage"), 0.0);
        let reset_ms = limit.get("nextResetTime").and_then(|v| v.as_i64()).unwrap_or(0);
        let remaining = (100.0 - used_pct).max(0.0);
        let unit = limit.get("unit").and_then(|v| v.as_i64()).unwrap_or(0);
        let number = limit.get("number").and_then(|v| v.as_i64()).unwrap_or(0);
        let key = if unit == 3 {
            format!("Session ({}h)", number)
        } else if unit == 6 {
            "Weekly (7d)".to_string()
        } else if ltype == "TOKENS_LIMIT" {
            "Tokens".to_string()
        } else {
            format!("Limit ({})", number)
        };
        let reset_at = if reset_ms > 0 {
            DateTime::from_timestamp_millis(reset_ms)
                .map(|dt| dt.to_rfc3339_opts(SecondsFormat::Millis, true))
        } else {
            None
        };
        quotas.insert(
            key,
            json!({
                "used": used_pct,
                "total": 100,
                "remaining": remaining,
                "remainingPercentage": remaining,
                "resetAt": reset_at,
                "unlimited": false,
            }),
        );
    }

    let level_raw = data
        .and_then(|d| d.get("level"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let plan = if level_raw.is_empty() {
        "Unknown".to_string()
    } else {
        let mut chars = level_raw.chars();
        match chars.next() {
            Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str().to_lowercase()),
            None => "Unknown".to_string(),
        }
    };

    json!({ "plan": plan, "quotas": Value::Object(quotas) })
}

async fn fetch_glm_usage(
    client: &reqwest::Client,
    provider: &str,
    token: Option<&str>,
) -> Result<Value, String> {
    let Some(tok) = token.filter(|s| !s.is_empty()) else {
        return Ok(json!({ "message": "GLM API key not available." }));
    };
    let url = if provider == "glm-cn" {
        "https://open.bigmodel.cn/api/monitor/usage/quota/limit"
    } else {
        "https://api.z.ai/api/monitor/usage/quota/limit"
    };

    let resp = match client
        .get(url)
        .header(header::AUTHORIZATION, format!("Bearer {}", tok))
        .header(header::ACCEPT, "application/json")
        .timeout(Duration::from_secs(10))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return Ok(json!({ "message": format!("GLM usage error: {}", e) })),
    };

    if !resp.status().is_success() {
        return Ok(json!({
            "message": format!("GLM usage API error ({}).", resp.status().as_u16())
        }));
    }
    let body: Value = resp.json().await.unwrap_or_default();
    Ok(parse_glm_quota_response(&body))
}

async fn fetch_minimax_usage(
    client: &reqwest::Client,
    provider: &str,
    api_key: Option<&str>,
) -> Result<Value, String> {
    let Some(key) = api_key.filter(|s| !s.is_empty()) else {
        return Ok(json!({ "message": "MiniMax API key not available." }));
    };
    let urls: &[&str] = if provider == "minimax-cn" {
        &[
            "https://www.minimaxi.com/v1/api/openplatform/coding_plan/remains",
            "https://api.minimaxi.com/v1/api/openplatform/coding_plan/remains",
        ]
    } else {
        &[
            "https://www.minimax.io/v1/token_plan/remains",
            "https://api.minimax.io/v1/api/openplatform/coding_plan/remains",
        ]
    };

    for url in urls {
        if let Ok(resp) = client
            .get(*url)
            .header(header::AUTHORIZATION, format!("Bearer {}", key))
            .header(header::ACCEPT, "application/json")
            .timeout(Duration::from_secs(10))
            .send()
            .await
        {
            if resp.status().is_success() {
                if let Ok(data) = resp.json::<Value>().await {
                    let mut quotas = serde_json::Map::new();
                    if let Some(arr) = data.get("model_remains").and_then(|v| v.as_array()) {
                        for m in arr {
                            let name = m
                                .get("model_name")
                                .or_else(|| m.get("modelName"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("MiniMax");
                            let label = if name == "MiniMax-M*" || name == "general" {
                                "M-series".to_string()
                            } else {
                                name.to_string()
                            };
                            let rem_pct = m
                                .get("current_interval_remaining_percent")
                                .or_else(|| m.get("currentIntervalRemainingPercent"))
                                .and_then(|v| v.as_f64());
                            let total = to_finite_f64(
                                m.get("current_interval_total_count")
                                    .or_else(|| m.get("currentIntervalTotalCount")),
                                0.0,
                            );
                            if let Some(pct) = rem_pct {
                                let clamped = pct.clamp(0.0, 100.0);
                                let used = if total > 0.0 {
                                    total * (1.0 - clamped / 100.0)
                                } else {
                                    100.0 - clamped
                                };
                                let eff_total = if total > 0.0 { total } else { 100.0 };
                                quotas.insert(
                                    label,
                                    json!({
                                        "used": used,
                                        "total": eff_total,
                                        "remainingPercentage": clamped,
                                        "resetAt": parse_reset_time(m.get("end_time").or_else(|| m.get("endTime"))),
                                        "unlimited": false,
                                    }),
                                );
                            }
                        }
                    }
                    return Ok(json!({ "plan": "MiniMax", "quotas": Value::Object(quotas) }));
                }
            }
        }
    }

    Ok(json!({ "message": "MiniMax usage API temporarily unavailable." }))
}

async fn fetch_deepseek_usage(
    client: &reqwest::Client,
    api_key: Option<&str>,
) -> Result<Value, String> {
    let Some(key) = api_key.filter(|s| !s.is_empty()) else {
        return Ok(json!({ "message": "DeepSeek API key not available. Add a key to view usage." }));
    };

    let resp = match client
        .get("https://api.deepseek.com/user/balance")
        .header(header::AUTHORIZATION, format!("Bearer {}", key))
        .header(header::ACCEPT, "application/json")
        .timeout(Duration::from_secs(10))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return Ok(json!({ "plan": "DeepSeek", "message": format!("DeepSeek error: {}", e) })),
    };

    if resp.status() == reqwest::StatusCode::UNAUTHORIZED
        || resp.status() == reqwest::StatusCode::FORBIDDEN
    {
        return Ok(json!({
            "plan": "DeepSeek",
            "message": "DeepSeek authentication failed. Check the API key."
        }));
    }
    if !resp.status().is_success() {
        return Ok(json!({
            "plan": "DeepSeek",
            "message": format!("DeepSeek balance API error ({})", resp.status().as_u16())
        }));
    }

    let data: Value = resp.json().await.unwrap_or_default();
    let mut quotas = serde_json::Map::new();
    if let Some(infos) = data.get("balance_infos").and_then(|v| v.as_array()) {
        for item in infos {
            let currency = item
                .get("currency")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_ascii_uppercase();
            if currency.is_empty() {
                continue;
            }
            let total = to_finite_f64(
                item.get("total_balance").or_else(|| item.get("totalBalance")),
                0.0,
            )
            .max(0.0);
            quotas.insert(
                format!("Balance ({})", currency),
                json!({
                    "used": 0,
                    "total": total,
                    "remainingPercentage": if total > 0.0 { 100 } else { 0 },
                    "resetAt": Value::Null,
                    "unlimited": false,
                    "isCreditBalance": true,
                    "currency": currency,
                }),
            );
        }
    }

    if quotas.is_empty() {
        return Ok(json!({
            "plan": "DeepSeek",
            "message": "DeepSeek connected. No balance data returned."
        }));
    }

    Ok(json!({
        "plan": "DeepSeek",
        "isAvailable": data.get("is_available").and_then(|v| v.as_bool()).unwrap_or(false),
        "quotas": Value::Object(quotas),
    }))
}

async fn fetch_groq_usage(
    client: &reqwest::Client,
    api_key: Option<&str>,
) -> Result<Value, String> {
    let Some(key) = api_key.filter(|s| !s.is_empty()) else {
        return Ok(json!({ "message": "Groq API key not available. Add a key to view usage." }));
    };

    let resp = match client
        .get("https://api.groq.com/openai/v1/models")
        .header(header::AUTHORIZATION, format!("Bearer {}", key))
        .header(header::ACCEPT, "application/json")
        .timeout(Duration::from_secs(10))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return Ok(json!({ "plan": "Groq", "message": format!("Groq error: {}", e) })),
    };

    if resp.status() == reqwest::StatusCode::UNAUTHORIZED
        || resp.status() == reqwest::StatusCode::FORBIDDEN
    {
        return Ok(json!({ "plan": "Groq", "message": "Groq authentication failed. Check the API key." }));
    }
    if !resp.status().is_success() {
        return Ok(json!({
            "plan": "Groq",
            "message": format!("Groq usage API error ({})", resp.status().as_u16())
        }));
    }

    let hdrs = resp.headers().clone();
    let mut quotas = serde_json::Map::new();
    let read_pair = |lim_k: &str, rem_k: &str| -> Option<Value> {
        let lim = hdrs.get(lim_k)?.to_str().ok()?.parse::<f64>().ok()?;
        let rem = hdrs.get(rem_k)?.to_str().ok()?.parse::<f64>().ok()?;
        Some(json!({
            "used": (lim - rem).max(0.0),
            "total": lim,
            "resetAt": Value::Null,
            "unlimited": false,
        }))
    };

    if let Some(q) = read_pair("x-ratelimit-limit-requests", "x-ratelimit-remaining-requests") {
        quotas.insert("Requests".to_string(), q);
    }
    if let Some(q) = read_pair("x-ratelimit-limit-tokens", "x-ratelimit-remaining-tokens") {
        quotas.insert("Tokens".to_string(), q);
    }

    Ok(json!({ "plan": "Groq", "quotas": Value::Object(quotas) }))
}

async fn fetch_kimi_usage(
    client: &reqwest::Client,
    conn: &ProviderConnection,
    access_token: Option<&str>,
    api_key: Option<&str>,
) -> Result<Value, String> {
    let mut req = client
        .get("https://api.kimi.com/coding/v1/usages")
        .header(header::ACCEPT, "application/json")
        .timeout(Duration::from_secs(10));

    if let Some(key) = api_key.filter(|s| !s.is_empty()) {
        req = req.header("x-api-key", key);
    } else if let Some(tok) = access_token.filter(|s| !s.is_empty()) {
        req = req.header(header::AUTHORIZATION, format!("Bearer {}", tok));
        if let Some(dev_id) = conn_str(conn, "deviceId") {
            req = req.header("X-Msh-Device-Id", dev_id);
        }
    } else {
        return Ok(json!({ "message": "Kimi access token or API key not available." }));
    }

    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => return Ok(json!({ "message": format!("Kimi usage error: {}", e) })),
    };

    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Ok(json!({ "message": "Kimi authentication expired. Please re-authorize." }));
    }
    if !resp.status().is_success() {
        return Ok(json!({
            "message": format!("Kimi Coding connected. API Error {}", resp.status().as_u16())
        }));
    }

    let data: Value = resp.json().await.unwrap_or_default();
    let mut quotas = serde_json::Map::new();
    if let Some(usage) = data.get("usage") {
        let total = to_finite_f64(usage.get("limit"), 0.0);
        let used = to_finite_f64(usage.get("used"), 0.0);
        let rem_pct = if total > 0.0 {
            ((total - used).max(0.0) / total) * 100.0
        } else {
            0.0
        };
        quotas.insert(
            "Usage".to_string(),
            json!({
                "used": used,
                "total": total,
                "remainingPercentage": rem_pct,
                "resetAt": parse_reset_time(usage.get("resetTime")),
                "unlimited": false,
            }),
        );
    }

    Ok(json!({ "plan": "Kimi Coding", "quotas": Value::Object(quotas) }))
}

async fn fetch_opencode_usage(
    client: &reqwest::Client,
    provider: &str,
    api_key: Option<&str>,
) -> Result<Value, String> {
    let label = if provider == "opencode-go" {
        "OpenCode Go"
    } else {
        "OpenCode Zen"
    };
    let Some(key) = api_key.filter(|s| !s.is_empty()) else {
        return Ok(json!({
            "message": format!("{} API key not available. Add a key to view usage.", label)
        }));
    };

    let url = if provider == "opencode-go" {
        "https://opencode.ai/inference/go/v1/usage"
    } else {
        "https://opencode.ai/zen/v1/usage"
    };

    let resp = match client
        .get(url)
        .header(header::AUTHORIZATION, format!("Bearer {}", key))
        .header(header::ACCEPT, "application/json")
        .timeout(Duration::from_secs(10))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return Ok(json!({ "plan": label, "message": format!("{} error: {}", label, e) })),
    };

    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Ok(json!({
            "plan": label,
            "message": format!("{} authentication failed. Check the API key.", label)
        }));
    }
    if !resp.status().is_success() {
        return Ok(json!({
            "plan": label,
            "message": format!("{} usage API error ({})", label, resp.status().as_u16())
        }));
    }

    let data: Value = resp.json().await.unwrap_or_default();
    let mut quotas = serde_json::Map::new();
    for (k, title) in [("rolling", "Rolling"), ("weekly", "Weekly"), ("monthly", "Monthly")] {
        if let Some(win) = data.get(k).and_then(|v| v.as_object()) {
            let pct = to_finite_f64(win.get("usagePercent").or_else(|| win.get("percent")), 0.0)
                .clamp(0.0, 100.0);
            quotas.insert(
                title.to_string(),
                json!({
                    "used": pct,
                    "total": 100,
                    "remainingPercentage": (100.0 - pct).max(0.0),
                    "resetAt": parse_reset_time(win.get("resetAt").or_else(|| win.get("resetInSec"))),
                    "unlimited": false,
                }),
            );
        }
    }

    Ok(json!({ "plan": label, "quotas": Value::Object(quotas) }))
}

async fn fetch_ollama_usage(
    client: &reqwest::Client,
    api_key: Option<&str>,
) -> Result<Value, String> {
    let Some(key) = api_key.filter(|s| !s.is_empty()) else {
        return Ok(json!({ "message": "Ollama Cloud API key not available." }));
    };

    let resp = match client
        .get("https://ollama.com/api/usage")
        .header(header::AUTHORIZATION, format!("Bearer {}", key))
        .header(header::ACCEPT, "application/json")
        .timeout(Duration::from_secs(10))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return Ok(json!({ "message": format!("Ollama Cloud error: {}", e) })),
    };

    if resp.status() == reqwest::StatusCode::UNAUTHORIZED
        || resp.status() == reqwest::StatusCode::FORBIDDEN
    {
        return Ok(json!({ "message": "Ollama Cloud API key invalid or expired." }));
    }
    if !resp.status().is_success() {
        return Ok(json!({
            "message": format!("Ollama Cloud usage API error ({}).", resp.status().as_u16())
        }));
    }

    let data: Value = resp.json().await.unwrap_or_default();
    let mut quotas = serde_json::Map::new();
    if let Some(limits) = data.get("limits").and_then(|v| v.as_object()) {
        for (k, label) in [
            ("session", "Session (5h)"),
            ("weekly", "Weekly (7d)"),
            ("monthly", "Monthly"),
        ] {
            if let Some(ratio) = limits.get(k).and_then(|w| w.get("usage")).and_then(|v| v.as_f64()) {
                let used_pct = (ratio.clamp(0.0, 1.0) * 100.0).round();
                quotas.insert(
                    label.to_string(),
                    json!({
                        "used": used_pct,
                        "total": 100,
                        "remainingPercentage": 100.0 - used_pct,
                        "resetAt": Value::Null,
                        "unlimited": false,
                    }),
                );
            }
        }
    }

    Ok(json!({ "plan": "Ollama Cloud", "quotas": Value::Object(quotas) }))
}

async fn fetch_vercel_ai_gateway_usage(
    client: &reqwest::Client,
    api_key: Option<&str>,
) -> Result<Value, String> {
    let Some(key) = api_key.filter(|s| !s.is_empty()) else {
        return Ok(json!({ "message": "Vercel AI Gateway API key not available." }));
    };

    let resp = match client
        .get("https://ai-gateway.vercel.sh/v1/credits")
        .header(header::AUTHORIZATION, format!("Bearer {}", key))
        .header(header::ACCEPT, "application/json")
        .timeout(Duration::from_secs(10))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return Ok(json!({ "message": format!("Vercel AI Gateway error: {}", e) })),
    };

    if resp.status() == reqwest::StatusCode::UNAUTHORIZED
        || resp.status() == reqwest::StatusCode::FORBIDDEN
    {
        return Ok(json!({ "message": "Vercel AI Gateway API key invalid or expired." }));
    }
    if !resp.status().is_success() {
        return Ok(json!({
            "message": format!("Vercel AI Gateway credits API error ({})", resp.status().as_u16())
        }));
    }

    let data: Value = resp.json().await.unwrap_or_default();
    let balance = to_finite_f64(data.get("balance"), 0.0);
    let total_used = to_finite_f64(data.get("total_used"), 0.0);
    let monthly_credit = 5.0;

    if balance <= 0.0 && total_used <= 0.0 {
        return Ok(json!({
            "plan": "Pay-as-you-go",
            "message": "Vercel AI Gateway connected. No credit allocation found (BYOK or unfunded account).",
            "quotas": {}
        }));
    }

    Ok(json!({
        "plan": "Pay-as-you-go",
        "quotas": {
            "Used (USD)": {
                "used": total_used,
                "total": 0,
                "remaining": 0,
                "remainingPercentage": 100,
                "unlimited": true,
            },
            "Remaining (USD)": {
                "used": balance,
                "total": monthly_credit,
                "remaining": balance,
                "remainingPercentage": (balance / monthly_credit) * 100.0,
                "unlimited": false,
            }
        }
    }))
}

async fn fetch_clinepass_usage(
    client: &reqwest::Client,
    access_token: Option<&str>,
    api_key: Option<&str>,
) -> Result<Value, String> {
    let bearer = api_key
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .or_else(|| {
            access_token.filter(|s| !s.is_empty()).map(|t| {
                if t.starts_with("workos:") {
                    t.to_string()
                } else {
                    format!("workos:{}", t)
                }
            })
        });
    let Some(tok) = bearer else {
        return Ok(json!({ "message": "ClinePass access token or API key not available." }));
    };

    let resp = match client
        .get("https://api.cline.bot/api/v1/users/me/plan/usage-limits")
        .header(header::AUTHORIZATION, format!("Bearer {}", tok))
        .header(header::ACCEPT, "application/json")
        .timeout(Duration::from_secs(10))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return Ok(json!({ "message": format!("ClinePass usage error: {}", e) })),
    };

    if !resp.status().is_success() {
        return Ok(json!({
            "message": format!("ClinePass usage API error ({})", resp.status().as_u16())
        }));
    }

    let data: Value = resp.json().await.unwrap_or_default();
    let mut quotas = serde_json::Map::new();
    if let Some(limits) = data
        .get("data")
        .and_then(|d| d.get("limits"))
        .and_then(|v| v.as_array())
    {
        for l in limits {
            let ltype = l.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let label = match ltype {
                "five_hour" => "Session (5h)",
                "weekly" => "Weekly (7d)",
                "monthly" => "Monthly",
                other => other,
            };
            let used = to_finite_f64(l.get("percentUsed"), 0.0).clamp(0.0, 100.0);
            quotas.insert(
                label.to_string(),
                json!({
                    "used": used,
                    "total": 100,
                    "remainingPercentage": (100.0 - used).max(0.0),
                    "resetAt": parse_reset_time(l.get("resetsAt")),
                }),
            );
        }
    }

    Ok(json!({ "plan": "ClinePass", "quotas": Value::Object(quotas) }))
}

async fn fetch_codebuddy_usage(
    client: &reqwest::Client,
    provider: &str,
    token: Option<&str>,
) -> Result<Value, String> {
    let Some(tok) = token.filter(|s| !s.is_empty()) else {
        return Ok(json!({ "message": format!("CodeBuddy ({}) credential not available.", provider) }));
    };

    let url = if provider == "codebuddy-intl" {
        "https://www.codebuddy.ai/v2/billing/meter/get-user-resource"
    } else {
        "https://copilot.tencent.com/v2/billing/meter/get-user-resource"
    };

    let resp = match client
        .post(url)
        .header(header::AUTHORIZATION, format!("Bearer {}", tok))
        .header(header::ACCEPT, "application/json")
        .json(&json!({}))
        .timeout(Duration::from_secs(10))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return Ok(json!({ "message": format!("CodeBuddy error: {}", e) })),
    };

    if !resp.status().is_success() {
        return Ok(json!({
            "message": format!("CodeBuddy ({}) usage error ({}).", provider, resp.status().as_u16())
        }));
    }

    let data: Value = resp.json().await.unwrap_or_default();
    Ok(json!({ "plan": "CodeBuddy", "raw": data, "quotas": {} }))
}

async fn fetch_qoder_usage(
    client: &reqwest::Client,
    provider: &str,
    token: Option<&str>,
) -> Result<Value, String> {
    let Some(tok) = token.filter(|s| !s.is_empty()) else {
        return Ok(json!({ "message": "Qoder usage unavailable: no access token" }));
    };
    let url = if provider == "qoder-cn" {
        "https://openapi.qoder.com.cn/api/v2/quota/usage"
    } else {
        "https://openapi.qoder.sh/api/v2/quota/usage"
    };

    let resp = match client
        .get(url)
        .header(header::AUTHORIZATION, format!("Bearer {}", tok))
        .header(header::ACCEPT, "application/json")
        .timeout(Duration::from_secs(10))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return Ok(json!({ "message": format!("Qoder connected. Unable to fetch usage: {}", e) })),
    };

    if !resp.status().is_success() {
        return Ok(json!({
            "message": format!("Qoder connected. Usage fetch returned {}.", resp.status().as_u16())
        }));
    }

    let body: Value = resp.json().await.unwrap_or_default();
    let expires_at_ms = body.get("expiresAt").and_then(|v| v.as_i64());
    let never_expires = expires_at_ms == Some(253402214400000);
    let reset_at = match expires_at_ms {
        Some(ms) if ms > 0 && !never_expires => {
            DateTime::from_timestamp_millis(ms)
                .map(|dt| dt.to_rfc3339_opts(SecondsFormat::Millis, true))
        }
        _ => None,
    };

    let map_q = |q: Option<&Value>| {
        json!({
            "total": to_finite_f64(q.and_then(|v| v.get("total")), 0.0),
            "used": to_finite_f64(q.and_then(|v| v.get("used")), 0.0),
            "remaining": to_finite_f64(q.and_then(|v| v.get("remaining")), 0.0),
            "unit": q.and_then(|v| v.get("unit")).and_then(|v| v.as_str()).unwrap_or("credits"),
            "resetAt": reset_at,
            "neverExpires": never_expires,
        })
    };

    let mut quotas = serde_json::Map::new();
    quotas.insert("user".to_string(), map_q(body.get("userQuota")));
    quotas.insert("organization".to_string(), map_q(body.get("orgResourcePackage")));

    let add_on_total = to_finite_f64(body.get("addOnQuota").and_then(|v| v.get("total")), 0.0);
    if add_on_total > 0.0 {
        let ao = body.get("addOnQuota");
        quotas.insert(
            "addOn".to_string(),
            json!({
                "total": add_on_total,
                "used": to_finite_f64(ao.and_then(|v| v.get("used")), 0.0),
                "remaining": to_finite_f64(ao.and_then(|v| v.get("remaining")), 0.0),
                "unit": ao.and_then(|v| v.get("unit")).and_then(|v| v.as_str()).unwrap_or("credits"),
                "resetAt": Value::Null,
                "neverExpires": false,
                "detailUrl": ao.and_then(|v| v.get("detailUrl")).cloned().unwrap_or(Value::Null),
            }),
        );
    }

    Ok(json!({
        "quotas": Value::Object(quotas),
        "totalUsagePercentage": to_finite_f64(body.get("totalUsagePercentage"), 0.0),
        "isQuotaExceeded": body.get("isQuotaExceeded").and_then(|v| v.as_bool()).unwrap_or(false),
        "expiresAt": expires_at_ms,
    }))
}

async fn fetch_synthetic_usage(
    client: &reqwest::Client,
    api_key: Option<&str>,
) -> Result<Value, String> {
    let Some(key) = api_key.filter(|s| !s.is_empty()) else {
        return Ok(json!({ "message": "Synthetic API key not available. Add a key to view usage." }));
    };

    let resp = match client
        .get("https://api.synthetic.new/v2/quotas")
        .header(header::AUTHORIZATION, format!("Bearer {}", key))
        .header(header::ACCEPT, "application/json")
        .timeout(Duration::from_secs(10))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return Ok(json!({ "plan": "Synthetic", "message": format!("Synthetic error: {}", e) })),
    };

    if resp.status() == reqwest::StatusCode::UNAUTHORIZED
        || resp.status() == reqwest::StatusCode::FORBIDDEN
    {
        return Ok(json!({
            "plan": "Synthetic",
            "message": "Synthetic authentication failed. Check the API key."
        }));
    }
    if !resp.status().is_success() {
        return Ok(json!({
            "plan": "Synthetic",
            "message": format!("Synthetic usage API error ({})", resp.status().as_u16())
        }));
    }

    let data: Value = resp.json().await.unwrap_or_default();
    let Some(sub) = data.get("subscription").and_then(|v| v.as_object()) else {
        return Ok(json!({
            "plan": "Synthetic",
            "message": "Synthetic returned no subscription quota."
        }));
    };

    let total = to_finite_f64(sub.get("limit"), 0.0);
    if total <= 0.0 {
        return Ok(json!({
            "plan": "Synthetic",
            "message": "No Synthetic subscription quota found for this key."
        }));
    }
    let used = to_finite_f64(sub.get("requests"), 0.0);
    let remaining = (total - used).max(0.0);

    Ok(json!({
        "plan": "Synthetic",
        "quotas": {
            "Requests": {
                "name": "Subscription requests",
                "used": used,
                "total": total,
                "remainingPercentage": (remaining / total) * 100.0,
                "resetAt": parse_reset_time(sub.get("renewsAt")),
                "unlimited": false,
            }
        }
    }))
}

async fn fetch_zed_usage(
    client: &reqwest::Client,
    conn: &ProviderConnection,
    access_token: Option<&str>,
) -> Result<Value, String> {
    let Some(tok) = access_token.filter(|s| !s.is_empty()) else {
        return Ok(json!({ "message": "Zed access token not available." }));
    };
    let user_id = conn_str(conn, "userId")
        .or_else(|| conn_str(conn, "user_id"))
        .unwrap_or("");
    let auth_val = if user_id.is_empty() {
        format!("Bearer {}", tok)
    } else {
        format!("{} {}", user_id, tok)
    };

    let resp = match client
        .get("https://cloud.zed.dev/client/users/me")
        .header(header::AUTHORIZATION, auth_val)
        .header(header::CONTENT_TYPE, "application/json")
        .timeout(Duration::from_secs(10))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return Ok(json!({ "message": format!("Zed usage error: {}", e) })),
    };

    if !resp.status().is_success() {
        return Ok(json!({
            "message": format!("Zed usage API error ({}).", resp.status().as_u16())
        }));
    }

    let data: Value = resp.json().await.unwrap_or_default();
    let plan = data
        .get("plan")
        .and_then(|p| p.get("plan_v3").or_else(|| p.get("plan_v2")).or_else(|| p.get("plan")))
        .and_then(|v| v.as_str())
        .unwrap_or("Zed");
    Ok(json!({ "plan": plan, "quotas": {}, "raw": data }))
}

async fn fetch_cursor_usage(
    client: &reqwest::Client,
    conn: &ProviderConnection,
    access_token: Option<&str>,
) -> Result<Value, String> {
    let Some(tok) = access_token.filter(|s| !s.is_empty()) else {
        return Ok(json!({ "message": "Cursor access token not available." }));
    };
    let _ = conn;
    let resp = match client
        .post("https://api2.cursor.sh/aiserver.v1.DashboardService/GetCurrentPeriodUsage")
        .header(header::AUTHORIZATION, format!("Bearer {}", tok))
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json")
        .body("{}")
        .timeout(Duration::from_secs(10))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return Ok(json!({ "message": format!("Cursor usage error: {}", e) })),
    };

    if !resp.status().is_success() {
        return Ok(json!({
            "message": format!("Cursor usage API error ({}).", resp.status().as_u16())
        }));
    }

    let data: Value = resp.json().await.unwrap_or_default();
    let mut quotas = serde_json::Map::new();
    if let Some(pu) = data.get("planUsage").and_then(|v| v.as_object()) {
        let limit_cents = to_finite_f64(pu.get("limit"), 0.0);
        let used_cents = to_finite_f64(
            pu.get("includedSpend").or_else(|| pu.get("totalSpend")),
            0.0,
        );
        if limit_cents > 0.0 {
            let total = (limit_cents / 100.0 * 100.0).round() / 100.0;
            let used = (used_cents / 100.0 * 100.0).round() / 100.0;
            let rem_pct = ((total - used).max(0.0) / total) * 100.0;
            quotas.insert(
                "Plan Spend".to_string(),
                json!({
                    "used": used,
                    "total": total,
                    "remainingPercentage": rem_pct,
                    "resetAt": parse_reset_time(data.get("billingCycleEnd")),
                    "unlimited": false,
                }),
            );
        }
    }
    Ok(json!({ "plan": "Cursor", "quotas": Value::Object(quotas) }))
}

async fn fetch_grok_cli_usage(
    client: &reqwest::Client,
    conn: &ProviderConnection,
    access_token: Option<&str>,
) -> Result<Value, String> {
    let Some(tok) = access_token.filter(|s| !s.is_empty()) else {
        return Ok(json!({ "message": "Grok CLI access token not available." }));
    };
    let _ = conn;
    let resp = match client
        .get("https://cli-chat-proxy.grok.com/v1/billing?format=credits")
        .header(header::AUTHORIZATION, format!("Bearer {}", tok))
        .header(header::ACCEPT, "application/json")
        .header("x-xai-token-auth", "xai-grok-cli")
        .timeout(Duration::from_secs(10))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return Ok(json!({ "message": format!("Grok CLI usage error: {}", e) })),
    };

    if !resp.status().is_success() {
        return Ok(json!({
            "message": format!("Grok CLI usage API error ({}).", resp.status().as_u16())
        }));
    }

    let data: Value = resp.json().await.unwrap_or_default();
    let cfg = data.get("config");
    let cap = to_finite_f64(cfg.and_then(|c| c.get("onDemandCap")).and_then(|v| v.get("val")), 0.0);
    let used = to_finite_f64(cfg.and_then(|c| c.get("onDemandUsed")).and_then(|v| v.get("val")), 0.0);
    let mut quotas = serde_json::Map::new();
    if cap > 0.0 {
        let rem_pct = ((cap - used).max(0.0) / cap) * 100.0;
        quotas.insert(
            "On-Demand".to_string(),
            json!({
                "used": used,
                "total": cap,
                "remainingPercentage": rem_pct,
                "resetAt": parse_reset_time(cfg.and_then(|c| c.get("billingPeriodEnd"))),
                "unlimited": false,
            }),
        );
    }
    Ok(json!({ "plan": "Grok Build", "quotas": Value::Object(quotas) }))
}

async fn fetch_commandcode_usage(
    client: &reqwest::Client,
    api_key: Option<&str>,
) -> Result<Value, String> {
    let Some(key) = api_key.filter(|s| !s.is_empty()) else {
        return Ok(json!({ "message": "Command Code API key not available. Add a key to view usage." }));
    };
    let resp = match client
        .get("https://api.commandcode.ai/alpha/whoami?limits=1")
        .header(header::AUTHORIZATION, format!("Bearer {}", key))
        .header(header::ACCEPT, "application/json")
        .timeout(Duration::from_secs(10))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return Ok(json!({ "plan": "Command Code", "message": format!("Command Code error: {}", e) })),
    };

    if resp.status() == reqwest::StatusCode::UNAUTHORIZED
        || resp.status() == reqwest::StatusCode::FORBIDDEN
    {
        return Ok(json!({
            "plan": "Command Code",
            "message": "Command Code authentication failed. Check the API key."
        }));
    }
    if !resp.status().is_success() {
        return Ok(json!({
            "plan": "Command Code",
            "message": format!("Command Code usage error ({}).", resp.status().as_u16())
        }));
    }
    let data: Value = resp.json().await.unwrap_or_default();
    Ok(json!({ "plan": "Command Code", "quotas": {}, "raw": data }))
}

async fn fetch_xiaomi_mimo_usage(
    client: &reqwest::Client,
    token: Option<&str>,
) -> Result<Value, String> {
    let Some(key) = token.filter(|s| !s.is_empty()) else {
        return Ok(json!({ "message": "Xiaomi MiMo Desktop not connected. Add credentials to view usage." }));
    };

    let resp = match client
        .get("https://aistudio.xiaomimimo.com/open-apis/v1/user/usage")
        .header(header::AUTHORIZATION, format!("Bearer {}", key))
        .header("X-Mimo-Source", "mimocode-cli")
        .header(header::ACCEPT, "application/json")
        .timeout(Duration::from_secs(10))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return Ok(json!({ "plan": "Xiaomi MiMo Desktop", "message": format!("Usage error: {}", e) })),
    };

    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Ok(json!({
            "plan": "Xiaomi MiMo Desktop",
            "message": "Weekly quota requires Xiaomi account session. API key alone is insufficient."
        }));
    }
    if !resp.status().is_success() {
        return Ok(json!({
            "plan": "Xiaomi MiMo Desktop",
            "message": format!("Usage API error ({})", resp.status().as_u16())
        }));
    }

    let data: Value = resp.json().await.unwrap_or_default();
    if data.get("code").and_then(|v| v.as_i64()) != Some(0) {
        return Ok(json!({
            "plan": "Xiaomi MiMo Desktop",
            "message": "Usage endpoint returned unexpected response."
        }));
    }

    let pct = data
        .get("data")
        .and_then(|d| d.get("percent"))
        .and_then(|v| v.as_f64())
        .map(|p| p.round().clamp(0.0, 100.0));
    let Some(remaining) = pct else {
        return Ok(json!({
            "plan": "Xiaomi MiMo Desktop",
            "message": "Usage data missing percent field."
        }));
    };

    let reset_at = data
        .get("data")
        .and_then(|d| d.get("resetDate"))
        .and_then(|v| v.as_str())
        .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|dt| dt.and_utc().to_rfc3339_opts(SecondsFormat::Millis, true));

    Ok(json!({
        "plan": "Xiaomi MiMo Desktop",
        "quotas": {
            "Weekly": {
                "used": 100.0 - remaining,
                "total": 100,
                "remainingPercentage": remaining,
                "resetAt": reset_at,
                "unlimited": false,
            }
        }
    }))
}

// ============================================================================
// 4. POST /api/usage/{connectionId}/claude-reset
// ============================================================================

fn is_valid_claude_grant_id(grant_id: &str) -> bool {
    !grant_id.is_empty()
        && grant_id.len() <= 40
        && grant_id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

pub async fn usage_claude_reset_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(connection_id): Path<String>,
    body: axum::body::Bytes,
) -> Response {
    if let Err(resp) = auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let payload: Value = serde_json::from_slice(&body).unwrap_or_else(|_| json!({}));
    let grant_id = payload
        .get("grantId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let mut connection = match state
        .db
        .load_provider_connections()
        .ok()
        .and_then(|list| list.into_iter().find(|c| c.id == connection_id))
    {
        Some(c) => c,
        None => {
            return json_response(
                StatusCode::NOT_FOUND,
                json!({ "error": "Connection not found" }),
            );
        }
    };

    if connection.provider != "claude" || connection.auth_type != "oauth" {
        return json_response(
            StatusCode::BAD_REQUEST,
            json!({ "error": "Limit reset is only available for Claude OAuth connections." }),
        );
    }

    match refresh_connection_if_needed(&state, &connection, false).await {
        Ok(updated) => connection = updated,
        Err(e) => {
            return json_response(StatusCode::INTERNAL_SERVER_ERROR, json!({ "error": e }));
        }
    }

    let Some(token) = conn_access_token(&connection) else {
        return json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({ "error": "No Claude access token available. Please re-authorize the connection." }),
        );
    };

    if !is_valid_claude_grant_id(&grant_id) {
        return json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({ "error": "Invalid reset grant id." }),
        );
    }

    let client = build_usage_http_client(&state, &connection);
    let profile_res = match client
        .get("https://api.anthropic.com/api/oauth/profile")
        .header(header::AUTHORIZATION, format!("Bearer {}", token))
        .header("anthropic-beta", "oauth-2025-04-20")
        .header("anthropic-version", "2023-06-01")
        .header(header::USER_AGENT, "claude-cli/2.1.322 (external, cli)")
        .header(header::CONTENT_TYPE, "application/json")
        .timeout(Duration::from_secs(12))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({ "error": e.to_string() }),
            );
        }
    };

    let p_status = profile_res.status().as_u16();
    let profile_json: Value = profile_res.json().await.unwrap_or(Value::Null);
    let org_id = profile_json
        .get("organization")
        .and_then(|o| o.get("uuid"))
        .and_then(|v| v.as_str())
        .map(ToString::to_string);

    let Some(org_id) = org_id else {
        return json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({ "error": format!("Cannot resolve Claude organization ({}).", p_status) }),
        );
    };

    let reset_url = format!(
        "https://api.anthropic.com/api/organizations/{}/reset_rate_limits",
        org_id
    );
    let reset_res = match client
        .post(&reset_url)
        .header(header::AUTHORIZATION, format!("Bearer {}", token))
        .header("anthropic-beta", "oauth-2025-04-20")
        .header("anthropic-version", "2023-06-01")
        .header(header::USER_AGENT, "claude-cli/2.1.322 (external, cli)")
        .header(header::CONTENT_TYPE, "application/json")
        .json(&json!({
            "program": "cedar_ember",
            "grant_id": grant_id,
            "request_id": generate_uuid(),
        }))
        .timeout(Duration::from_secs(12))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({ "error": e.to_string() }),
            );
        }
    };

    let r_status = reset_res.status().as_u16();
    let r_ok_http = reset_res.status().is_success();
    let data: Value = reset_res.json().await.unwrap_or(Value::Null);
    claude_usage_cache().remove(&token);

    let result_str = data.get("result").and_then(|v| v.as_str());
    let reason_str = data.get("reason").and_then(|v| v.as_str());
    let ok = r_ok_http && result_str == Some("reset");
    let resets_left = data.get("resets_left").cloned().unwrap_or(Value::Null);
    let err_msg = data
        .get("error")
        .and_then(|e| e.get("message"))
        .and_then(|v| v.as_str())
        .map(ToString::to_string);

    if ok {
        return json_response(
            StatusCode::OK,
            json!({
                "ok": true,
                "status": r_status,
                "result": result_str,
                "reason": reason_str,
                "resetsLeft": resets_left,
                "message": err_msg,
            }),
        );
    }

    let out_status = if (400..500).contains(&r_status) {
        StatusCode::from_u16(r_status).unwrap_or(StatusCode::CONFLICT)
    } else {
        StatusCode::CONFLICT
    };

    let fallback_msg = format!(
        "Reset not applied: {}",
        reason_str.or(result_str).unwrap_or("unknown")
    );
    json_response(
        out_status,
        json!({
            "ok": false,
            "status": r_status,
            "result": result_str,
            "reason": reason_str,
            "resetsLeft": resets_left,
            "message": err_msg.unwrap_or(fallback_msg),
        }),
    )
}

// ============================================================================
// 5. GET & POST /api/usage/{connectionId}/codex-reset-credits
// ============================================================================

fn resolve_codex_connection(
    db: &Database,
    connection_id: &str,
) -> Result<(ProviderConnection, bool), Response> {
    let conn = db
        .load_provider_connections()
        .ok()
        .and_then(|list| list.into_iter().find(|c| c.id == connection_id))
        .ok_or_else(|| json_response(StatusCode::NOT_FOUND, json!({ "error": "Connection not found" })))?;

    if conn.provider != "codex" {
        return Err(json_response(
            StatusCode::BAD_REQUEST,
            json!({ "error": "Codex reset credits are only available for Codex connections." }),
        ));
    }

    let is_oauth = conn.auth_type == "oauth";
    let is_access_token = conn.auth_type == "access_token";
    if !is_oauth && !is_access_token {
        return Err(json_response(
            StatusCode::BAD_REQUEST,
            json!({ "error": "Codex reset credits require an OAuth or access-token connection." }),
        ));
    }

    Ok((conn, is_oauth))
}

fn codex_account_id(conn: &ProviderConnection) -> Option<String> {
    conn_str(conn, "workspaceId")
        .or_else(|| conn_str(conn, "accountId"))
        .or_else(|| conn_str(conn, "chatgptAccountId"))
        .map(ToString::to_string)
}

async fn fetch_codex_reset_credits(
    client: &reqwest::Client,
    conn: &ProviderConnection,
) -> Result<Value, String> {
    let Some(token) = conn_access_token(conn) else {
        return Err("No Codex access token available. Please re-authorize the connection.".to_string());
    };

    let mut req = client
        .get("https://chatgpt.com/backend-api/wham/rate-limit-reset-credits")
        .header(header::AUTHORIZATION, format!("Bearer {}", token))
        .header(header::ACCEPT, "application/json")
        .header("OpenAI-Beta", "codex-1")
        .header("originator", "codex_cli_rs")
        .timeout(Duration::from_secs(12));
    if let Some(acc_id) = codex_account_id(conn) {
        req = req.header("ChatGPT-Account-ID", acc_id);
    }

    let resp = req.send().await.map_err(|e| e.to_string())?;
    let status = resp.status();
    let data: Value = resp.json().await.unwrap_or(Value::Null);

    if !status.is_success() {
        let msg = data
            .get("message")
            .or_else(|| data.get("error"))
            .or_else(|| data.get("detail"))
            .and_then(|v| {
                v.as_str()
                    .map(ToString::to_string)
                    .or_else(|| v.get("message").and_then(|m| m.as_str()).map(ToString::to_string))
            })
            .unwrap_or_else(|| format!("Codex reset credits API unavailable ({}).", status.as_u16()));
        return Err(msg);
    }

    let available_count = to_finite_f64(
        data.get("available_count").or_else(|| data.get("availableCount")),
        0.0,
    )
    .max(0.0);

    let credits: Vec<Value> = data
        .get("credits")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .map(|c| {
                    json!({
                        "status": c.get("status").and_then(|v| v.as_str()).unwrap_or("unknown"),
                        "grantedAt": parse_reset_time(c.get("granted_at").or_else(|| c.get("grantedAt"))),
                        "expiresAt": parse_reset_time(c.get("expires_at").or_else(|| c.get("expiresAt"))),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(json!({
        "availableCount": available_count,
        "credits": credits,
    }))
}

pub async fn usage_codex_reset_credits_get_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(connection_id): Path<String>,
) -> Response {
    if let Err(resp) = auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let (mut conn, is_oauth) = match resolve_codex_connection(&state.db, &connection_id) {
        Ok(v) => v,
        Err(resp) => return resp,
    };

    if is_oauth {
        match refresh_connection_if_needed(&state, &conn, false).await {
            Ok(updated) => conn = updated,
            Err(err) => {
                return json_response(
                    StatusCode::UNAUTHORIZED,
                    json!({ "error": format!("Credential refresh failed: {}", err) }),
                );
            }
        }
    }

    let client = build_usage_http_client(&state, &conn);
    match fetch_codex_reset_credits(&client, &conn).await {
        Ok(res) => json_response(StatusCode::OK, res),
        Err(err) => {
            if is_oauth && extract_refresh_token(&conn).is_some() && is_auth_expired_str(&err) {
                if let Ok(updated) = refresh_connection_if_needed(&state, &conn, true).await {
                    let retry_client = build_usage_http_client(&state, &updated);
                    match fetch_codex_reset_credits(&retry_client, &updated).await {
                        Ok(res) => return json_response(StatusCode::OK, res),
                        Err(e2) => return json_response(StatusCode::INTERNAL_SERVER_ERROR, json!({ "error": e2 })),
                    }
                }
            }
            json_response(StatusCode::INTERNAL_SERVER_ERROR, json!({ "error": err }))
        }
    }
}

struct CodexConsumeOutcome {
    ok: bool,
    no_credit: bool,
    status: u16,
    code: Option<String>,
    windows_reset: f64,
    message: Option<String>,
    raw: Value,
}

async fn consume_codex_reset_credit(
    client: &reqwest::Client,
    conn: &ProviderConnection,
    redeem_request_id: &str,
) -> Result<CodexConsumeOutcome, String> {
    let Some(token) = conn_access_token(conn) else {
        return Err("No Codex access token available. Please re-authorize the connection.".to_string());
    };

    let resp = client
        .post("https://chatgpt.com/backend-api/wham/rate-limit-reset-credits/consume")
        .header(header::AUTHORIZATION, format!("Bearer {}", token))
        .header(header::ACCEPT, "application/json")
        .header(header::CONTENT_TYPE, "application/json")
        .json(&json!({ "redeem_request_id": redeem_request_id }))
        .timeout(Duration::from_secs(12))
        .send()
        .await
        .map_err(|e| format!("Failed to consume Codex reset credit: {}", e))?;

    let status = resp.status();
    let status_u16 = status.as_u16();
    let text = resp.text().await.unwrap_or_default();
    let data: Value = if text.trim().is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text)
            .map_err(|e| format!("Failed to consume Codex reset credit: {}", e))?
    };

    let code = data.get("code").and_then(|v| v.as_str()).map(ToString::to_string);
    let windows_reset = to_finite_f64(data.get("windows_reset"), 0.0);
    let success = status.is_success() && (code.as_deref() == Some("reset") || windows_reset > 0.0);
    let no_credit = status.is_success() && code.as_deref() == Some("no_credit");
    let message = data.get("message").and_then(|v| v.as_str()).map(ToString::to_string);

    Ok(CodexConsumeOutcome {
        ok: success,
        no_credit,
        status: status_u16,
        code,
        windows_reset,
        message,
        raw: data,
    })
}

fn is_codex_consume_auth_expired(outcome: &CodexConsumeOutcome) -> bool {
    let candidates = [
        outcome.message.as_deref(),
        outcome.code.as_deref(),
        outcome.raw.get("detail").and_then(|v| v.as_str()),
        outcome.raw.get("error").and_then(|v| v.as_str()),
    ];
    candidates
        .into_iter()
        .flatten()
        .any(is_auth_expired_str)
}

pub async fn usage_codex_reset_credits_post_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(connection_id): Path<String>,
) -> Response {
    if let Err(resp) = auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let (mut conn, is_oauth) = match resolve_codex_connection(&state.db, &connection_id) {
        Ok(v) => v,
        Err(resp) => return resp,
    };

    if is_oauth {
        match refresh_connection_if_needed(&state, &conn, false).await {
            Ok(updated) => conn = updated,
            Err(err) => {
                return json_response(
                    StatusCode::UNAUTHORIZED,
                    json!({ "error": format!("Credential refresh failed: {}", err) }),
                );
            }
        }
    }

    let redeem_request_id = generate_uuid();
    let client = build_usage_http_client(&state, &conn);
    let mut outcome = match consume_codex_reset_credit(&client, &conn, &redeem_request_id).await {
        Ok(o) => o,
        Err(e) => return json_response(StatusCode::INTERNAL_SERVER_ERROR, json!({ "error": e })),
    };

    if is_oauth && is_codex_consume_auth_expired(&outcome) && extract_refresh_token(&conn).is_some()
    {
        if let Ok(updated) = refresh_connection_if_needed(&state, &conn, true).await {
            let retry_client = build_usage_http_client(&state, &updated);
            if let Ok(o2) = consume_codex_reset_credit(&retry_client, &updated, &redeem_request_id).await {
                outcome = o2;
            }
        }
    }

    if outcome.ok {
        return json_response(
            StatusCode::OK,
            json!({
                "code": outcome.code,
                "reset": true,
                "windows_reset": outcome.windows_reset,
                "redeemRequestId": redeem_request_id,
                "credit": outcome.raw.get("credit").cloned().unwrap_or(Value::Null),
            }),
        );
    }

    if outcome.no_credit {
        return json_response(
            StatusCode::CONFLICT,
            json!({
                "code": "no_credit",
                "reset": false,
                "windows_reset": outcome.windows_reset,
                "message": "No Codex reset credits available.",
            }),
        );
    }

    let status = if (400..500).contains(&outcome.status) {
        StatusCode::from_u16(outcome.status).unwrap_or(StatusCode::BAD_GATEWAY)
    } else {
        StatusCode::BAD_GATEWAY
    };

    json_response(
        status,
        json!({
            "code": outcome.code.unwrap_or_else(|| "unknown_response".to_string()),
            "reset": false,
            "windows_reset": outcome.windows_reset,
            "message": outcome.message.unwrap_or_else(|| "Codex reset credit consume returned an unexpected response.".to_string()),
        }),
    )
}

// ============================================================================
// 6. GET & OPTIONS /v1/models/info (+ /api/v1/models/info)
// ============================================================================

#[derive(Debug, Deserialize, Default)]
pub struct ModelsInfoQuery {
    pub id: Option<String>,
    pub kind: Option<String>,
}

fn kind_endpoint(kind: &str) -> Option<&'static str> {
    match kind {
        "llm" => Some("/v1/chat/completions"),
        "image" => Some("/v1/images/generations"),
        "tts" => Some("/v1/audio/speech"),
        "stt" => Some("/v1/audio/transcriptions"),
        "embedding" => Some("/v1/embeddings"),
        "imageToText" => Some("/v1/chat/completions"),
        "webSearch" => Some("/v1/search"),
        "webFetch" => Some("/v1/fetch"),
        _ => None,
    }
}

fn has_tts_voices_api(provider_id: &str) -> bool {
    matches!(
        provider_id,
        "elevenlabs" | "edge-tts" | "deepgram" | "inworld" | "local-device"
    )
}

pub fn lookup_model_info(full_id: &str, requested_kind: Option<&str>) -> Option<Value> {
    let (alias, model_id) = full_id.split_once('/')?;
    if alias.is_empty() || model_id.is_empty() {
        return None;
    }

    let provider = find_registry_provider(alias);
    let provider_id = provider
        .map(|p| p.id.as_str())
        .unwrap_or_else(|| alias);

    if let Some(p) = provider {
        let matched = if let Some(req_k) = requested_kind.filter(|s| !s.is_empty()) {
            p.models
                .iter()
                .find(|m| m.id == model_id && m.kind.as_deref().unwrap_or("llm") == req_k)
        } else {
            p.models.iter().find(|m| m.id == model_id)
        };

        if let Some(m) = matched {
            let kind = m.kind.as_deref().unwrap_or("llm");
            let mut out = json!({
                "id": format!("{}/{}", alias, m.id),
                "name": m.name.as_deref().unwrap_or(&m.id),
                "kind": kind,
                "owned_by": alias,
                "endpoint": kind_endpoint(kind),
            });
            if let Some(obj) = out.as_object_mut() {
                if let Some(v) = &m.params {
                    obj.insert("params".to_string(), v.clone());
                }
                if let Some(v) = &m.capabilities {
                    obj.insert("capabilities".to_string(), v.clone());
                }
                if let Some(v) = &m.options {
                    obj.insert("options".to_string(), v.clone());
                }
                if let Some(v) = &m.dimensions {
                    obj.insert("dimensions".to_string(), v.clone());
                }
                if let Some(cw) = m.context_window {
                    obj.insert("contextWindow".to_string(), json!(cw));
                }
                if kind == "tts" && has_tts_voices_api(provider_id) {
                    obj.insert(
                        "voicesUrl".to_string(),
                        json!(format!("/v1/audio/voices?provider={}", provider_id)),
                    );
                }
            }
            return Some(out);
        }

        if model_id == "search" {
            if let Some(sc) = &p.search_config {
                let mut out = json!({
                    "id": format!("{}/search", alias),
                    "name": format!("{} Search", p.name),
                    "kind": "webSearch",
                    "owned_by": alias,
                    "endpoint": "/v1/search",
                    "params": ["query", "max_results", "country", "language", "time_range", "domain_filter", "search_type"],
                });
                if let Some(obj) = out.as_object_mut() {
                    if let Some(v) = &sc.search_types {
                        obj.insert("searchTypes".to_string(), v.clone());
                    }
                    if let Some(v) = &sc.max_max_results {
                        obj.insert("maxResults".to_string(), v.clone());
                    }
                    if let Some(v) = &sc.required_options {
                        obj.insert("required".to_string(), v.clone());
                    }
                }
                return Some(out);
            }
        }

        if model_id == "fetch" && p.has_fetch {
            return Some(json!({
                "id": format!("{}/fetch", alias),
                "name": format!("{} Fetch", p.name),
                "kind": "webFetch",
                "owned_by": alias,
                "endpoint": "/v1/fetch",
                "params": ["url", "format", "max_characters"],
            }));
        }
    }

    None
}

pub async fn v1_models_info_options_handler() -> Response {
    (
        StatusCode::OK,
        [
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
            (header::ACCESS_CONTROL_ALLOW_METHODS, "GET, OPTIONS"),
        ],
        "",
    )
        .into_response()
}

pub async fn v1_models_info_handler(Query(query): Query<ModelsInfoQuery>) -> Response {
    let Some(id) = query.id.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
        return json_cors_response(
            StatusCode::BAD_REQUEST,
            json!({
                "error": {
                    "message": "Missing required query param: id (e.g. ?id=openai/dall-e-3)",
                    "type": "invalid_request_error",
                }
            }),
        );
    };

    match lookup_model_info(id, query.kind.as_deref()) {
        Some(info) => json_cors_response(StatusCode::OK, info),
        None => json_cors_response(
            StatusCode::NOT_FOUND,
            json!({
                "error": {
                    "message": format!("Model not found: {}", id),
                    "type": "not_found",
                }
            }),
        ),
    }
}

// ============================================================================
// 7. GET & OPTIONS /v1/models/{*model} (+ /api/v1/models/{*model})
// ============================================================================

pub async fn v1_models_wildcard_options_handler() -> Response {
    (
        StatusCode::OK,
        [
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
            (header::ACCESS_CONTROL_ALLOW_METHODS, "GET, OPTIONS"),
            (header::ACCESS_CONTROL_ALLOW_HEADERS, "*"),
        ],
        "",
    )
        .into_response()
}

fn extract_models_request_key(headers: &HeaderMap, uri: &Uri) -> Option<String> {
    if let Some(k) = auth::extract_api_key(headers) {
        return Some(k);
    }
    if let Some(query) = uri.query() {
        for pair in query.split('&') {
            if let Some((k, v)) = pair.split_once('=') {
                if k == "key" && !v.trim().is_empty() {
                    return Some(v.trim().to_string());
                }
            }
        }
    }
    None
}

fn models_list_response(status: StatusCode, body: Value, extra_build: Option<&str>) -> Response {
    let mut builder = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json; charset=utf-8")
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .header(header::CACHE_CONTROL, "private, no-cache, must-revalidate")
        .header(header::VARY, "Authorization, x-api-key, x-goog-api-key");

    if let Some(b) = extra_build {
        if let Ok(hv) = HeaderValue::from_str(b) {
            builder = builder.header("x-models-build", hv);
        }
    }

    builder
        .body(Body::from(serde_json::to_vec(&body).unwrap_or_default()))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn authenticate_models_request(
    headers: &HeaderMap,
    uri: &Uri,
    db: &Database,
) -> Result<ApiKey, Response> {
    let Some(offered) = extract_models_request_key(headers, uri) else {
        return Err(models_list_response(
            StatusCode::UNAUTHORIZED,
            json!({ "error": "API key required" }),
            None,
        ));
    };

    let Ok(Some(key_record)) = db.get_api_key_by_key(&offered) else {
        return Err(models_list_response(
            StatusCode::UNAUTHORIZED,
            json!({ "error": "Invalid API key" }),
            None,
        ));
    };

    if !key_record.is_active {
        return Err(models_list_response(
            StatusCode::UNAUTHORIZED,
            json!({ "error": "API key is disabled" }),
            None,
        ));
    }

    if !key_record.expires_at.trim().is_empty() {
        if let Ok(exp) = DateTime::parse_from_rfc3339(key_record.expires_at.trim()) {
            if exp.with_timezone(&Utc) <= Utc::now() {
                return Err(models_list_response(
                    StatusCode::UNAUTHORIZED,
                    json!({ "error": "API key has expired" }),
                    None,
                ));
            }
        }
    }

    Ok(key_record)
}

fn kind_slug_filter(slug: &str) -> Option<&'static [&'static str]> {
    match slug {
        "image" => Some(&["image"]),
        "tts" => Some(&["tts"]),
        "stt" => Some(&["stt"]),
        "embedding" => Some(&["embedding"]),
        "image-to-text" => Some(&["imageToText"]),
        "web" => Some(&["webSearch", "webFetch"]),
        _ => None,
    }
}

fn parse_allowed_list(raw: &str) -> Option<Vec<String>> {
    let items: Vec<String> = raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .collect();
    if items.is_empty() || items.iter().any(|s| s == "*") {
        None
    } else {
        Some(items)
    }
}

fn infer_kind_from_model_id(model_id: &str) -> &'static str {
    let lower = model_id.to_ascii_lowercase();
    if lower.contains("embed") {
        "embedding"
    } else if lower.contains("tts") || lower.contains("speech") || lower.contains("voice") {
        "tts"
    } else if lower.contains("image")
        || lower.contains("imagen")
        || lower.contains("dall-e")
        || lower.contains("dalle")
        || lower.contains("flux")
        || lower.contains("sdxl")
    {
        "image"
    } else {
        "llm"
    }
}

fn build_models_list_for_key(db: &Database, key: &ApiKey, kind_filter: &[&str]) -> Vec<Value> {
    let restricted = parse_allowed_list(&key.allowed_models);
    let force_model = key.force_model.trim();
    let force_provider = key.force_provider.trim();
    let force_restricted = restricted.is_none() && (!force_model.is_empty() || !force_provider.is_empty());

    let connections: Vec<ProviderConnection> = db
        .load_provider_connections()
        .unwrap_or_default()
        .into_iter()
        .filter(|c| c.is_active)
        .collect();
    let combos: Vec<Combo> = db.load_combos().unwrap_or_default();
    let custom_models = db.load_custom_models().unwrap_or_default();
    let model_aliases = db.load_model_aliases().unwrap_or_default();
    let disabled_by_alias = db.load_disabled_models().unwrap_or_default();
    let provider_nodes: Vec<ProviderNode> = db.load_provider_nodes().unwrap_or_default();

    let is_disabled = |alias: &str, model_id: &str| -> bool {
        disabled_by_alias
            .get(alias)
            .map(|list| list.iter().any(|x| x == model_id))
            .unwrap_or(false)
    };

    let mut combo_map: HashMap<String, &Combo> = HashMap::new();
    for c in &combos {
        combo_map.insert(c.name.clone(), c);
    }

    let reg = registry();
    let mut models = Vec::new();
    let mut emitted = HashSet::new();

    if restricted.is_some() || force_restricted {
        let mut wanted: Vec<String> = Vec::new();
        let mut seen_combo = HashSet::new();

        fn expand_combo(
            name: &str,
            combo_map: &HashMap<String, &Combo>,
            seen: &mut HashSet<String>,
            out: &mut Vec<String>,
        ) {
            if !seen.insert(name.to_string()) {
                return;
            }
            if let Some(c) = combo_map.get(name) {
                for m in &c.models {
                    if combo_map.contains_key(m) {
                        expand_combo(m, combo_map, seen, out);
                    } else {
                        out.push(m.clone());
                    }
                }
            }
        }

        if let Some(ref list) = restricted {
            for entry in list {
                if let Some(c) = combo_map.get(entry) {
                    let c_kind = c.kind.as_deref().unwrap_or("llm");
                    if !kind_filter.contains(&c_kind) {
                        continue;
                    }
                    expand_combo(entry, &combo_map, &mut seen_combo, &mut wanted);
                } else {
                    wanted.push(entry.clone());
                }
            }
        }

        if !force_model.is_empty() {
            if combo_map.contains_key(force_model) {
                wanted.clear();
                seen_combo.clear();
                expand_combo(force_model, &combo_map, &mut seen_combo, &mut wanted);
            } else {
                wanted = vec![force_model.to_string()];
            }
        } else if !force_provider.is_empty() {
            wanted.retain(|id| id.split('/').next() == Some(force_provider));
        }

        for raw_id in &wanted {
            if raw_id.trim().is_empty() {
                continue;
            }
            let full_id = model_aliases
                .get(raw_id)
                .map(|s| s.as_str())
                .unwrap_or(raw_id.as_str());
            let (alias, model_id) = match full_id.split_once('/') {
                Some((a, m)) => (a, m),
                None => (raw_id.as_str(), raw_id.as_str()),
            };
            if is_disabled(alias, model_id) {
                continue;
            }
            let kind = reg
                .static_kind_by_full_id
                .get(full_id)
                .map(|s| s.as_str())
                .unwrap_or_else(|| infer_kind_from_model_id(full_id));
            let in_scope = kind_filter.contains(&kind)
                || (kind == "imageToText" && kind_filter.contains(&"llm"));
            if !in_scope {
                continue;
            }
            if emitted.insert(full_id.to_string()) {
                models.push(json!({
                    "id": full_id,
                    "object": "model",
                    "owned_by": alias,
                }));
            }
            if model_aliases.contains_key(raw_id) && emitted.insert(raw_id.clone()) {
                models.push(json!({
                    "id": raw_id,
                    "object": "model",
                    "owned_by": alias,
                }));
            }
        }

        for cm in &custom_models {
            let Some(id) = cm.get("id").and_then(|v| v.as_str()) else {
                continue;
            };
            let pa = cm
                .get("providerAlias")
                .or_else(|| cm.get("provider"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let full_id = format!("{}/{}", pa, id);
            if !emitted.contains(&full_id)
                && (emitted.contains(id)
                    || wanted.iter().any(|w| w == &full_id || w == id))
            {
                emitted.insert(full_id.clone());
                models.push(json!({
                    "id": full_id,
                    "object": "model",
                    "owned_by": pa,
                }));
            }
        }

        return models;
    }

    // 1. Combos
    for c in &combos {
        let c_kind = c.kind.as_deref().unwrap_or("llm");
        if !kind_filter.contains(&c_kind) {
            continue;
        }
        if emitted.insert(c.name.clone()) {
            let mut obj = json!({
                "id": c.name,
                "object": "model",
                "owned_by": "combo",
            });
            if c_kind == "webSearch" || c_kind == "webFetch" {
                if let Some(m) = obj.as_object_mut() {
                    m.insert("kind".to_string(), json!(c_kind));
                }
            }
            models.push(obj);
        }
    }

    // 2. Active connections (+ noAuth free providers)
    let mut active_by_provider: HashMap<String, Option<&ProviderConnection>> = HashMap::new();
    for conn in &connections {
        active_by_provider
            .entry(conn.provider.clone())
            .or_insert(Some(conn));
    }
    for p in &reg.providers {
        if p.no_auth {
            active_by_provider.entry(p.id.clone()).or_insert(None);
        }
    }

    for (provider_id, conn_opt) in active_by_provider {
        let reg_prov = find_registry_provider(&provider_id);
        let custom_media_kind = conn_opt
            .and_then(|c| conn_psd(c))
            .and_then(|p| p.get("mediaKind"))
            .and_then(|v| v.as_str());

        let prov_matches = if let Some(mk) = custom_media_kind {
            kind_filter.contains(&mk)
        } else if let Some(rp) = reg_prov {
            let kinds = if rp.service_kinds.is_empty() {
                vec!["llm".to_string()]
            } else {
                rp.service_kinds.clone()
            };
            kind_filter.iter().any(|k| kinds.iter().any(|pk| pk == k))
        } else {
            kind_filter.contains(&"llm")
        };

        if !prov_matches {
            continue;
        }

        let static_alias = get_provider_alias(&provider_id);
        let node_prefix = provider_nodes
            .iter()
            .find(|n| n.id == provider_id || n.prefix == provider_id)
            .map(|n| n.prefix.clone())
            .filter(|s| !s.is_empty());
        let output_alias = conn_opt
            .and_then(|c| conn_str(c, "prefix"))
            .map(ToString::to_string)
            .or(node_prefix)
            .unwrap_or_else(|| static_alias.clone());

        // Static models for this provider
        if let Some(rp) = reg_prov {
            for m in &rp.models {
                let kind = custom_media_kind
                    .or(m.kind.as_deref())
                    .unwrap_or("llm");
                let allow_as_llm = kind == "imageToText" && kind_filter.contains(&"llm");
                if !kind_filter.contains(&kind) && !allow_as_llm {
                    continue;
                }
                if is_disabled(&output_alias, &m.id)
                    || is_disabled(&static_alias, &m.id)
                    || is_disabled(&provider_id, &m.id)
                {
                    continue;
                }
                let full_id = format!("{}/{}", output_alias, m.id);
                if emitted.insert(full_id.clone()) {
                    let mut entry = json!({
                        "id": full_id,
                        "object": "model",
                        "owned_by": output_alias,
                    });
                    if let Some(obj) = entry.as_object_mut() {
                        if let Some(caps) = &m.capabilities {
                            obj.insert("capabilities".to_string(), caps.clone());
                        }
                        if let Some(cw) = m.context_window {
                            obj.insert("context_length".to_string(), json!(cw));
                        }
                        if let Some(mo) = m.max_output {
                            obj.insert("max_completion_tokens".to_string(), json!(mo));
                        }
                    }
                    models.push(entry);
                }
            }

            if kind_filter.contains(&"webSearch") && rp.search_config.is_some() {
                let search_id = format!("{}/search", output_alias);
                if emitted.insert(search_id.clone()) {
                    models.push(json!({
                        "id": search_id,
                        "object": "model",
                        "kind": "webSearch",
                        "owned_by": output_alias,
                    }));
                }
            }
            if kind_filter.contains(&"webFetch") && rp.has_fetch {
                let fetch_id = format!("{}/fetch", output_alias);
                if emitted.insert(fetch_id.clone()) {
                    models.push(json!({
                        "id": fetch_id,
                        "object": "model",
                        "kind": "webFetch",
                        "owned_by": output_alias,
                    }));
                }
            }
        }

        // Configured defaultModel and models on connection
        if let Some(conn) = conn_opt {
            let mut extra_ids: Vec<String> = Vec::new();
            if let Some(dm) = conn.data.default_model.as_deref().map(str::trim) {
                if !dm.is_empty() {
                    extra_ids.push(dm.to_string());
                }
            }
            if let Some(arr) = conn_psd(conn)
                .and_then(|p| p.get("models").or_else(|| p.get("enabledModels")))
                .or_else(|| conn.data.extra.get("models"))
                .and_then(|v| v.as_array())
            {
                for item in arr {
                    if let Some(s) = item.as_str().map(str::trim).filter(|s| !s.is_empty()) {
                        extra_ids.push(s.to_string());
                    }
                }
            }
            for mid in extra_ids {
                let clean = mid
                    .strip_prefix(&format!("{}/", output_alias))
                    .or_else(|| mid.strip_prefix(&format!("{}/", static_alias)))
                    .or_else(|| mid.strip_prefix(&format!("{}/", provider_id)))
                    .unwrap_or(&mid);
                let kind = custom_media_kind.unwrap_or_else(|| infer_kind_from_model_id(clean));
                let allow_as_llm = kind == "imageToText" && kind_filter.contains(&"llm");
                if !kind_filter.contains(&kind) && !allow_as_llm {
                    continue;
                }
                if is_disabled(&output_alias, clean) || is_disabled(&provider_id, clean) {
                    continue;
                }
                let full_id = format!("{}/{}", output_alias, clean);
                if emitted.insert(full_id.clone()) {
                    models.push(json!({
                        "id": full_id,
                        "object": "model",
                        "owned_by": output_alias,
                    }));
                }
            }
        }

        // Custom models matching this provider
        for cm in &custom_models {
            let Some(raw_id) = cm.get("id").and_then(|v| v.as_str()).map(str::trim) else {
                continue;
            };
            if raw_id.is_empty() {
                continue;
            }
            let pa = cm
                .get("providerAlias")
                .or_else(|| cm.get("provider"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if pa != static_alias && pa != output_alias && pa != provider_id {
                continue;
            }
            let kind = cm
                .get("kind")
                .or_else(|| cm.get("type"))
                .and_then(|v| v.as_str())
                .unwrap_or("llm");
            let allow_as_llm = kind == "imageToText" && kind_filter.contains(&"llm");
            if !kind_filter.contains(&kind) && !allow_as_llm {
                continue;
            }
            let clean = raw_id
                .strip_prefix(&format!("{}/", output_alias))
                .or_else(|| raw_id.strip_prefix(&format!("{}/", static_alias)))
                .or_else(|| raw_id.strip_prefix(&format!("{}/", provider_id)))
                .unwrap_or(raw_id);
            if is_disabled(&output_alias, clean) || is_disabled(&provider_id, clean) {
                continue;
            }
            let full_id = format!("{}/{}", output_alias, clean);
            if emitted.insert(full_id.clone()) {
                models.push(json!({
                    "id": full_id,
                    "object": "model",
                    "owned_by": output_alias,
                }));
            }
        }
    }

    // 3. Emit alias entries
    let existing_models = models.clone();
    for (alias_key, target) in &model_aliases {
        if emitted.contains(alias_key) {
            continue;
        }
        if let Some(matched) = existing_models
            .iter()
            .find(|m| m.get("id").and_then(|v| v.as_str()) == Some(target.as_str()))
        {
            let mut alias_entry = matched.clone();
            if let Some(obj) = alias_entry.as_object_mut() {
                obj.insert("id".to_string(), json!(alias_key));
            }
            emitted.insert(alias_key.clone());
            models.push(alias_entry);
        }
    }

    models
}

pub async fn v1_models_wildcard_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    uri: Uri,
    Path(model_path): Path<String>,
) -> Response {
    let key_record = match authenticate_models_request(&headers, &uri, &state.db) {
        Ok(k) => k,
        Err(resp) => return resp,
    };

    let identifier = model_path.trim_matches('/').to_string();
    let is_single_segment = !identifier.is_empty() && !identifier.contains('/');
    let kind_filter = if is_single_segment {
        kind_slug_filter(&identifier)
    } else {
        None
    };

    if let Some(kf) = kind_filter {
        let data = build_models_list_for_key(&state.db, &key_record, kf);
        return models_list_response(
            StatusCode::OK,
            json!({
                "object": "list",
                "data": data,
            }),
            Some("fresh"),
        );
    }

    let llm_list = build_models_list_for_key(&state.db, &key_record, &["llm"]);
    if let Some(matched) = llm_list
        .into_iter()
        .find(|m| m.get("id").and_then(|v| v.as_str()) == Some(identifier.as_str()))
    {
        return models_list_response(StatusCode::OK, matched, None);
    }

    models_list_response(
        StatusCode::NOT_FOUND,
        json!({
            "error": {
                "message": format!(
                    "The model '{}' does not exist or you do not have access to it.",
                    identifier
                ),
                "type": "invalid_request_error",
                "code": "model_not_found",
            }
        }),
        None,
    )
}

// ============================================================================
// 8. POST & OPTIONS /v1/api/chat (+ /api/v1/api/chat) — Ollama NDJSON Bridge
// ============================================================================

pub async fn v1_api_chat_options_handler() -> Response {
    (
        StatusCode::OK,
        [
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
            (header::ACCESS_CONTROL_ALLOW_METHODS, "GET, POST, OPTIONS"),
            (header::ACCESS_CONTROL_ALLOW_HEADERS, "*"),
        ],
        "",
    )
        .into_response()
}

pub async fn v1_api_chat_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let model_name = serde_json::from_slice::<Value>(&body)
        .ok()
        .and_then(|v| v.get("model").and_then(|m| m.as_str()).map(ToString::to_string))
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "llama3.2".to_string());

    let inner_resp = chat_completions_handler(State(state), headers, body).await;
    transform_response_to_ollama(inner_resp, model_name).await
}

#[derive(Default)]
struct PendingOllamaToolCall {
    name: String,
    arguments: String,
}

fn process_sse_data_line_to_ollama(
    data: &str,
    model: &str,
    pending_tools: &mut HashMap<u64, PendingOllamaToolCall>,
    emitted_done: &mut bool,
) -> Vec<String> {
    let mut lines_out = Vec::new();
    let trimmed = data.trim();
    if trimmed.is_empty() {
        return lines_out;
    }

    if trimmed == "[DONE]" {
        if !*emitted_done {
            *emitted_done = true;
            let end_obj = json!({
                "model": model,
                "message": { "role": "assistant", "content": "" },
                "done": true,
            });
            lines_out.push(format!("{}\n", end_obj));
        }
        return lines_out;
    }

    let Ok(parsed) = serde_json::from_str::<Value>(trimmed) else {
        return lines_out;
    };

    let choice0 = parsed
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first());
    let delta = choice0.and_then(|c| c.get("delta"));

    if let Some(tool_calls) = delta
        .and_then(|d| d.get("tool_calls"))
        .and_then(|v| v.as_array())
    {
        for tc in tool_calls {
            let idx = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
            let entry = pending_tools.entry(idx).or_default();
            if let Some(func) = tc.get("function") {
                if let Some(n) = func.get("name").and_then(|v| v.as_str()) {
                    entry.name.push_str(n);
                }
                if let Some(args) = func.get("arguments").and_then(|v| v.as_str()) {
                    entry.arguments.push_str(args);
                }
            }
        }
    }

    if let Some(content) = delta
        .and_then(|d| d.get("content"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        let obj = json!({
            "model": model,
            "message": { "role": "assistant", "content": content },
            "done": false,
        });
        lines_out.push(format!("{}\n", obj));
    }

    let finish_reason = choice0
        .and_then(|c| c.get("finish_reason"))
        .and_then(|v| v.as_str());

    if matches!(finish_reason, Some("tool_calls") | Some("stop")) {
        if !pending_tools.is_empty() {
            let mut keys: Vec<u64> = pending_tools.keys().copied().collect();
            keys.sort_unstable();
            let formatted_calls: Vec<Value> = keys
                .into_iter()
                .filter_map(|k| pending_tools.remove(&k))
                .map(|tc| {
                    let args_val: Value = serde_json::from_str(&tc.arguments)
                        .unwrap_or_else(|_| json!({}));
                    json!({
                        "function": {
                            "name": tc.name,
                            "arguments": args_val,
                        }
                    })
                })
                .collect();
            *emitted_done = true;
            let obj = json!({
                "model": model,
                "message": {
                    "role": "assistant",
                    "content": "",
                    "tool_calls": formatted_calls,
                },
                "done": true,
            });
            lines_out.push(format!("{}\n", obj));
        } else if finish_reason == Some("stop") && !*emitted_done {
            *emitted_done = true;
            let obj = json!({
                "model": model,
                "message": { "role": "assistant", "content": "" },
                "done": true,
            });
            lines_out.push(format!("{}\n", obj));
        }
    }

    lines_out
}

pub async fn transform_response_to_ollama(resp: Response, model: String) -> Response {
    let status = resp.status();
    let content_type = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();

    if !status.is_success() {
        let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap_or_default();
        return Response::builder()
            .status(status)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
            .body(Body::from(bytes))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }

    // Non-streaming JSON response -> single Ollama NDJSON message
    if content_type.contains("application/json") {
        let bytes = axum::body::to_bytes(resp.into_body(), 16 * 1024 * 1024)
            .await
            .unwrap_or_default();
        let mut message = json!({ "role": "assistant", "content": "" });
        if let Ok(parsed) = serde_json::from_slice::<Value>(&bytes) {
            if let Some(msg) = parsed
                .get("choices")
                .and_then(|c| c.as_array())
                .and_then(|a| a.first())
                .and_then(|c| c.get("message"))
            {
                let content = msg.get("content").and_then(|v| v.as_str()).unwrap_or("");
                if let Some(tc_arr) = msg.get("tool_calls").and_then(|v| v.as_array()) {
                    let formatted: Vec<Value> = tc_arr
                        .iter()
                        .filter_map(|tc| {
                            let f = tc.get("function")?;
                            let name = f.get("name").and_then(|v| v.as_str())?;
                            let args_raw = f.get("arguments").and_then(|v| v.as_str()).unwrap_or("{}");
                            let args: Value = serde_json::from_str(args_raw).unwrap_or_else(|_| json!({}));
                            Some(json!({ "function": { "name": name, "arguments": args } }))
                        })
                        .collect();
                    if !formatted.is_empty() {
                        message = json!({
                            "role": "assistant",
                            "content": content,
                            "tool_calls": formatted,
                        });
                    } else {
                        message = json!({ "role": "assistant", "content": content });
                    }
                } else {
                    message = json!({ "role": "assistant", "content": content });
                }
            }
        }
        let line = format!(
            "{}\n",
            json!({ "model": model, "message": message, "done": true })
        );
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/x-ndjson")
            .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
            .body(Body::from(line))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }

    // Streaming SSE -> Ollama NDJSON stream
    let byte_stream = resp.into_body().into_data_stream();
    let out_stream = stream! {
        let mut buffer = String::new();
        let mut pending_tools: HashMap<u64, PendingOllamaToolCall> = HashMap::new();
        let mut emitted_done = false;
        futures_util::pin_mut!(byte_stream);

        while let Some(chunk_res) = byte_stream.next().await {
            let Ok(chunk) = chunk_res else {
                break;
            };
            buffer.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(pos) = buffer.find('\n') {
                let line = buffer[..pos].trim_end_matches('\r').to_string();
                buffer.drain(..=pos);
                if let Some(data) = line.strip_prefix("data:") {
                    for out_line in process_sse_data_line_to_ollama(data, &model, &mut pending_tools, &mut emitted_done) {
                        yield Ok::<_, Infallible>(axum::body::Bytes::from(out_line));
                    }
                }
            }
        }

        if let Some(data) = buffer.trim().strip_prefix("data:") {
            for out_line in process_sse_data_line_to_ollama(data, &model, &mut pending_tools, &mut emitted_done) {
                yield Ok::<_, Infallible>(axum::body::Bytes::from(out_line));
            }
        }

        if !emitted_done {
            let end_line = format!(
                "{}\n",
                json!({ "model": model, "message": { "role": "assistant", "content": "" }, "done": true })
            );
            yield Ok::<_, Infallible>(axum::body::Bytes::from(end_line));
        }
    };

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/x-ndjson")
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .body(Body::from_stream(out_stream))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

// ============================================================================
// 9-12. Translator Handlers (load / save / send / translate)
// Note: Exposed as pub handlers & tested here; NOT mounted in usage_extra_router()
// because sso_misc_api.rs already registers /api/translator/{load,save,send,translate}
// (per swarm board #186/#187 to prevent Axum .merge() route collision).
// ============================================================================

fn translator_logs_dir() -> PathBuf {
    if let Ok(custom) = std::env::var("NINEROUTER_TRANSLATOR_LOGS_DIR") {
        let trimmed = custom.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("logs")
        .join("translator")
}

#[derive(Debug, Deserialize, Default)]
pub struct TranslatorLoadQuery {
    pub file: Option<String>,
}

pub async fn translator_load_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<TranslatorLoadQuery>,
) -> Response {
    if let Err(resp) = auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let Some(file) = query.file.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
        return json_response(
            StatusCode::BAD_REQUEST,
            json!({ "success": false, "error": "File parameter required" }),
        );
    };

    if !TRANSLATOR_ALLOWED_FILES.contains(&file) {
        return json_response(
            StatusCode::BAD_REQUEST,
            json!({ "success": false, "error": "Invalid file name" }),
        );
    }

    let file_path = translator_logs_dir().join(file);
    if !file_path.exists() {
        return json_response(
            StatusCode::NOT_FOUND,
            json!({ "success": false, "error": "File not found" }),
        );
    }

    match std::fs::read_to_string(&file_path) {
        Ok(content) => json_response(StatusCode::OK, json!({ "success": true, "content": content })),
        Err(e) => json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({ "success": false, "error": e.to_string() }),
        ),
    }
}

pub async fn translator_save_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if let Err(resp) = auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let payload: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({ "success": false, "error": e.to_string() }),
            );
        }
    };

    let file = payload.get("file").and_then(|v| v.as_str()).unwrap_or("");
    let content = payload.get("content").and_then(|v| v.as_str());
    if file.is_empty() || content.is_none() {
        return json_response(
            StatusCode::BAD_REQUEST,
            json!({ "success": false, "error": "File and content required" }),
        );
    }

    if !TRANSLATOR_ALLOWED_FILES.contains(&file) {
        return json_response(
            StatusCode::BAD_REQUEST,
            json!({ "success": false, "error": "Invalid file name" }),
        );
    }

    let dir = translator_logs_dir();
    if !dir.exists() {
        if let Err(e) = std::fs::create_dir_all(&dir) {
            return json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({ "success": false, "error": e.to_string() }),
            );
        }
    }

    let file_path = dir.join(file);
    match std::fs::write(&file_path, content.unwrap_or_default()) {
        Ok(()) => json_response(StatusCode::OK, json!({ "success": true })),
        Err(e) => json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({ "success": false, "error": e.to_string() }),
        ),
    }
}

pub fn detect_request_format(body: &Value) -> &'static str {
    if let Some(input) = body.get("input") {
        if (input.is_array() || input.is_string()) && body.get("messages").is_none() {
            return "openai-responses";
        }
    }
    if body
        .get("request")
        .and_then(|r| r.get("contents"))
        .is_some()
        && body.get("userAgent").and_then(|v| v.as_str()) == Some("antigravity")
    {
        return "antigravity";
    }
    if body.get("contents").and_then(|v| v.as_array()).is_some() {
        return "gemini";
    }
    if body.get("stream_options").is_some()
        || body.get("response_format").is_some()
        || body.get("logprobs").is_some()
        || body.get("top_logprobs").is_some()
        || body.get("n").is_some()
        || body.get("presence_penalty").is_some()
        || body.get("frequency_penalty").is_some()
        || body.get("logit_bias").is_some()
        || body.get("user").is_some()
    {
        return "openai";
    }
    if let Some(msgs) = body.get("messages").and_then(|v| v.as_array()) {
        if let Some(content) = msgs.first().and_then(|m| m.get("content")).and_then(|c| c.as_array()) {
            let first_type = content.first().and_then(|c| c.get("type")).and_then(|v| v.as_str());
            if first_type == Some("text")
                && !body
                    .get("model")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .contains('/')
            {
                if body.get("system").is_some() || body.get("anthropic_version").is_some() {
                    return "claude";
                }
                if content.iter().any(|c| {
                    c.get("type").and_then(|v| v.as_str()) == Some("image")
                        && c.get("source").and_then(|s| s.get("type")).and_then(|v| v.as_str()) == Some("base64")
                }) {
                    return "claude";
                }
                if content.iter().any(|c| {
                    c.get("type").and_then(|v| v.as_str()) == Some("image_url")
                        && c.get("image_url").and_then(|s| s.get("url")).is_some()
                }) {
                    return "openai";
                }
                if content.iter().any(|c| {
                    matches!(
                        c.get("type").and_then(|v| v.as_str()),
                        Some("tool_use") | Some("tool_result")
                    )
                }) {
                    return "claude";
                }
            }
        }
        if body.get("system").is_some() || body.get("anthropic_version").is_some() {
            return "claude";
        }
    }
    "openai"
}

pub fn get_target_format_for_provider(provider: &str, conn: Option<&ProviderConnection>) -> &'static str {
    let p = provider.trim().to_ascii_lowercase();
    if p.starts_with("openai-compatible-") {
        let api_type = conn
            .and_then(|c| conn_str(c, "apiType"))
            .unwrap_or_else(|| if p.contains("responses") { "responses" } else { "chat" });
        return if api_type == "responses" {
            "openai-responses"
        } else {
            "openai"
        };
    }
    if p.starts_with("anthropic-compatible-") || p == "claude" || p == "anthropic" {
        return "claude";
    }
    if p == "codex" {
        return "openai-responses";
    }
    if p == "gemini" || p == "google" {
        return "gemini";
    }
    if p == "gemini-cli" {
        return "gemini-cli";
    }
    if p == "antigravity" {
        return "antigravity";
    }
    if p == "vertex" || p == "vertex-partner" {
        return "vertex";
    }
    if p == "kiro" {
        return "kiro";
    }
    if p == "cursor" {
        return "cursor";
    }
    "openai"
}

fn resolve_translator_model_info(db: &Database, raw_model: &str) -> (String, String) {
    let mut candidate = raw_model.trim().to_string();
    if let Ok(display_names) = db.load_model_display_names() {
        for (full_id, disp) in display_names {
            if disp == candidate {
                candidate = full_id;
                break;
            }
        }
    }
    if !candidate.contains('/') {
        if let Ok(aliases) = db.load_model_aliases() {
            if let Some(mapped) = aliases.get(&candidate) {
                candidate = mapped.clone();
            }
        }
    }
    if let Some((prefix, model)) = candidate.split_once('/') {
        let resolved_id = resolve_provider_id(prefix);
        return (resolved_id, model.to_string());
    }
    let lower = candidate.to_ascii_lowercase();
    let provider = if lower.starts_with("claude") {
        "claude"
    } else if lower.contains("gemini") {
        "gemini"
    } else {
        "openai"
    };
    (provider.to_string(), candidate)
}

fn build_translator_target_url_and_headers(
    provider: &str,
    model: &str,
    stream: bool,
    conn: &ProviderConnection,
) -> (String, serde_json::Map<String, Value>) {
    let p = provider.to_ascii_lowercase();
    let custom_base = conn.data.base_url.as_deref().or_else(|| conn_str(conn, "baseUrl"));
    let url = if let Some(b) = custom_base {
        let norm = b.trim_end_matches('/');
        if norm.ends_with("/chat/completions") || norm.ends_with("/messages") || norm.ends_with("/responses") {
            norm.to_string()
        } else if p.starts_with("anthropic-compatible-") || p == "claude" || p == "anthropic" {
            format!("{}/messages", norm)
        } else if p.starts_with("openai-compatible-") && get_target_format_for_provider(provider, Some(conn)) == "openai-responses" {
            format!("{}/responses", norm)
        } else {
            format!("{}/chat/completions", norm)
        }
    } else {
        match p.as_str() {
            "claude" | "anthropic" => "https://api.anthropic.com/v1/messages".to_string(),
            "codex" => "https://chatgpt.com/backend-api/codex/responses".to_string(),
            "gemini" | "google" => {
                let action = if stream { "streamGenerateContent?alt=sse" } else { "generateContent" };
                format!("https://generativelanguage.googleapis.com/v1beta/models/{}:{}", model, action)
            }
            "deepseek" => "https://api.deepseek.com/chat/completions".to_string(),
            "groq" => "https://api.groq.com/openai/v1/chat/completions".to_string(),
            "openrouter" => "https://openrouter.ai/api/v1/chat/completions".to_string(),
            _ => "https://api.openai.com/v1/chat/completions".to_string(),
        }
    };

    let mut hdrs = serde_json::Map::new();
    hdrs.insert("Content-Type".to_string(), json!("application/json"));
    if stream {
        hdrs.insert("Accept".to_string(), json!("text/event-stream"));
    }

    let api_key = conn_api_key(conn);
    let access_token = conn_access_token(conn);
    if p == "claude" || p == "anthropic" || p.starts_with("anthropic-compatible-") {
        hdrs.insert("anthropic-version".to_string(), json!("2023-06-01"));
        if let Some(k) = api_key {
            hdrs.insert("x-api-key".to_string(), json!(k));
        } else if let Some(t) = access_token {
            hdrs.insert("Authorization".to_string(), json!(format!("Bearer {}", t)));
        }
    } else if p == "gemini" || p == "google" {
        if let Some(k) = api_key {
            hdrs.insert("x-goog-api-key".to_string(), json!(k));
        } else if let Some(t) = access_token {
            hdrs.insert("Authorization".to_string(), json!(format!("Bearer {}", t)));
        }
    } else if let Some(t) = access_token {
        hdrs.insert("Authorization".to_string(), json!(format!("Bearer {}", t)));
    } else if let Some(k) = api_key {
        hdrs.insert("Authorization".to_string(), json!(format!("Bearer {}", k)));
    }

    (url, hdrs)
}

pub async fn translator_translate_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if let Err(resp) = auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let payload: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({ "success": false, "error": e.to_string() }),
            );
        }
    };

    let step = payload.get("step").and_then(|v| v.as_u64());
    let req_body = payload.get("body");
    let (Some(step), Some(req_body)) = (step, req_body) else {
        return json_response(
            StatusCode::BAD_REQUEST,
            json!({ "success": false, "error": "Step and body required" }),
        );
    };

    match step {
        1 => {
            let client_body = req_body.get("body").unwrap_or(req_body);
            let raw_model = client_body
                .get("model")
                .and_then(|v| v.as_str())
                .unwrap_or("gpt-4o");
            let (provider, model) = resolve_translator_model_info(&state.db, raw_model);
            let source_format = detect_request_format(client_body);
            let target_format = get_target_format_for_provider(&provider, None);
            json_response(
                StatusCode::OK,
                json!({
                    "success": true,
                    "result": {
                        "provider": provider,
                        "model": model,
                        "sourceFormat": source_format,
                        "targetFormat": target_format,
                    }
                }),
            )
        }
        2 => {
            let client_body = req_body.get("body").unwrap_or(req_body);
            let raw_model = client_body
                .get("model")
                .and_then(|v| v.as_str())
                .unwrap_or("gpt-4o");
            let (_provider, model) = resolve_translator_model_info(&state.db, raw_model);
            let source_format = detect_request_format(client_body);
            let stream = client_body
                .get("stream")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);

            let mut out = match source_format {
                "claude" => {
                    let mut c_val = client_body.clone();
                    let had_max_tokens = c_val.get("max_tokens").is_some();
                    if !had_max_tokens {
                        if let Some(obj) = c_val.as_object_mut() {
                            obj.insert("max_tokens".to_string(), json!(4096));
                        }
                    }
                    match serde_json::from_value::<ClaudeMessageRequest>(c_val) {
                        Ok(mut claude_req) => {
                            claude_req.model = model;
                            claude_req.stream = Some(stream);
                            let mut openai_req = claude_to_openai_request(&claude_req);
                            if !had_max_tokens {
                                openai_req.max_tokens = None;
                            }
                            serde_json::to_value(openai_req).unwrap_or_else(|_| client_body.clone())
                        }
                        Err(_) => client_body.clone(),
                    }
                }
                "openai-responses" => {
                    let mut normalized = client_body.clone();
                    normalize_responses_request(&mut normalized);
                    if let Ok(resp_req) = serde_json::from_value(normalized) {
                        match responses_to_chat_request(&resp_req) {
                            Ok((mut chat_req, _ctx)) => {
                                chat_req.model = model;
                                chat_req.stream = Some(stream);
                                serde_json::to_value(chat_req).unwrap_or_else(|_| client_body.clone())
                            }
                            Err(_) => client_body.clone(),
                        }
                    } else {
                        client_body.clone()
                    }
                }
                _ => {
                    let mut cloned = client_body.clone();
                    if let Some(obj) = cloned.as_object_mut() {
                        obj.insert("model".to_string(), json!(model));
                        obj.insert("stream".to_string(), json!(stream));
                    }
                    cloned
                }
            };

            if let Some(obj) = out.as_object_mut() {
                obj.remove("_toolNameMap");
            }

            json_response(
                StatusCode::OK,
                json!({ "success": true, "result": { "body": out } }),
            )
        }
        3 => {
            let openai_body = req_body.get("body").unwrap_or(req_body);
            let provider = req_body
                .get("provider")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let model = req_body
                .get("model")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            if provider.is_empty() || model.is_empty() {
                return json_response(
                    StatusCode::BAD_REQUEST,
                    json!({ "success": false, "error": "provider and model required" }),
                );
            }

            let conn = state
                .db
                .load_provider_connections()
                .ok()
                .and_then(|list| {
                    list.into_iter()
                        .find(|c| c.is_active && c.provider.eq_ignore_ascii_case(provider))
                });

            let Some(conn) = conn else {
                return json_response(
                    StatusCode::BAD_REQUEST,
                    json!({ "success": false, "error": format!("No active connection for provider: {}", provider) }),
                );
            };

            let target_format = get_target_format_for_provider(provider, Some(&conn));
            let stream = openai_body
                .get("stream")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);

            let mut translated = match target_format {
                "claude" => {
                    match serde_json::from_value::<ChatCompletionRequest>(openai_body.clone()) {
                        Ok(mut chat_req) => {
                            chat_req.model = model.to_string();
                            chat_req.stream = Some(stream);
                            let claude_req = openai_to_claude_request(&chat_req);
                            serde_json::to_value(claude_req).unwrap_or_else(|_| openai_body.clone())
                        }
                        Err(_) => openai_body.clone(),
                    }
                }
                _ => {
                    let mut cloned = openai_body.clone();
                    if let Some(obj) = cloned.as_object_mut() {
                        obj.insert("model".to_string(), json!(model));
                        obj.insert("stream".to_string(), json!(stream));
                    }
                    cloned
                }
            };
            if let Some(obj) = translated.as_object_mut() {
                obj.remove("_toolNameMap");
            }

            let (url, hdrs) = build_translator_target_url_and_headers(provider, model, stream, &conn);
            json_response(
                StatusCode::OK,
                json!({
                    "success": true,
                    "result": {
                        "url": url,
                        "headers": hdrs,
                        "body": translated,
                    }
                }),
            )
        }
        _ => json_response(
            StatusCode::BAD_REQUEST,
            json!({ "success": false, "error": "Invalid step (1-3)" }),
        ),
    }
}

pub async fn translator_send_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if let Err(resp) = auth::verify_internal_auth(&headers, &state.config.jwt_secret, &state.db) {
        return resp;
    }

    let payload: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({ "success": false, "error": e.to_string() }),
            );
        }
    };

    let provider = payload
        .get("provider")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let model = payload.get("model").and_then(|v| v.as_str()).unwrap_or("");
    let send_body = payload.get("body");
    if provider.is_empty() || model.is_empty() || send_body.is_none() {
        return json_response(
            StatusCode::BAD_REQUEST,
            json!({ "success": false, "error": "provider, model, and body required" }),
        );
    }

    let Some(mut conn) = state
        .db
        .load_provider_connections()
        .ok()
        .and_then(|list| {
            list.into_iter()
                .find(|c| c.is_active && c.provider.eq_ignore_ascii_case(provider))
        })
    else {
        return json_response(
            StatusCode::BAD_REQUEST,
            json!({ "success": false, "error": format!("No active connection for provider: {}", provider) }),
        );
    };

    let send_val = send_body.unwrap();
    let stream_flag = send_val
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let client = build_usage_http_client(&state, &conn);
    let send_once = |c: &ProviderConnection| {
        let (url, hdrs_map) = build_translator_target_url_and_headers(provider, model, stream_flag, c);
        let mut req = client.post(&url).json(send_val);
        for (k, v) in hdrs_map {
            if let Some(vs) = v.as_str() {
                req = req.header(k.as_str(), vs);
            }
        }
        req.timeout(Duration::from_secs(60)).send()
    };

    let mut resp = match send_once(&conn).await {
        Ok(r) => r,
        Err(e) => {
            return json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({ "success": false, "error": e.to_string() }),
            );
        }
    };

    if (resp.status() == reqwest::StatusCode::UNAUTHORIZED
        || resp.status() == reqwest::StatusCode::FORBIDDEN)
        && extract_refresh_token(&conn).is_some()
    {
        if let Ok(updated) = refresh_connection_if_needed(&state, &conn, true).await {
            conn = updated;
            if let Ok(retried) = send_once(&conn).await {
                resp = retried;
            }
        }
    }

    if !resp.status().is_success() {
        let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
        let err_text = resp.text().await.unwrap_or_default();
        return json_response(
            status,
            json!({
                "success": false,
                "error": format!("Provider error: {}", status.as_u16()),
                "details": err_text,
            }),
        );
    }

    let upstream_stream = resp.bytes_stream();
    let body_stream = stream! {
        futures_util::pin_mut!(upstream_stream);
        while let Some(chunk) = upstream_stream.next().await {
            match chunk {
                Ok(b) => yield Ok::<_, Infallible>(b),
                Err(_) => break,
            }
        }
    };

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::CONNECTION, "keep-alive")
        .body(Body::from_stream(body_stream))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

// ============================================================================
// Router Export
// ============================================================================

pub fn usage_extra_router() -> Router<AppState> {
    Router::new()
        // usage/* routes
        .route(
            "/api/usage/history",
            get(usage_history_handler).options(options_handler),
        )
        .route(
            "/api/usage/api-keys/{id}",
            get(usage_api_key_by_id_handler).options(options_handler),
        )
        .route(
            "/api/usage/{connectionId}",
            get(usage_connection_handler).options(options_handler),
        )
        .route(
            "/api/usage/{connectionId}/claude-reset",
            post(usage_claude_reset_handler).options(options_handler),
        )
        .route(
            "/api/usage/{connectionId}/codex-reset-credits",
            get(usage_codex_reset_credits_get_handler)
                .post(usage_codex_reset_credits_post_handler)
                .options(options_handler),
        )
        // v1/* routes (+ /api/v1/* aliases)
        .route(
            "/v1/models/info",
            get(v1_models_info_handler).options(v1_models_info_options_handler),
        )
        .route(
            "/api/v1/models/info",
            get(v1_models_info_handler).options(v1_models_info_options_handler),
        )
        .route(
            "/v1/models/{*model}",
            get(v1_models_wildcard_handler).options(v1_models_wildcard_options_handler),
        )
        .route(
            "/api/v1/models/{*model}",
            get(v1_models_wildcard_handler).options(v1_models_wildcard_options_handler),
        )
        .route(
            "/v1/api/chat",
            post(v1_api_chat_handler).options(v1_api_chat_options_handler),
        )
        .route(
            "/api/v1/api/chat",
            post(v1_api_chat_handler).options(v1_api_chat_options_handler),
        )
}

// ============================================================================
// Unit & Router Integration Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::db::ConnectionData;
    use std::sync::Arc;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn test_state() -> (AppState, String) {
        let db = Database::open_in_memory().expect("open in-memory db");
        let raw_key = "sk_test_usage_extra_123".to_string();
        let api_key = ApiKey {
            id: "key-usage-extra".to_string(),
            name: Some("Usage Extra Key".to_string()),
            key: raw_key.clone(),
            created_at: Utc::now().to_rfc3339(),
            ..ApiKey::default()
        };
        db.save_api_key(&api_key).expect("save api key");

        let config = Arc::new(Config::default());
        let proxy = Arc::new(crate::proxy::ProxyService::new());
        let original = crate::original::Original::empty();
        let combo_tracker = crate::combo_router::ComboTracker::new();
        let token_cache = crate::cache::TokenCache::default();
        let circuit_breaker = Arc::clone(&combo_tracker.circuit_breakers);
        let console_buffer = crate::routes::ConsoleLogBuffer::new();
        let rate_limiter = Arc::new(crate::ratelimit::RateLimiter::from_config(&config));

        let state = AppState {
            config,
            db,
            proxy,
            original,
            combo_tracker,
            token_cache,
            circuit_breaker,
            console_buffer,
            rate_limiter,
        };
        (state, raw_key)
    }

    fn insert_usage_row(db: &Database, api_key: &str, ts: &str, pt: i64, ct: i64, cost: f64) {
        let guard = db.writer();
        guard
            .execute(
                "INSERT INTO usageHistory(timestamp, provider, model, connectionId, apiKey, apiKeyId, promptTokens, completionTokens, cachedTokens, cost, status, endpoint) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                rusqlite::params![
                    ts,
                    "openai",
                    "gpt-4o",
                    "conn-1",
                    api_key,
                    "key-usage-extra",
                    pt,
                    ct,
                    0i64,
                    cost,
                    "ok",
                    "/v1/chat/completions"
                ],
            )
            .expect("insert usage row");
    }

    #[tokio::test]
    async fn test_usage_history_router_endpoint() {
        let (state, key) = test_state();
        insert_usage_row(&state.db, &key, &Utc::now().to_rfc3339(), 120, 45, 0.012);

        let app = usage_extra_router().with_state(state);
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/api/usage/history")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(res.into_body(), 65536).await.unwrap();
        let val: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(val["totalRequests"], 1);
        assert_eq!(val["totalPromptTokens"], 120);
        assert_eq!(val["totalCompletionTokens"], 45);
        assert!(val["byApiKey"].as_object().map(|m| !m.is_empty()).unwrap_or(false));
        assert!(val["byEndpoint"].as_object().map(|m| !m.is_empty()).unwrap_or(false));
        assert_eq!(val["last10Minutes"].as_array().map(|a| a.len()), Some(10));
    }

    #[tokio::test]
    async fn test_usage_api_key_by_id_validation_and_rows() {
        let (state, key) = test_state();
        insert_usage_row(&state.db, &key, "2026-11-25T10:00:00.000Z", 100, 50, 0.25);
        insert_usage_row(&state.db, &key, "2026-11-25T14:00:00.000Z", 200, 75, 0.50);

        let app = usage_extra_router().with_state(state);

        // 404 for unknown key id
        let not_found = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/usage/api-keys/missing-id")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(not_found.status(), StatusCode::NOT_FOUND);

        // 400 for invalid date
        let bad_date = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/usage/api-keys/key-usage-extra?from=not-a-date")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(bad_date.status(), StatusCode::BAD_REQUEST);

        // 400 for from > to
        let bad_range = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/usage/api-keys/key-usage-extra?from=2026-11-26&to=2026-11-25")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(bad_range.status(), StatusCode::BAD_REQUEST);

        // 200 with aggregated daily row
        let ok_res = app
            .oneshot(
                Request::builder()
                    .uri("/api/usage/api-keys/key-usage-extra?from=2026-11-25&to=2026-11-25")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(ok_res.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(ok_res.into_body(), 65536).await.unwrap();
        let val: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(val["keyId"], "key-usage-extra");
        assert_eq!(val["from"], "2026-11-25T00:00:00.000Z");
        assert_eq!(val["to"], "2026-11-25T23:59:59.999Z");
        let rows = val["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["day"], "2026-11-25");
        assert_eq!(rows[0]["requests"], 2);
        assert_eq!(rows[0]["tokensIn"], 300);
        assert_eq!(rows[0]["tokensOut"], 125);
    }

    #[tokio::test]
    async fn test_usage_connection_and_reset_validation_paths() {
        let (state, _key) = test_state();
        let mut conn = ProviderConnection {
            id: "conn-openai-1".to_string(),
            provider: "openai".to_string(),
            auth_type: "apikey".to_string(),
            name: Some("OpenAI Key".to_string()),
            email: None,
            priority: Some(1),
            is_active: true,
            data: ConnectionData {
                api_key: Some("sk-openai-123".to_string()),
                ..Default::default()
            },
            raw_data: String::new(),
            created_at: Utc::now().to_rfc3339(),
            updated_at: Utc::now().to_rfc3339(),
        };
        state.db.save_provider_connection(&conn).unwrap();

        conn.id = "conn-claude-api".to_string();
        conn.provider = "claude".to_string();
        state.db.save_provider_connection(&conn).unwrap();

        let app = usage_extra_router().with_state(state);

        // 404 unknown connection
        let r404 = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/usage/does-not-exist")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r404.status(), StatusCode::NOT_FOUND);

        // Non-eligible apikey provider returns 200 message
        let r_unavail = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/usage/conn-openai-1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_unavail.status(), StatusCode::OK);
        let b = axum::body::to_bytes(r_unavail.into_body(), 65536).await.unwrap();
        let v: Value = serde_json::from_slice(&b).unwrap();
        assert_eq!(v["message"], "Usage not available for this connection");

        // Claude reset rejects non-OAuth connection with 400
        let r_claude_400 = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/usage/conn-claude-api/claude-reset")
                    .body(Body::from(r#"{"grantId":"grant_1"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_claude_400.status(), StatusCode::BAD_REQUEST);

        // Codex reset credits rejects non-codex connection with 400
        let r_codex_get_400 = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/usage/conn-openai-1/codex-reset-credits")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_codex_get_400.status(), StatusCode::BAD_REQUEST);

        let r_codex_post_400 = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/usage/conn-openai-1/codex-reset-credits")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r_codex_post_400.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn test_usage_payload_parsers() {
        let claude_raw = json!({
            "five_hour": { "utilization": 42.5, "resets_at": "2026-11-25T20:00:00Z" },
            "seven_day": { "utilization": 15.0, "resets_at": 1795651200 },
            "seven_day_sonnet": { "utilization": 10.0, "resets_at": "2026-11-30T00:00:00Z" },
            "cedar_ember": {
                "eligible": true,
                "next_grant_id": "g1",
                "grants": [{ "id": "g1", "resets_left": 2, "resets_total": 3, "clears": ["five_hour"], "usable_now": true }]
            }
        });
        let claude_parsed = parse_claude_usage_payload(&claude_raw);
        assert_eq!(claude_parsed["plan"], "Claude Code");
        assert_eq!(claude_parsed["quotas"]["session (5h)"]["remaining"], 57.5);
        assert_eq!(claude_parsed["quotas"]["weekly sonnet (7d)"]["used"], 10.0);
        assert_eq!(claude_parsed["resetCredits"]["availableCount"], 2.0);

        let codex_raw = json!({
            "plan_type": "pro",
            "rate_limit": {
                "limit_reached": false,
                "primary_window": { "used_percent": 25, "reset_at": 1795651200 },
                "secondary_window": { "used_percent": 60, "reset_at": 1795737600 }
            },
            "rate_limit_reset_credits": { "available_count": 3 }
        });
        let codex_parsed = parse_codex_usage_payload(&codex_raw);
        assert_eq!(codex_parsed["plan"], "pro");
        assert_eq!(codex_parsed["quotas"]["session"]["remaining"], 75.0);
        assert_eq!(codex_parsed["quotas"]["weekly"]["used"], 60.0);
        assert_eq!(codex_parsed["resetCredits"]["availableCount"], 3.0);

        let glm_raw = json!({
            "data": {
                "level": "PRO",
                "limits": [
                    { "type": "TOKENS_LIMIT", "unit": 3, "number": 5, "percentage": 30, "nextResetTime": 1795651200000i64 },
                    { "type": "CREDIT_LIMIT", "unit": 6, "number": 7, "percentage": 10, "nextResetTime": 1795737600000i64 }
                ]
            }
        });
        let glm_parsed = parse_glm_quota_response(&glm_raw);
        assert_eq!(glm_parsed["plan"], "Pro");
        assert_eq!(glm_parsed["quotas"]["Session (5h)"]["remaining"], 70.0);
        assert_eq!(glm_parsed["quotas"]["Weekly (7d)"]["remaining"], 90.0);
    }

    #[tokio::test]
    async fn test_v1_models_info_and_wildcard_routes() {
        let (state, key) = test_state();
        let conn = ProviderConnection {
            id: "conn-openai-models".to_string(),
            provider: "openai".to_string(),
            auth_type: "apikey".to_string(),
            name: Some("OpenAI".to_string()),
            email: None,
            priority: Some(1),
            is_active: true,
            data: ConnectionData {
                api_key: Some("sk-test".to_string()),
                ..Default::default()
            },
            raw_data: String::new(),
            created_at: Utc::now().to_rfc3339(),
            updated_at: Utc::now().to_rfc3339(),
        };
        state.db.save_provider_connection(&conn).unwrap();

        let app = usage_extra_router().with_state(state);

        // /v1/models/info missing id -> 400
        let missing = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/models/info")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::BAD_REQUEST);

        // /v1/models/info valid model
        let info_ok = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/models/info?id=openai/gpt-4o")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(info_ok.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(info_ok.into_body(), 65536).await.unwrap();
        let val: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(val["id"], "openai/gpt-4o");
        assert_eq!(val["kind"], "llm");
        assert_eq!(val["endpoint"], "/v1/chat/completions");

        // /v1/models/info TTS voicesUrl & virtual search
        let tts_info = lookup_model_info("elevenlabs/eleven_multilingual_v2", Some("tts"));
        if let Some(ti) = tts_info {
            assert_eq!(ti["kind"], "tts");
            assert_eq!(ti["endpoint"], "/v1/audio/speech");
        }
        let search_info = lookup_model_info("brave/search", None).expect("brave/search virtual info");
        assert_eq!(search_info["kind"], "webSearch");
        assert_eq!(search_info["endpoint"], "/v1/search");

        // /v1/models/embedding without auth -> 401
        let unauth = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/models/embedding")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauth.status(), StatusCode::UNAUTHORIZED);

        // /v1/models/embedding with auth -> 200 list
        let embed_ok = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/models/embedding")
                    .header("Authorization", format!("Bearer {}", key))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(embed_ok.status(), StatusCode::OK);
        let ebytes = axum::body::to_bytes(embed_ok.into_body(), 262144).await.unwrap();
        let eval: Value = serde_json::from_slice(&ebytes).unwrap();
        assert_eq!(eval["object"], "list");
        assert!(!eval["data"].as_array().unwrap().is_empty());

        // Single model lookup /api/v1/models/openai/gpt-4o
        let single_ok = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/models/openai/gpt-4o")
                    .header("x-api-key", key.clone())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(single_ok.status(), StatusCode::OK);

        // Single model lookup missing -> 404
        let single_404 = app
            .oneshot(
                Request::builder()
                    .uri("/v1/models/openai/nonexistent-model-xyz")
                    .header("x-api-key", key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(single_404.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_v1_api_chat_ollama_transform_and_options() {
        let sse_body = concat!(
            "data: {\"id\":\"1\",\"choices\":[{\"delta\":{\"content\":\"Hello \"}}]}\n\n",
            "data: {\"id\":\"2\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"get_time\",\"arguments\":\"{\\\"tz\\\":\\\"UTC\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        let sse_resp = Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .body(Body::from(sse_body))
            .unwrap();

        let ollama_resp = transform_response_to_ollama(sse_resp, "llama3.2".to_string()).await;
        assert_eq!(ollama_resp.status(), StatusCode::OK);
        assert_eq!(
            ollama_resp.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/x-ndjson"
        );
        let bytes = axum::body::to_bytes(ollama_resp.into_body(), 65536).await.unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        let lines: Vec<Value> = text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert!(lines.len() >= 2);
        assert_eq!(lines[0]["model"], "llama3.2");
        assert_eq!(lines[0]["message"]["content"], "Hello ");
        assert_eq!(lines[0]["done"], false);
        assert_eq!(lines[1]["done"], true);
        assert_eq!(lines[1]["message"]["tool_calls"][0]["function"]["name"], "get_time");
        assert_eq!(lines[1]["message"]["tool_calls"][0]["function"]["arguments"]["tz"], "UTC");

        let (state, _key) = test_state();
        let app = usage_extra_router().with_state(state);
        let opt = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("OPTIONS")
                    .uri("/v1/api/chat")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(opt.status(), StatusCode::OK);

        // Unauthorized POST /v1/api/chat returns 401 cleanly
        let unauth = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/api/chat")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauth.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_translator_handlers_load_save_translate() {
        let (state, _key) = test_state();
        let temp_dir = std::env::temp_dir().join(format!("9r_trans_test_{}", generate_uuid()));
        let _ = std::fs::create_dir_all(&temp_dir);
        // SAFETY: test-only env isolation within this unit test
        unsafe {
            std::env::set_var("NINEROUTER_TRANSLATOR_LOGS_DIR", temp_dir.to_string_lossy().as_ref());
        }

        let conn = ProviderConnection {
            id: "conn-claude-trans".to_string(),
            provider: "claude".to_string(),
            auth_type: "oauth".to_string(),
            name: Some("Claude".to_string()),
            email: None,
            priority: Some(1),
            is_active: true,
            data: ConnectionData {
                extra: {
                    let mut m = HashMap::new();
                    m.insert("accessToken".to_string(), json!("tok-claude-xyz"));
                    m
                },
                ..Default::default()
            },
            raw_data: String::new(),
            created_at: Utc::now().to_rfc3339(),
            updated_at: Utc::now().to_rfc3339(),
        };
        state.db.save_provider_connection(&conn).unwrap();

        // Save valid file
        let save_res = translator_save_handler(
            State(state.clone()),
            HeaderMap::new(),
            axum::body::Bytes::from(
                r#"{"file":"1_req_client.json","content":"{\"model\":\"cc/claude-3-5-sonnet-20241022\"}"}"#,
            ),
        )
        .await;
        assert_eq!(save_res.status(), StatusCode::OK);

        // Save invalid file -> 400
        let save_bad = translator_save_handler(
            State(state.clone()),
            HeaderMap::new(),
            axum::body::Bytes::from(r#"{"file":"../etc/passwd","content":"x"}"#),
        )
        .await;
        assert_eq!(save_bad.status(), StatusCode::BAD_REQUEST);

        // Load saved file
        let load_res = translator_load_handler(
            State(state.clone()),
            HeaderMap::new(),
            Query(TranslatorLoadQuery {
                file: Some("1_req_client.json".to_string()),
            }),
        )
        .await;
        assert_eq!(load_res.status(), StatusCode::OK);

        // Step 1 translate
        let step1 = translator_translate_handler(
            State(state.clone()),
            HeaderMap::new(),
            axum::body::Bytes::from(
                r#"{"step":1,"body":{"model":"cc/claude-3-5-sonnet-20241022","messages":[{"role":"user","content":"hi"}]}}"#,
            ),
        )
        .await;
        assert_eq!(step1.status(), StatusCode::OK);
        let s1_val: Value =
            serde_json::from_slice(&axum::body::to_bytes(step1.into_body(), 65536).await.unwrap())
                .unwrap();
        assert_eq!(s1_val["result"]["provider"], "claude");
        assert_eq!(s1_val["result"]["model"], "claude-3-5-sonnet-20241022");
        assert_eq!(s1_val["result"]["targetFormat"], "claude");

        // Step 3 translate
        let step3 = translator_translate_handler(
            State(state),
            HeaderMap::new(),
            axum::body::Bytes::from(
                r#"{"step":3,"body":{"provider":"claude","model":"claude-3-5-sonnet-20241022","body":{"model":"claude-3-5-sonnet-20241022","messages":[{"role":"user","content":"hi"}],"stream":true}}}"#,
            ),
        )
        .await;
        assert_eq!(step3.status(), StatusCode::OK);
        let s3_val: Value =
            serde_json::from_slice(&axum::body::to_bytes(step3.into_body(), 65536).await.unwrap())
                .unwrap();
        assert_eq!(s3_val["result"]["url"], "https://api.anthropic.com/v1/messages");
        assert_eq!(s3_val["result"]["headers"]["Authorization"], "Bearer tok-claude-xyz");

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_models_scoping_disabled_and_internal_auth() {
        let (state, _key) = test_state();
        let conn = ProviderConnection {
            id: "conn-openai-scope".to_string(),
            provider: "openai".to_string(),
            auth_type: "apikey".to_string(),
            name: Some("OpenAI".to_string()),
            email: None,
            priority: Some(1),
            is_active: true,
            data: ConnectionData {
                api_key: Some("sk-test".to_string()),
                ..Default::default()
            },
            raw_data: String::new(),
            created_at: Utc::now().to_rfc3339(),
            updated_at: Utc::now().to_rfc3339(),
        };
        state.db.save_provider_connection(&conn).unwrap();

        // Save restricted key & disabled model
        let restricted_raw = "sk_restricted_extra_123".to_string();
        let restricted_key = ApiKey {
            id: "key-restricted-extra".to_string(),
            name: Some("Restricted".to_string()),
            key: restricted_raw.clone(),
            allowed_models: "openai/gpt-4o,openai/text-embedding-3-small".to_string(),
            created_at: Utc::now().to_rfc3339(),
            ..ApiKey::default()
        };
        state.db.save_api_key(&restricted_key).unwrap();
        state
            .db
            .disable_models("openai", &["gpt-4o-mini".to_string()])
            .unwrap();

        let app = usage_extra_router().with_state(state.clone());

        // Restricted key sees only its embedding model on /v1/models/embedding
        let emb = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/models/embedding")
                    .header("x-api-key", restricted_raw.clone())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(emb.status(), StatusCode::OK);
        let emb_val: Value =
            serde_json::from_slice(&axum::body::to_bytes(emb.into_body(), 65536).await.unwrap())
                .unwrap();
        let emb_ids: Vec<&str> = emb_val["data"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|m| m["id"].as_str())
            .collect();
        assert_eq!(emb_ids, vec!["openai/text-embedding-3-small"]);

        // Restricted key cannot access non-allowed model on /v1/models/openai/gpt-4o-mini
        let denied = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/models/openai/gpt-4o-mini")
                    .header("x-api-key", restricted_raw)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::NOT_FOUND);

        // Non-streaming JSON -> Ollama NDJSON
        let json_resp = Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                r#"{"id":"1","choices":[{"message":{"role":"assistant","content":"Done!"}}]}"#,
            ))
            .unwrap();
        let ollama_json = transform_response_to_ollama(json_resp, "qwen3".to_string()).await;
        assert_eq!(ollama_json.status(), StatusCode::OK);
        let o_bytes = axum::body::to_bytes(ollama_json.into_body(), 65536).await.unwrap();
        let o_val: Value = serde_json::from_slice(o_bytes.trim_ascii()).unwrap();
        assert_eq!(o_val["model"], "qwen3");
        assert_eq!(o_val["message"]["content"], "Done!");
        assert_eq!(o_val["done"], true);

        // Step 2 translate Claude -> OpenAI & Responses -> OpenAI
        let step2_claude = translator_translate_handler(
            State(state.clone()),
            HeaderMap::new(),
            axum::body::Bytes::from(
                r#"{"step":2,"body":{"model":"claude-3-5-sonnet-20241022","system":"Be brief","messages":[{"role":"user","content":"Hi"}]}}"#,
            ),
        )
        .await;
        assert_eq!(step2_claude.status(), StatusCode::OK);
        let s2_val: Value = serde_json::from_slice(
            &axum::body::to_bytes(step2_claude.into_body(), 65536)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(s2_val["result"]["body"]["messages"][0]["role"], "system");

        // Enable requireLogin -> internal usage endpoints enforce 401
        state
            .db
            .save_settings(&json!({ "requireLogin": true }))
            .unwrap();
        let unauth_hist = app
            .oneshot(
                Request::builder()
                    .uri("/api/usage/history")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauth_hist.status(), StatusCode::UNAUTHORIZED);
    }
}
