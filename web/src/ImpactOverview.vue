<script setup>
import { computed } from 'vue'
import { t, locale } from './i18n.js'

const props = defineProps({ status: Object })
defineEmits(['review', 'cases'])
const dashboard = computed(() => props.status?.dashboard)
const tokens = computed(() => dashboard.value?.tokens)
const learning = computed(() => dashboard.value?.learning)
const number = (value) => value == null ? '—' : Number(value).toLocaleString(locale.value)
const finiteNumber = (value) => typeof value === 'number' && Number.isFinite(value)
const validRange = (value) => value && finiteNumber(value.low) && finiteNumber(value.central) && finiteNumber(value.high) && value.low <= value.central && value.central <= value.high
const scenario = computed(() => {
  const value = tokens.value?.scenario
  if (!value || !['available', 'partial'].includes(value.status) || value.estimator_version !== 'scenario-v1') return null
  if (!validRange(value.saved) || !validRange(value.baseline) || value.baseline.low < 0) return null
  return value
})
const signedNumber = (value) => {
  if (!finiteNumber(value)) return '—'
  if (value > 0) return `+${number(value)}`
  return number(value)
}
const actualTotal = computed(() => {
  if (!scenario.value) return tokens.value?.recorded_total
  return finiteNumber(scenario.value.actual_total) && scenario.value.actual_total >= 0 ? scenario.value.actual_total : null
})
const includedCount = computed(() => scenario.value ? scenario.value.included_runs : tokens.value?.included_attempts)
const excludedCount = computed(() => scenario.value ? scenario.value.excluded_runs : tokens.value?.excluded_reports)
const assumptionValues = (values) => Array.isArray(values) && values.every(finiteNumber) ? values.map(number).join(' / ') : '—'
const usageStatus = computed(() => {
  if (scenario.value) return null
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
  <section class="impact-overview" :aria-label="t('Token efficiency and learning', 'Token 效率与学习')">
    <p class="impact-intro">{{ t('Delegate to suitable models. Learn from feedback. Verify the savings.', '合理分工，积累反馈，用数据验证节省。') }}</p>
    <div class="token-overview">
      <article class="token-estimate">
        <h2>{{ scenario?.saved.central < 0 ? t('Additional tokens used', '额外使用的 Token') : t('Estimated tokens saved', '预计节省 Token') }}</h2>
        <strong v-if="scenario">{{ scenario.saved.central < 0 ? number(Math.abs(scenario.saved.central)) : signedNumber(scenario.saved.central) }} <small>Token</small></strong>
        <strong v-else>— <small>Token</small></strong>
        <p>{{ t('Scenario estimate', '情景估算') }}</p>
        <template v-if="scenario">
          <p>{{ t('Savings range', '节省范围') }}: {{ signedNumber(scenario.saved.low) }} {{ t('to', '至') }} {{ signedNumber(scenario.saved.high) }} {{ t('host-model tokens', '主机模型 Token') }}</p>
          <p v-if="scenario.saved.central < 0">{{ t('Central savings', '中心节省值') }}: {{ signedNumber(scenario.saved.central) }} {{ t('host-model tokens', '主机模型 Token') }}</p>
          <p v-if="scenario.saved.low <= 0 && scenario.saved.high >= 0" role="status">{{ t('The range crosses zero; available evidence does not show whether this scenario saves tokens.', '范围跨越零点；现有证据无法判断该情景是否节省 Token。') }}</p>
          <p v-if="scenario.status === 'partial'" role="status">{{ t('Partial reported coverage', '仅覆盖部分已上报数据') }}</p>
          <small>{{ t('Estimated, not measured. Assumes the current orchestrator model performs the same requested work and acceptance checks without delegation. No rerun is required.', '这是估算值，不是实测值。假设当前编排模型不进行委派，完成相同的请求工作和验收检查。无需重新运行任务。') }}</small>
          <details>
            <summary>{{ t('Technical assumptions', '技术假设') }}</summary>
            <p>{{ t('Estimator', '估算器') }}: {{ scenario.estimator_version }} · {{ t('Scope', '范围') }}: {{ t('host-model tokens only', '仅主机模型 Token') }}</p>
            <p>{{ t('Hypothetical unsplit baseline', '假设的不拆分对照') }}: {{ number(scenario.baseline.central) }} ({{ number(scenario.baseline.low) }} {{ t('to', '至') }} {{ number(scenario.baseline.high) }}) Token</p>
            <p>{{ t('Local Laya inference is excluded; this is not total cross-model compute or cost.', '不包含 Laya 本地推理；该指标不是跨模型总计算量或成本。') }}</p>
            <p>{{ t('Retention scenarios', '上下文保留情景') }}: {{ assumptionValues(scenario.assumptions?.retention) }} · {{ t('Output multipliers', '输出倍数') }}: {{ assumptionValues(scenario.assumptions?.output_multiplier) }}</p>
            <p>{{ t('Included runs', '已包含执行') }}: {{ number(scenario.included_runs) }} · {{ t('Excluded runs', '已排除执行') }}: {{ number(scenario.excluded_runs) }}</p>
            <p v-for="run in (scenario.runs || [])" :key="run.run_id">{{ run.orchestrator_model || '—' }} / {{ run.reasoning_effort || '—' }} · {{ run.input_source || '—' }} · {{ run.coverage || '—' }}</p>
          </details>
        </template>
        <template v-else>
          <p>{{ t('Collecting estimation inputs', '正在收集估算输入') }}</p>
          <small>{{ t('No duplicate run or comparable measured baseline is required.', '无需重复运行任务，也无需可比较的实测对照。') }}</small>
        </template>
      </article>
      <article>
        <h2>{{ scenario ? t('Actual usage for included scenario runs', '已包含情景执行的实际用量') : t('Recorded actual usage', '已记录的实际消耗') }}</h2>
        <strong>{{ number(actualTotal) }} <small>Token</small></strong>
        <p>{{ t('Host-model tokens · excludes local Laya inference', '主机模型 Token · 不包含 Laya 本地推理') }}</p>
        <p v-if="scenario">{{ t('Only included scenario runs contribute to this actual; excluded recorded usage is omitted.', '该实际用量仅计入已包含的情景执行；已排除的记录用量不在其中。') }}</p>
        <p v-if="scenario?.status === 'partial'">{{ t('Partial reported coverage · not a whole-session total', '仅覆盖部分已上报数据 · 不代表整个会话总量') }}</p>
        <p v-else-if="scenario">{{ t('Reported scenario coverage · host-model scope', '已上报的情景覆盖 · 主机模型范围') }}</p>
        <p v-else>{{ t('Partial coverage · not a whole-session total', '部分覆盖 · 不代表整个会话总量') }}</p>
        <p v-if="usageStatus" role="status">{{ usageStatus }}</p>
        <small>{{ number(includedCount) }} {{ t('included run reports', '个已包含执行报告') }} · {{ number(excludedCount) }} {{ t('excluded run reports', '个已排除执行报告') }}</small>
        <details><summary>{{ t('Counting rules', '统计口径') }}</summary><p>{{ t('Merge only attempt totals reported as source-verified and non-overlapping. Ordered cumulative streams use source sequence, not delivery order. Conflicting streams, ambiguous legacy reports and inclusive task totals are excluded. Source verification is reported by the sender, not independently authenticated.', '仅合并上报为来源已核实、范围不重叠的执行用量。有序累计报告按来源序号选取，不按送达顺序；冲突用量流、顺序不明的旧报告及包含子代理的任务总量不合并。来源核实是发送方声明，不是独立认证。') }}</p><p v-for="(count, reason) in tokens?.exclusion_reasons || {}" :key="reason">{{ reasonLabel(reason) }}：{{ number(count) }}</p></details>
      </article>
    </div>
    <div class="learning-overview">
      <article>
        <h3>{{ t('How work was assigned', '任务分配到了哪里') }}</h3>
        <p v-if="!dashboard?.execution_models?.length">{{ t('No verified execution distribution yet.', '暂无执行模型分布记录。') }}</p>
        <div v-for="(item, index) in (dashboard?.execution_models || []).slice(0, 5)" :key="index" class="model-count"><span>{{ item.model || t('Unverified model', '未核实模型') }} / {{ item.reasoning_effort || '—' }}</span><b>{{ number(item.attempts) }} {{ t('attempts', '次执行') }}</b></div>
        <small>{{ t('Top 5 recorded combinations; distribution alone does not prove better task decomposition or quality.', '已记录搭配前 5 项；分配次数本身不能证明拆分更合理或质量更高。') }}</small>
      </article>
      <article>
        <h3>{{ t('Turn uncertainty into learning cases', '把不明确判断沉淀为学习案例') }}</h3>
        <p><b>{{ number(learning?.uncertain_pending) }}</b> {{ t('uncertain', '待复核的不明确判断') }} · <b>{{ number(learning?.uncertain_with_problem_pending) }}</b> {{ t('also have problem scores', '同时存在问题评分') }}</p>
        <p>{{ number(learning?.reviewer_corrections_pending) }} {{ t('await reviewer-correction review', '个审核纠正待复核') }} · {{ number(learning?.reviewed_cases) }} {{ t('reviewed cases', '个已复核案例') }} · {{ number(learning?.evaluated_versions) }} {{ t('evaluated versions', '个已评估版本') }}</p>
        <small>{{ t('Reviewed cases follow decision dates; the evaluated-version count follows version creation dates.', '已复核案例按决策时间统计；已评估版本数按版本创建时间统计。') }}</small>
        <div class="form-actions"><button class="button primary" @click="$emit('review')">{{ t('Review priority cases', '复核重点案例') }}</button><button class="button subtle" @click="$emit('cases')">{{ t('Case study', '学习案例库') }}</button></div>
        <small>{{ t('Review → evaluate → activate memory. Cases can inform later advice; collection is not training or proof of improvement.', '复核 → 评估 → 启用记忆，为后续决策提供案例。采集不等于训练，也不代表能力已经提升。') }}</small>
      </article>
    </div>
  </section>
</template>
