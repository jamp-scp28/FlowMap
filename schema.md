# Flow map schema (v0 draft)

Language-agnostic. Each extractor (Rust, C#) emits one `Flow` per entry point.

## Flow
| field | meaning |
|---|---|
| `id` | stable id, e.g. `POST /animals` |
| `kind` | `http` \| `event_handler` \| `background_job` \| `cron` |
| `trigger` | method + path, or event/topic/schedule |
| `handler` | `{ symbol, file, line, signature }` |
| `input` | `{ source: body|query|route|header, type, fields[] }` |
| `guards` | pre-conditions that gate everything (auth, feature flags, role checks) |
| `steps[]` | ordered steps, each a node below |
| `outputs` | status codes / response types, each with the `when` condition |

## Step (node) types
| type | fields |
|---|---|
| `call` | `symbol`, `file`, `line`, `resolved_via` (`static` \| `trait_impl` \| `di` \| `inferred`) |
| `decision` | `kind` (`feature_flag` \| `config` \| `version_check` \| `state_check` \| `authz` \| `validation`), `condition` (source text), `branches[]` (each with `when` and nested `steps`) |
| `effect` | `kind` (`db_read` \| `db_write` \| `event_publish` \| `http_call` \| `email` \| `storage` \| `log`), `target` (table / event type / service), `op`, `file`, `line`, `transactional` |
| `return` | `status`, `when` |

## Edges between flows (cross-flow links)
- `publishes -> consumed_by`: an `event_publish` effect links to every `event_handler` flow with the same event type.
- `writes/reads`: a `db_write` on table T links to every flow with `db_read` or `db_write` on T (shared-state coupling).

## Provenance (required on every step)
`confidence`: `extracted` (read straight from the syntax tree) | `resolved` (symbol-resolved by the compiler/analyzer) | `inferred` (heuristic or LLM). Anything not `extracted`/`resolved` must be verified before it is trusted for impact analysis.

## Tooling per language
| | Rust | C# |
|---|---|---|
| Parse | `syn` | Roslyn syntax trees |
| Symbol / call resolution | rust-analyzer (SCIP) | Roslyn semantic model |
| Entry points | `.route(path, method(handler))` in Axum `Router` builders | `[HttpGet/Post...]` + `[Route]` on controller actions |
| Sink catalog | `diesel::insert_into/update/delete`, `spawn_blocking`, S3, SMTP | `DbContext.SaveChanges*`, `AddDomainEvent`, API clients (`IPaymentApiClient`), `ILogger` |
| Decision catalog | `if`/`match`, `require_*` authz fns | `[RequireFeature]`, `if`/ternary on config, `Quote.*` state checks |
| DI / dispatch | `Arc<Diesel…Repository>` ↔ `impl Trait for` | constructor injection ↔ `AddScoped<I, T>` registrations |
