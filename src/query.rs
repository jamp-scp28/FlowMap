use crate::model::ConnectedFlowMap;
use colored::*;

pub struct QueryEngine;

impl QueryEngine {
    pub fn query_table(map: &ConnectedFlowMap, table: &str) {
        let table_lower = table.to_lowercase();
        let matches: Vec<_> = map
            .table_index
            .iter()
            .filter(|(k, _)| k.to_lowercase().contains(&table_lower))
            .collect();

        if matches.is_empty() {
            println!("No tables matching '{}' found.", table.yellow());
            return;
        }

        for (name, info) in matches {
            println!("{}", format!("Table: {}", name).bold().bright_white());
            println!("  Writers (Mutates state):");
            if info.writers.is_empty() {
                println!("    {}", "(none)".dimmed());
            } else {
                for w in &info.writers {
                    println!("    {} {}", "->".bright_red(), w);
                }
            }

            println!("  Readers:");
            if info.readers.is_empty() {
                println!("    {}", "(none)".dimmed());
            } else {
                for r in &info.readers {
                    println!("    {} {}", "<-".bright_cyan(), r);
                }
            }
            println!();
        }
    }

    pub fn query_event(map: &ConnectedFlowMap, event: &str) {
        let evt_lower = event.to_lowercase();
        let matches: Vec<_> = map
            .event_index
            .iter()
            .filter(|(k, _)| k.to_lowercase().contains(&evt_lower))
            .collect();

        if matches.is_empty() {
            println!("No events matching '{}' found.", event.yellow());
            return;
        }

        for (name, info) in matches {
            println!("{}", format!("Event: {}", name).bold().bright_white());
            if info.consumers.is_empty() {
                println!("  {}", "UNCONSUMED! No registered consumers found in codebase.".bold().bright_red());
            }

            println!("  Published by:");
            if info.publishers.is_empty() {
                println!("    {}", "(none)".dimmed());
            } else {
                for p in &info.publishers {
                    println!("    {} {}", "+>".bright_green(), p);
                }
            }

            println!("  Consumed by:");
            if info.consumers.is_empty() {
                println!("    {}", "(none)".dimmed());
            } else {
                for c in &info.consumers {
                    println!("    {} {}", "<+".bright_cyan(), c);
                }
            }
            println!();
        }
    }

    pub fn query_flag(map: &ConnectedFlowMap, flag: &str) {
        let flag_lower = flag.to_lowercase();
        let matches: Vec<_> = map
            .flag_index
            .iter()
            .filter(|(k, _)| k.to_lowercase().contains(&flag_lower))
            .collect();

        if matches.is_empty() {
            println!("No feature flags matching '{}' found.", flag.yellow());
            return;
        }

        for (name, flows) in matches {
            println!("{}", format!("Feature Flag: {}", name).bold().bright_purple());
            println!("  Guards {} flow(s):", flows.len());
            for f in flows {
                println!("    * {}", f.bright_white());
            }
            println!();
        }
    }

    pub fn search(map: &ConnectedFlowMap, query: &str) {
        let q = query.to_lowercase();
        println!("{}", format!("Search results for '{}':", query).bold().bright_blue());

        let mut matches = 0;
        for flow in &map.flows {
            let hit = flow.id.to_lowercase().contains(&q)
                || flow.handler.symbol.to_lowercase().contains(&q)
                || flow.coupling.writes.iter().any(|w| w.to_lowercase().contains(&q))
                || flow.coupling.reads.iter().any(|r| r.to_lowercase().contains(&q))
                || flow.coupling.events_published.iter().any(|e| e.to_lowercase().contains(&q));

            if hit {
                matches += 1;
                println!("  * {} ({})", flow.id.bold().white(), flow.handler.symbol.dimmed());
            }
        }

        if matches == 0 {
            println!("  No matching flows found.");
        } else {
            println!("\nFound {} matching flow(s).", matches.to_string().bold());
        }
    }
}
