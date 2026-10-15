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
use extractors::python::PythonExtractor;
use extractors::rust::RustExtractor;
use html::HtmlGenerator;
use impact::ImpactAnalyzer;
use linker::Linker;
use markdown::MarkdownGenerator;
use model::{ConnectedFlowMap, Flow, FlowAnnotation, Step};
use query::QueryEngine;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
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
            multi,
        } => {
            println!("{}", "Building Connected FlowMap...".bold().bright_blue());
            let flows = if let Some(ref sources) = multi {
                let mut all_flows = Vec::new();
                for src in sources {
                    println!("Loading flows from: {}", src.display());
                    let mut src_flows = collect_flows_from_source(src)?;
                    all_flows.append(&mut src_flows);
                }
                all_flows
            } else {
                load_flows_from_dir(&flows_dir)?
            };
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

            // 1. Detect language (supports nested solution/project hierarchies)
            let is_rust = repo.join("Cargo.toml").exists()
                || WalkDir::new(&repo)
                    .max_depth(6)
                    .into_iter()
                    .filter_entry(|e| !is_ignored_scan_dir(e))
                    .filter_map(|e| e.ok())
                    .any(|e| e.file_name() == "Cargo.toml");

            let csharp_entry = WalkDir::new(&repo)
                .max_depth(8)
                .into_iter()
                .filter_entry(|e| !is_ignored_scan_dir(e))
                .filter_map(|e| e.ok())
                .find(|e| {
                    e.path().extension().map_or(false, |ext| ext == "csproj" || ext == "sln")
                });

            let is_python = repo.join("requirements.txt").exists()
                || repo.join("pyproject.toml").exists()
                || repo.join("setup.py").exists()
                || WalkDir::new(&repo)
                    .max_depth(6)
                    .into_iter()
                    .filter_entry(|e| !is_ignored_scan_dir(e))
                    .filter_map(|e| e.ok())
                    .any(|e| e.path().extension().map_or(false, |ext| ext == "py"));

            if is_rust {
                println!("Detected Rust project (Cargo.toml). Running Native Rust Extractor...");
                let src_dir = repo.join("src");
                let scan_dir = if src_dir.exists() { &src_dir } else { &repo };
                let flows = RustExtractor::extract(scan_dir)?;
                write_flows(&raw_flows_dir, &flows)?;
                println!("Extracted {} Rust flows to: {}", flows.len().to_string().bold(), raw_flows_dir.display());
            } else if let Some(ref cs) = csharp_entry {
                println!("Detected C# project ({}). Running C# Roslyn Extractor...", cs.path().display());
                CSharpExtractor::run(&repo, &raw_flows_dir, refs.as_deref(), None)?;
            } else if is_python {
                println!("Detected Python project. Running Python AST Extractor...");
                PythonExtractor::run(&repo, &raw_flows_dir)?;
            } else {
                anyhow::bail!("Could not automatically determine project type in '{}'. Expected Cargo.toml, .csproj/.sln, or Python files.", repo.display());
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
            let actual_html = if html.exists() {
                html
            } else if Path::new("flows/index.html").exists() {
                PathBuf::from("flows/index.html")
            } else if Path::new("flows/flowmap.html").exists() {
                PathBuf::from("flows/flowmap.html")
            } else {
                html
            };

            if rebuild || !actual_html.exists() {
                println!("Rebuilding flowmap before launch...");
                let flows = load_flows_from_dir(Path::new("flows"))?;
                let connected = Linker::build(flows, HashMap::new());
                HtmlGenerator::generate(&connected, &actual_html)?;
            }

            println!("Opening interactive explorer in browser: {}", actual_html.display().to_string().bright_green());

            #[cfg(target_os = "windows")]
            {
                Command::new("cmd")
                    .args(["/C", "start", &actual_html.to_string_lossy()])
                    .spawn()?;
            }
            #[cfg(target_os = "macos")]
            {
                Command::new("open").arg(&actual_html).spawn()?;
            }
            #[cfg(target_os = "linux")]
            {
                Command::new("xdg-open").arg(&actual_html).spawn()?;
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

            let mut hunks = impact::Hunks::new();
            if let Some(git_ref) = diff {
                println!("Running 'git diff -U0 {}' to detect changed members...", git_ref.yellow());
                let output = Command::new("git")
                    .args(["diff", "-U0", "--no-color", "--no-ext-diff", &git_ref])
                    .output()
                    .context("Failed to run git diff")?;

                if output.status.success() {
                    hunks = ImpactAnalyzer::parse_unified_diff(&String::from_utf8_lossy(&output.stdout));
                    // Test files are only listed, never member-matched.
                    inputs.extend(hunks.keys().filter(|f| ImpactAnalyzer::is_test_file(f)).cloned());
                } else {
                    eprintln!("Warning: git diff command returned error. Proceeding with explicit arguments.");
                }
            }

            inputs.extend(files);
            inputs.extend(symbols);

            if inputs.is_empty() && hunks.is_empty() {
                eprintln!("{}", "Error: No files, symbols, or git diff provided for impact analysis.".red());
                eprintln!("Usage: flowmap impact --files <file1,file2> or --diff <ref>");
                std::process::exit(1);
            }

            let report = ImpactAnalyzer::analyze(&connected_map, &inputs, &hunks);
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
                write_flows(&out, &flows)?;
                println!("Extracted {} Rust flows to: {}", flows.len().to_string().bold(), out.display().to_string().bright_green());
            }
            "python" | "py" => {
                println!("{}", "Running Python AST Extractor...".bold().bright_blue());
                fs::create_dir_all(&out)?;
                PythonExtractor::run(&project, &out)?;
                let flows = load_flows_from_dir(&out)?;
                println!("Extracted {} Python flows to: {}", flows.len().to_string().bold(), out.display().to_string().bright_green());
            }
            other => {
                eprintln!("Unsupported language: '{}'. Supported: 'csharp', 'rust', 'python'.", other.red());
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

        Commands::Delta {
            against,
            flows_dir,
            current,
            baseline,
            pr,
            html,
            strict,
        } => {
            println!("{}", "Computing Behavioral Delta & Blast Radius...".bold().bright_blue());

            // 1. Load or scan current flowmap
            let curr_map = if let Some(curr_path) = current {
                load_connected_map(&curr_path)?
            } else {
                let flows_path = Path::new(&flows_dir).join("flowmap.json");
                if flows_path.exists() {
                    load_connected_map(&flows_path)?
                } else {
                    println!("No flowmap.json found in '{}'. Scanning current working directory...", flows_dir);
                    scan_repo_internal(Path::new("."))?
                }
            };

            // 2. Load baseline flowmap
            let base_map = if let Some(base_path) = baseline {
                load_connected_map(&base_path)?
            } else {
                retrieve_git_baseline(&against, &flows_dir)?
            };

            // 3. Compute Delta with context
            let report = FlowDiffer::diff_with_context(&base_map, &curr_map, &against);
            FlowDiffer::print_delta_report(&report);

            // 4. Generate PR Markdown Report
            let flows_folder = Path::new(&flows_dir);
            if pr || flows_folder.exists() {
                if !flows_folder.exists() {
                    let _ = fs::create_dir_all(flows_folder);
                }
                let pr_file = flows_folder.join("PR_DELTA.md");
                let pr_markdown = FlowDiffer::generate_pr_markdown(&report);
                fs::write(&pr_file, pr_markdown)?;
                println!("Generated PR Markdown Report: {}", pr_file.display().to_string().bright_green());
            }

            // 5. Generate interactive Visual Delta HTML
            if html {
                let html_file = flows_folder.join("delta.html");
                HtmlGenerator::generate(&curr_map, &html_file)?;
                println!("Generated Visual Delta Explorer: {}", html_file.display().to_string().bright_green());
            }

            // 6. Strict mode exit
            if strict && (!report.breaking_changes.is_empty() || report.hazards.iter().any(|h| h.severity == "CRITICAL" || h.severity == "HIGH")) {
                eprintln!("{}", "Delta check failed: breaking behavioral changes or critical hazards detected.".bold().red());
                std::process::exit(1);
            }
        }

        Commands::Federate {
            services,
            out,
            html,
            md,
        } => {
            println!("{}", "Federating Cross-Repository Service Mesh...".bold().bright_blue());
            let mut all_flows = Vec::new();
            for src in &services {
                println!("Loading service from: {}", src.display().to_string().bright_cyan());
                let mut src_flows = collect_flows_from_source(src)?;
                println!("  -> Discovered {} flow(s)", src_flows.len());
                all_flows.append(&mut src_flows);
            }

            println!("\nStitching {} total flows across {} service(s)...", all_flows.len().to_string().bold(), services.len());
            let connected = Linker::build(all_flows, HashMap::new());

            let json = serde_json::to_string_pretty(&connected)?;
            fs::write(&out, json)?;
            println!("Wrote federated graph dataset to: {}", out.display().to_string().bright_green());

            let md_content = MarkdownGenerator::generate(&connected, "Federated Mesh");
            fs::write(&md, md_content)?;
            println!("Wrote Federated Flowbook to:      {}", md.display().to_string().bright_green());

            HtmlGenerator::generate(&connected, &html)?;
            println!("Wrote Federated Explorer to:      {}", html.display().to_string().bright_green());

            // Summary Stats
            println!("\n{}", "------------ FEDERATION STATS ------------".bright_black());
            println!("Total Flows:          {}", connected.stats.total_flows.to_string().bold());
            println!("Decisions (Branches): {}", connected.stats.total_decisions.to_string().bold());
            println!("Effects (Sinks):      {}", connected.stats.total_effects.to_string().bold());
            println!("DB Tables Tracked:    {}", connected.stats.total_tables_tracked.to_string().bold());
            println!("Events Tracked:       {}", connected.stats.total_events_tracked.to_string().bold());
            println!("Feature Flags:        {}", connected.stats.total_flags_tracked.to_string().bold());

            if !connected.stats.unconsumed_events.is_empty() {
                println!(
                    "{} {} unconsumed events across federated mesh:",
                    "WARNING:".bold().yellow(),
                    connected.stats.unconsumed_events.len()
                );
                for ev in &connected.stats.unconsumed_events {
                    println!("  * {}", ev.yellow());
                }
            } else {
                println!("{}", "All cross-service events are linked to consumers!".bold().bright_green());
            }
            println!("{}", "------------------------------------------".bright_black());
        }
    }

    Ok(())
}

fn retrieve_git_baseline(against: &str, flows_dir: &str) -> Result<ConnectedFlowMap> {
    // 1. Try git show
    let candidates = [
        format!("{}:{}/flowmap.json", against, flows_dir),
        format!("origin/{}:{}/flowmap.json", against, flows_dir),
        format!("{}:flowmap.json", against),
        format!("origin/{}:flowmap.json", against),
    ];

    for target in &candidates {
        let output = Command::new("git")
            .args(["show", target])
            .output();

        if let Ok(out) = output {
            if out.status.success() && !out.stdout.is_empty() {
                if let Ok(map) = serde_json::from_slice::<ConnectedFlowMap>(&out.stdout) {
                    println!("Successfully loaded baseline flowmap from git object: {}", target.bright_cyan());
                    return Ok(map);
                }
            }
        }
    }

    // 2. Try ephemeral worktree if ref exists
    let ref_check = Command::new("git")
        .args(["rev-parse", "--verify", against])
        .output();

    let target_ref = if let Ok(rc) = ref_check {
        if rc.status.success() {
            against.to_string()
        } else {
            let origin_ref = format!("origin/{}", against);
            let origin_check = Command::new("git")
                .args(["rev-parse", "--verify", &origin_ref])
                .output();
            if let Ok(oc) = origin_check {
                if oc.status.success() {
                    origin_ref
                } else {
                    anyhow::bail!("Git ref '{}' or 'origin/{}' does not exist in local git repository.", against, against);
                }
            } else {
                anyhow::bail!("Could not verify git ref '{}'.", against);
            }
        }
    } else {
        anyhow::bail!("Git command execution failed. Ensure git is installed and in your PATH.");
    };

    println!("{}", format!("No committed flowmap.json on '{}'. Creating ephemeral git worktree to extract baseline...", target_ref).yellow());
    let temp_worktree = Path::new(".flowmap-temp-baseline");
    if temp_worktree.exists() {
        let _ = fs::remove_dir_all(temp_worktree);
        let _ = Command::new("git").args(["worktree", "remove", "--force", ".flowmap-temp-baseline"]).output();
    }

    let wt_res = Command::new("git")
        .args(["worktree", "add", "--detach", ".flowmap-temp-baseline", &target_ref])
        .output();

    match wt_res {
        Ok(out) if out.status.success() => {
            println!("Extracting baseline from branch '{}'...", target_ref.bright_cyan());
            let baseline_map = scan_repo_internal(temp_worktree)?;
            let _ = Command::new("git").args(["worktree", "remove", "--force", ".flowmap-temp-baseline"]).output();
            if temp_worktree.exists() {
                let _ = fs::remove_dir_all(temp_worktree);
            }
            Ok(baseline_map)
        }
        Ok(out) => {
            let err_msg = String::from_utf8_lossy(&out.stderr);
            anyhow::bail!("Failed to create git worktree for baseline '{}': {}", target_ref, err_msg.trim());
        }
        Err(e) => anyhow::bail!("Failed to execute 'git worktree': {}", e),
    }
}

fn scan_repo_internal(repo: &Path) -> Result<ConnectedFlowMap> {
    let is_rust = repo.join("Cargo.toml").exists();
    let is_csharp = WalkDir::new(repo).max_depth(4).into_iter().filter_map(|e| e.ok()).any(|e| {
        e.path().extension().map_or(false, |ext| ext == "csproj" || ext == "sln")
    });
    let is_python = repo.join("requirements.txt").exists()
        || repo.join("pyproject.toml").exists()
        || repo.join("setup.py").exists()
        || WalkDir::new(repo).max_depth(4).into_iter().filter_map(|e| e.ok()).any(|e| {
            e.path().extension().map_or(false, |ext| ext == "py")
        });

    let flows = if is_rust {
        let src_dir = repo.join("src");
        let scan_dir = if src_dir.exists() { &src_dir } else { repo };
        RustExtractor::extract(scan_dir)?
    } else if is_csharp {
        let temp_dir = std::env::temp_dir().join(format!("flowmap_extract_{}", uuid_simple()));
        fs::create_dir_all(&temp_dir)?;
        CSharpExtractor::run(repo, &temp_dir, None, None)?;
        let f = load_flows_from_dir(&temp_dir)?;
        let _ = fs::remove_dir_all(&temp_dir);
        f
    } else if is_python {
        let temp_dir = std::env::temp_dir().join(format!("flowmap_extract_py_{}", uuid_simple()));
        fs::create_dir_all(&temp_dir)?;
        PythonExtractor::run(repo, &temp_dir)?;
        let f = load_flows_from_dir(&temp_dir)?;
        let _ = fs::remove_dir_all(&temp_dir);
        f
    } else {
        anyhow::bail!("Could not determine project type in '{}'. Expected Cargo.toml, .csproj/.sln, or Python files.", repo.display());
    };

    Ok(Linker::build(flows, HashMap::new()))
}

fn collect_flows_from_source(source: &Path) -> Result<Vec<Flow>> {
    if !source.exists() {
        anyhow::bail!("Target path '{}' does not exist.", source.display());
    }

    if source.is_file() {
        if let Ok(content) = fs::read_to_string(source) {
            if let Ok(map) = serde_json::from_str::<ConnectedFlowMap>(&content) {
                return Ok(map.flows);
            }
            if let Ok(flows) = serde_json::from_str::<Vec<Flow>>(&content) {
                return Ok(flows);
            }
            if let Ok(flow) = serde_json::from_str::<Flow>(&content) {
                return Ok(vec![flow]);
            }
        }
        anyhow::bail!("Could not parse JSON flows from file '{}'.", source.display());
    }

    // Directory
    let in_flows_json = source.join("flows").join("flowmap.json");
    if in_flows_json.exists() {
        let content = fs::read_to_string(&in_flows_json)?;
        if let Ok(map) = serde_json::from_str::<ConnectedFlowMap>(&content) {
            return Ok(map.flows);
        }
    }

    let direct_json = source.join("flowmap.json");
    if direct_json.exists() {
        let content = fs::read_to_string(&direct_json)?;
        if let Ok(map) = serde_json::from_str::<ConnectedFlowMap>(&content) {
            return Ok(map.flows);
        }
    }

    let raw_dir = source.join("flows").join("raw");
    if raw_dir.exists() {
        let flows = load_flows_from_dir(&raw_dir)?;
        if !flows.is_empty() {
            return Ok(flows);
        }
    }

    let flows_dir = source.join("flows");
    if flows_dir.exists() {
        let flows = load_flows_from_dir(&flows_dir)?;
        if !flows.is_empty() {
            return Ok(flows);
        }
    }

    let direct_flows = load_flows_from_dir(source)?;
    if !direct_flows.is_empty() {
        return Ok(direct_flows);
    }

    // Auto-scan if source is a source directory
    println!("No existing flows found in '{}'. Running automatic scan...", source.display());
    let connected = scan_repo_internal(source)?;
    Ok(connected.flows)
}

fn uuid_simple() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("{:x}", nanos)
}

fn load_flows_from_dir(dir: &Path) -> Result<Vec<Flow>> {
    let mut flows = Vec::new();
    let mut failures = Vec::new();
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
                match serde_json::from_str::<Flow>(&content) {
                    Ok(flow) => flows.push(flow),
                    // A flow-shaped file that won't parse is an extractor/schema mismatch: fail loudly
                    // instead of silently dropping it. Other JSON (package.json, ...) is just skipped.
                    Err(e) if is_flow_shaped(&content) => {
                        failures.push(format!("  {}: {}", entry.path().display(), e))
                    }
                    Err(_) => {}
                }
            }
        }
    }

    if !failures.is_empty() {
        anyhow::bail!("{} flow file(s) failed to parse:\n{}", failures.len(), failures.join("\n"));
    }
    Ok(flows)
}

/// One JSON file per flow; colliding names (same route/id) get a numeric suffix instead of overwriting.
fn write_flows(dir: &Path, flows: &[Flow]) -> Result<()> {
    let mut written = std::collections::HashSet::new();
    for f in flows {
        let clean_part = f.trigger.path.as_deref().unwrap_or(&f.id)
            .replace(['/', '\\', ':', '<', '>', '"', '|', '?', '*', ' '], "_")
            .replace(['{', '}'], "");
        let base = format!("{}_{}_{}", f.domain_id, f.trigger.method.as_deref().unwrap_or("FLOW"), clean_part);
        let mut name = base.clone();
        let mut i = 2;
        while !written.insert(name.to_lowercase()) {
            name = format!("{base}_{i}");
            i += 1;
        }
        fs::write(dir.join(format!("{name}.json")), serde_json::to_string_pretty(f)?)?;
    }
    Ok(())
}

fn is_flow_shaped(content: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(content)
        .map(|v| v.get("trigger").is_some() && v.get("handler").is_some())
        .unwrap_or(false)
}

fn load_annotations(path: &Path) -> Result<HashMap<String, FlowAnnotation>> {
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let content = fs::read_to_string(path)?;
    let map: HashMap<String, FlowAnnotation> = serde_json::from_str(&content)?;
    Ok(map)
}

fn is_ignored_scan_dir(entry: &walkdir::DirEntry) -> bool {
    let name = entry.file_name().to_string_lossy();
    name == ".git"
        || name == "target"
        || name == "bin"
        || name == "obj"
        || name == "node_modules"
        || name == "packages"
        || name == ".vs"
        || name == ".idea"
}

fn resolve_flowmap_path(path: &Path) -> PathBuf {
    if path.exists() {
        if path.is_dir() {
            let in_flows = path.join("flows").join("flowmap.json");
            if in_flows.exists() {
                return in_flows;
            }
            let direct = path.join("flowmap.json");
            if direct.exists() {
                return direct;
            }
        }
        return path.to_path_buf();
    }
    // Check if relative to "flows" subdirectory when default "flowmap.json" was provided
    if path == Path::new("flowmap.json") {
        let in_flows = Path::new("flows").join("flowmap.json");
        if in_flows.exists() {
            return in_flows;
        }
    }
    path.to_path_buf()
}

fn load_connected_map(path: &Path) -> Result<ConnectedFlowMap> {
    let resolved = resolve_flowmap_path(path);
    let content = fs::read_to_string(&resolved)
        .with_context(|| format!("Could not read flowmap file at '{}' (checked '{}')", path.display(), resolved.display()))?;
    let map = serde_json::from_str(&content)
        .with_context(|| format!("Invalid flowmap JSON structure in '{}'", resolved.display()))?;
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
