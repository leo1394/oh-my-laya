// Read-only synthetic API for manual browser QA of the real App; no model or database.
import { createServer } from 'vite'
import { fileURLToPath } from 'node:url'

const observations = [
  { contract: 'efficiency_observation_v1', complete_task_coverage: false, attempt_ref: 'fixture-byte-packet', identity: { role: 'explorer', status: 'reported' }, context: { isolation: 'isolated', evidence_ref: 'fixture-only', input_size: { value: 1200, unit: 'bytes', source: 'fixture packet serialization' } }, outcome: { duration_ms: 1200 } },
  { contract: 'efficiency_observation_v1', complete_task_coverage: false, attempt_ref: 'fixture-native-input', identity: { role: 'tester', status: 'reported' }, context: { isolation: 'unsupported', input_size: { value: 300, unit: 'native_tokens', source: 'fixture-only native counter' } }, outcome: { duration_ms: 0 } },
  { contract: 'efficiency_observation_v1', complete_task_coverage: false, attempt_ref: 'fixture-unknown', identity: { role: 'worker', status: 'conflict' }, context: { isolation: 'unknown', input_size: { value: null, unit: 'unknown', source: null } }, outcome: { duration_ms: null } }
]
const decision = { id: 'visual-fixture', created_at: Math.floor(Date.now() / 1000), status: 'pending', request: { state: 'Visual fixture — no real model run' }, result: { answers: { complexity: { choice: 'low' }, risk: { choice: 'low' }, certainty: { choice: 'uncertain' } } }, feedback: [], efficiency_observations: observations }
const settings = { recording_enabled: false, memory_enabled: false, replay_enabled: false, retention_days: 30, soft_limit_bytes: 104857600 }
const overview = { counts: { decisions: 1, pending_reviews: 1, feedback: 4 }, risk_counts: { low: 1 }, dashboard: {
  tokens: { recorded_total: 1234, included_attempts: 1, excluded_reports: 0, aggregation_status: 'partial' },
  execution_models: [{ model: 'fixture-model', reasoning_effort: 'medium', attempts: 1 }],
  learning: { uncertain_pending: 1, uncertain_with_problem_pending: 1, reviewer_corrections_pending: 0, reviewed_cases: 0, evaluated_versions: 0 },
  efficiency: { contract: 'efficiency_summary_v1', scope: 'recorded_attempts', complete_task_coverage: false, observed_attempts: 3, reported_runs: 1, versioned_attempts: 2, identity_conflicts: 1, delegated_attempts: 2, repair_attempts: 1, upgrade_attempts: 0, initial_scored_attempts: 1, flagged_attempts: 1, usage_covered_attempts: 1, usage_missing_attempts: 2, reported_outcomes: { success: 1, failure: 1, unknown: 1 } }
} }
const server = await createServer({
  root: fileURLToPath(new URL('..', import.meta.url)),
  server: { host: '127.0.0.1', port: 0 },
  plugins: [{ name: 'read-only-v15-fixture', configureServer(vite) {
    vite.middlewares.use((req, res, next) => {
      if (!req.url.startsWith('/api/v1/')) return next()
      if (req.method !== 'GET') { res.statusCode = 405; res.end('Read-only fixture'); return }
      const path = new URL(req.url, 'http://localhost').pathname.slice('/api/v1'.length)
      if (path === '/events') { res.statusCode = 204; res.end(); return }
      const data = path === '/status' ? { settings, counts: overview.counts, worker: { state: 'stopped' } }
        : path === '/overview' ? overview : path === '/decisions' ? [decision]
          : path === '/decisions/visual-fixture' ? decision : path === '/settings' ? settings
            : path === '/advisor-preferences' ? { policy: 'always', needs_policy_selection: true } : []
      res.setHeader('Content-Type', 'application/json')
      res.end(JSON.stringify(data))
    })
  } }]
})
await server.listen()
console.log(JSON.stringify({ fixture_only: true, url: server.resolvedUrls.local[0] }))
for (const signal of ['SIGINT', 'SIGTERM']) process.on(signal, async () => { await server.close(); process.exit(0) })
