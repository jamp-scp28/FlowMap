# FlowMap Gap Analysis & Implementation Roadmap

> **Author:** Jorge Molano  
> **Date:** October 15, 2026  
> **Target:** FlowMap v0.3.0 Engine Maturation  
> **Status:** **ALL 20 GAPS (G1–G20) & 15 TEST VALIDATIONS (T1–T15) RESOLVED & PASSING**

---

## Executive Summary

FlowMap v0.3.0 closes the indirection gap in enterprise microservice architectures utilizing **Mediator (MediatR) commands/queries**, **Domain Aggregates raising events**, **cross-repo message meshes**, **EF Core tracked entities**, and **Python event workers**.

All 20 identified gaps (G1–G20) have been resolved with deep AST traversal, semantic type inference, strict symbol blast radius isolation, and multi-repo federated linking.

---

## Gap Tracking Matrix

### Group 1: C# Roslyn Extractor Core (G1 – G10)

- [x] **G1. Route Token Expansion (`[controller]`, `[action]`)**
  - *Implemented in:* `extractors/csharp/Program.cs:RouteHelper.ResolveRoute`
  - *Outcome:* `[controller]` is expanded to Controller name without suffix and `[action]` to method name (e.g. `POST api/Orders`, `POST api/Customers`), completely preventing flow ID collisions.
- [x] **G2. Mediator Call-Tree Inlining (`mediator.Send`)**
  - *Implemented in:* `extractors/csharp/Program.cs:RequestHandlersByCommand`
  - *Outcome:* `_mediator.Send(cmd)` and `_sender.Send(cmd)` resolve the request command/query type to its registered handler implementation and recursively inline steps, branches, DB queries, and entity methods down to depth 6.
- [x] **G3. Domain Events in Aggregates Linked to Calling Flow**
  - *Implemented in:* `extractors/csharp/Program.cs:Classify`
  - *Outcome:* Flows invoking aggregate methods (e.g. `order.Submit()`) recursively capture `AddDomainEvent(...)` dispatches into `flow.coupling.events_published` and link them to consumers in `event_index`.
- [x] **G4. Handler & EF Core Tracked Entity Table Writes**
  - *Implemented in:* `extractors/csharp/Program.cs:TrackedTables` & `src/linker.rs`
  - *Outcome:* EF Core entities modified along the execution chain or loaded via DbSet are tracked in `TrackedTables`. When `SaveChangesAsync()` is invoked, each tracked entity table is attributed as a write for the calling flow.
- [x] **G5. Empty Event Name Filtering & Call-Site Trace**
  - *Implemented in:* `extractors/csharp/Program.cs` & `src/linker.rs`
  - *Outcome:* Empty strings `""` and unresolved literals are filtered from `coupling.events_published` and `event_index`.
- [x] **G6. Configuration-Driven Branching as Flags**
  - *Implemented in:* `extractors/csharp/Program.cs:ClassifyCondition` & `src/linker.rs`
  - *Outcome:* Entity properties, configuration values, and feature toggles (e.g., `order.Program.AutoApproveEnabled`) evaluated in conditional guards are indexed under `configs_and_flags` and `flag_index`.
- [x] **G7. Minimal-API Endpoints from Discovered Classes**
  - *Implemented in:* `extractors/csharp/Program.cs:BuildMinimalApi`
  - *Outcome:* Minimal API routes registered with `app.MapGet`/`app.MapPost` inside endpoint classes (e.g. implementing `IEndpoint` or `MapEndpoint`) are discovered and mapped to HTTP flows.
- [x] **G8. Consumers Derived from Generic Base Classes**
  - *Implemented in:* `extractors/csharp/Program.cs:ExtractFromClass`
  - *Outcome:* Recursively inspects the base class hierarchy (including `IntegrationEventHandler<T>` base classes) to detect inherited event consumer interfaces.
- [x] **G9. Nested Solution / Project Search**
  - *Implemented in:* `src/main.rs:Commands::Scan`
  - *Outcome:* Recursively searches subdirectories (`src/`, `Source/`, etc.) up to depth 8 for `.sln` or `.csproj` files when scanning from the root.
- [x] **G10. Roslyn Compilation Diagnostics Suppression & Reference Resolution**
  - *Implemented in:* `extractors/csharp/Program.cs:ResolveAssemblyReferences`
  - *Outcome:* Reference priority is `--refs` → running framework → auto-discovered `bin/**` / `project.assets.json` DLLs (gap-fill only); DLLs built from the scanned sources are skipped. Diagnostics line prints the top error codes.

---

### Group 2: Analysis & Blast Radius Commands (G11 – G16)

- [x] **G11. Behavioral Diff on Modified Flows (`diff` & `delta`)**
  - *Implemented in:* `src/diff.rs:FlowDiffer`
  - *Outcome:* Modified flows are itemized with fine-grained deltas for added/removed guards, tables read/written, events published, and outcome return codes, rendering full markdown PR summaries.
- [x] **G12. Blast Radius on Changed Domain Aggregates (`impact --files`)**
  - *Implemented in:* `src/impact.rs:ImpactAnalyzer`
  - *Outcome:* Reports every flow calling into methods in aggregate files (e.g., `Order.cs`) across all inlining depths and MediatR dispatch chains.
- [x] **G13. Member-Level Impact Precision**
  - *Implemented in:* `src/impact.rs:parse_unified_diff` / `member_spans`
  - *Outcome:* `impact --diff` parses `git diff -U0` hunks and only hits flows executing a changed member; hunks outside any mapped member fall back to file-level. Traces impact from specific modified methods, properties, or DTO types instead of whole-file over-matching.
- [x] **G14. Strict Symbol Resolution for Impact (`impact --symbols`)**
  - *Implemented in:* `src/impact.rs:symbol_matches`
  - *Outcome:* Strict token boundary matching prevents false substring hits (e.g., `--symbols Accept` no longer matches `SendTermsAcceptedEmailHandler`).
- [x] **G15. Test File Exclusion from Production Blast Radius**
  - *Implemented in:* `src/impact.rs:is_test_file`
  - *Outcome:* Excludes test files and test projects (`*.Tests`, `*Test*.cs`) from production flow blast radius and categorizes them separately.
- [x] **G16. State Coupling Differentiation (New vs Background)**
  - *Implemented in:* `src/impact.rs:active_state_impacts` vs `background_state_impacts`
  - *Outcome:* Separates active database mutations touched by the change from existing background read coupling.

---

### Group 3: Architectural Audit & Usability (G17, G20)

- [x] **G17. Mediator Commands/Queries Filtered from `EVENT_HEALTH`**
  - *Implemented in:* `src/linker.rs:is_request_command_query`
  - *Outcome:* Request-response commands and queries (`*Command`, `*Query`, `*Request`) sent via `Send` are filtered from `EVENT_HEALTH` dead-event warnings, leaving only true orphaned pub/sub domain events.
- [x] **G20. Default FlowMap Path Alignment**
  - *Implemented in:* `src/main.rs:resolve_flowmap_path`
  - *Outcome:* All query and analysis commands default to checking `flows/flowmap.json` first, seamlessly falling back to `./flowmap.json`.

---

### Group 4: Cross-Repository & Multi-Language Scope (G18 – G19)

- [x] **G18. Cross-Repository Pub/Sub Mesh Stitching**
  - *Implemented in:* `src/main.rs:Commands::Federate` & `Commands::Build --multi`
  - *Outcome:* Merges flowmaps across multiple services and repositories into a single unified topology, connecting publishers in Service A with consumers in Service B and auditing cross-repo dead events.
- [x] **G19. Python Extractor Support for Event Consumers/Publishers**
  - *Implemented in:* `extractors/python/extractor.py` & `src/extractors/python.rs`
  - *Outcome:* Discovers Azure Functions v1 (`function.json` + `main()`) triggers, FastAPI/Flask HTTP routes, Celery tasks, event bus `@subscribe` handlers, event publications, database writes, and feature flags in Python services.

---

## Test Validation Cases (T1 – T15)

| Case | Scenario | Command | Expected Outcome | Forbidden Outcome | Status |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **T1** | Unique Route IDs | `flowmap scan` | `POST api/Orders` and `POST api/Customers` distinct | Flow IDs containing literal `[controller]` | **PASS** |
| **T2** | Follow Mediator | `flowmap show Submit` | Steps from `SubmitOrderCommandHandler`, `Orders` write, domain events | Command listed as dead event | **PASS** |
| **T3** | Aggregate Blast Radius | `flowmap impact --files Order.cs` | Every flow reaching `Order.Submit()` | 0 flows | **PASS** |
| **T4** | DTO Member Impact | `flowmap impact --diff <base>` | Endpoints returning `OrderDto` | Endpoints using other unrelated DTO mappings | **PASS** |
| **T5** | Behavioral Diff | `flowmap delta --against <base>` | `POST api/Orders/{id}/Submit` marked modified with added events/steps | No changes reported | **PASS** |
| **T6** | Property Symbol Impact | `flowmap impact --symbols <Property>` | Flows reading the property | Substring-matched unrelated handlers | **PASS** |
| **T7** | Test File Isolation | `flowmap impact --diff <base>` | Tests in test section or excluded | Production endpoint marked "handler changed" | **PASS** |
| **T8** | Shared Table Writers | `flowmap query --table Orders` | Multiple command handlers listed as writers | 0 writers | **PASS** |
| **T9** | Audit Cleanliness | `flowmap check` | No `EVENT_HEALTH` issues for commands with handlers | Commands/queries reported as dead events | **PASS** |
| **T10** | Config/Flag Branches | `flowmap query --flag <Setting>` | Flows branching on `AutoApproveEnabled` | No results | **PASS** |
| **T11** | Minimal API & Base Consumers | `flowmap scan` | Minimal API routes + base class consumers detected | 0 HTTP flows / missing consumers | **PASS** |
| **T12** | Cross-Repo Federation | `flowmap query --event <IntegrationEvent>` | Consumers across all repos listed | Only local repo consumers | **PASS** |
| **T13** | Nested Solution | `flowmap scan <root>` | Locates `Source/*.sln` or `src/*.csproj` | "Could not determine project type" | **PASS** |
| **T14** | Default Map Path | `flowmap scan .` then `flowmap show Submit` | Works seamlessly without `--map` | "Could not read flowmap file" | **PASS** |
| **T15** | Compiler Diagnostics | `flowmap scan` | Clean reference resolution with near-zero errors | Thousands of diagnostic errors | **PASS** |
