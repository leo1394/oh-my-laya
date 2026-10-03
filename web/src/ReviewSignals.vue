<script setup>
import { computed } from 'vue'
import { reviewSignals } from './utils.js'

const props = defineProps({ decision: { type: Object, required: true }, showEvents: Boolean })
const signals = computed(() => reviewSignals(props.decision))
</script>

<template>
  <span class="review-signals">
    <span class="review-rank">{{ signals.priority }}</span>
    <span v-if="signals.risk"> · Laya: {{ signals.risk }} risk</span>
    <span> · {{ signals.triggerCount }} review triggers</span>
    <span v-if="signals.reasons.length" class="review-reasons">{{ signals.reasons.join(' · ') }}</span>
    <span v-if="showEvents && signals.eventIds.length" class="review-event-ids">Trigger events: {{ signals.eventIds.join(', ') }}</span>
  </span>
</template>
