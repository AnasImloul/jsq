//! `jsq` — command-line front-end for the BigJSON engine.
//!
//! Thin wrapper: parse args, hand the query to the engine, render the
//! result set via `engine::render`, write to stdout. Every byte the
//! user sees is produced by code that also serves the Swift app — no
//! parallel implementation.

use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

use std::collections::HashMap;

use engine::document::Document;
use engine::query::{self, evaluator, ParamValue};
use engine::render;

/// SQL-shaped query tool for very-large JSON files. Always emits one
/// JSON value per line so the output is composable with `jq`, `wc`,
/// `head`, and friends — no table mode, no CSV, no `--format`. If you
/// need spreadsheet output, use the BigJSON desktop app's export menu.
#[derive(Parser, Debug)]
#[command(
    name = "jsq",
    version,
    about = "SQL-shaped queries on very-large JSON files.",
    long_about = None,
)]
struct Args {
    /// Path to a JSON file, or `-` to read from stdin.
    #[arg(value_name = "FILE")]
    file: Option<String>,

    /// Query expression. Quote it to keep your shell from globbing the
    /// surface syntax (e.g. brackets, asterisks).
    #[arg(value_name = "QUERY")]
    query: Option<String>,

    /// Print query stats (elapsed, rows scanned, bytes scanned, lookup
    /// count) to stderr after the result stream.
    #[arg(short, long)]
    stats: bool,

    /// Print only stats; suppress the result stream entirely. Useful
    /// for "how big is this query?" probes.
    #[arg(short = 'S', long)]
    stats_only: bool,

    /// Maximum number of result rows. Default is unlimited — pipe the
    /// output through `head` if you want a hard cap.
    #[arg(short = 'n', long, value_name = "N")]
    limit: Option<usize>,

    /// Bind a `$name` query parameter: `--param NAME=VALUE` (repeatable).
    /// VALUE is read as a JSON scalar — `true`/`false`/`null`, a number,
    /// or a `"quoted"` string; anything else is taken as a raw string.
    #[arg(short = 'p', long = "param", value_name = "NAME=VALUE")]
    params: Vec<String>,

    /// Print the lowered engine AST instead of running the query.
    /// Useful when debugging surface-syntax → engine translation.
    #[arg(short, long)]
    explain: bool,

    /// Re-emit the query as the formatter would render it. Doesn't
    /// open the document; pass only the query as the single positional.
    #[arg(long)]
    format_only: bool,
}

fn main() -> ExitCode {
    let args = Args::parse();

    // --format-only: query in (single positional), formatted query out.
    if args.format_only {
        let query = match args.file.as_deref() {
            // When `--format-only` is set, the first positional carries
            // the query — there's no document, so the "file" slot is
            // overloaded to keep the invocation `jsq --format-only '…'`
            // short and obvious.
            Some(q) => q.to_string(),
            None => {
                eprintln!("jsq: --format-only requires a query positional");
                return ExitCode::from(1);
            }
        };
        match query::surface::format(&query) {
            Ok(out) => {
                println!("{}", out);
                return ExitCode::SUCCESS;
            }
            Err(e) => {
                eprintln!("jsq: parse error at position {}: {}", e.position, e.message);
                return ExitCode::from(2);
            }
        }
    }

    let Some(file_arg) = args.file else {
        eprintln!("jsq: missing FILE positional (use `-` for stdin)\n\n{}", brief_usage());
        return ExitCode::from(1);
    };
    let Some(query) = args.query else {
        eprintln!("jsq: missing QUERY positional\n\n{}", brief_usage());
        return ExitCode::from(1);
    };

    let params = match parse_params(&args.params) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("jsq: {}", msg);
            return ExitCode::from(1);
        }
    };

    // --explain: compile and lower, print canonical AST. No document
    // needed — this is purely a parser/lowerer debugging aid.
    if args.explain {
        match query::compile_with_params(&query, &params) {
            Ok(ast) => {
                println!("{}", ast);
                return ExitCode::SUCCESS;
            }
            Err(e) => {
                eprintln!("jsq: parse error at position {}: {}", e.position, e.message);
                return ExitCode::from(2);
            }
        }
    }

    let (doc, _tempfile) = match open_document(&file_arg) {
        Ok(pair) => pair,
        Err(msg) => {
            eprintln!("jsq: {}", msg);
            return ExitCode::from(1);
        }
    };

    let ast = match query::compile_with_params(&query, &params) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("jsq: parse error at position {}: {}", e.position, e.message);
            return ExitCode::from(2);
        }
    };

    // Unlimited by default — represented to the evaluator as the
    // largest finite cap usize can hold (the FFI's u32 limit doesn't
    // apply here since we're calling Rust directly).
    let limit = args.limit.unwrap_or(usize::MAX);

    evaluator::build_indexes(&doc, &ast);

    let started = std::time::Instant::now();
    let output = evaluator::run(&doc, &ast, 0, limit);
    let elapsed = started.elapsed();

    if let Some(err) = &output.error {
        eprintln!("jsq: evaluation error: {:?}", err);
        return ExitCode::from(3);
    }

    if !args.stats_only {
        // One JSON value per line — composable with the rest of the
        // unix pipeline. The format choice is intentionally not
        // user-facing: a CLI tool that emits something other than its
        // domain's native data shape (here: JSON) breaks composition.
        let rendered = render::render_ndjson(&output.results, &doc);
        let mut stdout = io::stdout().lock();
        let _ = stdout.write_all(rendered.as_bytes());
    }

    if args.stats || args.stats_only {
        write_stats(&output, elapsed);
    }

    ExitCode::SUCCESS
}

/// Parses `--param NAME=VALUE` strings into a parameter map. VALUE is
/// read as a JSON scalar: `true`/`false`/`null`, a number, or a
/// `"quoted"` string; anything else is taken as a raw (unquoted) string.
fn parse_params(raw: &[String]) -> Result<HashMap<String, ParamValue>, String> {
    let mut out = HashMap::with_capacity(raw.len());
    for entry in raw {
        let (name, value) = entry
            .split_once('=')
            .ok_or_else(|| format!("--param expects NAME=VALUE, got `{}`", entry))?;
        if name.is_empty() {
            return Err(format!("--param has an empty name: `{}`", entry));
        }
        out.insert(name.to_string(), parse_param_value(value));
    }
    Ok(out)
}

fn parse_param_value(s: &str) -> ParamValue {
    let t = s.trim();
    match t {
        "true" => return ParamValue::Bool(true),
        "false" => return ParamValue::Bool(false),
        "null" => return ParamValue::Null,
        _ => {}
    }
    if t.len() >= 2 && t.starts_with('"') && t.ends_with('"') {
        return ParamValue::Str(t[1..t.len() - 1].to_string());
    }
    if let Ok(n) = t.parse::<f64>() {
        return ParamValue::Number(n);
    }
    ParamValue::Str(s.to_string())
}

fn write_stats(output: &evaluator::EvalOutput, elapsed: std::time::Duration) {
    let mut err = io::stderr().lock();
    let _ = writeln!(err, "─── stats ───");
    let _ = writeln!(err, "elapsed:   {}", fmt_duration(elapsed));
    let _ = writeln!(
        err,
        "scanned:   {} rows · {}",
        fmt_int(output.scanned_rows),
        fmt_bytes(output.scanned_bytes),
    );
    if output.lookup_calls > 0 {
        let _ = writeln!(err, "lookups:   {}", fmt_int(output.lookup_calls));
    }
    let _ = writeln!(
        err,
        "output:    {} row{}",
        fmt_int(output.results.len() as u64),
        if output.results.len() == 1 { "" } else { "s" },
    );
}

/// Opens the document at `arg`, or reads stdin into a temp file when
/// `arg == "-"`. Returns both the opened Document and the optional
/// TempFile guard that cleans up the staged stdin bytes on drop.
fn open_document(arg: &str) -> Result<(Document, Option<TempFile>), String> {
    if arg == "-" {
        let mut buf = Vec::new();
        io::stdin()
            .read_to_end(&mut buf)
            .map_err(|e| format!("reading stdin: {}", e))?;
        let temp = TempFile::write_bytes(&buf)
            .map_err(|e| format!("staging stdin: {}", e))?;
        let doc = Document::open(temp.path(), None)
            .map_err(|e| format!("opening stdin: {}", e.message()))?;
        return Ok((doc, Some(temp)));
    }
    let path = PathBuf::from(arg);
    let doc = Document::open(&path, None)
        .map_err(|e| format!("opening {}: {}", path.display(), e.message()))?;
    Ok((doc, None))
}

/// RAII wrapper around a temp file that deletes itself on drop. Used
/// to stage stdin bytes for mmap-backed document opening.
struct TempFile {
    path: PathBuf,
}

impl TempFile {
    fn write_bytes(bytes: &[u8]) -> io::Result<Self> {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!("jsq-stdin-{}.json", nanos));
        std::fs::write(&path, bytes)?;
        Ok(Self { path })
    }

    fn path(&self) -> &PathBuf {
        &self.path
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn fmt_duration(d: std::time::Duration) -> String {
    let secs = d.as_secs_f64();
    if secs >= 1.0 {
        format!("{:.2}s", secs)
    } else if secs >= 0.001 {
        format!("{:.0}ms", secs * 1000.0)
    } else {
        format!("{}µs", d.as_micros())
    }
}

fn fmt_bytes(n: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    if n >= GB {
        format!("{:.2} GB", n as f64 / GB as f64)
    } else if n >= MB {
        format!("{:.2} MB", n as f64 / MB as f64)
    } else if n >= KB {
        format!("{:.1} KB", n as f64 / KB as f64)
    } else {
        format!("{} B", n)
    }
}

fn fmt_int(n: u64) -> String {
    // Thousands separators, en_US style. The locale crate would be
    // overkill for a single integer formatter.
    let s = n.to_string();
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

fn brief_usage() -> &'static str {
    "Usage:\n  jsq <FILE> <QUERY> [--stats] [--limit N]\n  jsq --format-only <QUERY>\n  jsq --explain <FILE> <QUERY>\n  jsq --help"
}
