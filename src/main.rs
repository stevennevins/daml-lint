mod detector;
mod detectors;
mod ir;
mod parser;
mod reporter;

use clap::Parser;
use detector::parse_severity;
use reporter::OutputFormat;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "daml-lint")]
#[command(about = "Static analysis scanner for DAML smart contracts")]
#[command(version)]
struct Cli {
    /// DAML files or directories to scan
    #[arg(required = true)]
    paths: Vec<PathBuf>,

    /// Output format: sarif, markdown, json
    #[arg(short, long, default_value = "markdown")]
    format: String,

    /// Output file (default: stdout)
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Minimum severity to cause non-zero exit: critical, high, medium, low, info
    #[arg(long, default_value = "high")]
    fail_on: String,

    /// Custom rule files: .json (regex rules) or .rhai (AST rules). Repeatable.
    /// See examples/custom-rules.json and examples/*.rhai
    #[arg(long)]
    rules: Vec<PathBuf>,
}

fn main() {
    let cli = Cli::parse();

    let format = OutputFormat::from_str(&cli.format).unwrap_or_else(|| {
        eprintln!(
            "Unknown format '{}'. Use sarif, markdown, or json.",
            cli.format
        );
        std::process::exit(2);
    });

    let fail_on = parse_severity(&cli.fail_on).unwrap_or_else(|| {
        eprintln!(
            "Unknown severity '{}'. Use critical, high, medium, low, or info.",
            cli.fail_on
        );
        std::process::exit(2);
    });

    // Load detectors first so rule-file errors surface before scanning
    let mut detectors = detector::all_detectors();
    for rules_path in &cli.rules {
        let loaded = match rules_path.extension().and_then(|e| e.to_str()) {
            Some("json") => detectors::custom::load_rules(rules_path),
            Some("rhai") => detectors::script::load_script(rules_path).map(|d| vec![d]),
            _ => Err(format!(
                "rules file {} must end in .json (regex rules) or .rhai (AST rules)",
                rules_path.display()
            )),
        };
        match loaded {
            Ok(rules) => {
                if rules.is_empty() {
                    eprintln!("Warning: rules file {} contains no rules.", rules_path.display());
                }
                detectors.extend(rules);
            }
            Err(e) => {
                eprintln!("Error: {}", e);
                std::process::exit(2);
            }
        }
    }
    if let Some(dup) = detector::find_duplicate_name(&detectors) {
        eprintln!(
            "Error: rule '{}': name collides with a built-in detector or another rule",
            dup
        );
        std::process::exit(2);
    }

    // Discover .daml files
    let files = discover_files(&cli.paths);
    if files.is_empty() {
        eprintln!("No .daml files found.");
        std::process::exit(2);
    }

    eprintln!("daml-lint: scanning {} file(s)...", files.len());
    let mut all_findings = Vec::new();

    for file in &files {
        let source = match std::fs::read_to_string(file) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Warning: could not read {}: {}", file.display(), e);
                continue;
            }
        };

        let module = parser::parse_daml(&source, file);

        for det in &detectors {
            let findings = det.detect(&module);
            all_findings.extend(findings);
        }
    }

    // Sort findings by severity, then file, then line
    all_findings.sort_by(|a, b| {
        a.severity
            .cmp(&b.severity)
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.line.cmp(&b.line))
    });

    // Format output
    let output = reporter::format_findings(&all_findings, format);

    if let Some(output_path) = &cli.output {
        std::fs::write(output_path, &output).unwrap_or_else(|e| {
            eprintln!("Error writing to {}: {}", output_path.display(), e);
            std::process::exit(2);
        });
        eprintln!(
            "daml-lint: {} finding(s) written to {}",
            all_findings.len(),
            output_path.display()
        );
    } else {
        println!("{}", output);
    }

    let code = reporter::exit_code(&all_findings, fail_on);
    std::process::exit(code);
}

fn discover_files(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for path in paths {
        if path.is_file() {
            if path.extension().is_some_and(|e| e == "daml") {
                files.push(path.clone());
            }
        } else if path.is_dir() {
            walk_dir(path, &mut files);
        }
    }
    files.sort();
    files
}

fn walk_dir(dir: &PathBuf, files: &mut Vec<PathBuf>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk_dir(&path, files);
            } else if path.extension().is_some_and(|e| e == "daml") {
                files.push(path);
            }
        }
    }
}
