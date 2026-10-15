use crate::model::*;
use colored::*;
use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImpactLevel {
    Direct,   // Flow's handler or called method was directly edited
    State,    // Flow shares database state (reads/writes same table) with an edited flow
    Event,    // Flow consumes an event emitted by an edited flow (or vice versa)
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct ImpactedFlow {
    pub flow_id: String,
    pub level: ImpactLevel,
    pub reason: String,
    pub coupled_tables: Vec<String>,
    pub coupled_events: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ImpactReport {
    pub changed_inputs: Vec<String>,
    pub direct_impacts: Vec<ImpactedFlow>,
    pub state_impacts: Vec<ImpactedFlow>,
    pub event_impacts: Vec<ImpactedFlow>,
    pub total_impacted: usize,
}

pub struct ImpactAnalyzer;

impl ImpactAnalyzer {
    pub fn analyze(
        map: &ConnectedFlowMap,
        changed_files_or_symbols: &[String],
    ) -> ImpactReport {
        let mut direct = Vec::new();
        let mut directly_affected_ids = HashSet::new();
        let mut directly_affected_tables = HashSet::new();
        let mut directly_affected_events = HashSet::new();

        // 1. Identify Direct impacts
        for flow in &map.flows {
            let mut matched_reasons = Vec::new();

            for target in changed_files_or_symbols {
                let target_lower = target.to_lowercase().replace('\\', "/");
                let handler_file_lower = flow.handler.file.to_lowercase().replace('\\', "/");

                // Check file match
                if handler_file_lower.ends_with(&target_lower) || target_lower.ends_with(&handler_file_lower) {
                    matched_reasons.push(format!("Handler file changed: {}", flow.handler.file));
                }

                // Check symbol match
                if flow.handler.symbol.to_lowercase().contains(&target_lower) {
                    matched_reasons.push(format!("Handler symbol matched: {}", flow.handler.symbol));
                }

                // Check calls inside steps
                Self::find_matching_calls(&flow.steps, target, &mut matched_reasons);
            }

            if !matched_reasons.is_empty() {
                directly_affected_ids.insert(flow.id.clone());
                for w in &flow.coupling.writes {
                    directly_affected_tables.insert(w.clone());
                }
                for r in &flow.coupling.reads {
                    directly_affected_tables.insert(r.clone());
                }
                for e in &flow.coupling.events_published {
                    directly_affected_events.insert(e.clone());
                }

                direct.push(ImpactedFlow {
                    flow_id: flow.id.clone(),
                    level: ImpactLevel::Direct,
                    reason: matched_reasons.join("; "),
                    coupled_tables: flow.coupling.writes.clone(),
                    coupled_events: flow.coupling.events_published.clone(),
                });
            }
        }

        // 2. Identify State / Shared DB coupling impacts
        let mut state_impacts = Vec::new();
        for flow in &map.flows {
            if directly_affected_ids.contains(&flow.id) {
                continue;
            }

            let mut shared_tables = Vec::new();
            for tbl in &directly_affected_tables {
                if flow.coupling.reads.contains(tbl) || flow.coupling.writes.contains(tbl) {
                    shared_tables.push(tbl.clone());
                }
            }

            if !shared_tables.is_empty() {
                state_impacts.push(ImpactedFlow {
                    flow_id: flow.id.clone(),
                    level: ImpactLevel::State,
                    reason: format!("Shares database table(s): {}", shared_tables.join(", ")),
                    coupled_tables: shared_tables,
                    coupled_events: vec![],
                });
            }
        }

        // 3. Identify Event chain impacts
        let mut event_impacts = Vec::new();
        for flow in &map.flows {
            if directly_affected_ids.contains(&flow.id) {
                continue;
            }

            let mut shared_evts = Vec::new();
            for evt in &directly_affected_events {
                if flow.coupling.events_consumed.contains(evt) {
                    shared_evts.push(evt.clone());
                }
            }

            if !shared_evts.is_empty() {
                event_impacts.push(ImpactedFlow {
                    flow_id: flow.id.clone(),
                    level: ImpactLevel::Event,
                    reason: format!("Consumes event(s) emitted by changed flow: {}", shared_evts.join(", ")),
                    coupled_tables: vec![],
                    coupled_events: shared_evts,
                });
            }
        }

        let total = direct.len() + state_impacts.len() + event_impacts.len();

        ImpactReport {
            changed_inputs: changed_files_or_symbols.to_vec(),
            direct_impacts: direct,
            state_impacts,
            event_impacts,
            total_impacted: total,
        }
    }

    fn find_matching_calls(steps: &[Step], target: &str, reasons: &mut Vec<String>) {
        let target_lower = target.to_lowercase().replace('\\', "/");
        for s in steps {
            if let Some(sym) = &s.symbol {
                if sym.to_lowercase().contains(&target_lower) {
                    reasons.push(format!("Calls modified symbol: {}", sym));
                }
            }
            if let Some(file) = &s.file {
                let file_lower = file.to_lowercase().replace('\\', "/");
                if file_lower.ends_with(&target_lower) || target_lower.ends_with(&file_lower) {
                    reasons.push(format!("Calls code in modified file: {}", file));
                }
            }

            if let Some(branches) = &s.branches {
                for b in branches {
                    Self::find_matching_calls(&b.steps, target, reasons);
                }
            }
            if let Some(child_steps) = &s.steps {
                Self::find_matching_calls(child_steps, target, reasons);
            }
        }
    }

    pub fn print_report(report: &ImpactReport) {
        println!("{}", "============================================================".bright_blue());
        println!("{}", "               FLOWMAP IMPACT ANALYSIS REPORT                ".bold().bright_white());
        println!("{}", "============================================================".bright_blue());
        println!("Inputs analyzed: {}", report.changed_inputs.join(", ").yellow());
        println!("Total flows in blast radius: {}\n", report.total_impacted.to_string().bold().red());

        if !report.direct_impacts.is_empty() {
            println!("{}", "[ DIRECT IMPACTS ] (Handler or method code modified)".bold().bright_red());
            for imp in &report.direct_impacts {
                println!("  * {} ({})", imp.flow_id.bold().white(), imp.reason.bright_yellow());
            }
            println!();
        }

        if !report.state_impacts.is_empty() {
            println!("{}", "[ STATE / SHARED TABLE COUPLING ] (Unrelated flows sharing tables)".bold().bright_yellow());
            println!("  {} These flows were NOT directly edited, but modify or read the same DB tables!", "WARNING:".bold().red());
            for imp in &report.state_impacts {
                println!("  * {} -> {}", imp.flow_id.bold().white(), imp.reason.bright_cyan());
            }
            println!();
        }

        if !report.event_impacts.is_empty() {
            println!("{}", "[ EVENT CONSUMER IMPACTS ] (Async consumers of events)".bold().bright_cyan());
            for imp in &report.event_impacts {
                println!("  * {} -> {}", imp.flow_id.bold().white(), imp.reason.bright_green());
            }
            println!();
        }

        if report.total_impacted == 0 {
            println!("{}", "No flows detected in blast radius for these inputs.".green());
        }
    }
}
