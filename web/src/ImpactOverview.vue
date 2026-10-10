<script setup>
import { computed, ref } from 'vue'
import { t, locale } from './i18n.js'

const props = defineProps({ status: Object, hero: Boolean, view: { type: String, default: 'all' } })
defineEmits(['review', 'cases', 'usage-range'])
const usageOpen = ref(false)
const estimateOpen = ref(false)
const dashboard = computed(() => props.status?.dashboard)
const tokens = computed(() => dashboard.value?.tokens)
const localInference = computed(() => tokens.value?.local_inference)
const hasRecordedTotal = computed(() => tokens.value && Object.hasOwn(tokens.value, 'recorded_total'))
const learning = computed(() => dashboard.value?.learning)
const number = (value) => value == null ? '—' : Number(value).toLocaleString(locale.value)
const finiteNumber = (value) => typeof value === 'number' && Number.isFinite(value)
const efficiency = computed(() => {
  const value = dashboard.value?.efficiency
  const counts = ['observed_attempts', 'reported_runs', 'versioned_attempts', 'identity_conflicts', 'delegated_attempts', 'repair_attempts', 'upgrade_attempts', 'initial_scored_attempts', 'flagged_attempts', 'usage_covered_attempts', 'usage_missing_attempts']
  if (value?.contract !== 'efficiency_summary_v1' || value.scope !== 'recorded_attempts' || value.complete_task_coverage !== false) return null
  if (!counts.every(key => Number.isSafeInteger(value[key]) && value[key] >= 0)) return null
  if (!counts.every(key => value[key] <= value.observed_attempts)) return null
  if (value.usage_covered_attempts + value.usage_missing_attempts !== value.observed_attempts) return null
  const outcomes = value.reported_outcomes
  if (!outcomes || !Object.keys(outcomes).every(key => ['success', 'failure', 'partial', 'cancelled', 'unknown'].includes(key))) return null
  if (!Object.values(outcomes).every(count => Number.isSafeInteger(count) && count >= 0)) return null
  if (Object.values(outcomes).reduce((total, count) => total + count, 0) !== value.observed_attempts) return null
  return value
})
const validRange = (value) => value && finiteNumber(value.low) && finiteNumber(value.central) && finiteNumber(value.high) && value.low <= value.central && value.central <= value.high
const scenario = computed(() => {
  const value = tokens.value?.scenario
  if (!value || !['available', 'partial'].includes(value.status) || value.estimator_version !== 'scenario-v1') return null
  if (!validRange(value.saved) || !validRange(value.baseline) || value.baseline.low < 0) return null
  if (!finiteNumber(value.actual_total) || value.actual_total < 0) return null
  if (hasRecordedTotal.value && (tokens.value.aggregation_status === 'overflow' || tokens.value.recorded_total !== value.actual_total)) return null
  return value
})
const signedNumber = (value) => {
  if (!finiteNumber(value)) return '—'
  if (value > 0) return `+${number(value)}`
  return number(value)
}
const actualTotal = computed(() => {
  if (!scenario.value) return null
  if (hasRecordedTotal.value) return tokens.value.aggregation_status !== 'overflow' && finiteNumber(tokens.value.recorded_total) && tokens.value.recorded_total >= 0 ? tokens.value.recorded_total : null
  return finiteNumber(scenario.value.actual_total) && scenario.value.actual_total >= 0 ? scenario.value.actual_total : null
})
const includedCount = computed(() => hasRecordedTotal.value ? tokens.value.included_attempts : scenario.value?.included_runs)
const excludedCount = computed(() => hasRecordedTotal.value ? tokens.value.excluded_reports : scenario.value?.excluded_runs)
const assumptionValues = (values) => Array.isArray(values) && values.every(finiteNumber) ? values.map(number).join(' / ') : '—'
const usageStatus = computed(() => {
  if (scenario.value && !hasRecordedTotal.value) return null
  if (tokens.value?.aggregation_status === 'overflow') return t('Usage exceeds the supported numeric range; total unavailable.', '用量超出支持的数值范围，合计不可用。')
  if (tokens.value?.aggregation_status === 'unavailable') return t('No unambiguous, mergeable usage reports yet.', '暂无明确且可合并的用量报告。')
  if (tokens.value?.aggregation_status === 'partial') return t('Some reports could not be merged; the total is incomplete.', '部分报告无法合并，当前合计不完整。')
  return null
})
const reasonLabel = (reason) => ({
  unverified_source: t('Source not reported as verified', '来源未声明已核实'),
  ineligible_scope: t('Non-attempt scope', '非独立执行统计范围'),
  overlap_not_non_overlapping: t('Overlapping or unknown coverage', '范围重叠或不明确'),
  unknown_total_tokens: t('Unknown total', '用量未知'),
  ordered_sequence_conflict: t('Conflicting checkpoint', '检查点冲突'),
  multiple_ordered_streams: t('Multiple usage streams', '多条用量流'),
  mixed_ordered_and_legacy: t('Mixed ordered and legacy reports', '有序与旧式报告混用'),
  usage_stream_reused: t('Usage stream assigned to multiple attempts', '同一用量流关联了多个执行'),
  legacy_reports_ambiguous: t('Ambiguous legacy reports', '旧报告顺序不明确'),
}[reason] || reason)
</script>

<template>
  <section class="impact-overview" :class="{ 'impact-hero': hero }" :aria-label="t('Token efficiency and learning', 'Token 效率与学习')">
    <div v-if="view !== 'learning'" class="token-overview">
      <article class="token-actual">
        <h2>{{ scenario && !hasRecordedTotal ? t('Actual usage for included scenario runs', '已包含情景执行的实际用量') : t('Recorded actual usage', '已记录的实际消耗') }}</h2>
        <strong>{{ number(actualTotal) }} <small>Token</small><button class="token-toggle" :aria-label="t('Usage details', '用量详情')" :aria-expanded="usageOpen" @click="usageOpen = !usageOpen"><span aria-hidden="true">▸</span></button></strong>
        <p v-if="localInference" class="local-usage"><b>{{ t('Local Laya decisions', 'Laya 本地决策推理') }} · {{ number(localInference.total_tokens) }} Token</b><small>{{ t('Separate from host execution; not added to savings.', '与主代理及子代理执行分开计量，不混入节省估算。') }}</small><small v-if="localInference.missing_decisions">{{ number(localInference.missing_decisions) }} {{ t('decisions have unavailable inference usage.', '条决策的推理用量不可用。') }}</small></p>
        <p v-if="actualTotal == null" class="token-note" role="status">{{ finiteNumber(tokens?.recorded_total) ? t('Waiting for an estimate covering the same usage checkpoint; both cards update together.', '正在等待覆盖同一用量检查点的估算，两张卡片将同步更新。') : usageStatus || t('No usable usage reports in the selected date range.', '所选日期内暂无可用的 Token 用量报告。') }} {{ t('Decision counts are not token counts.', '决策次数不等于 Token 用量。') }}</p>
        <button v-if="hero && actualTotal == null" class="mini" @click="$emit('usage-range')">{{ t('View last 7 days', '查看最近 7 天') }}</button>
        <details class="token-details" :open="usageOpen"><summary hidden>{{ t('Usage details', '用量详情') }}</summary>
        <p>{{ t('Host-model tokens · excludes local Laya inference', '主机模型 Token · 不包含 Laya 本地推理') }}</p>
        <p v-if="finiteNumber(tokens?.recorded_total)">{{ t('Collected actual usage (including records awaiting estimates)', '已采集实际用量（包含等待估算的记录）') }}: {{ number(tokens.recorded_total) }} Token</p>
        <p v-if="scenario?.status === 'partial'">{{ t('Partial reported coverage · not a whole-session total', '仅覆盖部分已上报数据 · 不代表整个会话总量') }}</p>
        <p v-else-if="scenario">{{ t('Reported scenario coverage · host-model scope', '已上报的情景覆盖 · 主机模型范围') }}</p>
        <p v-else>{{ t('Partial coverage · not a whole-session total', '部分覆盖 · 不代表整个会话总量') }}</p>
        <p v-if="usageStatus" role="status">{{ usageStatus }}</p>
        <div><p v-if="scenario && !hasRecordedTotal">{{ t('Only included scenario runs contribute to this actual; excluded recorded usage is omitted.', '该实际用量仅计入已包含的情景执行；已排除的记录用量不在其中。') }}</p><p v-else>{{ t('Includes all mergeable host execution reports in the selected dates, even when they lack estimation inputs.', '统计所选日期内全部可合并的主代理及子代理用量报告，不因缺少估算输入而排除。') }}</p><p>{{ number(includedCount) }} {{ t('included run reports', '个已包含执行报告') }} · {{ number(excludedCount) }} {{ t('excluded run reports', '个已排除执行报告') }}</p><p>{{ t('Merge only attempt totals reported as source-verified and non-overlapping. Ordered cumulative streams use source sequence, not delivery order. Conflicting streams, ambiguous legacy reports and inclusive task totals are excluded. Source verification is reported by the sender, not independently authenticated.', '仅合并上报为来源已核实、范围不重叠的执行用量。有序累计报告按来源序号选取，不按送达顺序；冲突用量流、顺序不明的旧报告及包含子代理的任务总量不合并。来源核实是发送方声明，不是独立认证。') }}</p><p v-for="(count, reason) in tokens?.exclusion_reasons || {}" :key="reason">{{ reasonLabel(reason) }}：{{ number(count) }}</p></div>
        </details>
      </article>
      <article class="token-estimate">
        <h2>{{ scenario?.saved.central < 0 ? t('Additional tokens used', '额外使用的 Token') : t('Estimated tokens saved', '预计节省 Token') }}</h2>
        <strong>{{ scenario ? (scenario.saved.central < 0 ? number(Math.abs(scenario.saved.central)) : signedNumber(scenario.saved.central)) : '—' }} <small>Token</small><button class="token-toggle" :aria-label="t('Estimate details', '估算详情')" :aria-expanded="estimateOpen" @click="estimateOpen = !estimateOpen"><span aria-hidden="true">▸</span></button></strong>
        <p v-if="scenario?.saved.central < 0" class="token-note">{{ t('Actual usage exceeds the hypothetical unsplit baseline.', '实际用量高于假设的不拆分基准。') }} {{ number(scenario.actual_total) }} − {{ number(scenario.baseline.central) }} = {{ number(Math.abs(scenario.saved.central)) }} Token。{{ t('Estimated, not measured savings.', '为估算差值，非实测节省。') }}</p>
        <p v-if="scenario && hasRecordedTotal && actualTotal !== scenario.actual_total" class="token-note">{{ t('Estimate covers only runs with usable inputs:', '估算仅覆盖具备可用输入的执行：') }} {{ number(scenario.actual_total) }} Token · {{ t('not the full actual total shown on the left.', '不是左侧全部实际用量。') }}</p>
        <p v-else-if="!scenario" class="token-note">{{ t('This date range lacks usable usage or unsplit-estimate inputs; savings are unknown, not zero.', '当前日期范围缺少可用用量或不拆分估算输入；节省量未知，不是零。') }}</p>
        <details class="token-details" :open="estimateOpen"><summary hidden>{{ t('Estimate details', '估算详情') }}</summary>
        <p>{{ t('Scenario estimate', '情景估算') }}</p>
        <template v-if="scenario">
          <p>{{ t('Savings range', '节省范围') }}: {{ signedNumber(scenario.saved.low) }} {{ t('to', '至') }} {{ signedNumber(scenario.saved.high) }} {{ t('host-model tokens', '主机模型 Token') }}</p>
          <p v-if="scenario.saved.low <= 0 && scenario.saved.high >= 0" role="status">{{ t('The range crosses zero; available evidence does not show whether this scenario saves tokens.', '范围跨越零点；现有证据无法判断该情景是否节省 Token。') }}</p>
          <p v-if="scenario.status === 'partial'" role="status">{{ t('Partial reported coverage', '仅覆盖部分已上报数据') }}</p>
          <small>{{ t('Estimated, not measured · host-model tokens only.', '估算值，非实测值 · 仅主机模型 Token。') }}</small>
          <div>
            <p v-if="scenario.saved.central < 0">{{ t('Central savings', '中心节省值') }}: {{ signedNumber(scenario.saved.central) }} {{ t('host-model tokens', '主机模型 Token') }}</p>
            <p>{{ t('Assumes the current orchestrator model performs the same requested work and acceptance checks without delegation. No rerun is required.', '假设当前编排模型不进行委派，完成相同的请求工作和验收检查。无需重新运行任务。') }}</p>
            <p v-if="scenario.runs?.some(run => run.input_source?.includes('lifecycle-snapshot-v1'))">{{ t('Automatic estimate assumes the same number of model responses without delegation, using observed input context ranges and output usage. This is a rough counterfactual, not measured savings or a model-price comparison.', '自动估算假设不拆分时仍需相同的模型处理轮次，结合已观测输入上下文范围与输出用量计算。这是粗略假设对照，不是实测节省，也不是模型价格对比。') }}</p>
            <p>{{ t('Estimator', '估算器') }}: {{ scenario.estimator_version }} · {{ t('Scope', '范围') }}: {{ t('host-model tokens only', '仅主机模型 Token') }}</p>
            <p>{{ t('Hypothetical unsplit baseline', '假设的不拆分对照') }}: {{ number(scenario.baseline.central) }} ({{ number(scenario.baseline.low) }} {{ t('to', '至') }} {{ number(scenario.baseline.high) }}) Token</p>
            <p>{{ t('Local Laya inference is excluded; this is not total cross-model compute or cost.', '不包含 Laya 本地推理；该指标不是跨模型总计算量或成本。') }}</p>
            <p>{{ t('Retention scenarios', '上下文保留情景') }}: {{ assumptionValues(scenario.assumptions?.retention) }} · {{ t('Output multipliers', '输出倍数') }}: {{ assumptionValues(scenario.assumptions?.output_multiplier) }}</p>
            <p>{{ t('Included runs', '已包含执行') }}: {{ number(scenario.included_runs) }} · {{ t('Excluded runs', '已排除执行') }}: {{ number(scenario.excluded_runs) }}</p>
            <p v-for="run in (scenario.runs || [])" :key="run.run_id">{{ run.orchestrator_model || '—' }} / {{ run.reasoning_effort || '—' }} · {{ run.input_source || '—' }} · {{ run.coverage || '—' }}</p>
          </div>
        </template>
        <template v-else>
          <p>{{ t('Collecting estimation inputs', '正在收集估算输入') }}</p>
          <small>{{ t('No duplicate run or comparable measured baseline is required.', '无需重复运行任务，也无需可比较的实测对照。') }}</small>
        </template>
        </details>
      </article>
    </div>
    <div v-if="view !== 'tokens'" class="learning-overview">
      <article>
        <h3>{{ t('How work was assigned', '任务分配到了哪里') }}</h3>
        <template v-if="efficiency">
          <p><b>{{ number(efficiency.reported_runs) }}</b> {{ t('reported runs in this date range · not complete task coverage', '个当前时间范围内已上报运行 · 不代表完整任务覆盖') }}</p>
          <p><b>{{ number(efficiency.observed_attempts) }}</b> {{ t('recorded attempts', '次已记录执行') }} · <b>{{ number(efficiency.delegated_attempts) }}</b> {{ t('delegated', '次委派') }}</p>
          <small>{{ t('Only recorded attempts in this date range; missing agents or runs are not counted as zero.', '仅统计当前时间范围的已记录执行；未采集的代理或任务不按零计算。') }}</small>
        </template>
        <p v-if="!dashboard?.execution_models?.length">{{ t('No verified execution distribution yet.', '暂无执行模型分布记录。') }}</p>
        <div v-for="(item, index) in (dashboard?.execution_models || []).slice(0, 2)" :key="index" class="model-count"><span>{{ item.model || t('Unverified model', '未核实模型') }} / {{ item.reasoning_effort || '—' }}</span><b>{{ number(item.attempts) }} {{ t('attempts', '次执行') }}</b></div>
        <details><summary>{{ t('Distribution evidence', '分配证据') }}</summary><p v-if="efficiency">{{ number(efficiency.repair_attempts) }} {{ t('repair attempts', '次修复尝试') }} · {{ number(efficiency.upgrade_attempts) }} {{ t('upgrade attempts', '次升级尝试') }}</p><div v-for="(item, index) in (dashboard?.execution_models || []).slice(2, 5)" :key="index" class="model-count"><span>{{ item.model || t('Unverified model', '未核实模型') }} / {{ item.reasoning_effort || '—' }}</span><b>{{ number(item.attempts) }} {{ t('attempts', '次执行') }}</b></div><small>{{ t('Top 5 recorded combinations; distribution alone does not prove better task decomposition or quality.', '已记录搭配前 5 项；分配次数本身不能证明拆分更合理或质量更高。') }}</small></details>
      </article>
      <article>
        <h3>{{ t('Turn uncertainty into learning cases', '把不明确判断沉淀为学习案例') }}</h3>
        <template v-if="efficiency">
          <p>{{ number(efficiency.initial_scored_attempts) }} {{ t('with initial scores', '次执行有首次评分') }} · {{ number(efficiency.flagged_attempts) }} {{ t('with review signals', '次执行存在复核信号') }}</p>
          <small>{{ t('Usable usage evidence', '可用用量证据') }}: {{ number(efficiency.usage_covered_attempts) }} / {{ number(efficiency.observed_attempts) }} · {{ t('Recorded attempts, not complete task coverage.', '仅已记录执行，不代表完整任务覆盖。') }}</small>
          <details><summary>{{ t('Execution evidence', '执行证据') }}</summary><p>{{ number(efficiency.reported_outcomes.success ?? 0) }} {{ t('reported successful', '次上报成功') }} · {{ number(efficiency.reported_outcomes.failure ?? 0) }} {{ t('failed', '次失败') }} · {{ number(efficiency.reported_outcomes.partial ?? 0) }} {{ t('partial', '次部分完成') }} · {{ number(efficiency.reported_outcomes.cancelled ?? 0) }} {{ t('cancelled', '次取消') }} · {{ number(efficiency.reported_outcomes.unknown ?? 0) }} {{ t('without a known versioned outcome', '次缺少明确的版本化结果') }}</p><p>{{ number(efficiency.identity_conflicts) }} {{ t('identity conflicts', '处执行标识冲突') }}</p><p>{{ t('Review signals use the existing review rules, including original low scores and later corrections. They do not establish a training label or prove efficiency.', '复核信号沿用现有规则，包含原始低评分及后续纠正；它们不是训练标签，也不能证明效率提升。') }}</p></details>
        </template>
        <p><b>{{ number(learning?.uncertain_pending) }}</b> {{ t('uncertain', '待复核的不明确判断') }} · <b>{{ number(learning?.uncertain_with_problem_pending) }}</b> {{ t('also have problem scores', '同时存在问题评分') }}</p>
        <small>{{ t('Recorded cases only; collection is not training or proof of improvement.', '仅已记录案例；采集不等于训练，也不代表能力已经提升。') }}</small>
        <details><summary>{{ t('Learning evidence', '学习证据') }}</summary><p>{{ number(learning?.reviewer_corrections_pending) }} {{ t('await reviewer-correction review', '个审核纠正待复核') }} · {{ number(learning?.reviewed_cases) }} {{ t('reviewed cases', '个已复核案例') }} · {{ number(learning?.evaluated_versions) }} {{ t('evaluated versions', '个已评估版本') }}</p><small>{{ t('Reviewed cases follow decision dates; the evaluated-version count follows version creation dates.', '已复核案例按决策时间统计；已评估版本数按版本创建时间统计。') }}</small><small>{{ t('Review → evaluate → activate memory. Cases can inform later advice; collection is not training or proof of improvement.', '复核 → 评估 → 启用记忆，为后续决策提供案例。采集不等于训练，也不代表能力已经提升。') }}</small></details>
        <div class="form-actions"><button class="button primary" @click="$emit('review')">{{ t('Review priority cases', '复核重点案例') }}</button><button v-if="view !== 'learning'" class="button subtle" @click="$emit('cases')">{{ t('Case study', '案例学习') }}</button></div>
      </article>
    </div>
  </section>
</template>

<style scoped>
.impact-overview { gap: 12px; }
.token-overview, .learning-overview { gap: 12px; }
.token-overview article, .learning-overview article { padding: 14px 16px; }
.token-overview h2, .learning-overview h3 { margin-bottom: 8px; }
.token-overview strong { font-size: 28px; }
.token-overview strong small { display: inline; margin-left: 4px; }
.token-overview h2 { font-size: 14px; margin-bottom: 4px; }
.token-overview { align-items: start; }
.impact-overview p { margin: 6px 0; line-height: 1.45; }
.impact-overview .token-note { font-size: 11px; color: var(--ink-soft); max-width: 52ch; }
.local-usage { font-size: 12px; color: var(--ink-soft); }
.local-usage small { display: block; font-size: 10px; }
.impact-overview small { line-height: 1.45; }
.impact-overview details { margin-top: 8px; font-size: 12px; }
.impact-overview summary { cursor: pointer; }
.learning-overview .form-actions { margin: 10px 0 0; }
.model-count { padding: 4px 0; }
.impact-hero .token-overview { grid-template-columns: minmax(0, 1fr) minmax(0, 1fr); align-items: start; gap: 24px; }
.impact-hero .token-overview article { border: 0; border-radius: 0; background: transparent; padding: 0; }
.impact-hero .token-overview .token-estimate { padding-left: 24px; border-left: 1px solid var(--line); }
.impact-hero .token-actual h2 { color: var(--ink-soft); font-size: 13px; font-weight: 500; }
.impact-hero .token-actual strong { font-size: clamp(28px, 3vw, 42px); letter-spacing: -1px; }
.impact-hero .token-estimate h2 { color: var(--ink-soft); font-size: 12px; font-weight: 500; }
.impact-hero .token-estimate strong { font-size: 26px; color: var(--orange-dark); }
.token-overview article > strong { display: flex; align-items: center; gap: 6px; white-space: nowrap; }
.token-toggle { display: inline-flex; align-items: center; justify-content: center; flex: 0 0 36px; width: 36px; height: 36px; margin-left: 4px; padding: 0; border: 0; border-radius: 6px; background: transparent; color: var(--ink-soft); font-size: 26px; line-height: 1; cursor: pointer; }
.token-toggle:hover, .token-toggle:active, .token-toggle:focus { background: transparent; }
.token-toggle:focus-visible { outline: 2px solid var(--orange); outline-offset: 2px; }
.token-toggle[aria-expanded="true"] span { transform: rotate(90deg); }
.impact-overview .token-details:not([open]) { margin-top: 0; }
@media (max-width: 760px) {
  .impact-hero .token-overview { grid-template-columns: 1fr; gap: 16px; }
  .impact-hero .token-overview .token-estimate { padding: 14px 0 0; border-left: 0; border-top: 1px solid var(--line); }
}
</style>
