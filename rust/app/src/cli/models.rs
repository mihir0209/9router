use std::time::Duration;
use serde_json::Value;

use crate::cli::common::{resolve_api_key, resolve_gateway_url};

pub fn print_models_help() {
    println!(
        r#"9r models - List available models and provider routing from 9Router gateway

Usage:
  9r models [options]
  9r ls [options]

Options:
  -p, --port <port>      Gateway port (default 20127 or from active daemon)
  -u, --url <url>        Custom gateway base URL (default http://127.0.0.1:<port>)
  -k, --api-key <key>    API key for authentication (or NINEROUTER_API_KEY)
      --json             Output raw JSON response from gateway
  -h, --help             Show this help message

Description:
  Queries the running 9Router gateway at /v1/models, parses the JSON catalog,
  and renders a formatted terminal table of all active upstream models, combos,
  and provider routing destinations.
"#
    );
}

pub struct ModelRow {
    pub id: String,
    pub provider: String,
    pub model_type: String,
}

pub fn parse_model_rows(val: &Value) -> Vec<ModelRow> {
    let empty_vec = vec![];
    let models = val.get("data").and_then(|d| d.as_array()).unwrap_or(&empty_vec);

    let mut rows: Vec<ModelRow> = Vec::new();
    for m in models {
        let id = m.get("id").and_then(|s| s.as_str()).unwrap_or("-").to_string();
        let owned_by = m.get("owned_by").and_then(|s| s.as_str()).unwrap_or("gateway").to_string();
        let model_type = if owned_by == "custom" {
            "combo".to_string()
        } else {
            "model".to_string()
        };
        rows.push(ModelRow {
            id,
            provider: owned_by,
            model_type,
        });
    }

    // Sort rows alphabetically by provider, then model ID
    rows.sort_by(|a, b| a.provider.cmp(&b.provider).then_with(|| a.id.cmp(&b.id)));
    rows
}

pub fn render_models_table(val: &Value, base_url: &str) {
    let rows = parse_model_rows(val);

    if rows.is_empty() {
        println!("\x1b[33m•\x1b[0m No models currently available from gateway at {}", base_url);
        return;
    }

    let mut max_id_len = 8;        // "MODEL ID"
    let mut max_prov_len = 17;    // "PROVIDER / ROUTE"
    let mut max_type_len = 4;     // "TYPE"

    for r in &rows {
        if r.id.len() > max_id_len {
            max_id_len = r.id.len();
        }
        if r.provider.len() > max_prov_len {
            max_prov_len = r.provider.len();
        }
        if r.model_type.len() > max_type_len {
            max_type_len = r.model_type.len();
        }
    }

    // Add 2 padding spaces
    max_id_len += 2;
    max_prov_len += 2;
    max_type_len += 2;

    let sep_line = format!(
        "+{}+{}+{}+",
        "-".repeat(max_id_len),
        "-".repeat(max_prov_len),
        "-".repeat(max_type_len),
    );

    println!("{}", sep_line);
    println!(
        "| {:<id_w$}| {:<prov_w$}| {:<type_w$}|",
        "MODEL ID",
        "PROVIDER / ROUTE",
        "TYPE",
        id_w = max_id_len - 1,
        prov_w = max_prov_len - 1,
        type_w = max_type_len - 1,
    );
    println!("{}", sep_line);

    for r in &rows {
        println!(
            "| {:<id_w$}| {:<prov_w$}| {:<type_w$}|",
            r.id,
            r.provider,
            r.model_type,
            id_w = max_id_len - 1,
            prov_w = max_prov_len - 1,
            type_w = max_type_len - 1,
        );
    }
    println!("{}", sep_line);
    println!("{} model(s) available via {}/v1/models", rows.len(), base_url);
}

pub async fn run_models(args: &[String]) -> anyhow::Result<()> {
    let mut port = None;
    let mut url = None;
    let mut api_key = None;
    let mut json_output = false;
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                print_models_help();
                return Ok(());
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
            "--json" => json_output = true,
            other => {
                eprintln!("Unknown option '{}'. Run '9r models --help' for usage.", other);
                return Ok(());
            }
        }
        i += 1;
    }

    let base_url = resolve_gateway_url(port, url);
    let key = resolve_api_key(api_key);

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?;

    let target_url = format!("{}/v1/models", base_url);
    let mut req = client.get(&target_url).header("Accept", "application/json");
    if let Some(ref k) = key {
        req = req.header("Authorization", format!("Bearer {}", k));
    }

    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("\x1b[31m✗ Error:\x1b[0m Could not connect to 9Router gateway at {}", base_url);
            eprintln!("  Reason: {}\n", e);
            eprintln!("Is the 9Router gateway running?");
            eprintln!("  Run '9r status' to check daemon status");
            eprintln!("  Run '9r start -d' to start in background");
            eprintln!("  Run '9r' to run in foreground");
            return Ok(());
        }
    };

    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        eprintln!("\x1b[31m✗ Error:\x1b[0m Gateway returned 401 Unauthorized.");
        eprintln!("  An authorized API key is required to query models.");
        eprintln!("  Provide a key with: 9r models --api-key <key>");
        eprintln!("  Or export NINEROUTER_API_KEY in your environment.");
        return Ok(());
    }

    if !status.is_success() {
        let err_body = resp.text().await.unwrap_or_default();
        eprintln!("\x1b[31m✗ Gateway error (HTTP {}):\x1b[0m {}", status, err_body);
        return Ok(());
    }

    let json_val: Value = resp.json().await?;

    if json_output {
        println!("{}", serde_json::to_string_pretty(&json_val)?);
        return Ok(());
    }

    render_models_table(&json_val, &base_url);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_parse_model_rows() {
        let mock = json!({
            "object": "list",
            "data": [
                {
                    "id": "gpt-4o",
                    "object": "model",
                    "owned_by": "openai"
                },
                {
                    "id": "claude-3-5-sonnet",
                    "object": "model",
                    "owned_by": "anthropic"
                },
                {
                    "id": "dev-combo",
                    "object": "model",
                    "owned_by": "custom"
                }
            ]
        });

        let rows = parse_model_rows(&mock);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].provider, "anthropic");
        assert_eq!(rows[0].id, "claude-3-5-sonnet");
        assert_eq!(rows[0].model_type, "model");

        assert_eq!(rows[1].provider, "custom");
        assert_eq!(rows[1].id, "dev-combo");
        assert_eq!(rows[1].model_type, "combo");

        assert_eq!(rows[2].provider, "openai");
        assert_eq!(rows[2].id, "gpt-4o");
    }
}
