<script setup>
import { t } from './i18n.js'
import { presetDateRange } from './utils.js'

const props = defineProps({ modelValue: { type: Object, required: true } })
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
  <div class="date-range" :aria-label="t('Decision date', '决策时间')">
    <span>{{ t('Decision date', '决策时间') }}</span>
    <div class="date-presets">
      <button v-for="preset in presets" :key="preset.id" type="button" class="mini" :class="{ accent: modelValue.preset === preset.id }" :aria-pressed="modelValue.preset === preset.id" @click="selectPreset(preset.id)">{{ t(preset.en, preset.zh) }}</button>
    </div>
    <div v-if="modelValue.preset === 'custom'" class="custom-dates">
      <label>{{ t('Start date', '开始日期') }}<input type="date" :value="modelValue.start" @input="updateDate('start', $event.target.value)"/></label>
      <span aria-hidden="true">—</span>
      <label>{{ t('End date', '结束日期') }}<input type="date" :value="modelValue.end" @input="updateDate('end', $event.target.value)"/></label>
    </div>
  </div>
</template>
