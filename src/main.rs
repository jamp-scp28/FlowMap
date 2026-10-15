mod check;
mod cli;
mod diff;
mod extractors;
mod html;
mod impact;
mod linker;
mod markdown;
mod model;
mod query;

use anyhow::{Context, Result};
use check::FlowAuditor;
use clap::Parser;
use cli::{Cli, Commands};
use colored::*;
use diff::FlowDiffer;
use extractors::csharp::CSharpExtractor;
use extractors::rust::RustExtractor;
use html::HtmlGenerator;
use impact::ImpactAnalyzer;
use linker::Linker;
use markdown::MarkdownGenerator;
use model::{ConnectedFlowMap, Flow, FlowAnnotation, Step};
use query::QueryEngine;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::process::Command;
use walkdir::WalkDir;

fn main() -> Result<()> {
    let args = Cli::parse();

    match args.command {
        Commands::Build {
            flows_dir,
            out,
            html,
            md,
            annotations,
        } => {
            println!("{}", "Building Connected FlowMap...".bold().bright_blue());
            let flows = load_flows_from_dir(&flows_dir)?;
            println!("Loaded {} raw flow definition(s).", flows.len().to_string().bold());

            let annotations_map = if let Some(ann_path) = annotations {
                load_annotations(&ann_path)?
            } else if Path::new("annotations.json").exists() {
                load_annotations(Path::new("annotations.json"))?
            } else {
                HashMap::new()
            };

            let connected = Linker::build(flows, annotations_map);

            // Write flowmap.json
            let json = serde_json::to_string_pretty(&connected)?;
            fs::write(&out, json)?;
            println!("Wrote connected graph dataset to: {}", out.display().to_string().bright_green());

            // Write markdown flowbook (decision tables & Mermaid)
            let md_content = MarkdownGenerator::generate(&connected, "Repository");
            fs::write(&md, md_content)?;
            println!("Wrote Markdown Flowbook to:        {}", md.display().to_string().bright_green());

            // Write interactive flowmap.html
            HtmlGenerator::generate(&connected, &html)?;
            println!("Wrote visual explorer to:          {}", html.display().to_string().bright_green());

            // Summary Stats
            println!("\n{}", "---------------- STATS ----------------".bright_black());
            println!("Total Flows:          {}", connected.stats.total_flows.to_string().bold());
            println!("Decisions (Branches): {}", connected.stats.total_decisions.to_string().bold());
            println!("Effects (Sinks):      {}", connected.stats.total_effects.to_string().bold());
            println!("DB Tables Tracked:    {}", connected.stats.total_tables_tracked.to_string().bold());
            println!("Events Tracked:       {}", connected.stats.total_events_tracked.to_string().bold());
            println!("Feature Flags:        {}", connected.stats.total_flags_tracked.to_string().bold());

            if !connected.stats.unconsumed_events.is_empty() {
                println!(
                    "{} {} events published with zero consumers registered:",
                    "WARNING:".bold().yellow(),
                    connected.stats.unconsumed_events.len()
                );
                for ev in &connected.stats.unconsumed_events {
                    println!("  * {}", ev.yellow());
                }
            }
            println!("{}", "---------------------------------------".bright_black());
        }

        Commands::Scan { repo, out, refs } => {
            let repo_name = repo.file_name().and_then(|n| n.to_str()).unwrap_or("repository");
            println!("{}", format!("Scanning & Mapping Repository: {}...", repo.display()).bold().bright_blue());

            let target_dir = repo.join(&out);
            let raw_flows_dir = target_dir.join("raw");
            fs::create_dir_all(&raw_flows_dir)?;

            // 1. Detect language
            let is_rust = repo.join("Cargo.toml").exists();
            let is_csharp = WalkDir::new(&repo).max_depth(3).into_iter().filter_map(|e| e.ok()).any(|e| {
                e.path().extension().map_or(false, |ext| ext == "csproj" || ext == "sln")
            });

            if is_rust {
                println!("Detected Rust project (Cargo.toml). Running Native Rust Extractor...");
                let src_dir = repo.join("src");
                let scan_dir = if src_dir.exists() { &src_dir } else { &repo };
                let flows = RustExtractor::extract(scan_dir)?;
                for f in &flows {
                    let clean_part = f.trigger.path.as_deref().unwrap_or(&f.id)
                        .replace(['/', '\\', ':', '<', '>', '"', '|', '?', '*'], "_")
                        .replace(['{', '}'], "")
                        .replace(' ', "_");
                    let filename = format!(
                        "{}_{}_{}.json",
                        f.domain_id,
                        f.trigger.method.as_deref().unwrap_or("FLOW"),
                        clean_part
                    );
                    let file_path = raw_flows_dir.join(filename);
                    fs::write(&file_path, serde_json::to_string_pretty(f)?)?;
                }
                println!("Extracted {} Rust flows to: {}", flows.len().to_string().bold(), raw_flows_dir.display());
            } else if is_csharp {
                println!("Detected C# project (.csproj/.sln). Running C# Roslyn Extractor...");
                CSharpExtractor::run(&repo, &raw_flows_dir, refs.as_deref(), None)?;
            } else {
                anyhow::bail!("Could not automatically determine project type in '{}'. Expected Cargo.toml or .csproj/.sln.", repo.display());
            }

            // 2. Build connected flowmap
            let flows = load_flows_from_dir(&raw_flows_dir)?;
            let annotations_path = target_dir.join("annotations.json");
            let annotations_map = if annotations_path.exists() {
                load_annotations(&annotations_path)?
            } else {
                HashMap::new()
            };

            let connected = Linker::build(flows, annotations_map);

            // 3. Write outputs inside target repo's .flowmap folder!
            let json_path = target_dir.join("flowmap.json");
            fs::write(&json_path, serde_json::to_string_pretty(&connected)?)?;

            let md_path = target_dir.join("FLOWS.md");
            let md_content = MarkdownGenerator::generate(&connected, repo_name);
            fs::write(&md_path, md_content)?;

            let html_path = target_dir.join("index.html");
            HtmlGenerator::generate(&connected, &html_path)?;

            println!("\n{}", "================ MAP COMPLETE ================".bold().bright_green());
            println!("Target Artifacts saved in: {}", target_dir.display().to_string().bright_yellow());
            println!("  * Markdown Flowbook:     {}", md_path.display().to_string().bold().bright_white());
            println!("  * Visual Explorer:       {}", html_path.display().to_string().bold().bright_white());
            println!("  * Graph Dataset:         {}", json_path.display().to_string().bold().bright_white());
            println!("Total Flows:               {}", connected.stats.total_flows.to_string().bold());
            println!("Decisions (Branches):      {}", connected.stats.total_decisions.to_string().bold());
            println!("Effects (DB/Events/Calls): {}", connected.stats.total_effects.to_string().bold());
            println!("Tables Tracked:            {}", connected.stats.total_tables_tracked.to_string().bold());
            println!("{}", "==============================================".bold().bright_green());

            // 4. Run audit
            let audit = FlowAuditor::audit(&connected);
            if audit.total_issues > 0 {
                println!();
                FlowAuditor::print_report(&audit);
            }
        }

        Commands::Explore { html, rebuild } => {
            if rebuild || !html.exists() {
                println!("Rebuilding flowmap before launch...");
                let flows = load_flows_from_dir(Path::new("flows"))?;
                let connected = Linker::build(flows, HashMap::new());
                HtmlGenerator::generate(&connected, &html)?;
            }

            println!("Opening interactive explorer in browser: {}", html.display().to_string().bright_green());

            #[cfg(target_os = "windows")]
            {
                Command::new("cmd")
                    .args(["/C", "start", &html.to_string_lossy()])
                    .spawn()?;
            }
            #[cfg(target_os = "macos")]
            {
                Command::new("open").arg(&html).spawn()?;
            }
            #[cfg(target_os = "linux")]
            {
                Command::new("xdg-open").arg(&html).spawn()?;
            }
        }

        Commands::Impact {
            map,
            files,
            symbols,
            diff,
        } => {
            let connected_map: ConnectedFlowMap = load_connected_map(&map)?;
            let mut inputs = Vec::new();

            if let Some(git_ref) = diff {
                println!("Running 'git diff --name-only {}' to detect changed files...", git_ref.yellow());
                let output = Command::new("git")
                    .args(["diff", "--name-only", &git_ref])
                    .output()
                    .context("Failed to run git diff")?;

                if output.status.success() {
                    let diff_files = String::from_utf8_lossy(&output.stdout);
                    for line in diff_files.lines() {
                        let trimmed = line.trim();
                        if !trimmed.is_empty() {
                            inputs.push(trimmed.to_string());
                        }
                    }
                } else {
                    eprintln!("Warning: git diff command returned error. Proceeding with explicit arguments.");
                }
            }

            inputs.extend(files);
            inputs.extend(symbols);

            if inputs.is_empty() {
                eprintln!("{}", "Error: No files, symbols, or git diff provided for impact analysis.".red());
                eprintln!("Usage: flowmap impact --files <file1,file2> or --diff <ref>");
                std::process::exit(1);
            }

            let report = ImpactAnalyzer::analyze(&connected_map, &inputs);
            ImpactAnalyzer::print_report(&report);
        }

        Commands::Show { flow_id, map } => {
            let connected_map: ConnectedFlowMap = load_connected_map(&map)?;
            let q = flow_id.to_lowercase();
            let matched = connected_map
                .flows
                .iter()
                .find(|f| f.id.to_lowercase().contains(&q) || f.handler.symbol.to_lowercase().contains(&q));

            match matched {
                Some(f) => print_flow_tree(f),
                None => {
                    eprintln!("No flow found matching '{}'", flow_id.yellow());
                    std::process::exit(1);
                }
            }
        }

        Commands::Query {
            map,
            table,
            event,
            flag,
            search,
        } => {
            let connected_map: ConnectedFlowMap = load_connected_map(&map)?;

            if let Some(tbl) = table {
                QueryEngine::query_table(&connected_map, &tbl);
            }
            if let Some(evt) = event {
                QueryEngine::query_event(&connected_map, &evt);
            }
            if let Some(flg) = flag {
                QueryEngine::query_flag(&connected_map, &flg);
            }
            if let Some(q) = search {
                QueryEngine::search(&connected_map, &q);
            }
        }

        Commands::Extract {
            lang,
            project,
            out,
            refs,
            only,
        } => match lang.to_lowercase().as_str() {
            "csharp" | "cs" => {
                println!("{}", "Running C# Roslyn Extractor...".bold().bright_blue());
                CSharpExtractor::run(&project, &out, refs.as_deref(), only.as_deref())?;
                println!("Extraction completed. Flows saved to: {}", out.display().to_string().bright_green());
            }
            "rust" | "rs" => {
                println!("{}", "Running Native Rust Extractor...".bold().bright_blue());
                let flows = RustExtractor::extract(&project)?;
                fs::create_dir_all(&out)?;
                for f in &flows {
                    let filename = format!(
                        "rust_{}_{}.json",
                        f.trigger.method.as_deref().unwrap_or("FLOW"),
                        f.trigger.path.as_deref().unwrap_or(&f.id).replace('/', "_").replace(['{', '}'], "")
                    );
                    let file_path = out.join(filename);
                    fs::write(&file_path, serde_json::to_string_pretty(f)?)?;
                }
                println!("Extracted {} Rust flows to: {}", flows.len().to_string().bold(), out.display().to_string().bright_green());
            }
            other => {
                eprintln!("Unsupported language: '{}'. Supported: 'csharp', 'rust'.", other.red());
                std::process::exit(1);
            }
        },

        Commands::Diff {
            baseline,
            current,
            json,
        } => {
            let base_map = load_connected_map(&baseline)?;
            let curr_map = load_connected_map(&current)?;

            let report = FlowDiffer::diff(&base_map, &curr_map);
            FlowDiffer::print_report(&report);

            if let Some(json_out) = json {
                let serialized = serde_json::to_string_pretty(&report)?;
                fs::write(&json_out, serialized)?;
                println!("Saved diff report to: {}", json_out.display().to_string().bright_green());
            }

            if !report.breaking_changes.is_empty() {
                std::process::exit(1);
            }
        }

        Commands::Check { map, strict } => {
            let connected_map = load_connected_map(&map)?;
            let report = FlowAuditor::audit(&connected_map);
            FlowAuditor::print_report(&report);

            if strict && (report.critical_count > 0 || report.high_count > 0) {
                eprintln!("{}", "Audit failed: critical or high severity behavioral issues detected.".bold().red());
                std::process::exit(1);
            }
        }
    }

    Ok(())
}

fn load_flows_from_dir(dir: &Path) -> Result<Vec<Flow>> {
    let mut flows = Vec::new();
    if !dir.exists() {
        return Ok(flows);
    }

    for entry in WalkDir::new(dir).into_iter().filter_map(|e| e.ok()) {
        if entry.path().extension().map_or(false, |ext| ext == "json") {
            let filename = entry.file_name().to_string_lossy();
            if filename == "flowmap.json" || filename == "annotations.json" {
                continue;
            }
            if let Ok(content) = fs::read_to_string(entry.path()) {
                if let Ok(flow) = serde_json::from_str::<Flow>(&content) {
                    flows.push(flow);
                }
            }
        }
    }

    Ok(flows)
}

fn load_annotations(path: &Path) -> Result<HashMap<String, FlowAnnotation>> {
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let content = fs::read_to_string(path)?;
    let map: HashMap<String, FlowAnnotation> = serde_json::from_str(&content)?;
    Ok(map)
}

fn load_connected_map(path: &Path) -> Result<ConnectedFlowMap> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("Could not read flowmap file at '{}'", path.display()))?;
    let map = serde_json::from_str(&content)
        .with_context(|| format!("Invalid flowmap JSON structure in '{}'", path.display()))?;
    Ok(map)
}

fn print_flow_tree(flow: &Flow) {
    println!("{}", "============================================================".bright_blue());
    let method = flow.trigger.method.as_deref().unwrap_or("ACTION");
    println!("  {} {}", method.bold().bright_green(), flow.trigger.path.as_deref().unwrap_or(&flow.id).bold().bright_white());
    println!("{}", "============================================================".bright_blue());
    println!("Handler:   {} ({}:{})", flow.handler.symbol.bold(), flow.handler.file, flow.handler.line);
    println!("Signature: {}", flow.handler.signature.dimmed());

    if !flow.guards.is_empty() {
        println!("\nGuards:");
        for g in &flow.guards {
            let kind = g.kind.as_deref().unwrap_or("guard");
            let cond = g.condition.as_deref().unwrap_or("");
            println!("  [GUARD:{}] {}", kind.bright_purple(), cond);
        }
    }

    println!("\nSteps Pipeline:");
    for s in &flow.steps {
        print_step(s, "  ");
    }

    println!("\nCoupling / State:");
    println!("  Reads Tables:    {}", flow.coupling.reads.join(", ").bright_cyan());
    println!("  Writes Tables:   {}", flow.coupling.writes.join(", ").bright_red());
    println!("  Events Emitted:  {}", flow.coupling.events_published.join(", ").bright_green());
    println!("  External Calls:  {}", flow.coupling.calls_services.join(", ").bright_purple());
    if !flow.coupling.shared_state_with.is_empty() {
        println!("  Shared State With {} other flow(s)", flow.coupling.shared_state_with.len().to_string().bold());
    }
    println!();
}

fn print_step(step: &Step, indent: &str) {
    let line_str = step.line.map_or(String::new(), |l| format!(" L{}", l));
    match step.step_type.as_str() {
        "decision" => {
            let kind = step.kind.as_deref().unwrap_or("check");
            let cond = step.condition.as_deref().unwrap_or("cond");
            println!("{}[{}] {}{}", indent, kind.bright_yellow(), cond.bold(), line_str.dimmed());
            if let Some(branches) = &step.branches {
                for b in branches {
                    println!("{}  {} [when {}]", indent, "->".bright_yellow(), b.when.bright_blue());
                    let next_indent = format!("{}     ", indent);
                    for child in &b.steps {
                        print_step(child, &next_indent);
                    }
                }
            }
        }
        "effect" => {
            let kind = step.kind.as_deref().unwrap_or("effect");
            let target = step.target.as_deref().unwrap_or("");
            let op = step.op.as_deref().unwrap_or("");
            println!("{}[{}] {} {}{}", indent, kind.bright_cyan(), target.bold(), op, line_str.dimmed());
        }
        "call" => {
            let sym = step.symbol.as_deref().unwrap_or("call");
            println!("{}[CALL] {}{}", indent, sym.bold().bright_magenta(), line_str.dimmed());
            if let Some(steps) = &step.steps {
                let next_indent = format!("{}  ", indent);
                for child in steps {
                    print_step(child, &next_indent);
                }
            }
        }
        "return" => {
            let status = step.status.as_deref().unwrap_or("return");
            let expr = step.expr.as_deref().unwrap_or("");
            let colored_status = if status.contains("200") || status.contains("201") {
                status.bright_green()
            } else {
                status.bright_red()
            };
            println!("{}[RETURN] {} {}{}", indent, colored_status.bold(), expr.dimmed(), line_str.dimmed());
        }
        "loop" => {
            println!("{}[LOOP]{}", indent, line_str.dimmed());
        }
        _ => {
            println!("{}{}{}", indent, step.step_type, line_str.dimmed());
        }
    }
}
