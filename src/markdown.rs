use crate::model::*;
use std::path::Path;

pub struct MarkdownGenerator;

impl MarkdownGenerator {
    pub fn generate(map: &ConnectedFlowMap, repo_name: &str) -> String {
        let mut md = String::new();

        md.push_str(&format!("# Architecture & Behavior FlowMap: {}\n\n", repo_name));
        md.push_str("> Generated automatically by [FlowMap](https://github.com/jorge/flowmap). ");
        md.push_str("Visualizes domain bounded contexts, business execution journeys, DTO schemas, and database transactions.\n\n");

        // Summary Badges / Stats Table
        md.push_str("## System Overview\n\n");
        md.push_str("| Metric | Count |\n");
        md.push_str("| :--- | :--- |\n");
        md.push_str(&format!("| **Bounded Contexts / Domains** | `{}` |\n", map.domains.len()));
        md.push_str(&format!("| **Business Journeys / Flows** | `{}` |\n", map.stats.total_flows));
        md.push_str(&format!("| **Decision Branches Tracked** | `{}` |\n", map.stats.total_decisions));
        md.push_str(&format!("| **Effects & Sinks** | `{}` |\n", map.stats.total_effects));
        md.push_str(&format!("| **Database Tables Tracked** | `{}` |\n", map.stats.total_tables_tracked));
        md.push_str(&format!("| **Domain Events Tracked** | `{}` |\n", map.stats.total_events_tracked));
        md.push_str(&format!("| **Feature Flags Tracked** | `{}` |\n\n", map.stats.total_flags_tracked));

        if !map.stats.unconsumed_events.is_empty() {
            md.push_str("> [!WARNING]\n");
            md.push_str(&format!(
                "> **{} Unconsumed Domain Events Detected!** These events are published but have no registered listener in the codebase:\n",
                map.stats.unconsumed_events.len()
            ));
            for ev in &map.stats.unconsumed_events {
                md.push_str(&format!("> - `{}`\n", ev));
            }
            md.push_str("\n");
        }

        // Domain Architecture Matrix
        md.push_str("## Bounded Contexts & Domain Architecture\n\n");
        for (_dom_id, dom) in &map.domains {
            md.push_str(&format!("### {} {}\n\n", dom.icon, dom.name));
            md.push_str(&format!("> {}\n\n", dom.description));

            let writes = if dom.tables_written.is_empty() { "None (read-only)".to_string() } else { dom.tables_written.iter().map(|t| format!("`{}`", t)).collect::<Vec<_>>().join(", ") };
            let deps = if dom.dependencies.is_empty() { "-".to_string() } else { dom.dependencies.iter().map(|d| format!("`{}`", d)).collect::<Vec<_>>().join(", ") };

            md.push_str(&format!("- **Mutates Core Tables:** {}\n", writes));
            md.push_str(&format!("- **Injected Services:** {}\n\n", deps));

            md.push_str("| Method | Business Journey / Action | Route / Endpoint | Mutates State |\n");
            md.push_str("| :--- | :--- | :--- | :--- |\n");
            for flow_id in &dom.flows {
                if let Some(f) = map.flows.iter().find(|x| &x.id == flow_id) {
                    let method = f.trigger.method.as_deref().unwrap_or("POST");
                    let anchor = flow_anchor(&f.id);
                    let writes_str = if f.coupling.writes.is_empty() { "-".to_string() } else { f.coupling.writes.join(", ") };
                    md.push_str(&format!(
                        "| `{}` | [**{}**](#{}) | `{}` | `{}` |\n",
                        method,
                        f.scenario,
                        anchor,
                        f.trigger.path.as_deref().unwrap_or(&f.id),
                        writes_str
                    ));
                }
            }
            md.push_str("\n");
        }

        // Section: Database State Coupling Matrix
        md.push_str("## Database State Coupling Matrix\n\n");
        md.push_str("> Tracks which endpoints read or mutate each database entity. If you alter schema or status transitions on a table, all linked flows enter your blast radius.\n\n");
        md.push_str("| Table / Entity | Owning Domains | Mutating Flows (Writers) | Dependent Flows (Readers) |\n");
        md.push_str("| :--- | :--- | :--- | :--- |\n");
        for node in &map.entity_topology {
            let owners = if node.owners.is_empty() { "-".to_string() } else { node.owners.join(", ") };
            let writers = if node.writers.is_empty() { "-".to_string() } else { node.writers.iter().map(|w| format!("`{}`", w)).collect::<Vec<_>>().join("<br>") };
            let readers = if node.readers.is_empty() { "-".to_string() } else { node.readers.iter().map(|r| format!("`{}`", r)).collect::<Vec<_>>().join("<br>") };
            md.push_str(&format!("| **`{}`** | {} | {} | {} |\n", node.table, owners, writers, readers));
        }
        md.push_str("\n");

        // Section: Domain Event & Pub/Sub Topology Matrix
        if !map.event_index.is_empty() {
            md.push_str("## Domain Event & Pub/Sub Topology Matrix\n\n");
            md.push_str("> Maps asynchronous domain events, outbox queues, and messaging topologies across bounded contexts. Shows both local consumers and external cross-repo contracts.\n\n");
            md.push_str("| Domain Event / Topic | Scope | Published By (Producers) | Consumed By (Handlers) |\n");
            md.push_str("| :--- | :--- | :--- | :--- |\n");
            for (evt, info) in &map.event_index {
                let scope = if info.is_consumed_locally {
                    "🟢 **Local Pub/Sub**"
                } else {
                    "🟠 *External Contract*"
                };
                let pubs = if info.publishers.is_empty() {
                    "-".to_string()
                } else {
                    info.publishers
                        .iter()
                        .map(|p| format!("[`{}`](#{})", p, flow_anchor(p)))
                        .collect::<Vec<_>>()
                        .join("<br>")
                };
                let cons = if info.consumers.is_empty() {
                    "*External Consumer (No local handler)*".to_string()
                } else {
                    info.consumers
                        .iter()
                        .map(|c| format!("[`{}`](#{})", c, flow_anchor(c)))
                        .collect::<Vec<_>>()
                        .join("<br>")
                };
                md.push_str(&format!("| **`{}`** | {} | {} | {} |\n", evt, scope, pubs, cons));
            }
            md.push_str("\n");
        }

        // Section: Detailed Flow Decision Maps
        md.push_str("## Detailed Execution Flowbooks\n\n");
        for flow in &map.flows {
            md.push_str(&Self::render_flow_markdown(flow));
        }

        md
    }

    fn render_flow_markdown(flow: &Flow) -> String {
        let mut s = String::new();
        let method = flow.trigger.method.as_deref().unwrap_or("ACTION");
        let path = flow.trigger.path.as_deref().unwrap_or(&flow.id);
        let anchor = flow_anchor(&flow.id);

        s.push_str(&format!("<a id=\"{}\"></a>\n\n", anchor));
        s.push_str(&format!("### {} (`{} {}`)\n\n", flow.scenario, method, path));

        let file_path = flow.handler.file.replace('\\', "/");
        let file_name = Path::new(&file_path).file_name().and_then(|n| n.to_str()).unwrap_or(&flow.handler.file);
        s.push_str(&format!(
            "- **Domain Context:** `{}`\n- **Handler:** [`{}::{}`]({}#L{}) (line {})\n",
            flow.domain, file_name, flow.handler.symbol, file_path, flow.handler.line, flow.handler.line
        ));

        // Guards
        let guards_str = flow.guards.iter()
            .map(|g| format!("`{}`", g.condition.as_deref().unwrap_or("guard")))
            .collect::<Vec<_>>()
            .join(", ");
        if !guards_str.is_empty() {
            s.push_str(&format!("- **Pre-Conditions / Guards:** {}\n", guards_str));
        }

        // Pub/Sub Topology linkage
        if !flow.coupling.events_published.is_empty() || !flow.coupling.events_consumed.is_empty() {
            s.push_str("- **Domain Events & Pub/Sub:**\n");
            for ev in &flow.coupling.events_published {
                if let Some(consumers) = flow.coupling.event_consumers.get(ev) {
                    if consumers.is_empty() {
                        s.push_str(&format!("  - 📤 Publishes `{}`, *External cross-service contract (No local consumer)*\n", ev));
                    } else {
                        let c_links = consumers.iter().map(|c| format!("[`{}`](#{})", c.scenario, flow_anchor(&c.flow_id))).collect::<Vec<_>>().join(", ");
                        s.push_str(&format!("  - 📤 Publishes `{}`, consumed locally by {}\n", ev, c_links));
                    }
                } else {
                    s.push_str(&format!("  - 📤 Publishes `{}`, *External cross-service contract*\n", ev));
                }
            }
            for ev in &flow.coupling.events_consumed {
                if let Some(producers) = flow.coupling.event_producers.get(ev) {
                    if producers.is_empty() {
                        s.push_str(&format!("  - 📥 Consumes `{}`, *External publisher*\n", ev));
                    } else {
                        let p_links = producers.iter().map(|p| format!("[`{}`](#{})", p.scenario, flow_anchor(&p.flow_id))).collect::<Vec<_>>().join(", ");
                        s.push_str(&format!("  - 📥 Consumes `{}`, produced by {}\n", ev, p_links));
                    }
                } else {
                    s.push_str(&format!("  - 📥 Consumes `{}`\n", ev));
                }
            }
        }

        if !flow.coupling.shared_state_with.is_empty() {
            s.push_str(&format!(
                "- **State Coupled With:** {} other flows through tables `{}`\n",
                flow.coupling.shared_state_with.len(),
                flow.coupling.writes.join(", ")
            ));
        }

        s.push_str("\n");

        // Contract: Request DTO & Responses
        if let Some(rb) = &flow.contract.request_body {
            s.push_str(&format!("#### Request Payload Schema: `{}`\n\n", rb.type_name));
            if !rb.fields.is_empty() {
                s.push_str("| Field | Type | Required | Notes |\n");
                s.push_str("| :--- | :--- | :--- | :--- |\n");
                for f in &rb.fields {
                    s.push_str(&format!(
                        "| `{}` | `{}` | {} | {} |\n",
                        f.name,
                        f.field_type,
                        if f.required { "**Yes**" } else { "No" },
                        f.description.as_deref().unwrap_or("-")
                    ));
                }
                s.push_str("\n");
            }
        }

        if !flow.contract.parameters.is_empty() {
            s.push_str("#### Route & Query Parameters\n\n");
            s.push_str("| Parameter | Source | Type | Description |\n");
            s.push_str("| :--- | :--- | :--- | :--- |\n");
            for p in &flow.contract.parameters {
                s.push_str(&format!(
                    "| `{}` | `{}` | `{}` | {} |\n",
                    p.name,
                    p.source,
                    p.param_type.as_deref().unwrap_or("string"),
                    p.description.as_deref().unwrap_or("-")
                ));
            }
            s.push_str("\n");
        }

        if let Some(resp) = &flow.contract.response {
            s.push_str(&format!("#### Response Outcomes (Model: `{}`)\n\n", resp.type_name.as_deref().unwrap_or("Result")));
            s.push_str("| Status Code | Description |\n");
            s.push_str("| :--- | :--- |\n");
            for sc in &resp.status_codes {
                s.push_str(&format!("| `{}` | {} |\n", sc.code, sc.description));
            }
            s.push_str("\n");
        }

        // State Mutations Table
        if !flow.tables_touched.is_empty() {
            s.push_str("#### Database State Mutations\n\n");
            s.push_str("| Table | Operation | Transactional |\n");
            s.push_str("| :--- | :--- | :--- |\n");
            for m in &flow.tables_touched {
                s.push_str(&format!(
                    "| **`{}`** | `{}` | {} |\n",
                    m.table,
                    m.operation,
                    if m.transactional { "Yes (`conn.transaction`)" } else { "No" }
                ));
            }
            s.push_str("\n");
        }

        // Decision Table
        s.push_str("#### Decision Table (Action X &rarr; Results Y, Z)\n\n");
        let outcomes = extract_outcomes_with_context(flow);

        s.push_str("| Path / Condition | Gate / Branch | Side Effects (DB, Events, HTTP) | Result Outcome |\n");
        s.push_str("| :--- | :--- | :--- | :--- |\n");

        if outcomes.is_empty() {
            s.push_str("| Normal execution | None | None | `200 OK` |\n");
        } else {
            for out in &outcomes {
                let effects_str = if out.effects.is_empty() {
                    "None".to_string()
                } else {
                    out.effects.join("<br>")
                };
                s.push_str(&format!(
                    "| {} | {} | {} | `{}` |\n",
                    out.condition, out.gate, effects_str, out.status
                ));
            }
        }
        s.push_str("\n");

        // Native Mermaid Flowchart
        s.push_str("#### Visual Execution Path\n\n");
        s.push_str("```mermaid\n");
        s.push_str(&generate_mermaid_chart(flow));
        s.push_str("```\n\n---\n\n");

        s
    }
}

fn flow_anchor(id: &str) -> String {
    id.to_lowercase()
        .replace([' ', '/', '{', '}', ':'], "-")
        .replace("--", "-")
        .trim_matches('-')
        .to_string()
}

struct OutcomeRow {
    condition: String,
    gate: String,
    effects: Vec<String>,
    status: String,
}

fn extract_outcomes_with_context(flow: &Flow) -> Vec<OutcomeRow> {
    let mut rows = Vec::new();

    // Guards
    for g in &flow.guards {
        let cond = g.condition.as_deref().unwrap_or("Precondition");
        if let Some(branches) = &g.branches {
            for b in branches {
                let mut status = "Blocked / Rejected".to_string();
                let mut effects = Vec::new();
                for step in &b.steps {
                    if let Some(st) = &step.status {
                        status = st.clone();
                    }
                    if let Some(t) = &step.target {
                        effects.push(format!("{}: {}", step.kind.as_deref().unwrap_or("effect"), t));
                    }
                }
                rows.push(OutcomeRow {
                    condition: format!("`{}`", cond),
                    gate: b.when.clone(),
                    effects,
                    status,
                });
            }
        }
    }

    // Steps
    let mut main_effects = Vec::new();
    for step in &flow.steps {
        if step.step_type == "effect" {
            let desc = format!(
                "**{}** `{}` ({})",
                step.kind.as_deref().unwrap_or("effect"),
                step.target.as_deref().unwrap_or("target"),
                step.op.as_deref().unwrap_or("exec")
            );
            main_effects.push(desc);
        }

        if let Some(branches) = &step.branches {
            for b in branches {
                let mut status = "Branch Path".to_string();
                let mut branch_effects = Vec::new();
                for bs in &b.steps {
                    if let Some(st) = &bs.status {
                        status = st.clone();
                    }
                    if let Some(t) = &bs.target {
                        branch_effects.push(format!("{}: {}", bs.kind.as_deref().unwrap_or("effect"), t));
                    }
                }
                rows.push(OutcomeRow {
                    condition: format!("`{}`", step.condition.as_deref().unwrap_or("Decision")),
                    gate: b.when.clone(),
                    effects: branch_effects,
                    status,
                });
            }
        }
    }

    // Success outcome
    let success_status = flow
        .steps
        .iter()
        .filter(|s| s.step_type == "return")
        .last()
        .and_then(|s| s.status.clone())
        .unwrap_or_else(|| {
            if flow.trigger.method.as_deref() == Some("POST") {
                "201 Created".to_string()
            } else {
                "200 OK".to_string()
            }
        });

    rows.push(OutcomeRow {
        condition: "All pre-conditions satisfied".to_string(),
        gate: "Success Path".to_string(),
        effects: main_effects,
        status: success_status,
    });

    rows
}

fn generate_mermaid_chart(flow: &Flow) -> String {
    let mut m = String::new();
    m.push_str("flowchart TD\n");

    let method = flow.trigger.method.as_deref().unwrap_or("ACTION");
    let path = flow.trigger.path.as_deref().unwrap_or(&flow.id);
    m.push_str(&format!("  Start([\"{} {}\"]) --> Auth[\"🛡️ Auth & Security\"]\n", method, path));
    m.push_str("  Auth --> PreCheck{\"Valid Payload?\"}\n");
    m.push_str("  PreCheck -- No --> Ret400([\"❌ 400 Bad Request\"])\n");
    m.push_str("  PreCheck -- Yes --> Logic[\"⚙️ Business Logic\"]\n");

    let mut last = "Logic";
    let mut step_idx = 0;

    for s in &flow.steps {
        step_idx += 1;
        let id = format!("N{}", step_idx);
        if s.step_type == "loop" {
            m.push_str(&format!("  {} --> {}{{\"🔄 Retry Loop\"}}\n", last, id));
            last = "Loop";
        } else if s.kind.as_deref() == Some("db_write") {
            let target = s.target.as_deref().unwrap_or("Database");
            m.push_str(&format!("  {} --> {}[(\"💾 Write {}\")]\n", last, id, target));
            last = "Write";
        }
    }

    m.push_str(&format!("  {} --> Success([\"🏁 200 / 201 Success\"])\n", last));
    m.push_str("  classDef default fill:#1e293b,stroke:#3b82f6,stroke-width:1px,color:#f8fafc;\n");
    m.push_str("  classDef start fill:#0f172a,stroke:#38bdf8,stroke-width:2px,color:#38bdf8;\n");
    m.push_str("  class Start,Success start;\n");

    m
}
