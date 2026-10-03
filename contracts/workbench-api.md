# Workbench API v1

Browser-only base: `http://127.0.0.1:<dynamic-port>/api/v1`.
Every endpoint except pairing requires the HttpOnly session cookie. Host must
match that exact loopback origin; writes also require a matching Origin header.
There is no CORS wildcard. API responses are `Cache-Control: no-store`.

`laya dashboard` obtains a private-IPC single-use, 60-second pairing code. The
browser exchanges it with `POST /pair {"code":"…"}`, then removes the fragment.
Sessions last eight hours and do not persist across service restarts.

## Resources

| Method/path | Body or result |
| --- | --- |
| GET `/status` | Service, worker, per-process observations, storage pressure, known-attempt score coverage, outbox state |
| GET `/events` | Named `change` SSE, numeric event ID; resume with Last-Event-ID or `last_event_id` query |
| GET `/decisions?limit=50&offset=0&filter=pending` | `{items,limit,offset}`; items contain identifiers, a text request-state summary bounded to 280 Unicode characters (decision ID for structured/null state), status/protection/timestamps, risk, and review signals; raw request/result/error/context are detail-only; limit at most 200; filter `all`, `pending`, `finished` |
| GET `/decisions/{id}` | Redacted request/result/context, snapshots, feedback, model observations, execution attempts, reviews |
| POST `/decisions/{id}/reviews` | `{expected_revision,status,labels?,reason,task_family?,language?,applicability?}`; statuses confirmed/corrected/insufficient/excluded |
| DELETE `/decisions/{id}` | Explicit privacy delete, pending copies, derived case contexts, managed exports; backup files remain separately removable |
| GET `/cases`, `/memory-versions`, `/jobs` | Lists use `{items,limit,offset}` |
| DELETE `/cases/{id}` | Withdraw case, scrub derived copies, invalidate dependent versions |
| POST `/memory-versions` | `{case_ids:[...]}` creates a frozen candidate; does not activate |
| GET `/memory-versions/{id}` | Frozen members and evaluation report |
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
