<script setup>
import { computed, onBeforeUnmount, onMounted, reactive, ref, watch } from 'vue'

import { api, eventsUrl, jsonBody } from './api.js'
import { locale, setLanguage, t, label } from './i18n.js'
import { originalLabels, reviewLabels, reviewPayloadLabels } from './review.js'
import ReviewSignals from './ReviewSignals.vue'
import MemoryVersion from './MemoryVersion.vue'
import ImpactOverview from './ImpactOverview.vue'
import ExecutionEvidence from './ExecutionEvidence.vue'
import DateRangePicker from './DateRangePicker.vue'
import SquadRouting from './SquadRouting.vue'
import {
  dateRangeBounds,
  decisionQuery,
  decisionTitle,
  displayBytes,
  displayTime,
  errorMessage,
  exportArtifact,
  itemId,
  normalizeList,
  presetDateRange,
  queueNeedsRefresh,
  rangeQuery,
  refreshPresetRange,
  reviewScope,
  reviewSignals
} from './utils.js'

const tabs = computed(() => [
  { id: 'queue', number: '01', label: t('Overview', '概览') },
  { id: 'cases', number: '02', label: t('Case study', '案例学习') },
  { id: 'settings', number: '03', label: t('Settings', '设置') }
])
const tiers = ['low', 'medium', 'high']
const editingTiers = ref(false)
const tierDraft = reactive(Object.fromEntries(tiers.map((tier) => [tier, { model: '', reasoning_effort: '' }])))
const tierSummary = (tier) => {
  const pair = settings.value?.model_tiers?.[tier]
  return pair ? pair.model + ' / ' + pair.reasoning_effort : t('Not configured', '未配置')
}
function editTiers() {
  for (const tier of tiers) Object.assign(tierDraft[tier], settings.value?.model_tiers?.[tier] || { model: '', reasoning_effort: '' })
  editingTiers.value = true
}
async function saveTiers() {
  const model_tiers = {}
  for (const tier of tiers) {
    const model = tierDraft[tier].model.trim()
    const reasoning_effort = tierDraft[tier].reasoning_effort.trim()
    if (!model && !reasoning_effort) continue
    if (!model || !reasoning_effort) { errors.tiers = t('Complete both fields, or clear both.', '请填写完整组合，或同时清空两项。'); return }
    model_tiers[tier] = { model, reasoning_effort }
  }
  const result = await mutate('tiers', '/settings', { method: 'PATCH', body: jsonBody({ model_tiers }) }, t('Tier combinations saved.', '三档组合已保存。'), loadSettings)
  if (result != null) editingTiers.value = false
}


const activeTab = ref('queue')
const status = ref(null)
const overview = ref(null)
const dateRange = ref(presetDateRange('last7'))
const dateBounds = computed(() => dateRangeBounds(dateRange.value))
const decisions = ref([])
const selectedDecision = ref(null)
const rawEvidenceOpen = ref(false)
const observedPair = (pair) => pair?.model && pair?.reasoning_effort ? pair.model + ' / ' + pair.reasoning_effort : t('Not verified', '未核实')
const cases = ref([])
const versions = ref([])
const jobs = ref([])
const settings = ref(null)
const advisorPreferences = ref(null)
const backups = ref([])
const restoreWarning = ref('')
const outbox = ref([])
const selectedCases = ref([])
const queueFilters = reactive({ status: 'pending', risk: '', source: '' })
const queueOffset = ref(0)
const loading = reactive({})
const errors = reactive({})
const notice = ref('')
const pairing = ref(false)
const eventState = ref('connecting')
const settingsDraft = reactive({ retention_days: 30, soft_limit_bytes: 524288000 })
const review = reactive({
  status: 'confirmed',
  model_tier: '',
  complexity: '',
  risk: '',
  certainty: '',
  task_family: 'general',
  task_lineage: '',
  language: 'en',
  applicability: 'task-fact',
  validation_assignment_event_id: '',
  applicability_reason: '',
  reason: ''
})
let events
let noticeTimer
let refreshTimer
let overviewRequest = 0
let decisionRequest = 0
let caseRequest = 0
const pendingChanges = new Set()

const statusWorker = computed(() => status.value?.worker || status.value?.worker_status || {})
const serviceHealthy = computed(() => {
  const value = status.value?.ok ?? status.value?.status ?? status.value?.service_status ?? status.value?.healthy
  return value === 'ok' || value === 'ready' || value === true
})
const pendingCount = computed(() => status.value?.counts?.pending_reviews ?? status.value?.pending_reviews ?? decisions.value.length)
const activeVersion = computed(() => status.value?.settings?.active_memory_version || status.value?.memory_version || status.value?.active_memory_version || 'None')
const workerLabel = computed(() => {
  if (statusWorker.value.state || statusWorker.value.status) return statusWorker.value.state || statusWorker.value.status
  if (statusWorker.value.busy) return 'busy'
  if (Number(statusWorker.value.pid) > 0) return 'ready'
  if (statusWorker.value.pid === 0 || statusWorker.value.loaded === false) return 'stopped'
  if (statusWorker.value.loaded === true) return 'loaded'
  return 'unknown'
})
const evidenceGroups = computed(() => {
  const decision = selectedDecision.value || {}
  return [
    { label: t('Snapshots', '快照'), items: decision.snapshots },
    { label: t('Execution attempts', '执行记录'), items: decision.execution_attempts },
    { label: t('Efficiency observations', '效率观察'), items: decision.efficiency_observations },
    { label: t('Model observations', '模型观测'), items: decision.model_observations },
    { label: t('Feedback', '反馈'), items: decision.feedback },
    { label: t('Review revisions', '复核历史'), items: decision.reviews }
  ].filter((group) => Array.isArray(group.items) && group.items.length)
})

async function load(key, action) {
  loading[key] = true
  errors[key] = ''
  try {
    return await action()
  } catch (error) {
    errors[key] = errorMessage(error)
    return null
  } finally {
    loading[key] = false
  }
}

async function loadStatus() {
  const payload = await load('status', () => api('/status'))
  if (payload) status.value = payload
}

function rebasePresetRange() {
  const next = refreshPresetRange(dateRange.value)
  if (next === dateRange.value) return false
  dateRange.value = next
  return true
}

async function loadRanged(key, path, request, assign) {
  loading[key] = true
  errors[key] = ''
  try {
    const payload = await api(path)
    if (request()) assign(payload)
  } catch (error) {
    if (request()) errors[key] = errorMessage(error)
  } finally {
    if (request()) loading[key] = false
  }
}

function loadOverview() {
  if (!dateBounds.value.valid) return Promise.resolve()
  const request = ++overviewRequest
  return loadRanged('overview', `/overview?${rangeQuery(dateBounds.value)}`, () => request === overviewRequest, (payload) => { overview.value = payload })
}

function focusPriorityCases() {
  queueFilters.status = 'pending'
  queueFilters.risk = ''
  queueFilters.source = ''
  if (rebasePresetRange()) {
    document.querySelector('.queue-panel')?.scrollIntoView({ block: 'start' })
    return
  }
  applyQueueFilters()
  document.querySelector('.queue-panel')?.scrollIntoView({ block: 'start' })
}

async function loadDecisions() {
  if (!dateBounds.value.valid) return
  const request = ++decisionRequest
  await loadRanged('decisions', `/decisions?${decisionQuery(queueFilters, queueOffset.value, dateBounds.value)}`, () => request === decisionRequest, (payload) => { decisions.value = normalizeList(payload) })
}

function applyQueueFilters() {
  queueOffset.value = 0
  return loadDecisions()
}

function changeQueuePage(delta) {
  queueOffset.value = Math.max(0, queueOffset.value + delta * 50)
  return loadDecisions()
}

async function openDecision(decision) {
  rawEvidenceOpen.value = false
  const id = itemId(decision)
  if (!id) return
  activeTab.value = 'detail'
  selectedDecision.value = null
  const payload = await load('detail', () => api(`/decisions/${encodeURIComponent(id)}`))
  if (!payload) return
  selectedDecision.value = payload.item || payload.decision || payload
  const reviewHistory = Array.isArray(selectedDecision.value.reviews) ? selectedDecision.value.reviews : []
  const latestReview = reviewHistory[reviewHistory.length - 1]
  const labels = reviewLabels(selectedDecision.value)
  review.model_tier = labels.model_tier || ''
  review.status = latestReview?.status || selectedDecision.value.review?.status || 'confirmed'
  review.complexity = labels.complexity || ''
  review.risk = labels.risk || ''
  review.certainty = labels.certainty || ''
  Object.assign(review, reviewScope(selectedDecision.value, latestReview))
  review.reason = ''
  review.applicability = latestReview?.applicability || 'task-fact'
  review.validation_assignment_event_id = latestReview?.validation_assignment_event_id || ''
  review.applicability_reason = latestReview?.applicability_reason || ''
}

async function loadCases() {
  const request = ++caseRequest
  const casePath = dateBounds.value.valid ? `/cases?${rangeQuery(dateBounds.value)}` : null
  const [, versionPayload, jobPayload] = await Promise.all([
    casePath ? loadRanged('cases', casePath, () => request === caseRequest, (payload) => { cases.value = normalizeList(payload) }) : Promise.resolve(),
    load('versions', () => api('/memory-versions')),
    load('jobs', () => api('/jobs'))
  ])
  if (versionPayload) versions.value = normalizeList(versionPayload)
  if (jobPayload) jobs.value = normalizeList(jobPayload)
  selectedCases.value = selectedCases.value.filter((id) => cases.value.some((item) => itemId(item) === id))
}

async function loadSettings() {
  const [settingsPayload, backupsPayload, outboxPayload, preferencesPayload] = await Promise.all([
    load('settings', () => api('/settings')),
    load('backups', () => api('/backups')),
    load('outbox', () => api('/outbox')),
    load('advisorPreferences', () => api('/advisor-preferences'))
  ])
  if (outboxPayload) outbox.value = normalizeList(outboxPayload)
  advisorPreferences.value = preferencesPayload || null
  if (settingsPayload) {
    settings.value = settingsPayload.settings || settingsPayload
    settingsDraft.retention_days = settings.value.retention_days ?? 30
    settingsDraft.soft_limit_bytes = settings.value.storage_soft_limit_bytes ?? settings.value.soft_limit_bytes ?? 524288000
  }
  if (backupsPayload) backups.value = normalizeList(backupsPayload)
}

async function loadAll() {
  const rebased = rebasePresetRange()
  await Promise.all([loadStatus(), rebased ? Promise.resolve() : loadOverview(), rebased ? Promise.resolve() : loadDecisions(), rebased ? Promise.resolve() : loadCases(), loadSettings()])
}

function showNotice(message) {
  notice.value = message
  clearTimeout(noticeTimer)
  noticeTimer = setTimeout(() => { notice.value = '' }, 4200)
}

async function mutate(key, path, options, success, refresh) {
  const result = await load(key, () => api(path, options))
  if (result === null && errors[key]) return
  showNotice(success)
  if (refresh) await refresh()
  return result
}

async function saveReview() {
  if (!selectedDecision.value) return
  if (['confirmed', 'corrected'].includes(review.status) && (!review.complexity || !review.risk || !review.certainty)) {
    errors.review = t('The original assessment is incomplete. Set its labels in assessment details, or mark evidence insufficient.', '原始判断不完整，请在判断详情中补全标签，或标记为证据不足。')
    return
  }
  const id = itemId(selectedDecision.value)
  const reviewHistory = Array.isArray(selectedDecision.value.reviews) ? selectedDecision.value.reviews : []
  const latestReview = reviewHistory[reviewHistory.length - 1]
  const expectedRevision = selectedDecision.value.review_revision
    ?? latestReview?.revision
    ?? selectedDecision.value.review?.revision
    ?? selectedDecision.value.revision
    ?? 0
  await mutate('review', `/decisions/${encodeURIComponent(id)}/reviews`, {
    method: 'POST',
    body: jsonBody({
      expected_revision: expectedRevision,
      status: review.status,
      labels: reviewPayloadLabels(review),
      task_family: review.task_family.trim() || 'general',
      task_lineage: review.task_lineage.trim() || null,
      language: review.language,
      applicability: review.applicability,
      validation_assignment_event_id: review.applicability === 'configuration-dependent' ? review.validation_assignment_event_id || null : null,
      applicability_reason: review.applicability_reason.trim() || null,
      reason: review.reason.trim() || 'Human review submitted in workbench; no additional evidence supplied.'
    })
  }, t("Review saved as a new revision.", "复核已保存为新版本。"), async () => {
    await Promise.all([openDecision(selectedDecision.value), loadDecisions(), loadStatus(), loadOverview()])
  })
}

async function deleteDecision() {
  if (!selectedDecision.value) return
  const id = itemId(selectedDecision.value)
  if (!window.confirm(t("Delete this decision, its linked local evidence, and all managed export files? Existing backups and external copies are not erased; remove managed backups separately in Settings. This cannot be undone.", "删除此决策、关联本地证据和全部托管导出文件？已有备份与外部副本不会删除，托管备份需在设置中单独删除。此操作不可撤销。"))) return
  await mutate('deleteDecision', `/decisions/${encodeURIComponent(id)}`, {
    method: 'DELETE'
  }, t("Decision deleted.", "决策已删除。"), async () => {
    selectedDecision.value = null
    activeTab.value = 'queue'
    await Promise.all([loadDecisions(), loadStatus(), loadOverview()])
  })
}

async function deleteCase(item) {
  const id = itemId(item)
  if (!window.confirm(t("Delete this reviewed case and all managed export files? Active versions may be invalidated. Existing backups and external copies are not erased; remove managed backups separately in Settings.", "删除此案例和全部托管导出文件？关联版本可能失效。已有备份与外部副本不会删除，托管备份需在设置中单独删除。"))) return
  await mutate(`case-${id}`, `/cases/${encodeURIComponent(id)}`, {
    method: 'DELETE'
  }, t("Case deleted.", "案例已删除。"), async () => Promise.all([loadCases(), loadOverview()]))
}

async function createVersion() {
  if (!selectedCases.value.length) {
    errors.createVersion = t("Select at least one reviewed case.", "请至少选择一个已复核案例。")
    return
  }
  await mutate('createVersion', '/memory-versions', {
    method: 'POST', body: jsonBody({ case_ids: selectedCases.value })
  }, t("Candidate memory version created.", "候选记忆版本已创建。"), loadCases)
}

async function activateVersion(item) {
  const id = itemId(item)
  if (!window.confirm(t("Activate this evaluated memory version for future advisor decisions?", "启用此已评估的记忆版本，用于后续建议？"))) return
  await mutate(`version-${id}`, `/memory-versions/${encodeURIComponent(id)}/activate`, {
    method: 'POST', body: jsonBody({})
  }, t("Memory version activated.", "记忆版本已启用。"), async () => Promise.all([loadCases(), loadStatus()]))
}

async function startEvaluation(item) {
  const id = itemId(item)
  await mutate(`evaluate-${id}`, '/jobs', {
    method: 'POST', body: jsonBody({ kind: 'evaluation', version_id: id })
  }, t("Evaluation queued.", "评估已排队。"), loadCases)
}

async function cancelJob(item) {
  const id = itemId(item)
  await mutate(`job-${id}`, `/jobs/${encodeURIComponent(id)}/cancel`, {
    method: 'POST', body: jsonBody({})
  }, t("Cancellation requested.", "已请求取消。"), loadCases)
}

async function toggleSetting(field, label) {
  if (!settings.value) return
  const next = !settings.value[field]
  const action = next ? 'enable' : 'disable'
  if (!window.confirm(t(`${action[0].toUpperCase()}${action.slice(1)} ${label}? This changes what future tasks may store or use.`, `${next ? "启用" : "停用"}${label}？这会影响后续任务的记录或使用。`))) return
  await patchSettings({ [field]: next }, t(`${label} ${next ? 'enabled' : 'disabled'}.`, `${label}已${next ? '启用' : '停用'}。`))
}

async function patchSettings(changes, message) {
  await mutate('saveSettings', '/settings', {
    method: 'PATCH', body: jsonBody(changes)
  }, message, async () => Promise.all([loadSettings(), loadStatus()]))
}

async function saveLimits() {
  const retention = Number(settingsDraft.retention_days)
  const softLimit = Number(settingsDraft.soft_limit_bytes)
  if (!Number.isInteger(retention) || retention < 1 || !Number.isInteger(softLimit) || softLimit < 1048576) {
    errors.saveSettings = t("Retention must be a positive whole day and the soft limit at least 1 MiB.", "保留天数须为正整数，存储软上限至少为 1 MiB。")
    return
  }
  await patchSettings({ retention_days: retention, storage_soft_limit_bytes: softLimit }, t("Retention and storage limits saved.", "存储与保留设置已保存。"))
}

async function createBackup() {
  await mutate('createBackup', '/backups', {
    method: 'POST', body: jsonBody({})
  }, t("Backup created.", "备份已创建。"), loadSettings)
}

async function restoreBackup(item) {
  const id = itemId(item)
  if (!window.confirm(t("Restore this backup? Current writes will pause and current state will be replaced after validation.", "恢复此备份？将暂停当前写入，验证后替换现有状态。"))) return
  restoreWarning.value = ''
  const result = await mutate(`backup-${id}`, `/backups/${encodeURIComponent(id)}/restore`, {
    method: 'POST', body: jsonBody({})
  }, t("Backup restored.", "备份已恢复。"), loadAll)
  if (result?.safety_backup_complete === false) restoreWarning.value = t("Restored from the verified backup. The previous state contained missing or damaged evidence; its safety archive is incomplete and cannot be restored automatically.", "已从验证通过的备份恢复。先前状态含缺失或损坏证据，其安全存档不完整，不能自动恢复。")
}

async function deleteBackup(item) {
  const id = itemId(item)
  if (!window.confirm(t("Permanently delete this managed backup? This cannot be undone and does not erase external copies.", "永久删除此托管备份？此操作不可撤销，不会删除外部副本。"))) return
  await mutate(`backup-${id}`, `/backups/${encodeURIComponent(id)}`, {
    method: 'DELETE'
  }, t("Backup deleted.", "备份已删除。"), loadSettings)
}

async function retryEvent(item) {
  if (!window.confirm(t("Retry this unchanged event? Invalid data stays quarantined; the original score will not be rewritten.", "重试此原始事件？无效数据仍隔离保存，首次评分不会改写。"))) return
  await mutate(`retry-${item.event_id}`, `/outbox/${encodeURIComponent(item.event_id)}/retry`, {
    method: 'POST', body: jsonBody({})
  }, t("Unchanged event queued for retry.", "原始事件已排队重试。"), loadSettings)
}

async function startExport() {
  await mutate('export', '/jobs', {
    method: 'POST', body: jsonBody({ kind: 'export' })
  }, t("Export job queued.", "导出任务已排队。"), loadCases)
}

async function pairFromFragment() {
  const fragment = new URLSearchParams(window.location.hash.slice(1))
  const code = fragment.get('pair')
  if (!code) return true
  pairing.value = true
  try {
    await api('/pair', { method: 'POST', body: jsonBody({ code }) })
    history.replaceState(null, '', `${window.location.pathname}${window.location.search}`)
    showNotice(t("Workbench paired with the local service.", "工作台已连接本地服务。"))
    return true
  } catch (error) {
    errors.pairing = errorMessage(error)
    return false
  } finally {
    pairing.value = false
  }
}

function connectEvents() {
  if (!window.EventSource) {
    eventState.value = 'unsupported'
    return
  }
  events?.close()
  let lastEventId = ''
  try { lastEventId = sessionStorage.getItem('laya-last-event-id') || '' } catch { /* Private mode may deny storage. */ }
  events = new EventSource(eventsUrl(lastEventId), { withCredentials: true })
  events.onopen = () => { eventState.value = 'live' }
  events.onerror = () => { eventState.value = 'reconnecting' }
  const handleEvent = (event) => {
    try { if (event.lastEventId) sessionStorage.setItem('laya-last-event-id', event.lastEventId) } catch { /* Reconnect without a local checkpoint. */ }
    let kind = ''
    try {
      const data = JSON.parse(event.data)
      kind = data?.kind || data?.type || ''
    } catch { kind = '' }
    pendingChanges.add(kind)
    if (!refreshTimer) refreshTimer = setTimeout(() => {
      refreshTimer = null
      const changes = [...pendingChanges].join(' ')
      pendingChanges.clear()
      if (changes.includes('stream.reset')) {
        loadAll()
        return
      }
      const rebased = rebasePresetRange()
      loadStatus()
      const caseChanged = changes.includes('case') || changes.includes('memory') || changes.includes('version') || changes.includes('job')
      if (!rebased && (queueNeedsRefresh(changes) || caseChanged)) loadOverview()
      if (!rebased && queueNeedsRefresh(changes)) loadDecisions()
      if (!rebased && caseChanged) loadCases()
      if (changes.includes('setting') || changes.includes('backup') || changes.includes('feedback')) loadSettings()
    }, 100)
  }
  events.onmessage = handleEvent
  events.addEventListener('change', handleEvent)
}

watch(activeTab, (tab) => {
  const rebased = rebasePresetRange()
  if (tab === 'settings') loadSettings()
  if (rebased) return
  if (tab === 'queue') loadDecisions()
  if (tab === 'cases') loadCases()
})

watch(dateRange, () => {
  overviewRequest += 1
  decisionRequest += 1
  caseRequest += 1
  overview.value = null
  decisions.value = []
  cases.value = []
  selectedCases.value = []
  queueOffset.value = 0
  if (!dateBounds.value.valid) {
    errors.dateRange = t('Choose valid dates with the start on or before the end.', '请选择有效日期，且开始日期不能晚于结束日期。')
    loading.overview = false
    loading.decisions = false
    loading.cases = false
    return
  }
  errors.dateRange = ''
  Promise.all([loadOverview(), loadDecisions(), loadCases()])
}, { deep: true })

onMounted(async () => {
  setLanguage(locale.value)
  if (await pairFromFragment()) {
    await loadAll()
    connectEvents()
  }
})

onBeforeUnmount(() => {
  events?.close()
  clearTimeout(noticeTimer)
  clearTimeout(refreshTimer)
})
</script>

<template>
  <div class="app-shell">
    <aside class="rail">
      <div class="brand"><img class="brand-mark" src="/logo.svg" alt="" width="42" height="42"/><div><strong>Oh My Laya</strong><span>{{ t('Decision workbench', '决策工作台') }}</span></div></div>
      <nav :aria-label="t('Navigation', '导航')"><button v-for="tab in tabs" :key="tab.id" class="nav-item" :class="{ active: activeTab === tab.id || (activeTab === 'detail' && tab.id === 'queue') }" @click="activeTab = tab.id"><span>{{ tab.number }}</span>{{ tab.label }}</button></nav>
      <div class="rail-status"><span class="signal" :class="{ ok: serviceHealthy }"></span><div><strong>{{ serviceHealthy ? t('Service ready', '服务就绪') : t('Service unavailable', '服务不可用') }}</strong><span>{{ label(eventState) }}</span></div></div>
      <p class="rail-note">{{ t('Your decisions. Kept on this device.', '决策与反馈，保留在本机。') }}</p>
    </aside>
    <main>
      <header class="topbar"><div><p class="eyebrow brand-tagline">Oh My Laya — Reflect. Route. Refine.</p><h1>{{ activeTab === 'detail' ? t('Review case', '复核案例') : tabs.find(tab => tab.id === activeTab)?.label }}</h1></div><div class="top-actions"><select :value="locale" @change="setLanguage($event.target.value)" aria-label="Language / 语言"><option value="en">English</option><option value="zh">简体中文</option></select><button class="icon-button" :aria-label="t('Refresh', '刷新')" @click="loadAll">↻</button></div></header>
      <div v-if="errors.pairing" class="inline-error">{{ t('Unable to pair with the service.', '无法连接本地服务。') }} {{ errors.pairing }}</div>
      <div v-else-if="pairing" class="page-state">{{ t('Connecting…', '连接中…') }}</div>
      <template v-else>
        <div v-for="[key, message] in Object.entries(errors).filter(([key, message]) => message && !['pairing','status','overview','dateRange','settings','decisions','detail','review','tiers','createVersion','cases','versions','saveSettings'].includes(key))" :key="key" class="inline-error" role="alert">{{ message }}</div>
        <section v-if="activeTab === 'queue'" class="page-grid queue-page">
          <DateRangePicker v-model="dateRange"/>
          <div v-if="errors.dateRange" class="inline-error" role="alert">{{ errors.dateRange }}</div>
          <div v-if="loading.overview" class="page-state">{{ t('Loading selected date range…', '正在加载所选日期范围…') }}</div>
          <ImpactOverview v-else :status="overview" @review="focusPriorityCases" @cases="activeTab = 'cases'" />
          <div v-if="errors.status || errors.overview" class="inline-error">{{ errors.status || errors.overview }}</div>
          <div class="summary-strip">
            <article><span>{{ t('Decisions collected', '已采集决策') }}</span><strong>{{ overview?.counts?.decisions ?? '—' }}</strong></article>
            <article><span>{{ t('Needs your review', '待你复核') }}</span><strong>{{ overview?.counts?.pending_reviews ?? '—' }}</strong></article>
            <article><span>{{ t('Squad feedback', 'Squad 反馈') }}</span><strong>{{ overview?.counts?.feedback ?? '—' }}</strong></article>
          </div>
          <div class="risk-summary"><span>{{ t('Recorded risk', '已记录风险') }}</span><span v-for="tier in [...tiers, 'unknown']" :key="tier">{{ label(tier) }} <b>{{ overview?.risk_counts?.[tier] ?? '—' }}</b></span><small>{{ t('Not a model ranking or a success rate.', '不代表模型强弱或成功率。') }}</small></div>
          <article class="panel tier-panel">
            <div class="panel-head"><div><h2>{{ t('Three model combinations', '三档模型搭配') }}</h2><p>{{ t('Suggestions stay within your existing authorization ceiling.', 'Laya 在已有授权上限内建议，不扩大权限。') }}</p></div><button v-if="!editingTiers" class="button subtle" :disabled="!settings" @click="editTiers">{{ t('Configure', '配置') }}</button></div>
            <p v-if="advisorPreferences?.policy === 'auto'" class="muted">{{ t('Automatic ceiling', '自动建议上限') }}: {{ advisorPreferences.ceiling ? advisorPreferences.ceiling.model + ' / ' + advisorPreferences.ceiling.reasoning_effort : t('Not configured; confirmation required', '未配置，需先确认') }}</p>
            <form v-if="editingTiers" @submit.prevent="saveTiers">
              <div class="tier-grid"><div v-for="tier in tiers" :key="tier" class="tier-card"><strong>{{ label(tier) }}</strong><label>{{ t('Model', '模型') }}<input v-model="tierDraft[tier].model" :aria-label="label(tier) + ' / ' + t('Model', '模型')" maxlength="128" :placeholder="t('Exact model ID', '模型完整名称')"/></label><label>{{ t('Reasoning', '推理档位') }}<input v-model="tierDraft[tier].reasoning_effort" :aria-label="label(tier) + ' / ' + t('Reasoning', '推理档位')" maxlength="32" placeholder="low / medium / high"/></label></div></div>
              <p class="muted">{{ t('The host verifies availability at call time. An unsupported or over-ceiling combination cannot be used automatically. Clear both fields to leave a tier unconfigured.', '实际调用时由宿主校验。不可用或超出上限的组合不会自动采用；同时清空两项可取消配置。') }}</p>
              <p v-if="errors.tiers" class="form-error">{{ errors.tiers }}</p><div class="form-actions"><button type="button" class="button subtle" @click="editingTiers = false">{{ t('Cancel', '取消') }}</button><button class="button primary" :disabled="loading.tiers">{{ t('Save combinations', '保存搭配') }}</button></div>
            </form>
            <div v-else class="tier-grid"><div v-for="tier in tiers" :key="tier" class="tier-card"><strong>{{ label(tier) }}</strong><span>{{ tierSummary(tier) }}</span></div></div>
          </article>
          <article class="panel queue-panel">
            <div class="panel-head"><div><h2>{{ t('Cases to review', '需要关注的案例') }}</h2><p>{{ t('Uncertainty and problem feedback come first.', '优先关注不确定判断与问题反馈。') }}</p></div><select v-model="queueFilters.status" @change="applyQueueFilters" :aria-label="t('Case filter', '案例筛选')"><option value="pending">{{ t('Needs review', '待复核') }}</option><option value="all">{{ t('All decisions', '全部决策') }}</option><option value="finished">{{ t('Finished', '已完成') }}</option></select></div>
            <details class="disclosure"><summary>{{ t('More filters', '更多筛选') }}</summary><form class="filter queue-filters" @submit.prevent="applyQueueFilters"><label>{{ t('Risk', '风险') }}<select v-model="queueFilters.risk"><option value="">{{ t('All', '全部') }}</option><option v-for="tier in tiers" :key="tier" :value="tier">{{ label(tier) }}</option></select></label><label>{{ t('Host or role', '宿主或角色') }}<input v-model="queueFilters.source"/></label><button class="button subtle">{{ t('Apply', '应用') }}</button></form></details>
            <div v-if="loading.decisions" class="page-state">{{ t('Loading…', '加载中…') }}</div><div v-else-if="errors.decisions" class="inline-error">{{ errors.decisions }} <button @click="loadDecisions">{{ t('Retry', '重试') }}</button></div><div v-else-if="!decisions.length" class="empty-state"><span class="empty-glyph">✓</span><h3>{{ t('Nothing here yet', '暂无案例') }}</h3><p>{{ t('Collected decisions and Squad feedback appear here.', '采集到的决策与 Squad 反馈会显示在这里。') }}</p></div>
            <div v-else class="decision-list"><button v-for="decision in decisions" :key="itemId(decision)" class="decision-row" @click="openDecision(decision)"><span class="priority-mark" :class="'tier-' + (decision.review_priority || 0)"></span><span class="decision-copy"><strong>{{ decisionTitle(decision) }}</strong><small>{{ displayTime(decision.created_at) }}</small><ReviewSignals :decision="decision"/></span><span class="row-arrow">→</span></button></div>
            <div class="queue-pagination"><button class="button subtle" :disabled="loading.decisions || queueOffset === 0" @click="changeQueuePage(-1)">{{ t('Previous', '上一页') }}</button><span>{{ queueOffset / 50 + 1 }}</span><button class="button subtle" :disabled="loading.decisions || decisions.length < 50" @click="changeQueuePage(1)">{{ t('Next', '下一页') }}</button></div>
          </article>
        </section>
        <section v-if="activeTab === 'detail'" class="detail-page">
          <button class="text-button" @click="activeTab = 'queue'">← {{ t('Overview', '返回概览') }}</button>
          <p v-if="loading.detail">{{ t('Loading…', '加载中…') }}</p><p v-else-if="errors.detail" class="inline-error">{{ errors.detail }}</p>
          <template v-else-if="selectedDecision">
            <div class="detail-title"><h2>{{ decisionTitle(selectedDecision) }}</h2><ReviewSignals :decision="selectedDecision"/></div>
            <div class="detail-columns">
              <div class="evidence-column">
                <article class="panel evidence-card"><h3>{{ t('Laya’s original assessment', 'Laya 原始判断') }}</h3><div class="label-grid"><div v-for="key in ['complexity', 'risk', 'certainty']" :key="key"><span>{{ label(key) }}</span><strong>{{ label(originalLabels(selectedDecision)[key]) }}</strong></div></div><dl class="facts"><div><dt>{{ t('Recommended pair', '建议搭配') }}</dt><dd>{{ observedPair(selectedDecision.result?.advice?.recommendation) }}</dd></div><div><dt>{{ t('Latest execution', '最近实际执行') }}</dt><dd>{{ observedPair(selectedDecision.execution_attempts?.at(-1)?.effective) }}</dd></div></dl><p>{{ t('First feedback is retained alongside later corrections.', '首次反馈与后续修正独立保留，不覆盖。') }}</p><div v-for="feedback in (selectedDecision.feedback || []).slice(0, 3)" :key="feedback.event_id" class="feedback-summary"><strong>{{ feedback.source?.role || feedback.kind }}</strong><p>{{ feedback.payload?.summary || feedback.payload?.reason || feedback.payload?.outcome || feedback.payload?.result || t('Feedback recorded', '已记录反馈') }}</p><span v-for="(score, index) in feedback.payload?.scores || []" :key="index">{{ label(score.dimension) }}: {{ score.value ?? '—' }} · </span></div></article>
                <ExecutionEvidence :observations="selectedDecision.efficiency_observations"/>
                <details class="panel evidence-card" @toggle="rawEvidenceOpen = $event.target.open"><summary>{{ t('Original evidence & history', '原始证据与历史') }}</summary><p>{{ t('Original records are not translated or overwritten.', '原始记录不翻译、不覆盖。') }}</p><template v-if="rawEvidenceOpen"><details v-for="group in evidenceGroups" :key="group.label"><summary>{{ group.label }} · {{ group.items.length }}</summary><pre>{{ JSON.stringify(group.items, null, 2) }}</pre></details><details><summary>{{ t('Complete record', '完整记录') }}</summary><pre>{{ JSON.stringify(selectedDecision, null, 2) }}</pre></details></template></details>
              </div>
              <form class="panel review-form" @submit.prevent="saveReview">
                <div><h3>{{ t('Your judgment', '你的判断') }}</h3><p>{{ t('Choose a model tier. Original risk stays unchanged.', '选择适合的模型档次，不改变原始风险。') }}</p></div>
                <label>{{ t('Model tier', '模型档次') }}<select v-model="review.model_tier"><option value="">{{ t('No preference', '不指定') }}</option><option v-for="tier in tiers" :key="tier" :value="tier">{{ label(tier) }} · {{ tierSummary(tier) }}</option></select></label>
                <label>{{ t('Conclusion', '结论') }}<select v-model="review.status"><option v-for="value in ['confirmed','corrected','insufficient','excluded']" :key="value" :value="value">{{ label(value) }}</option></select></label>
                <label>{{ t('Note (optional)', '备注（选填）') }}<textarea v-model="review.reason" rows="2" :placeholder="t('Anything the evidence does not explain?', '有需要补充的依据吗？')"></textarea></label>
                <details class="disclosure"><summary>{{ t('Assessment details', '判断详情') }}</summary><div class="form-trio"><label v-for="key in ['complexity','risk','certainty']" :key="key">{{ label(key) }}<select v-model="review[key]"><option value="">{{ t('Unknown', '未知') }}</option><option v-for="value in key === 'certainty' ? ['clear','uncertain'] : tiers" :key="value" :value="value">{{ label(value) }}</option></select></label></div><p>{{ t('Changing model tier is a preference, not proof of lower task risk.', '模型档次是人工偏好，不是降低任务风险的证据。') }}</p></details>
                <details class="disclosure"><summary>{{ t('Source & reuse (advanced)', '来源与复用（高级）') }}</summary><label>{{ t('Task family', '任务类别') }}<input v-model="review.task_family" maxlength="64"/></label><label>{{ t('Task lineage (optional)', '任务来源标识（选填）') }}<input v-model="review.task_lineage" maxlength="128"/></label><p>{{ t('Leave unknown provenance blank. Never invent it to pass evaluation.', '未知来源请留空，不要编造标识以通过评估。') }}</p><label>{{ t('Case language', '案例语言') }}<select v-model="review.language"><option value="en">English</option><option value="zh">中文</option><option value="multilingual">{{ t('Multilingual', '多语言') }}</option><option value="other">{{ t('Other', '其他') }}</option></select></label><label>{{ t('Reuse scope', '复用范围') }}<select v-model="review.applicability"><option value="task-fact">{{ t('Task facts', '任务事实') }}</option><option value="configuration-dependent">{{ t('Depends on execution configuration', '依赖执行配置') }}</option></select></label><label v-if="review.applicability === 'configuration-dependent'">{{ t('Recorded execution', '已记录执行') }}<select v-model="review.validation_assignment_event_id"><option value="">{{ t('Unknown', '未知') }}</option><option v-for="attempt in selectedDecision.execution_attempts || []" :key="attempt.event_id" :value="attempt.event_id">{{ attempt.effective?.model || '—' }} / {{ attempt.effective?.reasoning_effort || '—' }} · {{ attempt.attempt_ref }}</option></select></label><label>{{ t('Reuse note (optional)', '复用说明（选填）') }}<textarea v-model="review.applicability_reason" rows="2"></textarea></label></details>
                <p v-if="errors.review" class="form-error">{{ errors.review }}</p><button class="button primary" :disabled="loading.review">{{ loading.review ? t('Saving…', '保存中…') : t('Save review', '保存复核') }}</button>
                <details class="disclosure"><summary>{{ t('Delete record', '删除记录') }}</summary><button class="button danger" type="button" @click="deleteDecision">{{ t('Delete decision…', '删除决策…') }}</button></details>
              </form>
            </div>
          </template>
        </section>
        <section v-if="activeTab === 'cases'" class="cases-page">
          <DateRangePicker v-model="dateRange"/>
          <p v-if="errors.dateRange" class="inline-error" role="alert">{{ errors.dateRange }}</p>
          <p>{{ t('Reviewed cases are kept here. Changes do not train the model or activate memory automatically.', '复核后的案例保存在这里；不会自动训练模型或启用记忆。') }}</p>
          <p>{{ t('The case list follows decision dates; memory and version management remains all time.', '案例列表按决策时间筛选；记忆与版本管理仍显示全部时间。') }}</p>
          <p v-if="errors.cases || errors.versions" class="inline-error">{{ errors.cases || errors.versions }}</p>
          <article class="panel evidence-card"><h2>{{ t('Reviewed cases', '已复核案例') }}</h2><p v-if="loading.cases">{{ t('Loading selected date range…', '正在加载所选日期范围…') }}</p><p v-else-if="!cases.length">{{ t('No reviewed cases yet.', '暂无已复核案例。') }}</p><div v-for="item in cases" :key="itemId(item)" class="case-row"><input v-model="selectedCases" type="checkbox" :value="itemId(item)" :aria-label="t('Select case', '选择案例')"/><span><strong>{{ item.summary || item.task_summary || itemId(item) }}</strong><small>{{ t('Decision date', '决策时间') }}: {{ displayTime(item.decision_created_at ?? item.created_at) }} · {{ t('Model tier', '模型档次') }}: {{ label(item.labels?.model_tier) }} · {{ t('Risk', '风险') }}: {{ label(item.labels?.risk) }}</small></span><button class="mini" @click="openDecision({ id: item.decision_id })">{{ t('Review', '复核') }}</button><button class="mini danger" @click="deleteCase(item)">{{ t('Delete…', '删除…') }}</button></div><small>{{ t('Latest 50 cases in this decision-date range. Source history remains available in decisions.', '显示此决策时间范围内最近 50 个案例，来源历史可在全部决策中查看。') }}</small></article>
          <details class="panel evidence-card"><summary>{{ t('Evaluate & publish case memory (advanced)', '评估与发布案例记忆（高级）') }}</summary><p>{{ t('Select cases, create a candidate, then evaluate. Only an eligible passing report permits activation.', '选择案例、创建候选、运行评估；只有符合要求的通过报告才允许启用。') }}</p><button class="button primary" :disabled="loading.createVersion || !selectedCases.length" @click="createVersion">{{ t('Create candidate', '创建候选版本') }} · {{ selectedCases.length }}</button><p v-if="errors.createVersion" class="form-error">{{ errors.createVersion }}</p><MemoryVersion v-for="item in versions" :key="itemId(item)" :item="item" :busy="loading['evaluate-' + itemId(item)] || loading['version-' + itemId(item)]" :error="errors['evaluate-' + itemId(item)] || errors['version-' + itemId(item)]" @evaluate="startEvaluation" @activate="activateVersion"/></details>
          <details class="panel evidence-card"><summary>{{ t('Background jobs', '后台任务') }} · {{ jobs.length }}</summary><div v-for="item in jobs" :key="itemId(item)" class="job-row"><span>{{ item.kind }} · {{ label(item.status) }}</span><button v-if="['queued','running'].includes(item.status)" class="mini" @click="cancelJob(item)">{{ t('Cancel', '取消') }}</button><a v-if="exportArtifact(item)" class="mini" :href="exportArtifact(item).href" :download="exportArtifact(item).name">{{ t('Download', '下载') }}</a></div></details>
        </section>
        <section v-if="activeTab === 'settings'" class="settings-page">
          <SquadRouting :preferences="advisorPreferences" @saved="advisorPreferences = $event"/>
          <p v-if="errors.settings || errors.status" class="inline-error">{{ errors.settings || errors.status }}</p><p v-if="restoreWarning" class="inline-error">{{ restoreWarning }}</p>
          <article class="panel setting-panel"><h2>{{ t('Collection & learning', '采集与学习') }}</h2><p>{{ t('Pausing collection keeps existing evidence.', '暂停采集不会删除现有证据。') }}</p><button v-for="item in [{key:'recording_enabled',en:'Collect decisions',zh:'采集新决策'},{key:'memory_enabled',en:'Use reviewed case memory',zh:'使用已评估的案例记忆'},{key:'replay_enabled',en:'Retry pending feedback',zh:'重试待送达反馈'}]" :key="item.key" class="switch-row" :disabled="!settings" :aria-pressed="settings?.[item.key]" @click="toggleSetting(item.key, t(item.en,item.zh))"><strong>{{ t(item.en,item.zh) }}</strong><span class="switch" :class="{on:settings?.[item.key]}"><i></i></span></button><p v-if="errors.saveSettings" class="form-error">{{ errors.saveSettings }}</p></article>
          <details class="panel setting-panel"><summary>{{ t('Storage & retention', '存储与保留') }}</summary><p>{{ displayBytes(status?.storage_bytes) }}</p><p v-if="status?.storage_pressure" class="inline-error">{{ t('Storage is above its soft limit. Protected evidence is retained.', '存储超过软上限，受保护证据不会自动删除。') }}</p><label>{{ t('Retention (days)', '保留天数') }}<input v-model="settingsDraft.retention_days" type="number" min="1"/></label><label>{{ t('Storage limit (bytes)', '存储软上限（字节）') }}<input v-model="settingsDraft.soft_limit_bytes" type="number" min="1048576"/></label><button class="button subtle" @click="saveLimits">{{ t('Save', '保存') }}</button></details>
          <details class="panel setting-panel"><summary>{{ t('Backup & export', '备份与导出') }}</summary><div class="form-actions"><button class="button subtle" @click="createBackup">{{ t('Create backup', '创建备份') }}</button><button class="button subtle" @click="startExport">{{ t('Export records', '导出记录') }}</button></div><div v-for="item in backups" :key="itemId(item)" class="backup-row"><span>{{ item.name || itemId(item) }} · {{ displayTime(item.created_at) }}</span><button class="mini danger" :disabled="item.complete === false" @click="restoreBackup(item)">{{ t('Restore…', '恢复…') }}</button><button class="mini danger" @click="deleteBackup(item)">{{ t('Delete…', '删除…') }}</button></div></details>
          <details class="panel setting-panel"><summary>{{ t('Diagnostics & pending evidence', '诊断与待送达证据') }}</summary><pre>{{ JSON.stringify(status, null, 2) }}</pre><details v-for="item in outbox" :key="item.event_id"><summary>{{ item.state }} · {{ item.event_id }}</summary><pre>{{ JSON.stringify(item, null, 2) }}</pre><button class="button subtle" @click="retryEvent(item)">{{ t('Retry unchanged event', '重试原始事件') }}</button></details></details>
        </section>
      </template>
    </main>
    <transition name="toast"><div v-if="notice" class="toast" role="status">{{ notice }}</div></transition>
  </div>
</template>
