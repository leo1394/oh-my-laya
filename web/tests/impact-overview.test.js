import test from 'node:test'
import assert from 'node:assert/strict'
import { fileURLToPath } from 'node:url'
import { createServer } from 'vite'
import { createSSRApp } from 'vue'
import { renderToString } from '@vue/server-renderer'

const scenario = (saved, overrides = {}) => ({
  status: 'available', estimator_version: 'scenario-v1', saved,
  baseline: { low: 800, central: 1000, high: 1200 }, actual_total: 500,
  included_runs: 2, excluded_runs: 1,
  assumptions: { retention: [.25, .5, 1], output_multiplier: [.8, 1, 1.2] },
  scope: 'host_only', excluded_components: ['local_laya_inference'],
  runs: [{ run_id: 'run-1', orchestrator_model: '<script>bad</script>', reasoning_effort: 'high', input_source: '<img src=x>', coverage: 'partial' }],
  ...overrides,
})

test('impact overview renders bilingual scenario estimates without inventing missing results', async () => {
  const server = await createServer({ root: fileURLToPath(new URL('..', import.meta.url)), server: { middlewareMode: true, hmr: false, ws: false }, appType: 'custom' })
  try {
    const { default: component } = await server.ssrLoadModule('/src/ImpactOverview.vue')
    const { setLanguage } = await server.ssrLoadModule('/src/i18n.js')
    setLanguage('en')
    const empty = await renderToString(createSSRApp(component, { status: null }))
    assert.match(empty, /Collecting estimation inputs/)
    assert.match(empty, /No duplicate run or comparable measured baseline is required/)
    assert.match(empty, /Partial coverage/)
    assert.match(empty, /evaluated-version count follows version creation dates/)
    assert.doesNotMatch(empty, /0%|100%/)

    const positive = await renderToString(createSSRApp(component, { status: { dashboard: { tokens: { scenario: scenario({ low: 300, central: 500, high: 700 }) } } } }))
    assert.match(positive, /Estimated tokens saved/)
    assert.match(positive, /\+500/)
    assert.match(positive, /Savings range[^]*\+300[^]*\+700/)
    assert.match(positive, /Scenario estimate/)
    assert.match(positive, /Estimated, not measured/)
    assert.match(positive, /without delegation/)
    assert.match(positive, /No rerun is required/)
    assert.match(positive, /host-model tokens only/)
    assert.match(positive, /excludes local Laya inference/)
    assert.match(positive, /Actual usage for included scenario runs/)
    assert.match(positive, /excluded recorded usage is omitted/)
    assert.match(positive, /0\.25 \/ 0\.5 \/ 1/)
    assert.doesNotMatch(positive, /<script>bad<\/script>|<img src=x>/)
    assert.match(positive, /&lt;script&gt;bad&lt;\/script&gt;/)

    const negative = await renderToString(createSSRApp(component, { status: { dashboard: { tokens: { scenario: scenario({ low: -300, central: -200, high: -100 }) } } } }))
    assert.match(negative, /Additional tokens used/)
    assert.match(negative, /<strong>200 <small>Token/)
    assert.match(negative, /Savings range[^]*-300[^]*-100/)
    assert.match(negative, /Central savings[^]*-200/)

    const crossing = await renderToString(createSSRApp(component, { status: { dashboard: { tokens: { scenario: scenario({ low: -100, central: 20, high: 150 }, { status: 'partial' }) } } } }))
    assert.match(crossing, /range crosses zero/)
    assert.match(crossing, /does not show whether this scenario saves tokens/)
    assert.match(crossing, /Partial reported coverage/)

    const zero = await renderToString(createSSRApp(component, { status: { dashboard: { tokens: { scenario: scenario({ low: 0, central: 0, high: 0 }, { actual_total: 0, included_runs: 0, excluded_runs: 0 }) } } } }))
    assert.match(zero, /<strong>0 <small>Token/)
    assert.doesNotMatch(zero, /Collecting estimation inputs/)

    const malformed = await renderToString(createSSRApp(component, { status: { dashboard: { tokens: { scenario: scenario({ low: 700, central: 500, high: 300 }) } } } }))
    assert.match(malformed, /Collecting estimation inputs/)
    assert.doesNotMatch(malformed, /\+500/)

    const missingScenarioActual = await renderToString(createSSRApp(component, { status: { dashboard: { tokens: {
      recorded_total: 999, aggregation_status: 'overflow', scenario: scenario({ low: 300, central: 500, high: 700 }, { actual_total: null }),
    } } } }))
    assert.match(missingScenarioActual, /Actual usage for included scenario runs<\/h2><strong>—/)
    assert.doesNotMatch(missingScenarioActual, /<strong>999/)
    assert.doesNotMatch(missingScenarioActual, /total unavailable/)

    setLanguage('zh')
    const chinese = await renderToString(createSSRApp(component, { status: { dashboard: { tokens: { scenario: scenario({ low: -20, central: 10, high: 40 }, { status: 'partial' }) } } } }))
    assert.match(chinese, /情景估算/)
    assert.match(chinese, /节省范围/)
    assert.match(chinese, /范围跨越零点/)
    assert.match(chinese, /仅覆盖部分已上报数据/)
    assert.match(chinese, /不包含 Laya 本地推理/)
    const chineseNegative = await renderToString(createSSRApp(component, { status: { dashboard: { tokens: { scenario: scenario({ low: -300, central: -200, high: -100 }) } } } }))
    assert.match(chineseNegative, /额外使用的 Token/)
    assert.match(chineseNegative, /节省范围[^]*-300[^]*-100/)
    assert.match(chineseNegative, /中心节省值[^]*-200/)
    const chineseMissing = await renderToString(createSSRApp(component, { status: { dashboard: { tokens: { scenario: { status: 'unavailable', saved: null, baseline: null } } } } }))
    assert.match(chineseMissing, /正在收集估算输入/)
    assert.doesNotMatch(chineseMissing, /待建立可比较的对照/)

    setLanguage('en')
    const partial = await renderToString(createSSRApp(component, { status: { dashboard: { tokens: {
      recorded_total: 20, aggregation_status: 'partial', included_attempts: 1, excluded_reports: 2,
      exclusion_reasons: { ordered_sequence_conflict: 1, legacy_reports_ambiguous: 1 },
    } } } }))
    assert.match(partial, /Some reports could not be merged/)
    assert.match(partial, /Conflicting checkpoint/)
    assert.match(partial, /Ambiguous legacy reports/)
    assert.match(partial, /not independently authenticated/)
    const overflow = await renderToString(createSSRApp(component, { status: { dashboard: { tokens: {
      recorded_total: null, aggregation_status: 'overflow', included_attempts: 2,
    } } } }))
    assert.match(overflow, /supported numeric range; total unavailable/)
    assert.match(overflow, /—/)
    const unavailable = await renderToString(createSSRApp(component, { status: { dashboard: { tokens: {
      recorded_total: null, aggregation_status: 'unavailable', included_attempts: 0,
    } } } }))
    assert.match(unavailable, /No unambiguous, mergeable usage reports yet/)
  } finally {
    await server.close()
  }
})
