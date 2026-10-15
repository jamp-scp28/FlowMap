use crate::model::ConnectedFlowMap;
use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

const TEMPLATE: &str = include_str!("template.html");

pub struct HtmlGenerator;

impl HtmlGenerator {
    pub fn generate(map: &ConnectedFlowMap, output_path: &Path) -> Result<()> {
        let json_data = serde_json::to_string(map)
            .context("Failed to serialize ConnectedFlowMap to JSON")?;

        let html = TEMPLATE.replace("/*DATA_PLACEHOLDER*/{}", &json_data);

        if let Some(parent) = output_path.parent() {
            fs::create_dir_all(parent)?;
        }

        fs::write(output_path, html)
            .with_context(|| format!("Failed to write HTML to {}", output_path.display()))?;

        Ok(())
    }
}
