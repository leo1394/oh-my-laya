<script setup>
import { t, label } from './i18n.js'
import { computed } from 'vue'
import { reviewSignals } from './utils.js'

const props = defineProps({ decision: { type: Object, required: true }, showEvents: Boolean })
const signals = computed(() => reviewSignals(props.decision))
const reviewStatus = computed(() => props.decision.reviews?.at(-1)?.status || props.decision.status)
const translations = {
  'Uncertain': '判断不确定', 'Problem score': '评分有问题', 'Test failed': '测试失败',
  'High risk': '高风险', 'User declined': '用户拒绝', 'Model changed': '模型变更', 'Task failed': '任务失败', 'Outcome failed': '结果失败', 'Reviewer disagreement': '审核存在分歧',
  'User changed selection': '用户调整了选择', 'Model upgrade': '模型已升级',
  'Reviewer proposed label correction': '审核建议修正标签', 'Invalid result': '无效结果',
  'Decision error': '决策错误', 'Reported high-risk correction': '反馈要求上调至高风险'
}
</script>

<template>
  <span class="review-signals">
    <span class="review-rank">{{ ['confirmed','corrected','insufficient','excluded'].includes(reviewStatus) ? label(reviewStatus) : t(signals.priority, ['无复核线索','待复核','问题反馈','不确定且有问题反馈','反馈提示安全或风险漏判'][signals.rank] || '待复核') }}</span>
    <span v-if="signals.risk"> · Laya: {{ label(signals.risk) }} {{ t('risk', '风险') }}</span>
    <span> · {{ signals.triggerCount }} {{ t('review triggers', '项复核线索') }}</span>
    <span v-if="signals.reasons.length" class="review-reasons">{{ signals.reasons.map(reason => t(reason, translations[reason] || reason)).join(' · ') }}</span>
    <span v-if="showEvents && signals.eventIds.length" class="review-event-ids">{{ t('Trigger events', '关联事件') }}: {{ signals.eventIds.join(', ') }}</span>
  </span>
</template>
