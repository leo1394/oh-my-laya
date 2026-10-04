# Workbench API v1

Browser-only base: `http://127.0.0.1:<dynamic-port>/api/v1`.
Every endpoint except pairing requires the HttpOnly session cookie. Host must
match that exact loopback origin; writes also require a matching Origin header.
There is no CORS wildcard. API responses are `Cache-Control: no-store`.

`laya dashboard` obtains a private-IPC single-use, 60-second pairing code. The
browser exchanges it with `POST /pair {"code":"…"}`, then removes the fragment.
Sessions last eight hours and do not persist across service restarts.

## Resources

### Shared overview / Case study date scope

`GET /overview` accepts optional `created_after` and `created_before` as inclusive,
nonnegative UTC epoch seconds and returns `{dashboard, counts, risk_counts}`.
All overview counters and risk buckets share the selected decision cohort.
`GET /cases` accepts
the same fields alongside pagination. Unknown, malformed or inverted filters are
rejected. Omitting both bounds retains all-time behavior. `/status` remains an
unfiltered service/diagnostics resource for CLI and compatibility consumers.

The cohort is source decision `created_at`, including cases reviewed later and
usage/feedback delivered later. Apply case filtering before pagination. Overview
versions are separately filtered by version creation time; all decision-derived
metrics use the shared decision cohort. Case study's version/job management lists
remain all-time resources and are labelled separately from its filtered case list.
Date filtering must not hide global
stream reuse/conflicts or manufacture savings from a fragment of a cross-range
manifest; a scenario whose referenced decisions cross the range is excluded.

The browser uses local calendar dates: today, last 7 days (default), last 30 days,
or an inclusive custom date pair. The last-N presets include today. Convert start
midnight and the midnight after the final date into UTC seconds, subtracting one
second from the latter; calendar construction handles DST, not fixed 86400-second
subtraction. Overview and Case study share selection state, including navigation
through overview buttons. Invalid custom ranges make no filtered request and must
not relabel stale results as belonging to the invalid range.

Workbench simplification additions (source preview):

- `GET /status.dashboard` adds the Token-first homepage summary. Token usage is
  explicitly partial. An ordered cumulative attempt stream supplies
  `aggregation=cumulative`, a `usage_stream_id`, and a positive `source_sequence`;
  all three fields are optional together and malformed partial metadata is rejected.
  The highest source sequence wins regardless of receive order. A later ineligible
  checkpoint therefore invalidates an older checkpoint in that stream. Conflicting
  content at one stream/sequence, multiple claimed streams for one decision/attempt,
  mixed ordered and legacy reports, or a stream ID claimed by multiple attempts is
  excluded rather than guessed. Legacy reports count once only when there is one
  report or all count/provenance fields (including checkpoint) are identical.
  Unrelated task/subtree/turn/response reports do not displace an eligible attempt
  stream. Only sender-reported `source_verified=true`, `scope=attempt`,
  `overlap_status=non_overlapping` reports with a known `total_tokens` are merged;
  this flag is recorded evidence, not independent authentication.
  No qualifying reports means `recorded_total=null`, not zero. `included_attempts`
  counts selected decision/attempt groups. `excluded_reports` and the keyed
  `exclusion_reasons` count reports or selected stream candidates withheld by the
  stated guard or ambiguity. `aggregation_status` is `available`, `partial`,
  `unavailable`, or `overflow`; `partial` means at least one attempt was included
  and at least one report was excluded. Totals above JavaScript's exact integer
  limit (9,007,199,254,740,991), including checked-add overflow, return null with
  `overflow`. These fields describe recorded evidence, not total session coverage.
  `scenario` separately describes the hypothetical unsplit-orchestrator estimate;
  it never uses the all-recorded total as the scenario's actual denominator.
  No duplicate baseline execution is required. Cheaper models, shorter runtimes
  and fewer tokens are different measurements.
- `dashboard.execution_models` groups the latest recorded effective model/effort
  per decision/attempt. Null observations stay unverified; model names are not
  strength rankings. `dashboard.learning` counts pending uncertainty, problem
  scores, their intersection, reviewer-correction signals, active reviewed cases,
  and evaluated versions (including failed/historical reports). None is a claim
  of improved quality or an authorization to activate memory or train weights.

### Scenario savings (source preview)

`laya_feedback` accepts the optional `run_manifest` kind using the bundled schema.
It links recorded usage events across decision/attempt identities and stores
sender-declared coverage; `complete` is not independently verified completeness.
The optional `scenario` payload records orchestrator model/effort, initial context,
logical stages (`context_growth_tokens`, `work_output_tokens`, `passes`) and their
`input_source`. Missing historical inputs are not inferred from token totals.

`dashboard.tokens.scenario` returns `status` (available/partial/unavailable),
`estimator_version`, signed `saved` and hypothetical `baseline` low/central/high
ranges, matched `actual_total`, included/excluded run counts, assumptions and run
provenance. Its scope is `host_only`, excluding local Laya inference. Version
`scenario-v1` is the heuristic defined in [the scenario design](../docs/token-savings-estimate.md),
not an empirical confidence interval. `estimated_saved` aliases its central delta;
`baseline_status` is `scenario_estimate` or `collecting_inputs`. No result is null,
not zero. Negative savings remain negative. Partial coverage stays labelled.

Usage references must be current, unambiguous and non-overlapping; conflicting
or reused runs/segments are excluded rather than selected for favorable savings.
Deleting a referenced decision removes dependent manifests and their queued
copies. This source path does not enable recording, run extra model calls or
claim that the companion Skill already automatically generates scenario inputs.

- `GET /advisor-preferences` reads the existing recommendation policy and ceiling
  through the private worker. It does not change policy, start inference or grant permission.
- `PATCH /settings` accepts `model_tiers`: an object keyed by `low`, `medium`,
  `high`, each containing an exact `model` and `reasoning_effort`. An empty object
  clears configured tiers. These are user candidates, not a ranked provider catalog.
  Rust supplies the stored mapping to advisor calls via the private worker protocol;
  it cannot be overridden through a caller-supplied mapping. Python validates a
  selected pair against the current host catalog. Auto routing still requires the
  exact authorized ceiling model and an effort at or below that ceiling. An
  unavailable or out-of-ceiling profile returns no recommendation and asks the
  user; it never expands authority. Explicit session/reviewer assignments remain
  unchanged. Unconfigured tiers retain existing advisor behavior.
- `GET /status` adds `risk_counts` (`low`, `medium`, `high`, `unknown`) over all
  non-deleted records, independent of browser pagination. Missing/expired labels
  count as unknown. This is original recorded risk, not model rank or success rate.
- Review `labels.model_tier` optionally stores a human `low`/`medium`/`high`
  model-tier preference alongside existing labels. It is not an observed result,
  not an execution-model identity, and must never substitute for the independent
  complexity/risk/certainty labels. Review history and immutable cases retain it.
  No schema change is required for these JSON extensions. The browser makes notes
  optional by submitting an explicit human-action audit reason when blank; it does
  not invent evidence. Complete reviews still need valid assessment labels.

| Method/path | Body or result |
| --- | --- |
| GET `/status` | Service, worker, per-process observations, storage pressure, known-attempt score coverage, outbox state |
| GET `/events` | Named `change` SSE, numeric event ID; resume with Last-Event-ID or `last_event_id` query |
| GET `/decisions?limit=50&offset=0&filter=pending` | `{items,limit,offset}`; items contain identifiers, a text request-state summary bounded to 280 Unicode characters (decision ID for structured/null state), status/protection/timestamps, risk, and review signals; raw request/result/error/context are detail-only; limit at most 200; filter `all`, `pending`, `finished` |
| GET `/decisions/{id}` | Redacted request/result/context, snapshots, feedback, model observations, execution attempts, reviews |
| POST `/decisions/{id}/reviews` | `{expected_revision,status,labels?,reason,task_family?,task_lineage?,language?,applicability?}`; statuses confirmed/corrected/insufficient/excluded. `task_family` is a canonical routing slug (`docs` aliases to `documentation`); `task_lineage` is independent holdout provenance and remains unknown when omitted. |
| DELETE `/decisions/{id}` | Explicit privacy delete, pending copies, derived case contexts, managed exports; backup files remain separately removable |
| GET `/cases`, `/memory-versions`, `/jobs` | Lists use `{items,limit,offset}` |
| DELETE `/cases/{id}` | Withdraw case, scrub derived copies, invalidate dependent versions |
| POST `/memory-versions` | `{case_ids:[...]}` creates a frozen candidate; does not activate |
| GET `/memory-versions/{id}` | Frozen members and evaluation report; list/detail include server-derived `activation_eligible` for current-policy compatibility, passed evaluation and non-invalidated state |
| POST `/memory-versions/{id}/activate` | `{}`; requires a passed, non-invalidated evaluation; explicit browser action |
| POST `/jobs` | `{kind:"evaluation",version_id:"…"}` or `{kind:"export"}`; one background job at a time |
| GET `/jobs/{id}` | Job state/progress/error/result; interrupted work is not automatically resumed |
| POST `/jobs/{id}/cancel` | `{}`; cancellation observed between evaluation calls/export records |
| GET `/exports/{artifact_id}` | Authenticated streamed NDJSON attachment, UUID-scoped filename; no arbitrary filesystem paths |
| GET `/settings` | Recording, memory, pending-delivery, retention and storage settings |
| PATCH `/settings` | Subset of recording_enabled/memory_enabled/replay_enabled (booleans), retention_days (1–36500), storage_soft_limit_bytes (at least 1 MiB) |
| GET `/backups` | Managed backup catalog |
| POST `/backups` | `{}` creates a verified SQLite snapshot |
| POST `/backups/{id}/restore` | `{}`; fails while active work holds maintenance access; preserves privacy tombstones |
| DELETE `/backups/{id}` | Removes that managed backup file; does not delete external copies |
| GET `/outbox` | Oldest 50 unacknowledged feedback events, hashes, errors and delivery states |
| POST `/outbox/{event_id}/retry` | `{}`; retries the unchanged event, never rewrites an initial score |

Labels are `{complexity:"low|medium|high",risk:"low|medium|high",certainty:"clear|uncertain"}`.
Completed reviews need all three labels. They invalidate older case revisions;
newly corrected cases must pass a fresh candidate evaluation.

Reports are immutable. Re-evaluation requires a newly frozen candidate. A report
with no actual candidate-case exposure cannot authorize activation. Historical
reports remain readable but cannot authorize retrieval or activation when their
evaluator/retrieval-policy identity is incompatible. Browser eligibility is only
a UI hint; the activation endpoint independently enforces the same checks.

Errors use `{error:"prefix: explanation"}` with 401 unauthorized, 403 forbidden,
404 not_found, 409 conflict, otherwise 400 (including busy/invalid input).
There is no browser endpoint for arbitrary SQL, files, programs, or weight training.

## MCP and private IPC

MCP exposes `laya_tell_me`, `laya_advisor_preferences`, and `laya_feedback`.
The [feedback JSON Schema](feedback.schema.json) is embedded in live tool discovery.
Feedback arguments are the event itself, not an `event` wrapper. Feedback from
agents never creates human-confirmed labels or activates memory versions.

Private Unix IPC uses one request/reply per connection, newline-delimited JSON:
`{protocol_version:1,request_id,method,params}` and `{request_id,result}` or
`{request_id,error}`. Keep the connection open while awaiting the reply; closing
it cancels in-flight work. Frames are at most 1 MiB. Online worker requests have
a 120-second queue-plus-execution deadline and a 32-entry queue.

Feedback `stored` requires a committed main database receipt with the same
event ID/hash. `queued_local` means a committed outbox entry only. Valid missing
revision dependencies stay retryable; invalid events remain quarantined. Pausing
delivery does not delete evidence. Deletion can deliberately change/remove
derived snapshots; ordinary reviews never rewrite initial scores.
