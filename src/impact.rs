use crate::model::*;
use colored::*;
use std::collections::HashSet;

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImpactLevel {
    Direct,      // Flow's handler or called method was directly edited
    ActiveState, // Flow touches tables modified by an edited flow (active concurrent hazard)
    State,       // Flow shares read-only background database state
    Event,       // Flow consumes an event emitted by an edited flow
    Test,        // Test suite modification
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
    pub active_state_impacts: Vec<ImpactedFlow>,
    pub background_state_impacts: Vec<ImpactedFlow>,
    pub event_impacts: Vec<ImpactedFlow>,
    pub test_files: Vec<String>,
    pub total_impacted: usize,
}

pub struct ImpactAnalyzer;

impl ImpactAnalyzer {
    pub fn is_test_file(path: &str) -> bool {
        let p = path.to_lowercase().replace('\\', "/");
        p.contains("/test/")
            || p.contains("/tests/")
            || p.contains(".tests/")
            || p.contains(".unittests/")
            || p.contains(".integrationtests/")
            || p.ends_with("tests.cs")
            || p.ends_with("test.cs")
            || p.ends_with("_test.rs")
            || p.ends_with("_tests.rs")
            || p.ends_with(".spec.ts")
            || p.ends_with(".test.ts")
    }

    pub fn symbol_matches(flow_symbol: &str, target: &str) -> bool {
        let s = flow_symbol.trim();
        let t = target.trim();
        if s.eq_ignore_ascii_case(t) {
            return true;
        }
        if t.contains('.') || t.contains("::") {
            let t_clean = t.replace("::", ".");
            let s_clean = s.replace("::", ".");
            return s_clean.ends_with(&t_clean) || s_clean.eq_ignore_ascii_case(&t_clean);
        }
        // Unqualified symbol matching against qualified symbol
        if let Some(pos) = s.rfind('.') {
            if s[pos + 1..].eq_ignore_ascii_case(t) {
                return true;
            }
        }
        if let Some(pos) = s.rfind("::") {
            if s[pos + 2..].eq_ignore_ascii_case(t) {
                return true;
            }
        }
        false
    }

    pub fn analyze(
        map: &ConnectedFlowMap,
        changed_files_or_symbols: &[String],
    ) -> ImpactReport {
        let mut direct = Vec::new();
        let mut directly_affected_ids = HashSet::new();
        let mut directly_affected_writes = HashSet::new();
        let mut directly_affected_reads = HashSet::new();
        let mut directly_affected_events = HashSet::new();
        let mut test_files = Vec::new();

        let mut production_inputs = Vec::new();
        for inp in changed_files_or_symbols {
            if Self::is_test_file(inp) {
                test_files.push(inp.clone());
            } else {
                production_inputs.push(inp.clone());
            }
        }

        // 1. Identify Direct impacts on production flows
        for flow in &map.flows {
            let mut matched_reasons = Vec::new();

            for target in &production_inputs {
                let target_trimmed = target.trim();
                let target_lower = target_trimmed.to_lowercase().replace('\\', "/");
                let handler_file_lower = flow.handler.file.to_lowercase().replace('\\', "/");
                let is_file = target_trimmed.contains('/') || target_trimmed.ends_with(".cs") || target_trimmed.ends_with(".rs");

                // Check file match
                if is_file && (handler_file_lower.ends_with(&target_lower) || target_lower.ends_with(&handler_file_lower)) {
                    matched_reasons.push(format!("Handler file changed: {}", flow.handler.file));
                }

                // Check symbol match with strict boundary matching
                if Self::symbol_matches(&flow.handler.symbol, target_trimmed) {
                    matched_reasons.push(format!("Handler symbol matched: {}", flow.handler.symbol));
                }

                // Check calls inside steps (inlining across mediator, aggregates, handlers)
                Self::find_matching_calls(&flow.steps, target_trimmed, &mut matched_reasons);
            }

            if !matched_reasons.is_empty() {
                directly_affected_ids.insert(flow.id.clone());
                for w in &flow.coupling.writes {
                    if w != "DbContext" && !w.is_empty() {
                        directly_affected_writes.insert(w.clone());
                    }
                }
                for r in &flow.coupling.reads {
                    if r != "DbContext" && !r.is_empty() {
                        directly_affected_reads.insert(r.clone());
                    }
                }
                for e in &flow.coupling.events_published {
                    if !e.is_empty() {
                        directly_affected_events.insert(e.clone());
                    }
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

        // 2. Identify Active State impacts (touch tables being written to by changed flows)
        let mut active_state_impacts = Vec::new();
        let mut background_state_impacts = Vec::new();

        for flow in &map.flows {
            if directly_affected_ids.contains(&flow.id) {
                continue;
            }

            let mut active_shared = Vec::new();
            for tbl in &directly_affected_writes {
                if flow.coupling.reads.contains(tbl) || flow.coupling.writes.contains(tbl) {
                    active_shared.push(tbl.clone());
                }
            }

            if !active_shared.is_empty() {
                active_state_impacts.push(ImpactedFlow {
                    flow_id: flow.id.clone(),
                    level: ImpactLevel::ActiveState,
                    reason: format!("Touches active table(s) modified by changed code: {}", active_shared.join(", ")),
                    coupled_tables: active_shared,
                    coupled_events: vec![],
                });
            } else {
                let mut bg_shared = Vec::new();
                for tbl in &directly_affected_reads {
                    if flow.coupling.reads.contains(tbl) {
                        bg_shared.push(tbl.clone());
                    }
                }
                if !bg_shared.is_empty() {
                    background_state_impacts.push(ImpactedFlow {
                        flow_id: flow.id.clone(),
                        level: ImpactLevel::State,
                        reason: format!("Shares read-only reference table(s): {}", bg_shared.join(", ")),
                        coupled_tables: bg_shared,
                        coupled_events: vec![],
                    });
                }
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

        let total = direct.len() + active_state_impacts.len() + background_state_impacts.len() + event_impacts.len();

        ImpactReport {
            changed_inputs: changed_files_or_symbols.to_vec(),
            direct_impacts: direct,
            active_state_impacts,
            background_state_impacts,
            event_impacts,
            test_files,
            total_impacted: total,
        }
    }

    fn find_matching_calls(steps: &[Step], target: &str, reasons: &mut Vec<String>) {
        let target_trimmed = target.trim();
        let target_lower = target_trimmed.to_lowercase().replace('\\', "/");
        let is_target_file = target_trimmed.contains('/') || target_trimmed.ends_with(".cs") || target_trimmed.ends_with(".rs");

        for s in steps {
            if let Some(sym) = &s.symbol {
                if Self::symbol_matches(sym, target_trimmed) {
                    reasons.push(format!("Calls modified symbol: {}", sym));
                }
            }
            if let Some(file) = &s.file {
                if is_target_file {
                    let file_lower = file.to_lowercase().replace('\\', "/");
                    if file_lower.ends_with(&target_lower) || target_lower.ends_with(&file_lower) {
                        reasons.push(format!("Calls code in modified file: {}", file));
                    }
                }
            }
            // Check decision conditions for property or flag references
            if let Some(cond) = &s.condition {
                let cond_clean = cond.trim_start_matches('!');
                if Self::symbol_matches(cond_clean, target_trimmed)
                    || cond.contains(&format!(".{}", target_trimmed))
                    || cond.contains(&format!("\"{}\"", target_trimmed))
                {
                    reasons.push(format!("Branches on symbol/property: {}", target_trimmed));
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

        if !report.test_files.is_empty() {
            println!("{}", "[ TEST SUITE MODIFICATIONS ] (Test files changed; excluded from production flows)".bold().bright_cyan());
            for t in &report.test_files {
                println!("  * {}", t.dimmed());
            }
            println!();
        }

        if !report.direct_impacts.is_empty() {
            println!("{}", "[ DIRECT IMPACTS ] (Handler or method code modified)".bold().bright_red());
            for imp in &report.direct_impacts {
                println!("  * {} ({})", imp.flow_id.bold().white(), imp.reason.bright_yellow());
            }
            println!();
        }

        if !report.active_state_impacts.is_empty() {
            println!("{}", "[ ACTIVE STATE COUPLING ] (Concurrent mutation hazard: touches tables modified by changed flows)".bold().bright_red());
            for imp in &report.active_state_impacts {
                println!("  * {} -> {}", imp.flow_id.bold().white(), imp.reason.bright_yellow());
            }
            println!();
        }

        if !report.background_state_impacts.is_empty() {
            println!("{}", "[ BACKGROUND STATE COUPLING ] (Shared read-only reference data)".bold().bright_yellow());
            for imp in &report.background_state_impacts {
                println!("  * {} -> {}", imp.flow_id.bold().white(), imp.reason.dimmed());
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

        if report.total_impacted == 0 && report.test_files.is_empty() {
            println!("{}", "No flows detected in blast radius for these inputs.".green());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strict_symbol_matching_g14() {
        assert!(ImpactAnalyzer::symbol_matches("Order.Accept", "Accept"));
        assert!(ImpactAnalyzer::symbol_matches("Order::Accept", "Accept"));
        assert!(ImpactAnalyzer::symbol_matches("Accept", "Accept"));
        assert!(ImpactAnalyzer::symbol_matches("Order.Accept", "Order.Accept"));

        // G14: Substring must NOT falsely match unrelated symbol!
        assert!(!ImpactAnalyzer::symbol_matches("SendTermsAcceptedEmailHandler", "Accept"));
        assert!(!ImpactAnalyzer::symbol_matches("UnacceptableException", "Accept"));
    }

    #[test]
    fn test_test_file_isolation_g15() {
        assert!(ImpactAnalyzer::is_test_file("OrdersControllerTests.cs"));
        assert!(ImpactAnalyzer::is_test_file("tests/OrderTests.cs"));
        assert!(ImpactAnalyzer::is_test_file("src/Services.Tests/OrderTest.cs"));
        assert!(ImpactAnalyzer::is_test_file("order_test.rs"));

        assert!(!ImpactAnalyzer::is_test_file("OrdersController.cs"));
        assert!(!ImpactAnalyzer::is_test_file("Order.cs"));
        assert!(!ImpactAnalyzer::is_test_file("SubmitOrderCommandHandler.cs"));
    }

    #[test]
    fn test_impact_blast_radius_aggregate_inlining_g12() {
        let flow_json = r#"{
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
                    "symbol": "SubmitOrderCommandHandler.Handle",
                    "file": "src/SubmitOrderCommandHandler.cs",
                    "steps": [
                        {
                            "type": "call",
                            "symbol": "Order.Submit",
                            "file": "src/Domain/Order.cs",
                            "steps": [
                                {
                                    "type": "decision",
                                    "kind": "feature_flag",
                                    "condition": "!order.Program.AutoApproveEnabled"
                                },
                                {
                                    "type": "effect",
                                    "kind": "event_publish",
                                    "target": "OrderSubmittedDomainEvent"
                                }
                            ]
                        }
                    ]
                }
            ]
        }"#;

        let flow: Flow = serde_json::from_str(flow_json).unwrap();
        let map = ConnectedFlowMap {
            version: "1.0".to_string(),
            generated_at: "now".to_string(),
            stats: FlowMapStats::default(),
            domains: Default::default(),
            flows: vec![flow],
            entity_topology: Default::default(),
            table_index: Default::default(),
            event_index: Default::default(),
            flag_index: Default::default(),
            service_index: Default::default(),
            annotations: Default::default(),
        };

        // G12: Blast radius on Order.cs finds POST api/Orders through deep inlining
        let report = ImpactAnalyzer::analyze(&map, &["src/Domain/Order.cs".to_string()]);
        assert_eq!(report.direct_impacts.len(), 1);
        assert_eq!(report.direct_impacts[0].flow_id, "POST api/Orders");

        // G14: Property symbol impact matches AutoApproveEnabled
        let prop_report = ImpactAnalyzer::analyze(&map, &["AutoApproveEnabled".to_string()]);
        assert_eq!(prop_report.direct_impacts.len(), 1);
        assert_eq!(prop_report.direct_impacts[0].flow_id, "POST api/Orders");

        // G15: Test file does NOT mark production flow as directly impacted
        let test_report = ImpactAnalyzer::analyze(&map, &["tests/OrderTests.cs".to_string()]);
        assert_eq!(test_report.direct_impacts.len(), 0);
        assert_eq!(test_report.test_files.len(), 1);
    }
}
