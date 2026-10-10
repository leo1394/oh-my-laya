<script setup>
import { t } from './i18n.js'
import { presetDateRange } from './utils.js'

const props = defineProps({ modelValue: { type: Object, required: true }, compact: Boolean })
const emit = defineEmits(['update:modelValue'])
const presets = [
  { id: 'today', en: 'Today', zh: '今天' },
  { id: 'last7', en: 'Last 7 days', zh: '最近 7 天' },
  { id: 'last30', en: 'Last 30 days', zh: '最近 30 天' },
  { id: 'custom', en: 'Custom', zh: '自定义' }
]
function selectPreset(preset) {
  if (preset === 'custom') emit('update:modelValue', { ...props.modelValue, preset })
  else emit('update:modelValue', presetDateRange(preset))
}
function updateDate(key, value) {
  emit('update:modelValue', { ...props.modelValue, preset: 'custom', [key]: value })
}
</script>

<template>
  <div class="date-range" :class="{ 'date-range-compact': compact }" :aria-label="t('Decision date', '决策时间')">
    <label v-if="compact" class="date-select"><svg viewBox="0 0 24 24" width="17" height="17" fill="none" stroke="currentColor" stroke-width="1.7" aria-hidden="true"><rect x="3" y="5" width="18" height="16" rx="3"/><path d="M7 2v6M17 2v6M3 11h18"/></svg><select :value="modelValue.preset" @change="selectPreset($event.target.value)" :aria-label="t('Decision date', '决策时间')"><option v-for="preset in presets" :key="preset.id" :value="preset.id" :selected="modelValue.preset === preset.id">{{ t(preset.en, preset.zh) }}</option></select></label>
    <span v-else>{{ t('Decision date', '决策时间') }}</span>
    <div v-if="!compact" class="date-presets">
      <button v-for="preset in presets" :key="preset.id" type="button" class="mini" :class="{ accent: modelValue.preset === preset.id }" :aria-pressed="modelValue.preset === preset.id" @click="selectPreset(preset.id)">{{ t(preset.en, preset.zh) }}</button>
    </div>
    <div v-if="modelValue.preset === 'custom'" class="custom-dates">
      <label>{{ t('Start date', '开始日期') }}<input type="date" :value="modelValue.start" @input="updateDate('start', $event.target.value)"/></label>
      <span aria-hidden="true">—</span>
      <label>{{ t('End date', '结束日期') }}<input type="date" :value="modelValue.end" @input="updateDate('end', $event.target.value)"/></label>
    </div>
  </div>
</template>
