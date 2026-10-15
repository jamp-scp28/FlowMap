use crate::model::*;
use colored::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowDiffReport {
    pub baseline_version: String,
    pub current_version: String,
    pub against_ref: String,
    pub flows_added: Vec<String>,
    pub flows_removed: Vec<String>,
    pub flows_modified: BTreeMap<String, FlowDelta>,
    pub breaking_changes: Vec<String>,
    pub blast_radius: Vec<DeltaBlastRadiusEntry>,
    pub hazards: Vec<DeltaHazard>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowDelta {
    pub guards_added: Vec<String>,
    pub guards_removed: Vec<String>,
    pub reads_added: Vec<String>,
    pub reads_removed: Vec<String>,
    pub writes_added: Vec<String>,
    pub writes_removed: Vec<String>,
    pub events_added: Vec<String>,
    pub events_removed: Vec<String>,
    pub calls_added: Vec<String>,
    pub calls_removed: Vec<String>,
    pub flags_added: Vec<String>,
    pub flags_removed: Vec<String>,
    pub outcomes_added: Vec<String>,
    pub outcomes_removed: Vec<String>,
    pub is_breaking: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeltaBlastRadiusEntry {
    pub flow_id: String,
    pub coupling_type: String, // "Shared DB Table" or "Pub/Sub Event"
    pub entity: String,        // Table name or Event name
    pub coupled_flows: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeltaHazard {
    pub severity: String, // "CRITICAL", "HIGH", "WARNING"
    pub flow_id: String,
    pub category: String, // "SECURITY", "DEAD_EVENT", "BREAKING_API"
    pub description: String,
    pub recommendation: String,
}

pub struct FlowDiffer;

impl FlowDiffer {
    pub fn diff(baseline: &ConnectedFlowMap, current: &ConnectedFlowMap) -> FlowDiffReport {
        Self::diff_with_context(baseline, current, "baseline")
    }

    pub fn diff_with_context(
        baseline: &ConnectedFlowMap,
        current: &ConnectedFlowMap,
        against_ref: &str,
    ) -> FlowDiffReport {
        let mut base_map = BTreeMap::new();
        for f in &baseline.flows {
            base_map.insert(&f.id, f);
        }

        let mut curr_map = BTreeMap::new();
        for f in &current.flows {
            curr_map.insert(&f.id, f);
        }

        let mut flows_added = Vec::new();
        let mut flows_removed = Vec::new();
        let mut flows_modified = BTreeMap::new();
        let mut breaking_changes = Vec::new();
        let mut hazards = Vec::new();

        // 1. Detect removed flows (breaking if public)
        for (id, _) in &base_map {
            if !curr_map.contains_key(id) {
                flows_removed.push((*id).clone());
                breaking_changes.push(format!("Endpoint removed: {}", id));
                hazards.push(DeltaHazard {
                    severity: "HIGH".to_string(),
                    flow_id: (*id).clone(),
                    category: "BREAKING_API".to_string(),
                    description: format!("Endpoint or flow '{}' was completely removed.", id),
                    recommendation: "Ensure clients have migrated before deprecating endpoints.".to_string(),
                });
            }
        }

        // 2. Detect added flows
        for (id, _) in &curr_map {
            if !base_map.contains_key(id) {
                flows_added.push((*id).clone());
            }
        }

        // 3. Detect modified flows
        for (id, base_flow) in &base_map {
            if let Some(curr_flow) = curr_map.get(id) {
                let delta = Self::diff_flow(base_flow, curr_flow);
                if delta.has_changes() {
                    if delta.is_breaking {
                        breaking_changes.push(format!("Breaking behavioral change in {}: outcomes removed or guards relaxed", id));
                    }

                    // Check for security guard removal
                    for g in &delta.guards_removed {
                        let g_low = g.to_lowercase();
                        if g_low.contains("auth") || g_low.contains("role") || g_low.contains("permission") {
                            hazards.push(DeltaHazard {
                                severity: "CRITICAL".to_string(),
                                flow_id: (*id).clone(),
                                category: "SECURITY".to_string(),
                                description: format!("Security guard was removed: '{}'", g),
                                recommendation: "Verify whether removing this guard exposes protected resources.".to_string(),
                            });
                        }
                    }

                    // Check for removed outcomes
                    for o in &delta.outcomes_removed {
                        if o.starts_with("200") || o.starts_with("201") || o.starts_with("204") {
                            hazards.push(DeltaHazard {
                                severity: "HIGH".to_string(),
                                flow_id: (*id).clone(),
                                category: "BREAKING_API".to_string(),
                                description: format!("Expected success status '{}' was removed.", o),
                                recommendation: "Ensure calling clients are prepared for alternative response codes.".to_string(),
                            });
                        }
                    }

                    flows_modified.insert((*id).clone(), delta);
                }
            }
        }

        // 4. Check added/modified flows for Unguarded Mutations and Unconsumed Events
        let changed_ids: HashSet<String> = flows_added.iter().cloned().chain(flows_modified.keys().cloned()).collect();

        for flow_id in &changed_ids {
            if let Some(flow) = curr_map.get(flow_id) {
                // Unguarded DB writes
                let has_auth = flow.guards.iter().any(|g| {
                    g.kind.as_deref() == Some("authn") || g.kind.as_deref() == Some("authz")
                });
                if !flow.coupling.writes.is_empty() && !has_auth {
                    hazards.push(DeltaHazard {
                        severity: "CRITICAL".to_string(),
                        flow_id: flow.id.clone(),
                        category: "SECURITY".to_string(),
                        description: format!("Mutating endpoint writes to DB [{}] without explicit authentication/authorization guards.", flow.coupling.writes.join(", ")),
                        recommendation: "Add [Authorize] or actor authorization guard before performing state mutations.".to_string(),
                    });
                }

                // Dead / unconsumed published events
                for evt in &flow.coupling.events_published {
                    let consumer_count = current.event_index.get(evt).map(|e| e.consumers.len()).unwrap_or(0);
                    if consumer_count == 0 {
                        hazards.push(DeltaHazard {
                            severity: "HIGH".to_string(),
                            flow_id: flow.id.clone(),
                            category: "DEAD_EVENT".to_string(),
                            description: format!("Publishes domain event '{}' but NO consumer is registered in the entire system.", evt),
                            recommendation: format!("Implement an event handler for '{}' or remove the unused domain event.", evt),
                        });
                    }
                }
            }
        }

        // 5. Compute Secondary Blast Radius
        let mut blast_radius = Vec::new();
        for flow_id in &changed_ids {
            if let Some(flow) = curr_map.get(flow_id) {
                // Table blast radius
                for tbl in &flow.coupling.writes {
                    if let Some(entry) = current.table_index.get(tbl) {
                        let mut coupled = Vec::new();
                        for other in entry.readers.iter().chain(entry.writers.iter()) {
                            if !changed_ids.contains(other) && !coupled.contains(other) {
                                coupled.push(other.clone());
                            }
                        }
                        if !coupled.is_empty() {
                            blast_radius.push(DeltaBlastRadiusEntry {
                                flow_id: flow.id.clone(),
                                coupling_type: "Shared DB Table".to_string(),
                                entity: tbl.clone(),
                                coupled_flows: coupled,
                            });
                        }
                    }
                }

                // Event blast radius
                for evt in &flow.coupling.events_published {
                    if let Some(entry) = current.event_index.get(evt) {
                        let mut consumers = Vec::new();
                        for c in &entry.consumers {
                            if !changed_ids.contains(c) && !consumers.contains(c) {
                                consumers.push(c.clone());
                            }
                        }
                        if !consumers.is_empty() {
                            blast_radius.push(DeltaBlastRadiusEntry {
                                flow_id: flow.id.clone(),
                                coupling_type: "Pub/Sub Event".to_string(),
                                entity: evt.clone(),
                                coupled_flows: consumers,
                            });
                        }
                    }
                }
            }
        }

        FlowDiffReport {
            baseline_version: baseline.generated_at.clone(),
            current_version: current.generated_at.clone(),
            against_ref: against_ref.to_string(),
            flows_added,
            flows_removed,
            flows_modified,
            breaking_changes,
            blast_radius,
            hazards,
        }
    }

    fn diff_flow(base: &Flow, curr: &Flow) -> FlowDelta {
        let base_guards: HashSet<_> = base.guards.iter().filter_map(|g| g.condition.clone()).collect();
        let curr_guards: HashSet<_> = curr.guards.iter().filter_map(|g| g.condition.clone()).collect();
        let guards_added: Vec<_> = curr_guards.difference(&base_guards).cloned().collect();
        let guards_removed: Vec<_> = base_guards.difference(&curr_guards).cloned().collect();

        // Reads diff
        let base_reads: HashSet<_> = base.coupling.reads.iter().cloned().collect();
        let curr_reads: HashSet<_> = curr.coupling.reads.iter().cloned().collect();
        let reads_added: Vec<_> = curr_reads.difference(&base_reads).cloned().collect();
        let reads_removed: Vec<_> = base_reads.difference(&curr_reads).cloned().collect();

        // Writes diff
        let base_writes: HashSet<_> = base.coupling.writes.iter().cloned().collect();
        let curr_writes: HashSet<_> = curr.coupling.writes.iter().cloned().collect();
        let writes_added: Vec<_> = curr_writes.difference(&base_writes).cloned().collect();
        let writes_removed: Vec<_> = base_writes.difference(&curr_writes).cloned().collect();

        // Events published diff
        let base_events: HashSet<_> = base.coupling.events_published.iter().cloned().collect();
        let curr_events: HashSet<_> = curr.coupling.events_published.iter().cloned().collect();
        let events_added: Vec<_> = curr_events.difference(&base_events).cloned().collect();
        let events_removed: Vec<_> = base_events.difference(&curr_events).cloned().collect();

        // Calls diff
        let base_calls: HashSet<_> = Self::extract_calls(base);
        let curr_calls: HashSet<_> = Self::extract_calls(curr);
        let calls_added: Vec<_> = curr_calls.difference(&base_calls).cloned().collect();
        let calls_removed: Vec<_> = base_calls.difference(&curr_calls).cloned().collect();

        // Flags diff
        let base_flags: HashSet<_> = Self::extract_flags(base);
        let curr_flags: HashSet<_> = Self::extract_flags(curr);
        let flags_added: Vec<_> = curr_flags.difference(&base_flags).cloned().collect();
        let flags_removed: Vec<_> = base_flags.difference(&curr_flags).cloned().collect();

        // Outcomes diff
        let base_outcomes: HashSet<_> = Self::extract_outcomes(base);
        let curr_outcomes: HashSet<_> = Self::extract_outcomes(curr);
        let outcomes_added: Vec<_> = curr_outcomes.difference(&base_outcomes).cloned().collect();
        let outcomes_removed: Vec<_> = base_outcomes.difference(&curr_outcomes).cloned().collect();

        let is_breaking = !guards_removed.is_empty() || !outcomes_removed.is_empty();

        FlowDelta {
            guards_added,
            guards_removed,
            reads_added,
            reads_removed,
            writes_added,
            writes_removed,
            events_added,
            events_removed,
            calls_added,
            calls_removed,
            flags_added,
            flags_removed,
            outcomes_added,
            outcomes_removed,
            is_breaking,
        }
    }

    fn extract_calls(flow: &Flow) -> HashSet<String> {
        let mut set = HashSet::new();
        fn collect(steps: &[Step], set: &mut HashSet<String>) {
            for s in steps {
                if s.step_type == "call" {
                    if let Some(sym) = &s.symbol {
                        set.insert(sym.clone());
                    }
                }
                if let Some(branches) = &s.branches {
                    for b in branches {
                        collect(&b.steps, set);
                    }
                }
                if let Some(child_steps) = &s.steps {
                    collect(child_steps, set);
                }
            }
        }
        collect(&flow.steps, &mut set);
        set
    }

    fn extract_flags(flow: &Flow) -> HashSet<String> {
        let mut set = HashSet::new();
        for c in &flow.configs_and_flags {
            set.insert(c.name.clone());
        }
        fn collect(steps: &[Step], set: &mut HashSet<String>) {
            for s in steps {
                if s.step_type == "decision" && (s.kind.as_deref() == Some("feature_flag") || s.kind.as_deref() == Some("config")) {
                    if let Some(cond) = &s.condition {
                        set.insert(cond.clone());
                    }
                }
                if let Some(branches) = &s.branches {
                    for b in branches {
                        collect(&b.steps, set);
                    }
                }
                if let Some(child_steps) = &s.steps {
                    collect(child_steps, set);
                }
            }
        }
        collect(&flow.steps, &mut set);
        set
    }

    fn extract_outcomes(flow: &Flow) -> HashSet<String> {
        let mut set = HashSet::new();
        fn collect(steps: &[Step], set: &mut HashSet<String>) {
            for s in steps {
                if s.step_type == "return" {
                    if let Some(st) = &s.status {
                        set.insert(st.clone());
                    }
                }
                if let Some(branches) = &s.branches {
                    for b in branches {
                        collect(&b.steps, set);
                    }
                }
                if let Some(child_steps) = &s.steps {
                    collect(child_steps, set);
                }
            }
        }
        collect(&flow.steps, &mut set);
        set
    }

    pub fn print_report(report: &FlowDiffReport) {
        Self::print_delta_report(report);
    }

    pub fn print_delta_report(report: &FlowDiffReport) {
        println!("{}", "============================================================".bright_blue());
        println!("{}", "            FLOWMAP BEHAVIORAL DELTA & BLAST RADIUS         ".bold().bright_white());
        println!("{}", "============================================================".bright_blue());
        println!("Compared Against Ref: {}", report.against_ref.bold().bright_cyan());
        println!("Baseline Generated:   {}", report.baseline_version.dimmed());
        println!("Current Generated:    {}", report.current_version.dimmed());
        println!();

        if !report.hazards.is_empty() {
            println!("{}", "[ ARCHITECTURAL HAZARDS & BREAKING CHANGES ]".bold().bright_red());
            for h in &report.hazards {
                let badge = match h.severity.as_str() {
                    "CRITICAL" => "CRITICAL".bold().on_red().white(),
                    "HIGH" => "HIGH".bold().red(),
                    _ => "WARN".bold().yellow(),
                };
                println!("  [{}] {} - {}", badge, h.flow_id.bold().white(), h.description.bright_yellow());
                println!("         {} {}", "↳ Fix:".dimmed(), h.recommendation.bright_cyan());
            }
            println!();
        }

        if !report.flows_added.is_empty() {
            println!("{}", format!("[ ADDED FLOWS ] (+{})", report.flows_added.len()).bold().bright_green());
            for f in &report.flows_added {
                println!("  + {}", f.green());
            }
            println!();
        }

        if !report.flows_removed.is_empty() {
            println!("{}", format!("[ REMOVED FLOWS ] (-{})", report.flows_removed.len()).bold().bright_red());
            for f in &report.flows_removed {
                println!("  - {}", f.red());
            }
            println!();
        }

        if !report.flows_modified.is_empty() {
            println!("{}", format!("[ MODIFIED BEHAVIOR ] ({} flows)", report.flows_modified.len()).bold().bright_yellow());
            for (id, delta) in &report.flows_modified {
                let badge = if delta.is_breaking { "[BREAKING]".bold().red() } else { "[MODIFIED]".bold().yellow() };
                println!("  * {} {}", id.bold().white(), badge);
                for g in &delta.guards_added {
                    println!("      + guard added:   {}", g.bright_cyan());
                }
                for g in &delta.guards_removed {
                    println!("      - guard removed: {}", g.bright_red());
                }
                for r in &delta.reads_added {
                    println!("      + table read:    {}", r.bright_green());
                }
                for r in &delta.reads_removed {
                    println!("      - table read:    {}", r.bright_red());
                }
                for w in &delta.writes_added {
                    println!("      + table write:   {}", w.bright_green());
                }
                for w in &delta.writes_removed {
                    println!("      - table write:   {}", w.bright_red());
                }
                for e in &delta.events_added {
                    println!("      + event pub:     {}", e.bright_green());
                }
                for e in &delta.events_removed {
                    println!("      - event pub:     {}", e.bright_red());
                }
                for c in &delta.calls_added {
                    println!("      + call added:    {}", c.bright_cyan());
                }
                for c in &delta.calls_removed {
                    println!("      - call removed:  {}", c.bright_red());
                }
                for f in &delta.flags_added {
                    println!("      + flag check:    {}", f.bright_magenta());
                }
                for f in &delta.flags_removed {
                    println!("      - flag check:    {}", f.bright_red());
                }
                for o in &delta.outcomes_added {
                    println!("      + outcome added: {}", o.bright_green());
                }
                for o in &delta.outcomes_removed {
                    println!("      - outcome removed:{}", o.bright_red());
                }
            }
            println!();
        }

        if !report.blast_radius.is_empty() {
            println!("{}", format!("[ SECONDARY BLAST RADIUS ] ({} coupled entities)", report.blast_radius.len()).bold().bright_magenta());
            for b in &report.blast_radius {
                println!("  Entity: {} ({}) touched by {}", b.entity.bold().bright_white(), b.coupling_type.cyan(), b.flow_id.dimmed());
                println!("    ↳ Couples with {} existing flow(s):", b.coupled_flows.len());
                for c in b.coupled_flows.iter().take(5) {
                    println!("        • {}", c.dimmed());
                }
                if b.coupled_flows.len() > 5 {
                    println!("        ... and {} more", b.coupled_flows.len() - 5);
                }
            }
            println!();
        }

        if report.flows_added.is_empty() && report.flows_removed.is_empty() && report.flows_modified.is_empty() {
            println!("{}", "No behavioral differences detected between branches.".green());
        }
    }

    pub fn generate_pr_markdown(report: &FlowDiffReport) -> String {
        let mut md = String::new();
        md.push_str("# 🌊 FlowMap Behavioral Delta & Blast Radius Report\n\n");
        md.push_str(&format!("**Target Base Ref:** `{}`\n\n", report.against_ref));

        md.push_str("### 📊 Executive Summary\n\n");
        md.push_str("| Metric | Count |\n| :--- | :--- |\n");
        md.push_str(&format!("| **Flows Added** | `+{}` |\n", report.flows_added.len()));
        md.push_str(&format!("| **Flows Modified** | `*{}` |\n", report.flows_modified.len()));
        md.push_str(&format!("| **Flows Removed** | `-{}` |\n", report.flows_removed.len()));
        md.push_str(&format!("| **Breaking Changes** | `{}` |\n", report.breaking_changes.len()));
        md.push_str(&format!("| **Architectural / Security Hazards** | `{}` |\n\n", report.hazards.len()));

        if !report.hazards.is_empty() {
            md.push_str("### 🚨 Architectural & Security Hazards\n\n");
            md.push_str("| Severity | Flow | Issue | Recommendation |\n| :--- | :--- | :--- | :--- |\n");
            for h in &report.hazards {
                let badge = match h.severity.as_str() {
                    "CRITICAL" => "🔴 **CRITICAL**",
                    "HIGH" => "🟠 **HIGH**",
                    _ => "🟡 **WARN**",
                };
                md.push_str(&format!("| {} | `{}` | {} | {} |\n", badge, h.flow_id, h.description, h.recommendation));
            }
            md.push('\n');
        }

        if !report.flows_added.is_empty() {
            md.push_str("### ➕ Newly Introduced Flows\n\n");
            for f in &report.flows_added {
                md.push_str(&format!("- `+` `{}`\n", f));
            }
            md.push('\n');
        }

        if !report.flows_modified.is_empty() {
            md.push_str("### ✏️ Modified Flows & Behavioral Diffs\n\n");
            for (id, delta) in &report.flows_modified {
                let badge = if delta.is_breaking { "⚠️ **BREAKING**" } else { "📝 **MODIFIED**" };
                md.push_str(&format!("#### `{}` ({})\n\n", id, badge));
                if !delta.guards_added.is_empty() || !delta.guards_removed.is_empty() {
                    md.push_str("**Guards:**\n");
                    for g in &delta.guards_added {
                        md.push_str(&format!("- `+ guard:` `{}`\n", g));
                    }
                    for g in &delta.guards_removed {
                        md.push_str(&format!("- `- guard:` `{}`\n", g));
                    }
                }
                if !delta.reads_added.is_empty() || !delta.reads_removed.is_empty() {
                    md.push_str("\n**Database Reads:**\n");
                    for r in &delta.reads_added {
                        md.push_str(&format!("- `+ table read:` `{}`\n", r));
                    }
                    for r in &delta.reads_removed {
                        md.push_str(&format!("- `- table read:` `{}`\n", r));
                    }
                }
                if !delta.writes_added.is_empty() || !delta.writes_removed.is_empty() {
                    md.push_str("\n**Database Writes:**\n");
                    for w in &delta.writes_added {
                        md.push_str(&format!("- `+ table write:` `{}`\n", w));
                    }
                    for w in &delta.writes_removed {
                        md.push_str(&format!("- `- table write:` `{}`\n", w));
                    }
                }
                if !delta.events_added.is_empty() || !delta.events_removed.is_empty() {
                    md.push_str("\n**Events Published:**\n");
                    for e in &delta.events_added {
                        md.push_str(&format!("- `+ event pub:` `{}`\n", e));
                    }
                    for e in &delta.events_removed {
                        md.push_str(&format!("- `- event pub:` `{}`\n", e));
                    }
                }
                if !delta.calls_added.is_empty() || !delta.calls_removed.is_empty() {
                    md.push_str("\n**Method Calls:**\n");
                    for c in &delta.calls_added {
                        md.push_str(&format!("- `+ call:` `{}`\n", c));
                    }
                    for c in &delta.calls_removed {
                        md.push_str(&format!("- `- call:` `{}`\n", c));
                    }
                }
                if !delta.flags_added.is_empty() || !delta.flags_removed.is_empty() {
                    md.push_str("\n**Feature Flags & Configs:**\n");
                    for f in &delta.flags_added {
                        md.push_str(&format!("- `+ flag check:` `{}`\n", f));
                    }
                    for f in &delta.flags_removed {
                        md.push_str(&format!("- `- flag check:` `{}`\n", f));
                    }
                }
                if !delta.outcomes_added.is_empty() || !delta.outcomes_removed.is_empty() {
                    md.push_str("\n**Outcomes (Return Statuses):**\n");
                    for o in &delta.outcomes_added {
                        md.push_str(&format!("- `+ return:` `{}`\n", o));
                    }
                    for o in &delta.outcomes_removed {
                        md.push_str(&format!("- `- return:` `{}`\n", o));
                    }
                }
                md.push('\n');
            }
        }

        if !report.blast_radius.is_empty() {
            md.push_str("### 💥 Secondary Blast Radius & Shared State Coupling\n\n");
            md.push_str("| Changed Flow | Coupled Entity | Coupling Type | Other Flows Impacted |\n| :--- | :--- | :--- | :--- |\n");
            for b in &report.blast_radius {
                let other_flows_preview = if b.coupled_flows.len() > 3 {
                    format!("`{}` and {} more", b.coupled_flows[..3].join("`, `"), b.coupled_flows.len() - 3)
                } else {
                    format!("`{}`", b.coupled_flows.join("`, `"))
                };
                md.push_str(&format!("| `{}` | `{}` | {} | {} |\n", b.flow_id, b.entity, b.coupling_type, other_flows_preview));
            }
            md.push('\n');
        }

        // Mermaid diagram of modified/added flows and their couplings
        if !report.flows_added.is_empty() || !report.flows_modified.is_empty() {
            md.push_str("### 🗺️ Visual Behavioral Topology\n\n```mermaid\nflowchart LR\n");
            let mut node_idx = 0;
            for (id, _) in report.flows_modified.iter().take(6) {
                node_idx += 1;
                md.push_str(&format!("    Flow_{}[\"{}\"]:::modified\n", node_idx, id));
            }
            for id in report.flows_added.iter().take(4) {
                node_idx += 1;
                md.push_str(&format!("    Flow_{}[\"{}\"]:::added\n", node_idx, id));
            }
            for b in report.blast_radius.iter().take(5) {
                node_idx += 1;
                md.push_str(&format!("    Entity_{}[(\"{}\")]\n", node_idx, b.entity));
                md.push_str(&format!("    Flow_1 -.->|\"{}\"| Entity_{}\n", b.coupling_type, node_idx));
            }
            md.push_str("    classDef modified fill:#fef3c7,stroke:#d97706,stroke-width:2px;\n");
            md.push_str("    classDef added fill:#dcfce7,stroke:#16a34a,stroke-width:2px;\n");
            md.push_str("```\n\n");
        }

        md.push_str("---\n*Generated autonomously by [FlowMap](https://github.com/jamp-scp28/FlowMap)*\n");
        md
    }
}

impl FlowDelta {
    pub fn has_changes(&self) -> bool {
        !self.guards_added.is_empty()
            || !self.guards_removed.is_empty()
            || !self.reads_added.is_empty()
            || !self.reads_removed.is_empty()
            || !self.writes_added.is_empty()
            || !self.writes_removed.is_empty()
            || !self.events_added.is_empty()
            || !self.events_removed.is_empty()
            || !self.calls_added.is_empty()
            || !self.calls_removed.is_empty()
            || !self.flags_added.is_empty()
            || !self.flags_removed.is_empty()
            || !self.outcomes_added.is_empty()
            || !self.outcomes_removed.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_behavioral_diff_modified_flows_g11() {
        let base_flow_json = r#"{
            "id": "POST api/Orders",
            "kind": "http",
            "trigger": { "method": "POST", "path": "api/Orders" },
            "handler": { "symbol": "OrdersController.Create", "file": "src/OrdersController.cs", "line": 20 },
            "coupling": {
                "reads": ["Orders"],
                "writes": [],
                "events_published": [],
                "events_consumed": [],
                "calls_services": []
            },
            "steps": []
        }"#;

        let curr_flow_json = r#"{
            "id": "POST api/Orders",
            "kind": "http",
            "trigger": { "method": "POST", "path": "api/Orders" },
            "handler": { "symbol": "OrdersController.Create", "file": "src/OrdersController.cs", "line": 20 },
            "coupling": {
                "reads": ["Orders"],
                "writes": ["Orders"],
                "events_published": ["OrderSubmittedDomainEvent"],
                "events_consumed": [],
                "calls_services": []
            },
            "steps": [
                {
                    "type": "call",
                    "symbol": "Order.Submit",
                    "file": "src/Domain/Order.cs"
                }
            ]
        }"#;

        let base_flow: Flow = serde_json::from_str(base_flow_json).unwrap();
        let curr_flow: Flow = serde_json::from_str(curr_flow_json).unwrap();

        let base_map = ConnectedFlowMap {
            version: "1.0".to_string(),
            generated_at: "v1".to_string(),
            stats: FlowMapStats::default(),
            domains: Default::default(),
            flows: vec![base_flow],
            entity_topology: Default::default(),
            table_index: Default::default(),
            event_index: Default::default(),
            flag_index: Default::default(),
            service_index: Default::default(),
            annotations: Default::default(),
        };

        let curr_map = ConnectedFlowMap {
            version: "1.0".to_string(),
            generated_at: "v2".to_string(),
            stats: FlowMapStats::default(),
            domains: Default::default(),
            flows: vec![curr_flow],
            entity_topology: Default::default(),
            table_index: Default::default(),
            event_index: Default::default(),
            flag_index: Default::default(),
            service_index: Default::default(),
            annotations: Default::default(),
        };

        let diff_report = FlowDiffer::diff(&base_map, &curr_map);

        // G11: Must report the modified flow, not 0 changes!
        assert_eq!(diff_report.flows_added.len(), 0);
        assert_eq!(diff_report.flows_removed.len(), 0);
        assert_eq!(diff_report.flows_modified.len(), 1);

        let delta = diff_report.flows_modified.get("POST api/Orders").unwrap();
        assert!(delta.has_changes());
        assert_eq!(delta.writes_added, vec!["Orders"]);
        assert_eq!(delta.events_added, vec!["OrderSubmittedDomainEvent"]);
        assert_eq!(delta.calls_added, vec!["Order.Submit"]);
    }
}
