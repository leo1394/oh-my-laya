<script setup>
import { computed, ref } from 'vue'
import { t } from './i18n.js'
import { observationRecords, inputSize, isolation, duration } from './executionEvidence.js'

const props = defineProps({ observations: Array })
const open = ref(false)
const records = computed(() => observationRecords(props.observations))
const visible = computed(() => records.value.slice(0, 50))
</script>

<template>
  <details v-if="records.length" class="panel evidence-card" @toggle="open = $event.target.open">
    <summary>{{ t('Execution context & duration', '执行上下文与耗时') }} · {{ records.length }}</summary>
    <template v-if="open">
      <p>{{ t('Per-attempt reported evidence, not a task total or proof of savings. Bytes, characters and native tokens are not interchangeable; parallel durations are not added.', '逐次执行的上报证据，不是任务总量或节省证明。字节、字符和原生 Token 不能互换；并行耗时不相加。') }}</p>
      <p>{{ t('Only dispatch input size is recorded here; handoff size is not reported.', '这里只记录派发输入大小，尚无交接输出大小记录。') }}</p>
      <article v-for="item in visible" :key="item.attempt_ref" class="feedback-summary">
        <strong>{{ item.identity?.role || t('Unknown role', '角色未知') }} · {{ item.attempt_ref }}</strong>
        <p v-if="item.identity?.status === 'conflict'" role="status">{{ t('Conflicting execution identity; do not treat this association as verified.', '执行标识冲突，不应将此关联视为已核实。') }}</p>
        <dl class="facts">
          <div><dt>{{ t('Dispatch input', '派发输入') }}</dt><dd>{{ inputSize(item) }}</dd></div>
          <div><dt>{{ t('Context isolation', '上下文隔离') }}</dt><dd>{{ isolation(item) }}</dd></div>
          <div><dt>{{ t('Attempt duration', '本次执行耗时') }}</dt><dd>{{ duration(item) }}</dd></div>
        </dl>
        <small v-if="item.context?.input_size?.source">{{ t('Input measurement source', '输入计量来源') }}: {{ item.context.input_size.source }}</small>
      </article>
      <p v-if="records.length > visible.length">{{ t('Showing the first 50 records. All records remain in Original evidence & history.', '显示前 50 条；全部记录可在原始证据与历史中查看。') }}</p>
    </template>
  </details>
</template>
