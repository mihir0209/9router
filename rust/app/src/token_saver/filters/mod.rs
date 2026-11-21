use std::collections::BTreeMap;

pub fn filter_git_diff(input: &str) -> Option<String> {
    let mut result = Vec::new();
    let mut current_file = String::new();
    let mut added = 0;
    let mut removed = 0;
    let mut in_hunk = false;
    let mut hunk_shown = 0;
    let mut hunk_skipped = 0;
    let max_hunk_lines = 100;

    for line in input.lines() {
        if line.starts_with("diff --git") {
            if hunk_skipped > 0 {
                result.push(format!("  ... ({} lines truncated)", hunk_skipped));
                hunk_skipped = 0;
            }
            if !current_file.is_empty() && (added > 0 || removed > 0) {
                result.push(format!("  +{} -{}", added, removed));
            }
            let parts: Vec<&str> = line.split(" b/").collect();
            current_file = if parts.len() > 1 {
                parts[1..].join(" b/")
            } else {
                "unknown".to_string()
            };
            result.push(format!("\n{}", current_file));
            added = 0;
            removed = 0;
            in_hunk = false;
            hunk_shown = 0;
        } else if line.starts_with("@@") {
            if hunk_skipped > 0 {
                result.push(format!("  ... ({} lines truncated)", hunk_skipped));
                hunk_skipped = 0;
            }
            in_hunk = true;
            hunk_shown = 0;
            result.push(format!("  {}", line));
        } else if in_hunk {
            if line.starts_with('+') && !line.starts_with("+++") {
                added += 1;
                if hunk_shown < max_hunk_lines {
                    result.push(format!("  {}", line));
                    hunk_shown += 1;
                } else {
                    hunk_skipped += 1;
                }
            } else if line.starts_with('-') && !line.starts_with("---") {
                removed += 1;
                if hunk_shown < max_hunk_lines {
                    result.push(format!("  {}", line));
                    hunk_shown += 1;
                } else {
                    hunk_skipped += 1;
                }
            } else if hunk_shown < max_hunk_lines {
                result.push(format!("  {}", line));
                hunk_shown += 1;
            } else {
                hunk_skipped += 1;
            }
        }
    }

    if hunk_skipped > 0 {
        result.push(format!("  ... ({} lines truncated)", hunk_skipped));
    }
    if !current_file.is_empty() && (added > 0 || removed > 0) {
        result.push(format!("  +{} -{}", added, removed));
    }

    if result.is_empty() {
        None
    } else {
        Some(result.join("\n"))
    }
}

pub fn filter_grep(input: &str) -> Option<String> {
    let mut by_file: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    let mut total = 0;

    for line in input.lines() {
        let first = match line.find(':') {
            Some(idx) => idx,
            None => continue,
        };
        let second = match line[first + 1..].find(':') {
            Some(idx) => first + 1 + idx,
            None => continue,
        };

        let file = &line[..first];
        let line_num_str = &line[first + 1..second];
        let content = &line[second + 1..];

        if line_num_str.parse::<usize>().is_err() {
            continue;
        }

        total += 1;
        by_file
            .entry(file.to_string())
            .or_default()
            .push((line_num_str.to_string(), content.to_string()));
    }

    if total == 0 {
        return None;
    }

    let mut out = format!("{} matches in {} files:\n\n", total, by_file.len());
    let per_file_max = 10;

    for (file, matches) in by_file {
        out.push_str(&format!("[file] {} ({}):\n", file, matches.len()));
        let show = &matches[..matches.len().min(per_file_max)];
        for (num, content) in show {
            out.push_str(&format!("  {:>4}: {}\n", num, content.trim()));
        }
        if matches.len() > per_file_max {
            out.push_str(&format!("  +{} more matches\n", matches.len() - per_file_max));
        }
        out.push('\n');
    }

    Some(out)
}

pub fn filter_find(input: &str) -> Option<String> {
    let lines: Vec<&str> = input.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.is_empty() {
        return None;
    }

    let mut by_dir: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for path in &lines {
        let last_sep = path.rfind('/').or_else(|| path.rfind('\\'));
        let (dir, basename) = match last_sep {
            Some(idx) => {
                let d = if idx == 0 { "/" } else { &path[..idx] };
                (d, &path[idx + 1..])
            }
            None => (".", *path),
        };

        by_dir
            .entry(dir.replace('\\', "/"))
            .or_default()
            .push(basename.to_string());
    }

    let mut out = format!("{} files in {} dirs:\n\n", lines.len(), by_dir.len());
    let total_dir_max = 20;
    let per_dir_max = 10;

    let dirs: Vec<(String, Vec<String>)> = by_dir.into_iter().collect();
    let show_dirs = &dirs[..dirs.len().min(total_dir_max)];

    for (dir, files) in show_dirs {
        out.push_str(&format!("{}/  ({})\n", dir, files.len()));
        let show_files = &files[..files.len().min(per_dir_max)];
        for f in show_files {
            out.push_str(&format!("  {}\n", f));
        }
        if files.len() > per_dir_max {
            out.push_str(&format!("  +{} more files\n", files.len() - per_dir_max));
        }
    }

    if dirs.len() > total_dir_max {
        out.push_str(&format!("\n+{} more dirs\n", dirs.len() - total_dir_max));
    }

    Some(out)
}

pub fn filter_ls(input: &str) -> Option<String> {
    let mut dirs = Vec::new();
    let mut files = Vec::new();

    for line in input.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("total ") || trimmed.is_empty() {
            continue;
        }

        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        if parts.len() < 8 {
            continue;
        }

        let perms = parts[0];
        let file_type = perms.chars().next().unwrap_or('-');
        let name = parts[8..].join(" ");

        if name == "." || name == ".." {
            continue;
        }

        // Skip noise dirs
        if ["node_modules", ".git", "target", "__pycache__", ".next", "dist", "build", ".cache", ".venv"].contains(&name.as_str()) {
            continue;
        }

        if file_type == 'd' {
            dirs.push(name);
        } else if file_type == '-' || file_type == 'l' {
            let size = parts[4];
            files.push((name, size.to_string()));
        }
    }

    if dirs.is_empty() && files.is_empty() {
        return None;
    }

    let mut out = String::new();
    if !dirs.is_empty() {
        out.push_str("Directories:\n");
        for d in dirs {
            out.push_str(&format!("  {}/\n", d));
        }
        out.push('\n');
    }

    if !files.is_empty() {
        out.push_str("Files:\n");
        for (f, sz) in files {
            out.push_str(&format!("  {:<30} {}\n", f, sz));
        }
    }

    Some(out)
}

pub fn filter_build_output(input: &str) -> Option<String> {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let mut compiling_count = 0;

    for line in input.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if trimmed.starts_with("Compiling ") || trimmed.starts_with("Downloading ") {
            compiling_count += 1;
            continue;
        }

        if trimmed.starts_with("error") || trimmed.starts_with("npm ERR!") || trimmed.starts_with("ERROR:") {
            errors.push(trimmed.to_string());
        } else if trimmed.starts_with("warning") || trimmed.starts_with("npm warn") {
            warnings.push(trimmed.to_string());
        }
    }

    if errors.is_empty() && warnings.is_empty() && compiling_count == 0 {
        return None;
    }

    let mut out = String::new();
    if compiling_count > 0 {
        out.push_str(&format!("(Processed {} compiling/downloading artifacts)\n\n", compiling_count));
    }
    if !errors.is_empty() {
        out.push_str("Errors:\n");
        for e in errors {
            out.push_str(&format!("  {}\n", e));
        }
        out.push('\n');
    }
    if !warnings.is_empty() {
        out.push_str("Warnings:\n");
        for w in warnings.iter().take(20) {
            out.push_str(&format!("  {}\n", w));
        }
        if warnings.len() > 20 {
            out.push_str(&format!("  +{} more warnings\n", warnings.len() - 20));
        }
    }

    Some(out)
}
