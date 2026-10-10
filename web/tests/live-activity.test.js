import test from 'node:test'
import assert from 'node:assert/strict'
import { fileURLToPath } from 'node:url'
import { createServer } from 'vite'
import vue from '@vitejs/plugin-vue'
import { createSSRApp } from 'vue'
import { renderToString } from '@vue/server-renderer'
import { activityBounds, activityCharts, activityCurve, activityLogTime, activityPair, activityPath, activityRecommendation, activityRequestGate, activityTime, activityWorker, localDayBounds, pausedSnapshotSafe } from '../src/liveActivity.js'
import { setLanguage } from '../src/i18n.js'

test('log search uses bounded server pagination and safely encodes literal queries', () => {
  const path = activityPath({ start: 0, end: 100 }, { offset: 10, search: '案例 & 100%' })
  const query = new URL(path, 'http://localhost').searchParams
  assert.equal(query.get('limit'), '10')
  assert.equal(query.get('offset'), '10')
  assert.equal(query.get('q'), '案例 & 100%')
})

test('activity requests exact local calendar day bounds, including daylight saving days', () => {
  const previous = process.env.TZ
  try {
    process.env.TZ = 'America/New_York'
    const spring = localDayBounds(new Date('2026-03-08T16:00:00Z'))
    const fall = localDayBounds(new Date('2026-11-01T16:00:00Z'))
    assert.equal(spring.end - spring.start, 23 * 3600)
    assert.equal(fall.end - fall.start, 25 * 3600)
    assert.equal(activityPath(spring), `/activity?created_after=${spring.start}&created_before=${spring.end}&limit=50`)
  } finally {
    if (previous === undefined) delete process.env.TZ
    else process.env.TZ = previous
  }
})

test('activity converts selected inclusive dates to exclusive request bounds', () => {
  const selected = { valid: true, created_after: 100, created_before: 200 }
  assert.deepEqual(activityBounds(selected), { start: 100, end: 201 })
  assert.equal(activityPath(activityBounds(selected)), '/activity?created_after=100&created_before=201&limit=50')
  assert.equal(activityBounds({ ...selected, valid: false }), null)
  assert.equal(activityBounds({ ...selected, created_before: 99 }), null)
})

test('activity chart folds local 15-minute buckets into hourly bands and stops at the current clock time', () => {
  const previous = process.env.TZ
  try {
    process.env.TZ = 'UTC'
    const start = localDayBounds(new Date('2026-01-03T12:00:00Z')).start
    const snapshot = { start, end: start + 86400, buckets: [
      { start, count: 2 }, { start: start + 900, count: 1 },
      { start: start + 3600, count: 0 }, { start: start + 5400, count: 4 },
      { start: start + 7200, count: 9 },
    ] }
    const chart = activityCharts(snapshot, start + 6300)
    assert.equal(chart.total, 7)
    assert.deepEqual(chart.hours.slice(0, 3).map(hour => hour.count), [3, 4, 0])
    assert.deepEqual(chart.bars.map(bar => [bar.x, bar.count]), [[2, 3], [27, 4]])
    assert.deepEqual(chart.points, [{ x: 0, total: 0 }, { x: 25, total: 3 }, { x: 43.75, total: 7 }])
    assert.equal(chart.cutoffHour, 1.75)
    assert.equal(chart.countMaximum, 4)
    assert.equal(chart.cumulativeMaximum, 8)
    assert.equal((chart.curve.match(/ C /g) || []).length, 2)
    assert.equal(activityCharts({ start, end: start + 86400, buckets: [] }, start + 6300).hasData, false)
  } finally {
    if (previous === undefined) delete process.env.TZ
    else process.env.TZ = previous
  }
})

test('activity chart retains both repeated local hours and leaves a skipped hour empty', () => {
  const previous = process.env.TZ
  try {
    process.env.TZ = 'America/New_York'
    const fall = localDayBounds(new Date('2026-11-01T16:00:00Z'))
    const fallChart = activityCharts({ ...fall, buckets: [{ start: fall.start + 3600, count: 2 }, { start: fall.start + 7200, count: 3 }] }, fall.end)
    assert.equal(fallChart.hours[1].count, 5)
    assert.equal(fallChart.total, 5)
    const spring = localDayBounds(new Date('2026-03-08T16:00:00Z'))
    const springChart = activityCharts({ ...spring, buckets: [{ start: spring.start + 3600, count: 2 }, { start: spring.start + 7200, count: 3 }] }, spring.end)
    assert.equal(springChart.hours[2].count, 0)
    assert.equal(springChart.hours[3].count, 3)
    assert.equal(springChart.total, 5)
  } finally {
    if (previous === undefined) delete process.env.TZ
    else process.env.TZ = previous
  }
})

test('multi-date chart aggregates ordered buckets by local date across selected range', () => {
  const previous = process.env.TZ
  try {
    process.env.TZ = 'UTC'
    const start = Date.parse('2026-01-01T00:00:00Z') / 1000
    const end = start + 3 * 86400
    const chart = activityCharts({ start, end, buckets: [
      { start, end: start + 900, count: 2 },
      { start: start + 900, end: start + 1800, count: 3 },
      { start: start + 86400, end: start + 2 * 86400, count: 4 },
      { start: start + 2 * 86400, end, count: 0 },
    ] }, end)
    assert.equal(chart.singleDay, false)
    assert.equal(chart.total, 9)
    assert.deepEqual(chart.bars.map(bar => bar.count), [5, 4, 0])
    assert.deepEqual(chart.points.map(point => point.total), [0, 5, 9, 9])
    assert.equal(chart.ticks[0].label, '2026-01-01')
    assert.equal(chart.ticks.at(-1).label, '2026-01-03')
    assert.equal((chart.curve.match(/ C /g) || []).length, 3)
    const empty = activityCharts({ start, end, buckets: [] }, end)
    assert.equal(empty.hasData, false)
    assert.deepEqual(empty.points, [{ x: 0, total: 0 }])
  } finally {
    if (previous === undefined) delete process.env.TZ
    else process.env.TZ = previous
  }
})

test('fine buckets after New York spring DST midnight stay on March 9', () => {
  const previous = process.env.TZ
  try {
    process.env.TZ = 'America/New_York'
    const start = new Date(2026, 2, 8).getTime() / 1000
    const end = new Date(2026, 2, 10).getTime() / 1000
    const before = new Date(2026, 2, 8, 23, 45).getTime() / 1000
    const after = new Date(2026, 2, 9, 0, 30).getTime() / 1000
    const chart = activityCharts({ start, end, bucket_seconds: 900, buckets: [
      { start: before, end: before + 900, count: 2 },
      { start: after, end: after + 900, count: 3 },
    ] }, end)
    assert.deepEqual(chart.bars.map(bar => [bar.label, bar.count]), [['2026-03-08', 2], ['2026-03-09', 3]])
    assert.equal(chart.total, 5)
  } finally {
    if (previous === undefined) delete process.env.TZ
    else process.env.TZ = previous
  }
})

test('coarse buckets retain their full date intervals', () => {
  const previous = process.env.TZ
  try {
    process.env.TZ = 'UTC'
    const start = Date.parse('2026-01-01T00:00:00Z') / 1000
    const end = start + 6 * 86400
    const chart = activityCharts({ start, end, bucket_seconds: 3 * 86400, buckets: [
      { start, end: start + 3 * 86400, count: 5 },
      { start: start + 3 * 86400, end, count: 7 },
    ] }, end)
    assert.equal(chart.coarse, true)
    assert.deepEqual(chart.bars.map(bar => bar.label), ['2026-01-01–2026-01-03', '2026-01-04–2026-01-06'])
    assert.equal(chart.total, 12)
  } finally {
    if (previous === undefined) delete process.env.TZ
    else process.env.TZ = previous
  }
})

test('smooth cumulative curve preserves endpoints and cannot overshoot its counts', () => {
  assert.equal(activityCurve([], 10), '')
  const points = [{ x: 0, total: 0 }, { x: 25, total: 0 }, { x: 50, total: 5 }, { x: 75, total: 10 }]
  const segments = activityCurve(points, 10).split(' C ')
  assert.equal(segments[0], 'M 0,120')
  for (let index = 1; index < segments.length; index++) {
    const [first, second, end] = segments[index].split(' ').map(pair => pair.split(',').map(Number))
    const startY = 120 - points[index - 1].total * 12
    assert.equal(first[1], startY)
    assert.equal(second[1], end[1])
    assert.equal(end[0], points[index].x)
    assert.equal(end[1], 120 - points[index].total * 12)
    assert.ok(first[0] >= points[index - 1].x && second[0] <= end[0])
    assert.ok(startY >= end[1])
  }
})

test('worker indicator follows fetch and live process state, not historical failures', () => {
  const snapshot = { worker: { pid: 123, busy: false, queued: 0 }, totals: { failed: 20 } }
  assert.equal(activityWorker(snapshot, 'live', ''), 'idle')
  assert.equal(activityWorker({ worker: { pid: 123, busy: true, queued: 0 } }, 'live', ''), 'busy')
  assert.equal(activityWorker({ worker: { pid: 123, busy: false, queued: 2 } }, 'live', ''), 'busy')
  assert.equal(activityWorker({ worker: { pid: 0, busy: false, queued: 0 } }, 'live', ''), 'unloaded')
  assert.equal(activityWorker(snapshot, 'connecting', ''), 'unloaded')
  assert.equal(activityWorker(snapshot, 'live', 'fetch failed'), 'error')
})

test('recommendation badge shows only recorded model and effort', () => {
  assert.equal(activityRecommendation({ model: 'gpt-6.1-sol', reasoning_effort: 'medium' }), 'gpt-6.1-sol Medium')
  assert.equal(activityRecommendation({ model: 'gpt-6.1-sol', reasoning_effort: null }), 'gpt-6.1-sol')
  assert.equal(activityRecommendation(null), '')
})

test('log time uses local clock and only recorded fractional precision', () => {
  const previous = process.env.TZ
  try {
    process.env.TZ = 'Asia/Shanghai'
    assert.equal(activityLogTime(1767427200), '2026-01-03 16:00:00')
    assert.equal(activityLogTime(1767427200, null), '2026-01-03 16:00:00')
    assert.equal(activityLogTime(1767427200, 1767427200823), '2026-01-03 16:00:00.823')
    assert.equal(activityLogTime(1767427200, 1767427200000), '2026-01-03 16:00:00.000')
    assert.equal(activityLogTime(1767427200.25), '2026-01-03 16:00:00.25')
    assert.equal(activityLogTime('2026-01-03T08:00:00.123456Z'), '2026-01-03 16:00:00.123456')
    assert.equal(activityLogTime(null), 'Time not recorded')
  } finally {
    if (previous === undefined) delete process.env.TZ
    else process.env.TZ = previous
  }
})

test('paused snapshot follows privacy revision across ordinary updates and deletion', () => {
  const old = { privacy_revision: 'r1', service_instance: 's1', recording_enabled: true, items: [{ id: 'a', summary: 'private', status: 'pending' }], totals: { decisions: 1, completed: 0, failed: 0, pending: 1 }, buckets: [{ start: 0, count: 1 }] }
  assert.equal(pausedSnapshotSafe(old, { ...old, items: [{ id: 'b', summary: 'new' }, ...old.items], totals: { ...old.totals, decisions: 2 }, buckets: [{ start: 0, count: 2 }] }), true)
  assert.equal(pausedSnapshotSafe(old, { ...old, items: [{ ...old.items[0], status: 'completed', assignments: [{ role: 'worker' }] }], totals: { ...old.totals, completed: 1, pending: 0 } }), true)
  assert.equal(pausedSnapshotSafe(old, { ...old, privacy_revision: 'r2' }), false)
  assert.equal(pausedSnapshotSafe(old, { ...old, service_instance: 's2' }), false)
  assert.equal(pausedSnapshotSafe(old, { ...old, recording_enabled: false }), false)
  assert.equal(pausedSnapshotSafe(old, { ...old, privacy_revision: undefined }), false)
})

test('activity request gate aborts and rejects stale requests on refresh and reset', () => {
  const gate = activityRequestGate()
  const first = gate.start()
  assert.equal(gate.current(first), true)
  const second = gate.start()
  assert.equal(first.signal.aborted, true)
  assert.equal(gate.current(first), false)
  assert.equal(gate.current(second), true)
  gate.cancel()
  assert.equal(second.signal.aborted, true)
  assert.equal(gate.current(second), false)
})

test('activity component starts with compact bilingual loading controls', async () => {
  const server = await createServer({ configFile: false, plugins: [vue()], root: fileURLToPath(new URL('..', import.meta.url)), server: { middlewareMode: true, hmr: false, ws: false }, appType: 'custom' })
  try {
    const { default: component } = await server.ssrLoadModule('/src/LiveActivity.vue')
    const { setLanguage: setServerLanguage } = await server.ssrLoadModule('/src/i18n.js')
    setServerLanguage('en')
    const range = { valid: true, created_after: 100, created_before: 200 }
    const english = await renderToString(createSSRApp(component, { eventVersion: 0, eventState: 'connecting', resetVersion: 0, range }))
    assert.match(english, /Recorded activity/)
    assert.match(english, /Pause display/)
    assert.match(english, /Retry \/ refresh/)
    assert.doesNotMatch(english, /Charts: today in your local time/)
    assert.match(english, /Loading recorded activity/)
    assert.doesNotMatch(english, /Updates connected/)
    setServerLanguage('zh')
    const chinese = await renderToString(createSSRApp(component, { eventVersion: 0, eventState: 'connecting', resetVersion: 0, range }))
    assert.match(chinese, /已记录活动/)
    assert.match(chinese, /暂停显示/)
    const invalid = await renderToString(createSSRApp(component, { eventVersion: 0, eventState: 'connecting', resetVersion: 0, range: { valid: false } }))
    assert.match(invalid, /请选择有效日期范围/)
    assert.doesNotMatch(invalid, /正在加载已记录活动/)
  } finally {
    await server.close()
  }
  setLanguage('en')
  assert.equal(activityTime(null), 'Time not recorded')
  assert.match(activityTime(1), /:\d{2}:\d{2}/)
  assert.equal(activityPair({ model: 'observed', reasoning_effort: null }), 'observed / Not recorded')
  assert.equal(activityPair(null), 'Not verified')
  setLanguage('zh')
  assert.equal(activityPair({ model: 'observed', reasoning_effort: null }), 'observed / 未记录')
})
