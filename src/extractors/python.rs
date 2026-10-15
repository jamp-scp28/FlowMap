use anyhow::{Context, Result};
use std::path::Path;
use std::process::Command;

pub struct PythonExtractor;

impl PythonExtractor {
    pub fn run(project_dir: &Path, out_dir: &Path) -> Result<()> {
        let current_exe = std::env::current_exe().ok();
        let exe_dir = current_exe.as_ref().and_then(|p| p.parent());

        let candidate_standalone = exe_dir.map(|d| d.join("extractors/python/extractor.py"));
        let manifest_script = Path::new(env!("CARGO_MANIFEST_DIR")).join("extractors/python/extractor.py");

        let script_path = if let Some(ref p) = candidate_standalone.filter(|p| p.exists()) {
            p.clone()
        } else if manifest_script.exists() {
            manifest_script
        } else {
            Path::new("extractors/python/extractor.py").to_path_buf()
        };

        // Try 'python', fallback to 'python3'
        let mut cmd = Command::new("python");
        cmd.arg(&script_path).arg(project_dir).arg(out_dir);

        let output = match cmd.output() {
            Ok(out) => out,
            Err(_) => {
                let mut cmd3 = Command::new("python3");
                cmd3.arg(&script_path).arg(project_dir).arg(out_dir);
                cmd3.output().context("Failed to execute python/python3 extractor. Ensure Python is installed and available in PATH.")?
            }
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            eprintln!("Python Extractor stdout: {}", stdout);
            eprintln!("Python Extractor stderr: {}", stderr);
            anyhow::bail!("Python Extractor failed with exit code: {:?}", output.status.code());
        }

        println!("{}", String::from_utf8_lossy(&output.stdout));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Flow;
    use std::fs;

    #[test]
    fn test_python_ast_extractor_g19() {
        let temp_dir = std::env::temp_dir().join(format!("flowmap_py_test_{}", std::time::SystemTime::now().elapsed().unwrap().as_nanos()));
        let out_dir = temp_dir.join("flows");
        fs::create_dir_all(&temp_dir).unwrap();

        let sample_service_py = r#"
from fastapi import FastAPI, Depends
app = FastAPI()

@app.post("/api/orders")
def create_order(request: dict):
    if config.AutoApproveEnabled:
        pass
    session.add("Orders")
    bus.publish("OrderSubmittedIntegrationEvent", {"order_id": 123})
    return {"status": "ok"}

@app.post("/api/orders/{id}/cancel")
def cancel_order(id: int):
    session.add("Orders")
    bus.publish("OrderCancelledIntegrationEvent")
    return {"status": "cancelled"}

@subscribe("OrderSubmittedIntegrationEvent")
def handle_order_submitted(event):
    session.add("Inventory")
    return True
"#;

        fs::write(temp_dir.join("main.py"), sample_service_py).unwrap();

        let res = PythonExtractor::run(&temp_dir, &out_dir);
        assert!(res.is_ok(), "Python extractor should succeed on valid Python AST");

        // Read extracted flows
        let mut flows = Vec::new();
        for entry in fs::read_dir(&out_dir).unwrap() {
            let entry = entry.unwrap();
            if entry.path().extension().map_or(false, |ext| ext == "json") {
                let content = fs::read_to_string(entry.path()).unwrap();
                let flow: Flow = serde_json::from_str(&content).unwrap();
                flows.push(flow);
            }
        }

        assert_eq!(flows.len(), 3, "Expected 3 flows (2 HTTP endpoints, 1 event subscriber)");

        // 1. Verify HTTP endpoint flow
        let create_flow = flows.iter().find(|f| f.id == "POST api/orders").expect("POST api/orders flow must exist");
        assert_eq!(create_flow.kind, "http");
        assert!(create_flow.coupling.writes.contains(&"Orders".to_string()));
        assert!(create_flow.coupling.events_published.contains(&"OrderSubmittedIntegrationEvent".to_string()));
        assert!(create_flow.configs_and_flags.iter().any(|cfg| cfg.name == "AutoApproveEnabled"));

        // 2. Verify event handler flow
        let handler_flow = flows.iter().find(|f| f.kind == "event_handler").expect("Event handler flow must exist");
        assert_eq!(handler_flow.trigger.topic.as_deref(), Some("OrderSubmittedIntegrationEvent"));
        assert!(handler_flow.coupling.events_consumed.contains(&"OrderSubmittedIntegrationEvent".to_string()));
        assert!(handler_flow.coupling.writes.contains(&"Inventory".to_string()));

        // Cleanup
        let _ = fs::remove_dir_all(&temp_dir);
    }
}
