import test from 'node:test'
import assert from 'node:assert/strict'

import { dateRangeBounds, decisionQuery, decisionTitle, displayBytes, displayTime, errorMessage, exportArtifact, itemId, localDateKey, normalizeList, presetDateRange, queueNeedsRefresh, rangeQuery, refreshPresetRange, reviewScope, reviewSignals } from '../src/utils.js'

test('review scope retains provenance without manufacturing lineage for legacy decisions', () => {
  assert.deepEqual(reviewScope({ id: 'not-provenance', request: { state: '修复文档' } }), {
    task_family: 'general', task_lineage: '', language: 'zh'
  })
  const decision = { request: { state: 'Fix README', advisor: { task_family: 'documentation', task_lineage: 'issue:42' } } }
  assert.deepEqual(reviewScope(decision), { task_family: 'documentation', task_lineage: 'issue:42', language: 'en' })
  assert.deepEqual(reviewScope(decision, { task_family: 'migration', task_lineage: null, language: 'zh' }), {
    task_family: 'migration', task_lineage: '', language: 'zh'
  })
  assert.equal(reviewScope(decision, { task_lineage: 'issue:43' }).task_lineage, 'issue:43')
  assert.equal(reviewScope({ request: { state: 'Fix README', advisor: { language: 'zh' } } }).language, 'en')
})

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
  const bounds = dateRangeBounds({ start: '2026-10-01', end: '2026-10-02' })
  const params = new URLSearchParams(decisionQuery({ status: 'pending', risk: 'high', source: 'reviewer' }, 50, bounds))
  assert.equal(params.get('filter'), 'pending')
  assert.equal(params.get('risk'), 'high')
  assert.equal(params.get('source'), 'reviewer')
  assert.equal(params.get('offset'), '50')
  assert.equal(Number(params.get('created_after')), bounds.created_after)
  assert.equal(Number(params.get('created_before')), bounds.created_before)
})

test('date presets include today and use local calendar subtraction', () => {
  const now = new Date(2026, 9, 5, 17, 30)
  assert.deepEqual(presetDateRange('today', now), { preset: 'today', start: '2026-10-05', end: '2026-10-05' })
  assert.deepEqual(presetDateRange('last7', now), { preset: 'last7', start: '2026-09-29', end: '2026-10-05' })
  assert.deepEqual(presetDateRange('last30', now), { preset: 'last30', start: '2026-09-06', end: '2026-10-05' })
  assert.equal(localDateKey(new Date(2026, 0, 2)), '2026-01-02')
})

test('preset ranges rebase after midnight while custom dates remain untouched', () => {
  const previous = presetDateRange('last7', new Date(2026, 9, 5, 23, 59))
  assert.deepEqual(refreshPresetRange(previous, new Date(2026, 9, 6, 0, 1)), { preset: 'last7', start: '2026-09-30', end: '2026-10-06' })
  const current = presetDateRange('last7', new Date(2026, 9, 5, 12))
  assert.equal(refreshPresetRange(current, new Date(2026, 9, 5, 18)), current)
  const custom = { preset: 'custom', start: '2025-01-01', end: '2025-02-01' }
  assert.equal(refreshPresetRange(custom, new Date(2026, 9, 6)), custom)
})

test('inclusive date bounds use local midnights across DST instead of 86400 arithmetic', () => {
  const previous = process.env.TZ
  process.env.TZ = 'America/New_York'
  try {
    const spring = dateRangeBounds({ start: '2026-03-08', end: '2026-03-08' })
    const fall = dateRangeBounds({ start: '2026-11-01', end: '2026-11-01' })
    assert.equal(spring.created_before - spring.created_after + 1, 23 * 60 * 60)
    assert.equal(fall.created_before - fall.created_after + 1, 25 * 60 * 60)
    const query = new URLSearchParams(rangeQuery(spring))
    assert.equal(Number(query.get('created_after')), spring.created_after)
    assert.equal(Number(query.get('created_before')), spring.created_before)
  } finally {
    if (previous == null) delete process.env.TZ
    else process.env.TZ = previous
  }
})

test('invalid or reversed custom ranges cannot produce server queries', () => {
  for (const range of [{ start: '', end: '2026-10-05' }, { start: '2026-02-30', end: '2026-03-01' }, { start: '2026-10-06', end: '2026-10-05' }]) {
    const bounds = dateRangeBounds(range)
    assert.equal(bounds.valid, false)
    assert.throws(() => rangeQuery(bounds), /valid|before/)
  }
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
