use anyhow::{Context, Result};
use std::path::Path;
use std::process::Command;

pub struct CSharpExtractor;

impl CSharpExtractor {
    pub fn run(
        project_dir: &Path,
        out_dir: &Path,
        refs_dir: Option<&Path>,
        only_controller: Option<&str>,
    ) -> Result<()> {
        let current_exe = std::env::current_exe().ok();
        let exe_dir = current_exe.as_ref().and_then(|p| p.parent());

        let candidate_standalone = exe_dir.map(|d| d.join("CSharpFlowExtractor.exe"));
        let candidate_nested = exe_dir.map(|d| d.join("extractors/csharp/CSharpFlowExtractor.exe"));
        let manifest_csproj = Path::new(env!("CARGO_MANIFEST_DIR")).join("extractors/csharp/CSharpFlowExtractor.csproj");

        let mut cmd = if let Some(ref p) = candidate_standalone.filter(|p| p.exists()) {
            let mut c = Command::new(p);
            c.arg(project_dir).arg(out_dir);
            c
        } else if let Some(ref p) = candidate_nested.filter(|p| p.exists()) {
            let mut c = Command::new(p);
            c.arg(project_dir).arg(out_dir);
            c
        } else {
            // Development fallback: dotnet run
            let mut c = Command::new("dotnet");
            c.arg("run")
                .arg("--no-build")
                .arg("--project")
                .arg(&manifest_csproj)
                .arg("--")
                .arg(project_dir)
                .arg(out_dir);
            c
        };

        if let Some(refs) = refs_dir {
            cmd.arg("--refs").arg(refs);
        }

        if let Some(only) = only_controller {
            cmd.arg("--only").arg(only);
        }

        let output = cmd.output().context("Failed to execute dotnet extractor")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            eprintln!("C# Extractor stdout: {}", stdout);
            eprintln!("C# Extractor stderr: {}", stderr);
            anyhow::bail!("C# Extractor failed with exit code: {:?}", output.status.code());
        }

        println!("{}", String::from_utf8_lossy(&output.stdout));
        Ok(())
    }
}
