<script setup>
import { ref } from 'vue'
import { t, label } from './i18n.js'
import { displayTime, itemId } from './utils.js'

defineProps({ item: { type: Object, required: true }, busy: Boolean, error: String })
defineEmits(['evaluate', 'activate'])
const expanded = ref(false)
</script>

<template>
  <div class="version-row">
    <div><strong>{{ item.name || itemId(item) }}</strong><small>{{ item.case_count ?? item.case_ids?.length ?? '—' }} {{ t('cases', '个案例') }} · {{ displayTime(item.created_at) }}</small><small>{{ t('Evaluation', '评估') }}: {{ label(item.evaluation_status || 'pending') }}</small></div>
    <span class="status-stamp" :class="item.status">{{ label(item.status || 'candidate') }}</span>
    <div class="row-actions"><button class="mini" :disabled="busy || item.invalidated || !!item.evaluation || item.evaluation_status !== 'pending'" @click="$emit('evaluate', item)">{{ t('Evaluate', '评估') }}</button><button class="mini accent" :disabled="busy || item.activation_eligible !== true || item.status === 'active'" @click="$emit('activate', item)">{{ t('Activate', '启用') }}</button></div>
    <div v-if="error" class="version-report form-error">{{ error }}</div>
    <details v-if="item.evaluation" class="version-report" @toggle="expanded = $event.target.open">
      <summary>{{ t('Evaluation report', '评估报告') }} · {{ item.evaluation.sample_count ?? t('Unknown', '未知') }} {{ t('samples', '个样本') }} · {{ item.evaluation.candidate_memory_exposure ?? t('Unknown', '未知') }} {{ t('candidate case references', '次候选案例引用') }}</summary>
      <p v-if="item.evaluation.candidate_memory_exposure === 0">{{ t('No candidate cases were injected. This evaluation cannot authorize activation; review source provenance and relevant held-out coverage.', '未注入候选案例，不能凭此次评估启用记忆。请检查来源与独立评估样本的覆盖。') }}</p>
      <p v-else-if="item.evaluation_status === 'passed' && item.activation_eligible !== true">{{ t('This historical result is not eligible for activation under the current policy.', '历史结果不符合当前启用条件。') }}</p>
      <p>{{ t('Reports are immutable. Select eligible cases and freeze a new candidate to evaluate again. A passing regression set does not prove general improvement.', '报告不可修改。重新评估需创建新候选；通过回归评估不等于普遍能力提升。') }}</p>
      <pre v-if="expanded">{{ JSON.stringify(item.evaluation, null, 2) }}</pre>
    </details>
  </div>
</template>
