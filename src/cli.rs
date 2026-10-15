use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "flowmap")]
#[command(about = "Behavior and Flow Mapping engine for C# and Rust codebases", long_about = None)]
#[command(version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Build a connected flowmap from flow definitions and generate the interactive explorer
    Build {
        /// Directory containing flow JSON files
        #[arg(short, long, default_value = "flows")]
        flows_dir: PathBuf,

        /// Output path for the connected flowmap JSON
        #[arg(short, long, default_value = "flowmap.json")]
        out: PathBuf,

        /// Output path for the interactive HTML explorer
        #[arg(long, default_value = "flowmap.html")]
        html: PathBuf,

        /// Output path for the Markdown Flowbook (decision tables & Mermaid)
        #[arg(long, default_value = "FLOWS.md")]
        md: PathBuf,

        /// Optional path to annotations.json
        #[arg(short, long)]
        annotations: Option<PathBuf>,
    },

    /// Automatically scan and map a repository, saving results into <repo>/flows/
    Scan {
        /// Path to the target repository directory
        repo: PathBuf,

        /// Subdirectory inside target repo to store flowmap artifacts (default: flows)
        #[arg(short, long, default_value = "flows")]
        out: String,

        /// (C# only) Directory with built DLL references
        #[arg(long)]
        refs: Option<PathBuf>,
    },

    /// Launch or open the interactive FlowMap HTML explorer in your browser
    Explore {
        /// Path to flowmap.html
        #[arg(short, long, default_value = "flowmap.html")]
        html: PathBuf,

        /// Rebuild the HTML from flow definitions first
        #[arg(short, long)]
        rebuild: bool,
    },

    /// Perform impact analysis / blast radius calculation on changed files or symbols
    Impact {
        /// Path to flowmap.json
        #[arg(short, long, default_value = "flowmap.json")]
        map: PathBuf,

        /// Comma-separated list of changed files
        #[arg(short, long, value_delimiter = ',')]
        files: Vec<String>,

        /// Comma-separated list of changed symbols/methods
        #[arg(short, long, value_delimiter = ',')]
        symbols: Vec<String>,

        /// Git diff ref (e.g. HEAD~1, origin/main) to automatically extract changed files
        #[arg(short, long)]
        diff: Option<String>,
    },

    /// Display terminal flow visualization for an endpoint or action
    Show {
        /// Flow ID or route substring (e.g. 'start-deposit' or 'POST /animals')
        flow_id: String,

        /// Path to flowmap.json
        #[arg(short, long, default_value = "flowmap.json")]
        map: PathBuf,
    },

    /// Query flows by table, event, feature flag, or text
    Query {
        /// Path to flowmap.json
        #[arg(short, long, default_value = "flowmap.json")]
        map: PathBuf,

        /// Query which flows read or write to a database table
        #[arg(short, long)]
        table: Option<String>,

        /// Query which flows publish or consume an event
        #[arg(short, long)]
        event: Option<String>,

        /// Query which flows are gated or branched by a feature flag
        #[arg(short, long)]
        flag: Option<String>,

        /// Search flows by keyword across route, handlers, tables, and events
        #[arg(short, long)]
        search: Option<String>,
    },

    /// Run native code extractors against a target C# or Rust project
    Extract {
        /// Language of the target codebase: 'csharp' or 'rust'
        #[arg(short, long)]
        lang: String,

        /// Path to the service / project directory to scan
        #[arg(short, long)]
        project: PathBuf,

        /// Output directory where individual flow JSONs will be saved
        #[arg(short, long, default_value = "flows")]
        out: PathBuf,

        /// (C# only) Directory with built DLL references for full semantic resolution
        #[arg(short, long)]
        refs: Option<PathBuf>,

        /// (C# only) Filter to extract only a specific controller name
        #[arg(long)]
        only: Option<String>,
    },

    /// Compare two flowmaps (e.g. baseline vs current) and detect behavioral/breaking changes
    Diff {
        /// Baseline flowmap JSON path
        baseline: PathBuf,

        /// Current/modified flowmap JSON path
        current: PathBuf,

        /// Export diff report as JSON to this path
        #[arg(short, long)]
        json: Option<PathBuf>,
    },

    /// Audit flowmap for architectural hazards, unconsumed events, security gaps, and state anomalies
    Check {
        /// Path to flowmap.json
        #[arg(short, long, default_value = "flowmap.json")]
        map: PathBuf,

        /// Fail with non-zero exit code if CRITICAL or HIGH issues are detected
        #[arg(long)]
        strict: bool,
    },
}
