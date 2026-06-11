//! Corpus harness for the transcript parser (task 3.2).
//!
//! Parses every `*.jsonl` under a directory tree (read-only) with the
//! production parser and prints aggregate stats plus the collapsed request
//! count, for cross-checking against an independent scan of
//! `~/.claude/projects/`. Also handy while developing the backfill engine
//! (task 3.4).
//!
//! Usage:
//!
//! ```sh
//! cargo run --example parse_transcripts -- ~/.claude/projects
//! ```

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use claude_usage_tracker_lib::transcript::{collapse_requests, parse_file, ParseStats};

fn main() {
    let root = std::env::args()
        .nth(1)
        .expect("usage: parse_transcripts <dir>");
    let mut files = Vec::new();
    collect_jsonl(Path::new(&root), &mut files);
    files.sort();

    let mut totals = ParseStats::default();
    let mut request_ids: HashSet<String> = HashSet::new();
    let mut collapsed_total = 0usize;
    let mut sidechain_lines = 0u64;
    let mut io_errors = 0u32;

    for file in &files {
        let parsed = match parse_file(file) {
            Ok(parsed) => parsed,
            Err(err) => {
                eprintln!("{}: {err}", file.display());
                io_errors += 1;
                continue;
            }
        };
        totals.lines_read += parsed.stats.lines_read;
        totals.assistant_lines += parsed.stats.assistant_lines;
        totals.skipped_lines += parsed.stats.skipped_lines;
        totals.malformed_lines += parsed.stats.malformed_lines;
        totals.invalid_assistant_lines += parsed.stats.invalid_assistant_lines;
        sidechain_lines += parsed.lines.iter().filter(|l| l.is_sidechain).count() as u64;
        let collapsed = collapse_requests(&parsed.lines);
        collapsed_total += collapsed.len();
        request_ids.extend(collapsed.iter().filter_map(|r| r.request_id.clone()));
    }

    println!("files:                   {}", files.len());
    println!("io_errors:               {io_errors}");
    println!("lines_read:              {}", totals.lines_read);
    println!("assistant_lines:         {}", totals.assistant_lines);
    println!("  sidechain:             {sidechain_lines}");
    println!("skipped_lines:           {}", totals.skipped_lines);
    println!("malformed_lines:         {}", totals.malformed_lines);
    println!(
        "invalid_assistant_lines: {}",
        totals.invalid_assistant_lines
    );
    println!("collapsed_requests:      {collapsed_total}");
    println!("distinct_request_ids:    {}", request_ids.len());
}

fn collect_jsonl(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_jsonl(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "jsonl") {
            out.push(path);
        }
    }
}
