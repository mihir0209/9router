pub mod filters;

use filters::{filter_build_output, filter_find, filter_git_diff, filter_grep, filter_ls};
use crate::translator::{ChatCompletionRequest, ClaudeBlock, ClaudeContent, ClaudeMessageRequest, MessageContent};

pub const MIN_COMPRESS_SIZE: usize = 100;
pub const MAX_COMPRESS_SIZE: usize = 10 * 1024 * 1024; // 10 MB

#[derive(Debug, PartialEq, Eq)]
pub enum DetectedFilter {
    GitDiff,
    Grep,
    Find,
    Ls,
    BuildOutput,
    None,
}

pub fn autodetect_filter(text: &str) -> DetectedFilter {
    let peek_len = text.len().min(1024);
    let head = &text[..peek_len];

    if head.contains("diff --git ") || head.contains("\n@@ ") || head.starts_with("@@ ") {
        return DetectedFilter::GitDiff;
    }

    if head.contains("Compiling ") || head.contains("npm ERR!") || head.contains("BUILD SUCCESS") || head.contains("error[E") {
        return DetectedFilter::BuildOutput;
    }

    if head.starts_with("total ") || head.lines().take(5).any(|l| {
        let t = l.trim();
        (t.starts_with("-r") || t.starts_with("dr")) && t.len() > 15
    }) {
        return DetectedFilter::Ls;
    }

    // Grep check: look for "path:123:content"
    let grep_matches = head.lines().take(5).filter(|l| {
        let parts: Vec<&str> = l.splitn(3, ':').collect();
        parts.len() == 3 && parts[1].parse::<usize>().is_ok()
    }).count();
    if grep_matches > 0 {
        return DetectedFilter::Grep;
    }

    // Find check: path-like lines without colons
    let lines: Vec<&str> = head.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() >= 3 && lines.iter().all(|l| (l.contains('/') || l.contains('\\')) && !l.contains(':')) {
        return DetectedFilter::Find;
    }

    DetectedFilter::None
}

pub fn compress_text(input: &str) -> (String, usize) {
    let bytes_in = input.len();
    if bytes_in < MIN_COMPRESS_SIZE || bytes_in > MAX_COMPRESS_SIZE {
        return (input.to_string(), 0);
    }

    let filter = autodetect_filter(input);
    let compressed_opt = match filter {
        DetectedFilter::GitDiff => filter_git_diff(input),
        DetectedFilter::Grep => filter_grep(input),
        DetectedFilter::Find => filter_find(input),
        DetectedFilter::Ls => filter_ls(input),
        DetectedFilter::BuildOutput => filter_build_output(input),
        DetectedFilter::None => None,
    };

    match compressed_opt {
        Some(out) => {
            // Safety: never return empty, never expand text
            if out.is_empty() || out.len() >= bytes_in {
                (input.to_string(), 0)
            } else {
                let saved = bytes_in - out.len();
                (out, saved)
            }
        }
        None => (input.to_string(), 0),
    }
}

pub fn compress_chat_request(req: &mut ChatCompletionRequest) -> usize {
    let mut total_saved = 0;
    for msg in &mut req.messages {
        if msg.role == "tool" {
            match msg.content {
                Some(MessageContent::Text(ref mut s)) => {
                    let (compressed, saved) = compress_text(s);
                    if saved > 0 {
                        *s = compressed;
                        total_saved += saved;
                    }
                }
                Some(MessageContent::Parts(ref mut parts)) => {
                    for part in parts {
                        if let Some(ref mut s) = part.text {
                            let (compressed, saved) = compress_text(s);
                            if saved > 0 {
                                *s = compressed;
                                total_saved += saved;
                            }
                        }
                    }
                }
                None => {}
            }
        }
    }
    total_saved
}

pub fn compress_claude_request(req: &mut ClaudeMessageRequest) -> usize {
    let mut total_saved = 0;
    for msg in &mut req.messages {
        if let ClaudeContent::Blocks(ref mut blocks) = msg.content {
            for block in blocks {
                if let ClaudeBlock::ToolResult { content, is_error, .. } = block {
                    if is_error != &Some(true) {
                        if let Some(val) = content {
                            if let Some(s) = val.as_str() {
                                let (compressed, saved) = compress_text(s);
                                if saved > 0 {
                                    *val = serde_json::Value::String(compressed);
                                    total_saved += saved;
                                }
                            } else if let Some(arr) = val.as_array_mut() {
                                for item in arr {
                                    if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                                        if let Some(text_str) = item.get("text").and_then(|t| t.as_str()) {
                                            let (compressed, saved) = compress_text(text_str);
                                            if saved > 0 {
                                                item["text"] = serde_json::Value::String(compressed);
                                                total_saved += saved;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    total_saved
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::translator::{ChatMessage, MessageContent};

    #[test]
    fn test_autodetect_and_compress_git_diff() {
        let diff = r#"diff --git a/src/lib.rs b/src/lib.rs
index abcdef1..1234567 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -10,6 +10,8 @@ fn compute() {
     let x = 1;
+    let y = 2;
+    let z = 3;
     let w = 4;
 }
"#;
        assert_eq!(autodetect_filter(diff), DetectedFilter::GitDiff);
        let (compressed, saved) = compress_text(diff);
        assert!(saved > 0);
        assert!(compressed.contains("src/lib.rs"));
        assert!(compressed.contains("+2 -0"));
    }

    #[test]
    fn test_autodetect_and_compress_grep() {
        let mut grep_text = String::new();
        for i in 1..=30 {
            grep_text.push_str(&format!("crates/super_long_nested_path/src/deeply/nested/module/subsystem.rs:{}:println!(\"debug {}\");\n", i, i));
        }
        assert_eq!(autodetect_filter(&grep_text), DetectedFilter::Grep);
        let (compressed, saved) = compress_text(&grep_text);
        assert!(saved > 0);
        assert!(compressed.contains("matches in"));
        assert!(compressed.contains("[file] crates/super_long_nested_path/src/deeply/nested/module/subsystem.rs"));
    }

    #[test]
    fn test_autodetect_and_compress_ls() {
        let ls_text = r#"total 64
-rw-r--r-- 1 user group 1234 Jan 01 10:00 file1.txt
-rw-r--r-- 1 user group 5678 Jan 01 10:01 file2.rs
drwxr-xr-x 2 user group 4096 Jan 01 10:02 mydir
"#;
        assert_eq!(autodetect_filter(ls_text), DetectedFilter::Ls);
        let (compressed, saved) = compress_text(ls_text);
        assert!(saved > 0);
        assert!(compressed.contains("mydir/"));
        assert!(compressed.contains("file1.txt"));
    }

    #[test]
    fn test_compress_chat_request_tool_messages() {
        let mut req = ChatCompletionRequest {
            model: "gpt-4".into(),
            messages: vec![
                ChatMessage {
                    role: "user".into(),
                    content: Some(MessageContent::Text("run git diff".into())),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                },
                ChatMessage {
                    role: "tool".into(),
                    content: Some(MessageContent::Text(r#"diff --git a/app.rs b/app.rs
--- a/app.rs
+++ b/app.rs
@@ -1,3 +1,4 @@
 fn test() {
+    println!("ok");
 }
"#.into())),
                    name: None,
                    tool_calls: None,
                    tool_call_id: Some("call_123".into()),
                },
            ],
            ..Default::default()
        };

        let saved = compress_chat_request(&mut req);
        assert!(saved > 0);
        let tool_content = req.messages[1].content.as_ref().unwrap().as_text();
        assert!(tool_content.contains("app.rs"));
    }

    #[test]
    fn test_compress_claude_request_skips_errors() {
        let sample_diff = r#"diff --git a/src/lib.rs b/src/lib.rs
index abcdef1..1234567 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -10,6 +10,8 @@ fn compute() {
     let x = 1;
+    let y = 2;
+    let z = 3;
     let w = 4;
 }
"#;
        let mut req = ClaudeMessageRequest {
            model: "claude-3-5-sonnet".into(),
            max_tokens: 1024,
            messages: vec![crate::translator::ClaudeMessage {
                role: "user".into(),
                content: ClaudeContent::Blocks(vec![
                    ClaudeBlock::ToolResult {
                        tool_use_id: "tool_err".into(),
                        content: Some(serde_json::Value::String(sample_diff.into())),
                        is_error: Some(true),
                    },
                    ClaudeBlock::ToolResult {
                        tool_use_id: "tool_ok".into(),
                        content: Some(serde_json::Value::String(sample_diff.into())),
                        is_error: None,
                    },
                ]),
            }],
            system: None,
            metadata: None,
            stop_sequences: None,
            stream: None,
            temperature: None,
            top_p: None,
            top_k: None,
            tools: None,
            tool_choice: None,
        };

        let saved = compress_claude_request(&mut req);
        assert!(saved > 0);

        if let ClaudeContent::Blocks(ref blocks) = req.messages[0].content {
            // First block (is_error: true) should be untouched
            if let ClaudeBlock::ToolResult { content, is_error, .. } = &blocks[0] {
                assert_eq!(is_error, &Some(true));
                assert!(content.as_ref().unwrap().as_str().unwrap().contains("diff --git"));
            }
            // Second block should be compressed
            if let ClaudeBlock::ToolResult { content, is_error, .. } = &blocks[1] {
                assert_eq!(is_error, &None);
                assert!(!content.as_ref().unwrap().as_str().unwrap().contains("diff --git"));
            }
        }
    }
}
