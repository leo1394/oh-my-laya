import test from 'node:test'
import assert from 'node:assert/strict'
import { fileURLToPath } from 'node:url'
import { createServer } from 'vite'
import { createSSRApp } from 'vue'
import { renderToString } from '@vue/server-renderer'

test('efficiency evidence stays bilingual, scoped, and distinct from token estimates', async () => {
  const server = await createServer({ root: fileURLToPath(new URL('..', import.meta.url)), server: { middlewareMode: true, ws: false }, appType: 'custom' })
  try {
    const { default: component } = await server.ssrLoadModule('/src/ImpactOverview.vue')
    const { setLanguage } = await server.ssrLoadModule('/src/i18n.js')
    const evidence = {
      contract: 'efficiency_summary_v1', scope: 'recorded_attempts', complete_task_coverage: false,
      observed_attempts: 5, reported_runs: 2, versioned_attempts: 4, identity_conflicts: 0,
      delegated_attempts: 3, repair_attempts: 1, upgrade_attempts: 1,
      initial_scored_attempts: 3, flagged_attempts: 2, usage_covered_attempts: 2, usage_missing_attempts: 3,
      reported_outcomes: { success: 1, failure: 1, partial: 1, cancelled: 1, unknown: 1 },
    }
    const render = (efficiency) => renderToString(createSSRApp(component, { status: { dashboard: { efficiency } } }))
    setLanguage('en')
    const english = await render(evidence)
    assert.match(english, /recorded attempts/)
    assert.match(english, /<b>2<\/b> reported runs in this date range · not complete task coverage/)
    assert.match(english, /repair attempts/)
    assert.match(english, /upgrade attempts/)
    assert.match(english, /Usable usage evidence: 2 \/ 5/)
    assert.match(english, /1 failed/)
    assert.match(english, /1 partial/)
    assert.match(english, /1 cancelled/)
    assert.match(english, /missing agents or runs are not counted as zero/)
    assert.match(english, /not complete task coverage/)
    assert.match(english, /Collecting estimation inputs/)
    assert.doesNotMatch(await render(undefined), /Execution evidence/)
    setLanguage('zh')
    const chinese = await render(evidence)
    assert.match(chinese, /次修复尝试/)
    assert.match(chinese, /<b>2<\/b> 个当前时间范围内已上报运行 · 不代表完整任务覆盖/)
    assert.match(chinese, /案例学习/)
    assert.match(chinese, /有首次评分/)
    assert.match(chinese, /可用用量证据: 2 \/ 5/)
    assert.match(chinese, /不代表完整任务覆盖/)
    for (const invalid of [
      { ...evidence, complete_task_coverage: true },
      { ...evidence, reported_runs: undefined },
      { ...evidence, reported_runs: 6 },
      { ...evidence, usage_covered_attempts: 9 },
      { ...evidence, flagged_attempts: -1 },
      { ...evidence, reported_outcomes: {} },
    ]) assert.doesNotMatch(await render(invalid), /执行证据/)
  } finally {
    await server.close()
  }
})
