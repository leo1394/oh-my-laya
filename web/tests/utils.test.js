import test from 'node:test'
import assert from 'node:assert/strict'

import { decisionQuery, decisionTitle, displayBytes, displayTime, errorMessage, exportArtifact, itemId, normalizeList, queueNeedsRefresh, reviewSignals } from '../src/utils.js'

test('feedback SSE invalidates the pending review queue', () => {
  for (const event of ['feedback.stored', 'decision.finished', 'review.created']) assert.equal(queueNeedsRefresh(event), true)
  assert.equal(queueNeedsRefresh('settings.updated'), false)
  assert.equal(queueNeedsRefresh(''), false)
})

test('review risk prefers normalized backend risk and supports raw native answers', () => {
  assert.equal(reviewSignals({ risk: 'medium', result: { answers: { risk: { choice: 'low' } } } }).risk, 'medium')
  assert.equal(reviewSignals({ result: { answers: { risk: { choice: 'low' } } } }).risk, 'low')
  assert.deepEqual(reviewSignals({ review_reasons: ['reported_high_risk_correction', 'invalid_result', 'user_choice_changed', 'model_upgrade', 'reported_label_change'] }).reasons,
    ['Reported high-risk correction', 'Invalid result', 'User changed selection', 'Model upgrade', 'Reviewer proposed label correction'])
})

test('decision query sends validated filters before server pagination', () => {
  const params = new URLSearchParams(decisionQuery({ status: 'pending', risk: 'high', source: 'reviewer', after: '2026-10-01T00:00', before: '2026-10-02T00:00' }, 50))
  assert.equal(params.get('filter'), 'pending')
  assert.equal(params.get('risk'), 'high')
  assert.equal(params.get('source'), 'reviewer')
  assert.equal(params.get('offset'), '50')
  assert.equal(Number(params.get('created_after')), new Date('2026-10-01T00:00').getTime() / 1000)
  assert.throws(() => decisionQuery({ after: 'bad date' }), /valid/)
  assert.throws(() => decisionQuery({ after: '2026-10-02T00:00', before: '2026-10-01T00:00' }), /before/)
})

test('review signals use backend triage and retain evidence identities', () => {
  const signals = reviewSignals({ review_priority: 3, review_reasons: ['uncertain', 'problem_score'], review_trigger_count: 2, review_trigger_event_ids: ['event-1', 'event-2'], review_sources: ['reviewer'], result: { advice: { assessment: { risk: { choice: 'low' } } } } })
  assert.equal(signals.priority, 'Uncertain with problem feedback')
  assert.equal(signals.triggerCount, 2)
  assert.deepEqual(signals.eventIds, ['event-1', 'event-2'])
  assert.deepEqual(signals.sources, ['reviewer'])
  assert.equal(signals.risk, 'low')
  assert.ok(signals.reasons.includes('Problem score'))
  assert.equal(reviewSignals({}).triggerCount, 0)
})

test('displayTime interprets backend UTC epoch seconds, not milliseconds', () => {
  assert.equal(displayTime(1790812800), new Date(1790812800000).toLocaleString())
})

test('normalizeList accepts direct arrays and common API wrappers', () => {
  assert.deepEqual(normalizeList([1, 2]), [1, 2])
  assert.deepEqual(normalizeList({ items: [3] }), [3])
  assert.deepEqual(normalizeList({ decisions: [4] }), [4])
  assert.deepEqual(normalizeList(null), [])
})

test('display helpers tolerate partial backend records', () => {
  assert.equal(decisionTitle({ task_ref: 'task-4' }), 'task-4')
  assert.equal(itemId({ case_id: 'case-2' }), 'case-2')
  assert.equal(displayBytes(1536), '1.5 KiB')
  assert.equal(errorMessage({ message: 'offline' }), 'offline')
})

test('exportArtifact exposes only completed export downloads', () => {
  const artifact = exportArtifact({
    kind: 'export',
    status: 'completed',
    result: {
      artifact_id: '6dfddf7a-1630-48d4-bfb8-a6898575a73a',
      artifact_name: 'laya.ndjson',
      media_type: 'application/x-ndjson',
      record_count: 12
    }
  })
  assert.equal(artifact.href, '/api/v1/exports/6dfddf7a-1630-48d4-bfb8-a6898575a73a')
  assert.equal(artifact.recordCount, 12)
  assert.equal(exportArtifact({ kind: 'export', status: 'running' }), null)
  assert.equal(exportArtifact({ kind: 'evaluation', status: 'completed' }), null)
})
