<script setup>
import { reactive, ref } from 'vue'
import { api, jsonBody } from './api.js'
import { t } from './i18n.js'

const props = defineProps({ preferences: Object })
const emit = defineEmits(['saved'])
const editing = ref(false)
const busy = ref(false)
const error = ref('')
const draft = reactive({ enabled: false, policy: 'always', model: '', effort: 'high', reviewer: false, reviewerModel: '', reviewerEffort: 'high' })
const efforts = ['low', 'medium', 'high', 'xhigh']
const policyLabel = (policy) => ({ always: t('Always ask', '每次询问'), conditional: t('Ask for difficult, risky or uncertain work', '高复杂度、高风险或不确定时询问'), auto: t('Automatic within the selected ceiling', '在指定模型与推理上限内自动建议') })[policy] || t('Not configured', '未配置')
const pairLabel = (pair) => pair ? `${pair.model} / ${pair.reasoning_effort}` : t('Current session', '当前主会话')
function edit() {
  const value = props.preferences
  Object.assign(draft, { enabled: value.squad?.enabled === true, policy: value.policy || 'always', model: value.ceiling?.model || '', effort: value.ceiling?.reasoning_effort || 'high', reviewer: !!value.squad?.reviewer, reviewerModel: value.squad?.reviewer?.model || '', reviewerEffort: value.squad?.reviewer?.reasoning_effort || 'high' })
  error.value = ''
  editing.value = true
}
async function save() {
  busy.value = true
  error.value = ''
  try {
    const value = await api('/advisor-preferences', { method: 'PATCH', body: jsonBody({ confirmed: true, policy: draft.policy, ceiling: draft.policy === 'auto' ? { model: draft.model.trim(), reasoning_effort: draft.effort } : null, squad: { enabled: draft.enabled, reviewer: draft.reviewer ? { model: draft.reviewerModel.trim(), reasoning_effort: draft.reviewerEffort } : null } }) })
    emit('saved', value)
    editing.value = false
  } catch (cause) { error.value = cause.message }
  finally { busy.value = false }
}
</script>

<template>
  <article class="panel setting-panel squad-routing">
    <h2>{{ t('Squad role routing', 'Squad 角色路由') }}</h2>
    <p>{{ t('Keep the main model unchanged. Let Laya guide subagent assignments within your preferences.', '保持主会话模型不变，让 Laya 在你的配置范围内建议子代理分工。') }}</p>
    <p v-if="!preferences">{{ t('Preferences unavailable. Refresh before editing.', '配置尚未加载，请刷新后再编辑。') }}</p>
    <template v-else-if="!editing">
      <dl><dt>{{ t('Routing', '路由状态') }}</dt><dd>{{ preferences.squad?.enabled ? t('Enabled', '已启用') : t('Disabled', '未启用') }}</dd><dt>{{ t('Strategy', '建议策略') }}</dt><dd>{{ policyLabel(preferences.policy) }}</dd><dt>{{ t('Execution roles', '执行角色') }}</dt><dd>{{ preferences.policy === 'auto' && preferences.ceiling ? pairLabel(preferences.ceiling) : t('Select in the host session', '在宿主会话中选择') }}</dd><dt>Reviewer</dt><dd>{{ pairLabel(preferences.squad?.reviewer) }}</dd></dl>
      <button class="button subtle" @click="edit">{{ t('Configure routing', '配置角色路由') }}</button>
    </template>
    <form v-else @submit.prevent="save">
      <fieldset :disabled="busy">
        <label class="routing-check"><input v-model="draft.enabled" type="checkbox"/>{{ t('Enable Laya routing for Squad', '启用 Squad 的 Laya 角色路由') }}</label>
        <label>{{ t('Strategy', '建议策略') }}<select v-model="draft.policy"><option v-for="policy in ['always', 'conditional', 'auto']" :key="policy" :value="policy">{{ policyLabel(policy) }}</option></select></label>
        <div v-if="draft.policy === 'auto'" class="routing-pair">
          <label>{{ t('Execution model (explorer, worker, tester, researcher)', '执行模型（explorer、worker、tester、researcher）') }}<input v-model="draft.model" required maxlength="128" placeholder="Model ID"/></label>
          <label>{{ t('Reasoning ceiling', '推理上限') }}<select v-model="draft.effort" required><option v-for="effort in efforts" :key="effort">{{ effort }}</option></select></label>
        </div>
        <p v-else>{{ t('Execution models are selected and verified in the host session.', '执行模型在宿主会话中选择并核验。') }}</p>
        <label class="routing-check"><input v-model="draft.reviewer" type="checkbox"/>{{ t('Use a separate model for ordinary reviews', '普通审核使用独立模型') }}</label>
        <div v-if="draft.reviewer" class="routing-pair">
          <label>{{ t('Reviewer model', '审核模型') }}<input v-model="draft.reviewerModel" required maxlength="128" placeholder="Model ID"/></label>
          <label>{{ t('Reasoning', '推理档位') }}<select v-model="draft.reviewerEffort" required><option v-for="effort in efforts" :key="effort">{{ effort }}</option></select></label>
        </div>
        <p>{{ t('Difficult, high-risk or uncertain reviews use the main session model and effort.', '复杂、高风险或不确定的审核使用主会话模型与推理档位。') }}</p>
        <p>{{ t('Model availability is verified by the host when used. Advanced reasoning must be configured in the host. Saving neither switches the main model nor enables collection.', '模型可用性由宿主在使用时复核，高级推理档位需在宿主中配置。保存不会切换主模型，也不会开启采集。') }}</p>
        <div class="form-actions"><button class="button primary" type="submit">{{ busy ? t('Saving…', '保存中…') : t('Confirm and save', '确认并保存') }}</button><button class="button subtle" type="button" @click="editing = false">{{ t('Cancel', '取消') }}</button></div>
      </fieldset>
    </form>
    <p v-if="error" role="alert" class="form-error">{{ error }}</p>
  </article>
</template>

<style scoped>
fieldset { border: 0; padding: 0; margin: 0; min-width: 0; }
dl { display: grid; grid-template-columns: auto 1fr; gap: 10px 20px; }
dd { margin: 0; overflow-wrap: anywhere; }
.routing-check { display: flex; align-items: center; gap: 10px; }
.routing-check input { width: auto; }
.routing-pair { display: grid; grid-template-columns: minmax(0, 2fr) minmax(120px, 1fr); gap: 16px; }
@media (max-width: 600px) { .routing-pair { grid-template-columns: 1fr; } }
</style>
