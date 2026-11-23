use std::io::{IsTerminal, Read, Write};
use std::path::Path;
use std::time::Duration;

use anyhow::Context;
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::multipart::{Form, Part};
use serde_json::{json, Value};

use crate::cli::common::{resolve_api_key, resolve_gateway_url};

// ============================================================================
// Help Printers
// ============================================================================

pub fn print_chat_help() {
    println!(
        r#"9r chat - Send a chat prompt to local 9Router gateway and print response

Usage:
  9r chat [options] [prompt]

Options:
  -m, --model <model>    Model ID to query (default: gpt-4o-mini)
  -s, --stream           Stream assistant response tokens to stdout in real-time
      --no-stream        Disable streaming and print completed response
      --system <prompt>  System message instructions to guide model behavior
  -p, --port <port>      Gateway port (default 20127 or from active daemon)
  -u, --url <url>        Custom gateway base URL (default http://127.0.0.1:<port>)
  -k, --api-key <key>    API key for authentication (or NINEROUTER_API_KEY)
  -h, --help             Show this help message

Description:
  Sends chat prompt to /v1/chat/completions on 9Router gateway.
  If [prompt] is omitted or '-', reads input from standard input (stdin).
"#
    );
}

pub fn print_image_help() {
    println!(
        r#"9r image - Generate images from local 9Router gateway

Usage:
  9r image [options] <prompt>

Options:
  -o, --output <path>    Output image file path (default: image.png)
  -m, --model <model>    Target image model (e.g. dall-e-3, flux-schnell)
  -s, --size <size>      Image dimensions (e.g. 1024x1024, 512x512)
  -n <number>            Number of images to generate (default: 1)
  -p, --port <port>      Gateway port (default 20127 or from active daemon)
  -u, --url <url>        Custom gateway base URL (default http://127.0.0.1:<port>)
  -k, --api-key <key>    API key for authentication (or NINEROUTER_API_KEY)
  -h, --help             Show this help message

Description:
  Generates image via /v1/images/generations and saves to destination file.
"#
    );
}

pub fn print_speech_help() {
    println!(
        r#"9r speech - Convert text to audio speech from local 9Router gateway

Usage:
  9r speech [options] <text>

Options:
  -o, --output <path>    Output audio file path or '-' for stdout pipe (default: speech.mp3)
  -m, --model <model>    Target TTS model (default: tts-1)
  -v, --voice <voice>    Voice name (e.g. alloy, echo, fable, onyx, nova, shimmer) (default: alloy)
      --format <format>  Audio encoding format (mp3, wav, opus, aac, flac) (default: mp3)
      --speed <speed>    Playback speed between 0.25 and 4.0 (default: 1.0)
  -p, --port <port>      Gateway port (default 20127 or from active daemon)
  -u, --url <url>        Custom gateway base URL (default http://127.0.0.1:<port>)
  -k, --api-key <key>    API key for authentication (or NINEROUTER_API_KEY)
  -h, --help             Show this help message

Description:
  Synthesizes speech via /v1/audio/speech. If -o is '-', audio bytes are written
  directly to stdout for piping to media players (e.g. 9r speech "Hi" -o - | mpv -).
"#
    );
}

pub fn print_transcribe_help() {
    println!(
        r#"9r transcribe - Transcribe audio files to text via local 9Router gateway

Usage:
  9r transcribe [options] <audio-file>

Options:
  -m, --model <model>       Target STT model (default: whisper-1)
  -l, --language <lang>     Language code in ISO-639-1 format (e.g. en, fr, de)
      --prompt <prompt>     Optional text guide to improve transcription accuracy
      --temperature <temp>  Sampling temperature between 0.0 and 1.0 (default: 0.0)
      --format <format>     Output format: json, text, srt, verbose_json, vtt (default: json)
  -p, --port <port>         Gateway port (default 20127 or from active daemon)
  -u, --url <url>           Custom gateway base URL (default http://127.0.0.1:<port>)
  -k, --api-key <key>       API key for authentication (or NINEROUTER_API_KEY)
  -h, --help                Show this help message

Description:
  Transcribes local audio recording via /v1/audio/transcriptions and outputs text to stdout.
"#
    );
}

pub fn print_video_help() {
    println!(
        r#"9r video - Generate videos from text prompts via local 9Router gateway

Usage:
  9r video [options] <prompt>

Options:
  -o, --output <path>        Output video file path (default: video.mp4)
  -m, --model <model>        Target video model (e.g. luma-ray, cogvideox, runway-gen3)
  -d, --duration <sec>       Target video duration in seconds
  -a, --aspect-ratio <ratio> Target aspect ratio (e.g. 16:9, 9:16, 1:1)
  -p, --port <port>          Gateway port (default 20127 or from active daemon)
  -u, --url <url>            Custom gateway base URL (default http://127.0.0.1:<port>)
  -k, --api-key <key>        API key for authentication (or NINEROUTER_API_KEY)
  -h, --help                 Show this help message

Description:
  Submits prompt to /v1/videos/generations and downloads resulting video to output file.
"#
    );
}

// ============================================================================
// Helper Utilities
// ============================================================================

/// Reads input text from CLI arguments or stdin.
/// If arguments contain "-", or if arguments are empty and stdin is piped, reads stdin.
pub fn resolve_cli_input(positional: &[String]) -> anyhow::Result<String> {
    let raw = positional.join(" ").trim().to_string();

    if raw == "-" {
        let mut buffer = String::new();
        std::io::stdin().read_to_string(&mut buffer)?;
        return Ok(buffer.trim().to_string());
    }

    if !raw.is_empty() {
        if !std::io::stdin().is_terminal() {
            let mut stdin_buf = String::new();
            if let Ok(n) = std::io::stdin().read_to_string(&mut stdin_buf) {
                if n > 0 && !stdin_buf.trim().is_empty() {
                    return Ok(format!("{}\n\n{}", raw, stdin_buf.trim()));
                }
            }
        }
        return Ok(raw);
    }

    if !std::io::stdin().is_terminal() {
        let mut buffer = String::new();
        std::io::stdin().read_to_string(&mut buffer)?;
        return Ok(buffer.trim().to_string());
    }

    Ok(String::new())
}

/// Builds common request headers including authorization if an API key is available.
pub fn build_headers(api_key: Option<&str>) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    if let Some(key) = api_key {
        if let Ok(hv) = HeaderValue::from_str(&format!("Bearer {}", key)) {
            headers.insert(AUTHORIZATION, hv);
        }
    }
    headers
}

// ============================================================================
// Command 1: chat
// ============================================================================

#[derive(Debug, Clone, PartialEq)]
pub struct ChatArgs {
    pub port: Option<u16>,
    pub url: Option<String>,
    pub api_key: Option<String>,
    pub model: String,
    pub stream: bool,
    pub system: Option<String>,
    pub prompt: String,
}

pub fn parse_chat_args(args: &[String]) -> anyhow::Result<Option<ChatArgs>> {
    let mut port = None;
    let mut url = None;
    let mut api_key = None;
    let mut model = "gpt-4o-mini".to_string();
    let mut stream = false;
    let mut system = None;
    let mut positional = Vec::new();
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                print_chat_help();
                return Ok(None);
            }
            "-p" | "--port" => {
                i += 1;
                if i < args.len() {
                    port = args[i].parse().ok();
                }
            }
            "-u" | "--url" => {
                i += 1;
                if i < args.len() {
                    url = Some(args[i].clone());
                }
            }
            "-k" | "--api-key" => {
                i += 1;
                if i < args.len() {
                    api_key = Some(args[i].clone());
                }
            }
            "-m" | "--model" => {
                i += 1;
                if i < args.len() {
                    model = args[i].clone();
                }
            }
            "-s" | "--stream" => {
                stream = true;
            }
            "--no-stream" => {
                stream = false;
            }
            "--system" => {
                i += 1;
                if i < args.len() {
                    system = Some(args[i].clone());
                }
            }
            other => {
                if other.starts_with('-') {
                    eprintln!("Unknown option '{}'. Run '9r chat --help' for usage.", other);
                    anyhow::bail!("Unknown option: {}", other);
                } else {
                    positional.push(args[i].clone());
                }
            }
        }
        i += 1;
    }

    let prompt = resolve_cli_input(&positional)?;

    Ok(Some(ChatArgs {
        port,
        url,
        api_key,
        model,
        stream,
        system,
        prompt,
    }))
}

pub fn build_chat_payload(
    model: &str,
    prompt: &str,
    system: Option<&str>,
    stream: bool,
) -> Value {
    let mut messages = Vec::new();
    if let Some(sys) = system {
        if !sys.is_empty() {
            messages.push(json!({ "role": "system", "content": sys }));
        }
    }
    messages.push(json!({ "role": "user", "content": prompt }));

    json!({
        "model": model,
        "messages": messages,
        "stream": stream
    })
}

pub async fn run_chat(args: &[String]) -> anyhow::Result<()> {
    let parsed = match parse_chat_args(args)? {
        Some(p) => p,
        None => return Ok(()),
    };

    if parsed.prompt.trim().is_empty() {
        eprintln!("\x1b[31m✗ Error:\x1b[0m No prompt provided. Provide a prompt argument or pipe input to stdin.");
        eprintln!("Example: 9r chat 'Explain WebSockets in 2 sentences'");
        eprintln!("Run '9r chat --help' for usage.");
        anyhow::bail!("Missing prompt");
    }

    let base_url = resolve_gateway_url(parsed.port, parsed.url);
    let key = resolve_api_key(parsed.api_key);
    let endpoint = format!("{}/v1/chat/completions", base_url);

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()?;

    let payload = build_chat_payload(
        &parsed.model,
        &parsed.prompt,
        parsed.system.as_deref(),
        parsed.stream,
    );

    let mut req_builder = client
        .post(&endpoint)
        .header(CONTENT_TYPE, "application/json")
        .json(&payload);

    if let Some(ref k) = key {
        req_builder = req_builder.header(AUTHORIZATION, format!("Bearer {}", k));
    }

    let res = match req_builder.send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("\x1b[31m✗ Error:\x1b[0m Could not reach gateway at {}", base_url);
            eprintln!("  Reason: {}\n", e);
            eprintln!("Is the 9Router gateway running?");
            eprintln!("  Run '9r status' to check daemon status");
            eprintln!("  Run '9r start -d' to start in background");
            anyhow::bail!("Gateway connection failed");
        }
    };

    let status = res.status();
    if !status.is_success() {
        let err_body = res.text().await.unwrap_or_default();
        let parsed_err = serde_json::from_str::<Value>(&err_body).ok();
        let msg = parsed_err
            .as_ref()
            .and_then(|v| v.get("error"))
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
            .unwrap_or(&err_body);

        eprintln!("\x1b[31m✗ Chat request failed (HTTP {}):\x1b[0m {}", status, msg);
        anyhow::bail!("Chat request failed: {}", msg);
    }

    let content_type = res
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    if parsed.stream || content_type.contains("text/event-stream") {
        let mut stream = res.bytes_stream();
        let mut line_buffer = String::new();
        let mut printed_any = false;

        while let Some(item) = stream.next().await {
            let chunk = item.context("Failed reading SSE chunk")?;
            line_buffer.push_str(&String::from_utf8_lossy(&chunk));

            while let Some(pos) = line_buffer.find('\n') {
                let line = line_buffer[..pos].trim_end().to_string();
                line_buffer = line_buffer[pos + 1..].to_string();

                if line.is_empty() || line.starts_with(':') {
                    continue;
                }

                if let Some(data) = line.strip_prefix("data: ") {
                    let data = data.trim();
                    if data == "[DONE]" {
                        break;
                    }
                    if let Ok(val) = serde_json::from_str::<Value>(data) {
                        if let Some(delta) = val
                            .get("choices")
                            .and_then(|c| c.get(0))
                            .and_then(|ch| ch.get("delta"))
                            .and_then(|d| d.get("content"))
                            .and_then(|s| s.as_str())
                        {
                            print!("{}", delta);
                            let _ = std::io::stdout().flush();
                            printed_any = true;
                        }
                    }
                }
            }
        }

        // Flush any remaining line in buffer
        let trimmed_buf = line_buffer.trim();
        if let Some(data) = trimmed_buf.strip_prefix("data: ") {
            let data = data.trim();
            if data != "[DONE]" {
                if let Ok(val) = serde_json::from_str::<Value>(data) {
                    if let Some(delta) = val
                        .get("choices")
                        .and_then(|c| c.get(0))
                        .and_then(|ch| ch.get("delta"))
                        .and_then(|d| d.get("content"))
                        .and_then(|s| s.as_str())
                    {
                        print!("{}", delta);
                        let _ = std::io::stdout().flush();
                        printed_any = true;
                    }
                }
            }
        }

        if printed_any {
            println!();
        }
    } else {
        let val: Value = res.json().await.context("Failed parsing chat JSON response")?;
        if let Some(content) = val
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|ch| ch.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|s| s.as_str())
        {
            println!("{}", content);
        } else if let Some(err) = val.get("error").and_then(|e| e.get("message")).and_then(|m| m.as_str()) {
            eprintln!("\x1b[31m✗ Error:\x1b[0m {}", err);
        } else {
            println!("{}", serde_json::to_string_pretty(&val)?);
        }
    }

    Ok(())
}

// ============================================================================
// Command 2: image
// ============================================================================

#[derive(Debug, Clone, PartialEq)]
pub struct ImageArgs {
    pub port: Option<u16>,
    pub url: Option<String>,
    pub api_key: Option<String>,
    pub prompt: String,
    pub model: Option<String>,
    pub size: Option<String>,
    pub output: String,
    pub n: Option<u32>,
}

pub fn parse_image_args(args: &[String]) -> anyhow::Result<Option<ImageArgs>> {
    let mut port = None;
    let mut url = None;
    let mut api_key = None;
    let mut model = None;
    let mut size = None;
    let mut output = "image.png".to_string();
    let mut n = None;
    let mut positional = Vec::new();
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                print_image_help();
                return Ok(None);
            }
            "-p" | "--port" => {
                i += 1;
                if i < args.len() {
                    port = args[i].parse().ok();
                }
            }
            "-u" | "--url" => {
                i += 1;
                if i < args.len() {
                    url = Some(args[i].clone());
                }
            }
            "-k" | "--api-key" => {
                i += 1;
                if i < args.len() {
                    api_key = Some(args[i].clone());
                }
            }
            "-m" | "--model" => {
                i += 1;
                if i < args.len() {
                    model = Some(args[i].clone());
                }
            }
            "-s" | "--size" => {
                i += 1;
                if i < args.len() {
                    size = Some(args[i].clone());
                }
            }
            "-o" | "--output" => {
                i += 1;
                if i < args.len() {
                    output = args[i].clone();
                }
            }
            "-n" => {
                i += 1;
                if i < args.len() {
                    n = args[i].parse().ok();
                }
            }
            other => {
                if other.starts_with('-') {
                    eprintln!("Unknown option '{}'. Run '9r image --help' for usage.", other);
                    anyhow::bail!("Unknown option: {}", other);
                } else {
                    positional.push(args[i].clone());
                }
            }
        }
        i += 1;
    }

    let prompt = resolve_cli_input(&positional)?;

    Ok(Some(ImageArgs {
        port,
        url,
        api_key,
        prompt,
        model,
        size,
        output,
        n,
    }))
}

pub fn build_image_payload(
    prompt: &str,
    model: Option<&str>,
    size: Option<&str>,
    n: Option<u32>,
) -> Value {
    let mut payload = json!({
        "prompt": prompt,
        "response_format": "b64_json",
        "n": n.unwrap_or(1)
    });

    if let Some(m) = model {
        payload["model"] = json!(m);
    }
    if let Some(s) = size {
        payload["size"] = json!(s);
    }

    payload
}

pub async fn run_image(args: &[String]) -> anyhow::Result<()> {
    let parsed = match parse_image_args(args)? {
        Some(p) => p,
        None => return Ok(()),
    };

    if parsed.prompt.trim().is_empty() {
        eprintln!("\x1b[31m✗ Error:\x1b[0m Prompt is required. Provide a prompt argument or pipe input to stdin.");
        eprintln!("Example: 9r image 'A cute robot cat playing piano' -o cat.png");
        eprintln!("Run '9r image --help' for usage.");
        anyhow::bail!("Missing prompt");
    }

    let base_url = resolve_gateway_url(parsed.port, parsed.url);
    let key = resolve_api_key(parsed.api_key);
    let endpoint = format!("{}/v1/images/generations", base_url);

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(180))
        .build()?;

    let payload = build_image_payload(
        &parsed.prompt,
        parsed.model.as_deref(),
        parsed.size.as_deref(),
        parsed.n,
    );

    let mut req_builder = client
        .post(&endpoint)
        .header(CONTENT_TYPE, "application/json")
        .json(&payload);

    if let Some(ref k) = key {
        req_builder = req_builder.header(AUTHORIZATION, format!("Bearer {}", k));
    }

    let res = match req_builder.send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("\x1b[31m✗ Error:\x1b[0m Could not reach gateway at {}", base_url);
            eprintln!("  Reason: {}\n", e);
            anyhow::bail!("Gateway connection failed");
        }
    };

    let status = res.status();
    if !status.is_success() {
        let err_body = res.text().await.unwrap_or_default();
        let parsed_err = serde_json::from_str::<Value>(&err_body).ok();
        let msg = parsed_err
            .as_ref()
            .and_then(|v| v.get("error"))
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
            .unwrap_or(&err_body);

        eprintln!("\x1b[31m✗ Image generation failed (HTTP {}):\x1b[0m {}", status, msg);
        anyhow::bail!("Image generation failed: {}", msg);
    }

    let content_type = res
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let bytes = if content_type.starts_with("image/") {
        res.bytes().await?.to_vec()
    } else {
        let val: Value = res.json().await.context("Failed parsing image JSON response")?;
        let first_item = val
            .get("data")
            .and_then(|d| d.as_array())
            .and_then(|a| a.first());

        if let Some(b64) = first_item.and_then(|d| d.get("b64_json")).and_then(|b| b.as_str()) {
            use base64::Engine;
            base64::prelude::BASE64_STANDARD
                .decode(b64.trim())
                .context("Failed to decode base64 image data")?
        } else if let Some(url_str) = first_item.and_then(|d| d.get("url")).and_then(|u| u.as_str()) {
            if let Some(b64) = url_str
                .strip_prefix("data:image/png;base64,")
                .or_else(|| url_str.strip_prefix("data:image/jpeg;base64,"))
                .or_else(|| url_str.strip_prefix("data:image/webp;base64,"))
            {
                use base64::Engine;
                base64::prelude::BASE64_STANDARD
                    .decode(b64.trim())
                    .context("Failed to decode image data URI")?
            } else if url_str.starts_with("http://") || url_str.starts_with("https://") {
                client
                    .get(url_str)
                    .send()
                    .await?
                    .bytes()
                    .await?
                    .to_vec()
            } else {
                anyhow::bail!("Unrecognized image URL scheme: {}", url_str);
            }
        } else {
            anyhow::bail!("No image binary, base64 or URL found in response: {}", val);
        }
    };

    if let Some(parent) = Path::new(&parsed.output).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    std::fs::write(&parsed.output, &bytes)
        .with_context(|| format!("Failed writing image to '{}'", parsed.output))?;

    println!("✓ Image saved to {} ({} bytes)", parsed.output, bytes.len());
    Ok(())
}

// ============================================================================
// Command 3: speech
// ============================================================================

#[derive(Debug, Clone, PartialEq)]
pub struct SpeechArgs {
    pub port: Option<u16>,
    pub url: Option<String>,
    pub api_key: Option<String>,
    pub text: String,
    pub model: String,
    pub voice: String,
    pub output: String,
    pub format: Option<String>,
    pub speed: Option<f32>,
}

pub fn parse_speech_args(args: &[String]) -> anyhow::Result<Option<SpeechArgs>> {
    let mut port = None;
    let mut url = None;
    let mut api_key = None;
    let mut model = "tts-1".to_string();
    let mut voice = "alloy".to_string();
    let mut output = "speech.mp3".to_string();
    let mut format = None;
    let mut speed = None;
    let mut positional = Vec::new();
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                print_speech_help();
                return Ok(None);
            }
            "-p" | "--port" => {
                i += 1;
                if i < args.len() {
                    port = args[i].parse().ok();
                }
            }
            "-u" | "--url" => {
                i += 1;
                if i < args.len() {
                    url = Some(args[i].clone());
                }
            }
            "-k" | "--api-key" => {
                i += 1;
                if i < args.len() {
                    api_key = Some(args[i].clone());
                }
            }
            "-m" | "--model" => {
                i += 1;
                if i < args.len() {
                    model = args[i].clone();
                }
            }
            "-v" | "--voice" => {
                i += 1;
                if i < args.len() {
                    voice = args[i].clone();
                }
            }
            "-o" | "--output" => {
                i += 1;
                if i < args.len() {
                    output = args[i].clone();
                }
            }
            "--format" => {
                i += 1;
                if i < args.len() {
                    format = Some(args[i].clone());
                }
            }
            "--speed" => {
                i += 1;
                if i < args.len() {
                    speed = args[i].parse().ok();
                }
            }
            other => {
                if other.starts_with('-') {
                    eprintln!("Unknown option '{}'. Run '9r speech --help' for usage.", other);
                    anyhow::bail!("Unknown option: {}", other);
                } else {
                    positional.push(args[i].clone());
                }
            }
        }
        i += 1;
    }

    let text = resolve_cli_input(&positional)?;

    Ok(Some(SpeechArgs {
        port,
        url,
        api_key,
        text,
        model,
        voice,
        output,
        format,
        speed,
    }))
}

pub fn build_speech_payload(
    text: &str,
    model: &str,
    voice: &str,
    format: Option<&str>,
    speed: Option<f32>,
) -> Value {
    let mut payload = json!({
        "input": text,
        "model": model,
        "voice": voice,
        "response_format": format.unwrap_or("mp3")
    });

    if let Some(sp) = speed {
        payload["speed"] = json!(sp);
    }

    payload
}

pub async fn run_speech(args: &[String]) -> anyhow::Result<()> {
    let parsed = match parse_speech_args(args)? {
        Some(p) => p,
        None => return Ok(()),
    };

    if parsed.text.trim().is_empty() {
        eprintln!("\x1b[31m✗ Error:\x1b[0m Input text is required. Provide text argument or pipe input to stdin.");
        eprintln!("Example: 9r speech 'Welcome to 9Router single-binary AI Gateway' -o speech.mp3");
        eprintln!("Run '9r speech --help' for usage.");
        anyhow::bail!("Missing input text");
    }

    let base_url = resolve_gateway_url(parsed.port, parsed.url);
    let key = resolve_api_key(parsed.api_key);
    let endpoint = format!("{}/v1/audio/speech", base_url);

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()?;

    let payload = build_speech_payload(
        &parsed.text,
        &parsed.model,
        &parsed.voice,
        parsed.format.as_deref(),
        parsed.speed,
    );

    let mut req_builder = client
        .post(&endpoint)
        .header(CONTENT_TYPE, "application/json")
        .json(&payload);

    if let Some(ref k) = key {
        req_builder = req_builder.header(AUTHORIZATION, format!("Bearer {}", k));
    }

    let res = match req_builder.send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("\x1b[31m✗ Error:\x1b[0m Could not reach gateway at {}", base_url);
            eprintln!("  Reason: {}\n", e);
            anyhow::bail!("Gateway connection failed");
        }
    };

    let status = res.status();
    if !status.is_success() {
        let err_body = res.text().await.unwrap_or_default();
        let parsed_err = serde_json::from_str::<Value>(&err_body).ok();
        let msg = parsed_err
            .as_ref()
            .and_then(|v| v.get("error"))
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
            .unwrap_or(&err_body);

        eprintln!("\x1b[31m✗ Speech synthesis failed (HTTP {}):\x1b[0m {}", status, msg);
        anyhow::bail!("Speech synthesis failed: {}", msg);
    }

    let audio_bytes = res.bytes().await.context("Failed reading audio response body")?;

    if parsed.output == "-" {
        std::io::stdout()
            .write_all(&audio_bytes)
            .context("Failed writing audio bytes to stdout")?;
        std::io::stdout().flush()?;
    } else {
        if let Some(parent) = Path::new(&parsed.output).parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        std::fs::write(&parsed.output, &audio_bytes)
            .with_context(|| format!("Failed writing audio to '{}'", parsed.output))?;

        println!("✓ Audio saved to {} ({} bytes)", parsed.output, audio_bytes.len());
    }

    Ok(())
}

// ============================================================================
// Command 4: transcribe
// ============================================================================

#[derive(Debug, Clone, PartialEq)]
pub struct TranscribeArgs {
    pub port: Option<u16>,
    pub url: Option<String>,
    pub api_key: Option<String>,
    pub audio_file: String,
    pub model: String,
    pub language: Option<String>,
    pub prompt: Option<String>,
    pub format: Option<String>,
    pub temperature: Option<f32>,
}

pub fn parse_transcribe_args(args: &[String]) -> anyhow::Result<Option<TranscribeArgs>> {
    let mut port = None;
    let mut url = None;
    let mut api_key = None;
    let mut model = "whisper-1".to_string();
    let mut language = None;
    let mut prompt = None;
    let mut format = None;
    let mut temperature = None;
    let mut positional = Vec::new();
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                print_transcribe_help();
                return Ok(None);
            }
            "-p" | "--port" => {
                i += 1;
                if i < args.len() {
                    port = args[i].parse().ok();
                }
            }
            "-u" | "--url" => {
                i += 1;
                if i < args.len() {
                    url = Some(args[i].clone());
                }
            }
            "-k" | "--api-key" => {
                i += 1;
                if i < args.len() {
                    api_key = Some(args[i].clone());
                }
            }
            "-m" | "--model" => {
                i += 1;
                if i < args.len() {
                    model = args[i].clone();
                }
            }
            "-l" | "--language" => {
                i += 1;
                if i < args.len() {
                    language = Some(args[i].clone());
                }
            }
            "--prompt" => {
                i += 1;
                if i < args.len() {
                    prompt = Some(args[i].clone());
                }
            }
            "--format" => {
                i += 1;
                if i < args.len() {
                    format = Some(args[i].clone());
                }
            }
            "--temperature" => {
                i += 1;
                if i < args.len() {
                    temperature = args[i].parse().ok();
                }
            }
            other => {
                if other.starts_with('-') {
                    eprintln!("Unknown option '{}'. Run '9r transcribe --help' for usage.", other);
                    anyhow::bail!("Unknown option: {}", other);
                } else {
                    positional.push(args[i].clone());
                }
            }
        }
        i += 1;
    }

    if positional.is_empty() {
        eprintln!("\x1b[31m✗ Error:\x1b[0m Audio file path is required.");
        eprintln!("Example: 9r transcribe speech.mp3 --model whisper-1");
        eprintln!("Run '9r transcribe --help' for usage.");
        anyhow::bail!("Missing audio file path");
    }

    let audio_file = positional.join(" ");

    Ok(Some(TranscribeArgs {
        port,
        url,
        api_key,
        audio_file,
        model,
        language,
        prompt,
        format,
        temperature,
    }))
}

pub async fn run_transcribe(args: &[String]) -> anyhow::Result<()> {
    let parsed = match parse_transcribe_args(args)? {
        Some(p) => p,
        None => return Ok(()),
    };

    let audio_path = Path::new(&parsed.audio_file);
    if !audio_path.exists() {
        eprintln!("\x1b[31m✗ Error:\x1b[0m Audio file '{}' does not exist.", parsed.audio_file);
        anyhow::bail!("Audio file not found: {}", parsed.audio_file);
    }

    let file_bytes = std::fs::read(audio_path)
        .with_context(|| format!("Failed to read audio file '{}'", parsed.audio_file))?;

    let filename = audio_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("audio.wav")
        .to_string();

    let mime = mime_guess::from_path(audio_path)
        .first_or_octet_stream()
        .to_string();

    let file_part = Part::bytes(file_bytes)
        .file_name(filename)
        .mime_str(&mime)
        .context("Failed setting multipart MIME type")?;

    let mut form = Form::new()
        .part("file", file_part)
        .text("model", parsed.model);

    if let Some(l) = parsed.language {
        form = form.text("language", l);
    }
    if let Some(p) = parsed.prompt {
        form = form.text("prompt", p);
    }
    if let Some(f) = parsed.format {
        form = form.text("response_format", f);
    }
    if let Some(t) = parsed.temperature {
        form = form.text("temperature", t.to_string());
    }

    let base_url = resolve_gateway_url(parsed.port, parsed.url);
    let key = resolve_api_key(parsed.api_key);
    let endpoint = format!("{}/v1/audio/transcriptions", base_url);

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(180))
        .build()?;

    let mut req_builder = client.post(&endpoint).multipart(form);
    if let Some(ref k) = key {
        req_builder = req_builder.header(AUTHORIZATION, format!("Bearer {}", k));
    }

    let res = match req_builder.send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("\x1b[31m✗ Error:\x1b[0m Could not reach gateway at {}", base_url);
            eprintln!("  Reason: {}\n", e);
            anyhow::bail!("Gateway connection failed");
        }
    };

    let status = res.status();
    if !status.is_success() {
        let err_body = res.text().await.unwrap_or_default();
        let parsed_err = serde_json::from_str::<Value>(&err_body).ok();
        let msg = parsed_err
            .as_ref()
            .and_then(|v| v.get("error"))
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
            .unwrap_or(&err_body);

        eprintln!("\x1b[31m✗ Transcription failed (HTTP {}):\x1b[0m {}", status, msg);
        anyhow::bail!("Transcription failed: {}", msg);
    }

    let resp_text = res.text().await.context("Failed reading transcription response")?;

    if let Ok(val) = serde_json::from_str::<Value>(&resp_text) {
        if let Some(text) = val.get("text").and_then(|t| t.as_str()) {
            println!("{}", text);
        } else {
            println!("{}", resp_text);
        }
    } else {
        print!("{}", resp_text);
        if !resp_text.ends_with('\n') {
            println!();
        }
    }

    Ok(())
}

// ============================================================================
// Command 5: video
// ============================================================================

#[derive(Debug, Clone, PartialEq)]
pub struct VideoArgs {
    pub port: Option<u16>,
    pub url: Option<String>,
    pub api_key: Option<String>,
    pub prompt: String,
    pub model: Option<String>,
    pub output: String,
    pub duration: Option<u32>,
    pub aspect_ratio: Option<String>,
}

pub fn parse_video_args(args: &[String]) -> anyhow::Result<Option<VideoArgs>> {
    let mut port = None;
    let mut url = None;
    let mut api_key = None;
    let mut model = None;
    let mut output = "video.mp4".to_string();
    let mut duration = None;
    let mut aspect_ratio = None;
    let mut positional = Vec::new();
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                print_video_help();
                return Ok(None);
            }
            "-p" | "--port" => {
                i += 1;
                if i < args.len() {
                    port = args[i].parse().ok();
                }
            }
            "-u" | "--url" => {
                i += 1;
                if i < args.len() {
                    url = Some(args[i].clone());
                }
            }
            "-k" | "--api-key" => {
                i += 1;
                if i < args.len() {
                    api_key = Some(args[i].clone());
                }
            }
            "-m" | "--model" => {
                i += 1;
                if i < args.len() {
                    model = Some(args[i].clone());
                }
            }
            "-o" | "--output" => {
                i += 1;
                if i < args.len() {
                    output = args[i].clone();
                }
            }
            "-d" | "--duration" => {
                i += 1;
                if i < args.len() {
                    duration = args[i].parse().ok();
                }
            }
            "-a" | "--aspect-ratio" => {
                i += 1;
                if i < args.len() {
                    aspect_ratio = Some(args[i].clone());
                }
            }
            other => {
                if other.starts_with('-') {
                    eprintln!("Unknown option '{}'. Run '9r video --help' for usage.", other);
                    anyhow::bail!("Unknown option: {}", other);
                } else {
                    positional.push(args[i].clone());
                }
            }
        }
        i += 1;
    }

    let prompt = resolve_cli_input(&positional)?;

    Ok(Some(VideoArgs {
        port,
        url,
        api_key,
        prompt,
        model,
        output,
        duration,
        aspect_ratio,
    }))
}

pub fn build_video_payload(
    prompt: &str,
    model: Option<&str>,
    duration: Option<u32>,
    aspect_ratio: Option<&str>,
) -> Value {
    let mut payload = json!({
        "prompt": prompt
    });

    if let Some(m) = model {
        payload["model"] = json!(m);
    }
    if let Some(d) = duration {
        payload["duration"] = json!(d);
    }
    if let Some(ar) = aspect_ratio {
        payload["aspect_ratio"] = json!(ar);
    }

    payload
}

pub async fn run_video(args: &[String]) -> anyhow::Result<()> {
    let parsed = match parse_video_args(args)? {
        Some(p) => p,
        None => return Ok(()),
    };

    if parsed.prompt.trim().is_empty() {
        eprintln!("\x1b[31m✗ Error:\x1b[0m Prompt is required. Provide prompt argument or pipe input to stdin.");
        eprintln!("Example: 9r video 'Snowy mountains at sunset in 4k' -o mountains.mp4");
        eprintln!("Run '9r video --help' for usage.");
        anyhow::bail!("Missing prompt");
    }

    let base_url = resolve_gateway_url(parsed.port, parsed.url);
    let key = resolve_api_key(parsed.api_key);
    let endpoint = format!("{}/v1/videos/generations", base_url);

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(300))
        .build()?;

    let payload = build_video_payload(
        &parsed.prompt,
        parsed.model.as_deref(),
        parsed.duration,
        parsed.aspect_ratio.as_deref(),
    );

    let mut req_builder = client
        .post(&endpoint)
        .header(CONTENT_TYPE, "application/json")
        .json(&payload);

    if let Some(ref k) = key {
        req_builder = req_builder.header(AUTHORIZATION, format!("Bearer {}", k));
    }

    let res = match req_builder.send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("\x1b[31m✗ Error:\x1b[0m Could not reach gateway at {}", base_url);
            eprintln!("  Reason: {}\n", e);
            anyhow::bail!("Gateway connection failed");
        }
    };

    let status = res.status();
    if !status.is_success() {
        let err_body = res.text().await.unwrap_or_default();
        let parsed_err = serde_json::from_str::<Value>(&err_body).ok();
        let msg = parsed_err
            .as_ref()
            .and_then(|v| v.get("error"))
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
            .unwrap_or(&err_body);

        eprintln!("\x1b[31m✗ Video generation failed (HTTP {}):\x1b[0m {}", status, msg);
        anyhow::bail!("Video generation failed: {}", msg);
    }

    let content_type = res
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let bytes = if content_type.starts_with("video/") {
        res.bytes().await?.to_vec()
    } else {
        let val: Value = res.json().await.context("Failed parsing video JSON response")?;
        let first_item = val
            .get("data")
            .and_then(|d| d.as_array())
            .and_then(|a| a.first());

        if let Some(b64) = first_item.and_then(|d| d.get("b64_json")).and_then(|b| b.as_str()) {
            use base64::Engine;
            base64::prelude::BASE64_STANDARD
                .decode(b64.trim())
                .context("Failed to decode base64 video data")?
        } else if let Some(url_str) = first_item.and_then(|d| d.get("url")).and_then(|u| u.as_str()) {
            if let Some(b64) = url_str
                .strip_prefix("data:video/mp4;base64,")
                .or_else(|| url_str.strip_prefix("data:video/;base64,"))
            {
                use base64::Engine;
                base64::prelude::BASE64_STANDARD
                    .decode(b64.trim())
                    .context("Failed to decode video data URI")?
            } else if url_str.starts_with("http://") || url_str.starts_with("https://") {
                client
                    .get(url_str)
                    .send()
                    .await?
                    .bytes()
                    .await?
                    .to_vec()
            } else {
                println!("✓ Video generation task response: url={}", url_str);
                return Ok(());
            }
        } else if let Some(id) = first_item.and_then(|d| d.get("id")).and_then(|i| i.as_str()) {
            let task_status = first_item
                .and_then(|d| d.get("status"))
                .and_then(|s| s.as_str())
                .unwrap_or("queued");
            println!("✓ Video generation task {}: id={}", task_status, id);
            return Ok(());
        } else {
            anyhow::bail!("No video binary, base64 or URL found in response: {}", val);
        }
    };

    if let Some(parent) = Path::new(&parsed.output).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    std::fs::write(&parsed.output, &bytes)
        .with_context(|| format!("Failed writing video to '{}'", parsed.output))?;

    println!("✓ Video saved to {} ({} bytes)", parsed.output, bytes.len());
    Ok(())
}

// ============================================================================
// Unit Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_chat_args() {
        let args = vec![
            "-m".to_string(),
            "claude-3-5-sonnet".to_string(),
            "-s".to_string(),
            "--system".to_string(),
            "You are a helpful coding assistant".to_string(),
            "-p".to_string(),
            "20128".to_string(),
            "Write".to_string(),
            "a".to_string(),
            "Rust".to_string(),
            "function".to_string(),
        ];
        let parsed = parse_chat_args(&args).unwrap().unwrap();
        assert_eq!(parsed.model, "claude-3-5-sonnet");
        assert!(parsed.stream);
        assert_eq!(
            parsed.system,
            Some("You are a helpful coding assistant".to_string())
        );
        assert_eq!(parsed.port, Some(20128));
        assert_eq!(parsed.prompt, "Write a Rust function");
    }

    #[test]
    fn test_build_chat_payload() {
        let payload = build_chat_payload(
            "gpt-4o",
            "Explain quantum computing",
            Some("Be concise"),
            true,
        );
        assert_eq!(payload["model"], "gpt-4o");
        assert_eq!(payload["stream"], true);
        let msgs = payload["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[0]["content"], "Be concise");
        assert_eq!(msgs[1]["role"], "user");
        assert_eq!(msgs[1]["content"], "Explain quantum computing");
    }

    #[test]
    fn test_parse_image_args() {
        let args = vec![
            "-o".to_string(),
            "output_test.png".to_string(),
            "-m".to_string(),
            "dall-e-3".to_string(),
            "-s".to_string(),
            "1024x1024".to_string(),
            "-n".to_string(),
            "1".to_string(),
            "A".to_string(),
            "sunset".to_string(),
            "over".to_string(),
            "the".to_string(),
            "ocean".to_string(),
        ];
        let parsed = parse_image_args(&args).unwrap().unwrap();
        assert_eq!(parsed.output, "output_test.png");
        assert_eq!(parsed.model, Some("dall-e-3".to_string()));
        assert_eq!(parsed.size, Some("1024x1024".to_string()));
        assert_eq!(parsed.n, Some(1));
        assert_eq!(parsed.prompt, "A sunset over the ocean");
    }

    #[test]
    fn test_build_image_payload() {
        let payload = build_image_payload("A cyberpunk neon city", Some("flux"), Some("512x512"), Some(1));
        assert_eq!(payload["prompt"], "A cyberpunk neon city");
        assert_eq!(payload["model"], "flux");
        assert_eq!(payload["size"], "512x512");
        assert_eq!(payload["n"], 1);
        assert_eq!(payload["response_format"], "b64_json");
    }

    #[test]
    fn test_parse_speech_args() {
        let args = vec![
            "-o".to_string(),
            "-".to_string(),
            "-m".to_string(),
            "tts-1-hd".to_string(),
            "-v".to_string(),
            "echo".to_string(),
            "--format".to_string(),
            "wav".to_string(),
            "--speed".to_string(),
            "1.25".to_string(),
            "Hello".to_string(),
            "world".to_string(),
        ];
        let parsed = parse_speech_args(&args).unwrap().unwrap();
        assert_eq!(parsed.output, "-");
        assert_eq!(parsed.model, "tts-1-hd");
        assert_eq!(parsed.voice, "echo");
        assert_eq!(parsed.format, Some("wav".to_string()));
        assert_eq!(parsed.speed, Some(1.25));
        assert_eq!(parsed.text, "Hello world");
    }

    #[test]
    fn test_build_speech_payload() {
        let payload = build_speech_payload(
            "Read this aloud",
            "tts-1",
            "alloy",
            Some("mp3"),
            Some(1.0),
        );
        assert_eq!(payload["input"], "Read this aloud");
        assert_eq!(payload["model"], "tts-1");
        assert_eq!(payload["voice"], "alloy");
        assert_eq!(payload["response_format"], "mp3");
        assert_eq!(payload["speed"], 1.0);
    }

    #[test]
    fn test_parse_transcribe_args() {
        let args = vec![
            "sample_audio.mp3".to_string(),
            "-m".to_string(),
            "whisper-large".to_string(),
            "-l".to_string(),
            "fr".to_string(),
            "--prompt".to_string(),
            "Bonjour".to_string(),
            "--format".to_string(),
            "verbose_json".to_string(),
            "--temperature".to_string(),
            "0.2".to_string(),
        ];
        let parsed = parse_transcribe_args(&args).unwrap().unwrap();
        assert_eq!(parsed.audio_file, "sample_audio.mp3");
        assert_eq!(parsed.model, "whisper-large");
        assert_eq!(parsed.language, Some("fr".to_string()));
        assert_eq!(parsed.prompt, Some("Bonjour".to_string()));
        assert_eq!(parsed.format, Some("verbose_json".to_string()));
        assert_eq!(parsed.temperature, Some(0.2));
    }

    #[test]
    fn test_parse_video_args() {
        let args = vec![
            "-o".to_string(),
            "cinematic.mp4".to_string(),
            "-m".to_string(),
            "luma-ray".to_string(),
            "-d".to_string(),
            "10".to_string(),
            "-a".to_string(),
            "16:9".to_string(),
            "A".to_string(),
            "river".to_string(),
            "flowing".to_string(),
            "through".to_string(),
            "a".to_string(),
            "valley".to_string(),
        ];
        let parsed = parse_video_args(&args).unwrap().unwrap();
        assert_eq!(parsed.output, "cinematic.mp4");
        assert_eq!(parsed.model, Some("luma-ray".to_string()));
        assert_eq!(parsed.duration, Some(10));
        assert_eq!(parsed.aspect_ratio, Some("16:9".to_string()));
        assert_eq!(parsed.prompt, "A river flowing through a valley");
    }

    #[test]
    fn test_build_video_payload() {
        let payload = build_video_payload(
            "Spaceship approaching Mars",
            Some("cogvideox"),
            Some(6),
            Some("16:9"),
        );
        assert_eq!(payload["prompt"], "Spaceship approaching Mars");
        assert_eq!(payload["model"], "cogvideox");
        assert_eq!(payload["duration"], 6);
        assert_eq!(payload["aspect_ratio"], "16:9");
    }

    #[test]
    fn test_print_help_functions() {
        print_chat_help();
        print_image_help();
        print_speech_help();
        print_transcribe_help();
        print_video_help();
    }
}
