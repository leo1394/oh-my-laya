<script setup>
import { computed, onBeforeUnmount, onMounted, reactive, ref, watch } from 'vue'

import { api, eventsUrl, jsonBody } from './api.js'
import ReviewSignals from './ReviewSignals.vue'
import {
  decisionQuery,
  decisionTitle,
  displayBytes,
  displayTime,
  errorMessage,
  exportArtifact,
  itemId,
  normalizeList,
  queueNeedsRefresh,
  reviewSignals
} from './utils.js'

const tabs = [
  { id: 'queue', number: '01', label: 'Review queue' },
  { id: 'detail', number: '02', label: 'Decision detail' },
  { id: 'cases', number: '03', label: 'Cases & versions' },
  { id: 'settings', number: '04', label: 'Settings & status' }
]

const activeTab = ref('queue')
const status = ref(null)
const decisions = ref([])
const selectedDecision = ref(null)
const cases = ref([])
const versions = ref([])
const jobs = ref([])
const settings = ref(null)
const backups = ref([])
const restoreWarning = ref('')
const outbox = ref([])
const selectedCases = ref([])
const queueFilters = reactive({ status: 'pending', risk: '', source: '', after: '', before: '' })
const queueOffset = ref(0)
const loading = reactive({})
const errors = reactive({})
const notice = ref('')
const pairing = ref(false)
const eventState = ref('connecting')
const settingsDraft = reactive({ retention_days: 30, soft_limit_bytes: 524288000 })
const review = reactive({
  status: 'confirmed',
  complexity: '',
  risk: '',
  certainty: '',
  task_family: 'general',
  language: 'en',
  applicability: 'task-fact',
  validation_assignment_event_id: '',
  applicability_reason: '',
  reason: ''
})
let events
let noticeTimer
let refreshTimer
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
    { label: 'Snapshots', items: decision.snapshots },
    { label: 'Execution attempts', items: decision.execution_attempts },
    { label: 'Model observations', items: decision.model_observations },
    { label: 'Feedback', items: decision.feedback },
    { label: 'Review revisions', items: decision.reviews }
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

async function loadDecisions() {
  const payload = await load('decisions', () => api(`/decisions?${decisionQuery(queueFilters, queueOffset.value)}`))
  if (payload) decisions.value = normalizeList(payload)
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
  const id = itemId(decision)
  if (!id) return
  activeTab.value = 'detail'
  selectedDecision.value = null
  const payload = await load('detail', () => api(`/decisions/${encodeURIComponent(id)}`))
  if (!payload) return
  selectedDecision.value = payload.item || payload.decision || payload
  const reviewHistory = Array.isArray(selectedDecision.value.reviews) ? selectedDecision.value.reviews : []
  const latestReview = reviewHistory[reviewHistory.length - 1]
  const labels = selectedDecision.value.labels || latestReview?.labels || selectedDecision.value.review?.labels || {}
  review.status = latestReview?.status || selectedDecision.value.review?.status || 'confirmed'
  review.complexity = labels.complexity || ''
  review.risk = labels.risk || ''
  review.certainty = labels.certainty || ''
  review.task_family = latestReview?.task_family || selectedDecision.value.task_family || 'general'
  const requestText = JSON.stringify(selectedDecision.value.request || '')
  review.language = latestReview?.language || selectedDecision.value.language || (/\p{Script=Han}/u.test(requestText) ? 'zh' : 'en')
  review.reason = ''
  review.applicability = latestReview?.applicability || 'task-fact'
  review.validation_assignment_event_id = latestReview?.validation_assignment_event_id || ''
  review.applicability_reason = latestReview?.applicability_reason || ''
}

async function loadCases() {
  const [casePayload, versionPayload, jobPayload] = await Promise.all([
    load('cases', () => api('/cases')),
    load('versions', () => api('/memory-versions')),
    load('jobs', () => api('/jobs'))
  ])
  if (casePayload) cases.value = normalizeList(casePayload)
  if (versionPayload) versions.value = normalizeList(versionPayload)
  if (jobPayload) jobs.value = normalizeList(jobPayload)
  selectedCases.value = selectedCases.value.filter((id) => cases.value.some((item) => itemId(item) === id))
}

async function loadSettings() {
  const [settingsPayload, backupsPayload, outboxPayload] = await Promise.all([
    load('settings', () => api('/settings')),
    load('backups', () => api('/backups')),
    load('outbox', () => api('/outbox'))
  ])
  if (outboxPayload) outbox.value = normalizeList(outboxPayload)
  if (settingsPayload) {
    settings.value = settingsPayload.settings || settingsPayload
    settingsDraft.retention_days = settings.value.retention_days ?? 30
    settingsDraft.soft_limit_bytes = settings.value.storage_soft_limit_bytes ?? settings.value.soft_limit_bytes ?? 524288000
  }
  if (backupsPayload) backups.value = normalizeList(backupsPayload)
}

async function loadAll() {
  await Promise.all([loadStatus(), loadDecisions(), loadCases(), loadSettings()])
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
  if (!review.complexity || !review.risk || !review.certainty || !review.reason.trim()) {
    errors.review = 'Choose all three labels and add a concise evidence-based reason.'
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
      labels: {
        complexity: review.complexity,
        risk: review.risk,
        certainty: review.certainty
      },
      task_family: review.task_family.trim() || 'general',
      language: review.language,
      applicability: review.applicability,
      validation_assignment_event_id: review.applicability === 'configuration-dependent' ? review.validation_assignment_event_id || null : null,
      applicability_reason: review.applicability_reason.trim() || null,
      reason: review.reason.trim()
    })
  }, 'Review saved as a new revision.', async () => {
    await Promise.all([openDecision(selectedDecision.value), loadDecisions(), loadStatus()])
  })
}

async function deleteDecision() {
  if (!selectedDecision.value) return
  const id = itemId(selectedDecision.value)
  if (!window.confirm('Delete this decision, its linked local evidence, and all managed export files? Existing backups and external copies are not erased; remove managed backups separately in Settings. This cannot be undone.')) return
  await mutate('deleteDecision', `/decisions/${encodeURIComponent(id)}`, {
    method: 'DELETE'
  }, 'Decision deleted.', async () => {
    selectedDecision.value = null
    activeTab.value = 'queue'
    await Promise.all([loadDecisions(), loadStatus()])
  })
}

async function deleteCase(item) {
  const id = itemId(item)
  if (!window.confirm('Delete this reviewed case and all managed export files? Active versions may be invalidated. Existing backups and external copies are not erased; remove managed backups separately in Settings.')) return
  await mutate(`case-${id}`, `/cases/${encodeURIComponent(id)}`, {
    method: 'DELETE'
  }, 'Case deleted.', loadCases)
}

async function createVersion() {
  if (!selectedCases.value.length) {
    errors.createVersion = 'Select at least one reviewed case.'
    return
  }
  await mutate('createVersion', '/memory-versions', {
    method: 'POST', body: jsonBody({ case_ids: selectedCases.value })
  }, 'Candidate memory version created.', loadCases)
}

async function activateVersion(item) {
  const id = itemId(item)
  if (!window.confirm('Activate this evaluated memory version for future advisor decisions?')) return
  await mutate(`version-${id}`, `/memory-versions/${encodeURIComponent(id)}/activate`, {
    method: 'POST', body: jsonBody({})
  }, 'Memory version activated.', async () => Promise.all([loadCases(), loadStatus()]))
}

async function startEvaluation(item) {
  const id = itemId(item)
  await mutate(`evaluate-${id}`, '/jobs', {
    method: 'POST', body: jsonBody({ kind: 'evaluation', version_id: id })
  }, 'Evaluation queued.', loadCases)
}

async function cancelJob(item) {
  const id = itemId(item)
  await mutate(`job-${id}`, `/jobs/${encodeURIComponent(id)}/cancel`, {
    method: 'POST', body: jsonBody({})
  }, 'Cancellation requested.', loadCases)
}

async function toggleSetting(field, label) {
  if (!settings.value) return
  const next = !settings.value[field]
  const action = next ? 'enable' : 'disable'
  if (!window.confirm(`${action[0].toUpperCase()}${action.slice(1)} ${label}? This changes what future tasks may store or use.`)) return
  await patchSettings({ [field]: next }, `${label} ${next ? 'enabled' : 'disabled'}.`)
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
    errors.saveSettings = 'Retention must be a positive whole day and the soft limit at least 1 MiB.'
    return
  }
  await patchSettings({ retention_days: retention, storage_soft_limit_bytes: softLimit }, 'Retention and storage limits saved.')
}

async function createBackup() {
  await mutate('createBackup', '/backups', {
    method: 'POST', body: jsonBody({})
  }, 'Backup created.', loadSettings)
}

async function restoreBackup(item) {
  const id = itemId(item)
  if (!window.confirm('Restore this backup? Current writes will pause and current state will be replaced after validation.')) return
  restoreWarning.value = ''
  const result = await mutate(`backup-${id}`, `/backups/${encodeURIComponent(id)}/restore`, {
    method: 'POST', body: jsonBody({})
  }, 'Backup restored.', loadAll)
  if (result?.safety_backup_complete === false) restoreWarning.value = 'Restored from the verified backup. The previous state contained missing or damaged evidence; its safety archive is incomplete and cannot be restored automatically.'
}

async function deleteBackup(item) {
  const id = itemId(item)
  if (!window.confirm('Permanently delete this managed backup? This cannot be undone and does not erase external copies.')) return
  await mutate(`backup-${id}`, `/backups/${encodeURIComponent(id)}`, {
    method: 'DELETE'
  }, 'Backup deleted.', loadSettings)
}

async function retryEvent(item) {
  if (!window.confirm('Retry this unchanged event? Invalid data stays quarantined; the original score will not be rewritten.')) return
  await mutate(`retry-${item.event_id}`, `/outbox/${encodeURIComponent(item.event_id)}/retry`, {
    method: 'POST', body: jsonBody({})
  }, 'Unchanged event queued for retry.', loadSettings)
}

async function startExport() {
  await mutate('export', '/jobs', {
    method: 'POST', body: jsonBody({ kind: 'export' })
  }, 'Export job queued.', loadCases)
}

async function pairFromFragment() {
  const fragment = new URLSearchParams(window.location.hash.slice(1))
  const code = fragment.get('pair')
  if (!code) return true
  pairing.value = true
  try {
    await api('/pair', { method: 'POST', body: jsonBody({ code }) })
    history.replaceState(null, '', `${window.location.pathname}${window.location.search}`)
    showNotice('Workbench paired with the local service.')
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
  const lastEventId = sessionStorage.getItem('laya-last-event-id') || ''
  events = new EventSource(eventsUrl(lastEventId), { withCredentials: true })
  events.onopen = () => { eventState.value = 'live' }
  events.onerror = () => { eventState.value = 'reconnecting' }
  const handleEvent = (event) => {
    if (event.lastEventId) sessionStorage.setItem('laya-last-event-id', event.lastEventId)
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
      loadStatus()
      if (queueNeedsRefresh(changes)) loadDecisions()
      if (changes.includes('case') || changes.includes('memory') || changes.includes('version') || changes.includes('job')) loadCases()
      if (changes.includes('setting') || changes.includes('backup') || changes.includes('feedback')) loadSettings()
    }, 100)
  }
  events.onmessage = handleEvent
  events.addEventListener('change', handleEvent)
}

watch(activeTab, (tab) => {
  if (tab === 'queue') loadDecisions()
  if (tab === 'cases') loadCases()
  if (tab === 'settings') loadSettings()
})

onMounted(async () => {
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
      <div class="brand">
        <svg class="brand-mark" viewBox="0 0 48 48" aria-hidden="true">
          <path d="M8 14 18 5l2 10h8L30 5l10 9-3 20-13 9-13-9Z" />
          <path class="brand-mark-cut" d="m16 25 5 2-4 3m15-5-5 2 4 3M21 35h6" />
        </svg>
        <div><strong>Laya</strong><span>Decision workbench</span></div>
      </div>

      <nav aria-label="Workbench sections">
        <button
          v-for="tab in tabs"
          :key="tab.id"
          class="nav-item"
          :class="{ active: activeTab === tab.id }"
          :aria-current="activeTab === tab.id ? 'page' : undefined"
          @click="activeTab = tab.id"
        >
          <span>{{ tab.number }}</span>{{ tab.label }}
        </button>
      </nav>

      <div class="rail-status">
        <span class="signal" :class="serviceHealthy ? 'ok' : 'warn'"></span>
        <div><strong>{{ serviceHealthy ? 'Service ready' : 'Service unavailable' }}</strong>
          <span>{{ eventState === 'live' ? 'Live updates connected' : `Events: ${eventState}` }}</span>
        </div>
      </div>
      <p class="rail-note">Local evidence stays on this machine unless you explicitly export it.</p>
    </aside>

    <main>
      <header class="topbar">
        <div>
          <p class="eyebrow">Local learning loop / {{ tabs.find((tab) => tab.id === activeTab)?.number }}</p>
          <h1>{{ tabs.find((tab) => tab.id === activeTab)?.label }}</h1>
        </div>
        <div class="top-actions">
          <div class="live-pill"><span></span>{{ eventState }}</div>
          <button class="icon-button" aria-label="Refresh current data" @click="loadAll">↻</button>
        </div>
      </header>

      <div v-if="errors.pairing" class="fatal-state">
        <span>Pairing failed</span><h2>Workbench access was not established.</h2><p>{{ errors.pairing }}</p>
      </div>
      <div v-else-if="pairing" class="page-state"><span class="spinner"></span>Pairing with the local service…</div>

      <template v-else>
        <section v-if="activeTab === 'queue'" class="page-grid queue-page">
          <div class="summary-strip">
            <article><span>Awaiting review</span><strong>{{ pendingCount }}</strong></article>
            <article><span>Active memory</span><strong>{{ activeVersion }}</strong></article>
            <article><span>Worker</span><strong>{{ workerLabel }}</strong></article>
          </div>

          <div class="panel queue-panel">
            <div class="panel-head">
              <div><p class="eyebrow">Evidence inbox</p><h2>Decisions needing a human signal</h2></div>
              <form class="filter queue-filters" @submit.prevent="applyQueueFilters">
                <label>Status <select v-model="queueFilters.status" aria-label="Status"><option value="pending">Awaiting review</option><option value="all">All decisions</option><option value="finished">Finished</option></select></label>
                <label>Risk <select v-model="queueFilters.risk" aria-label="Risk"><option value="">All risks</option><option value="low">Low</option><option value="medium">Medium</option><option value="high">High</option></select></label>
                <label>Source <input v-model="queueFilters.source" placeholder="Exact host or role" /></label>
                <label>From (local time) <input v-model="queueFilters.after" type="datetime-local" /></label>
                <label>To (local time) <input v-model="queueFilters.before" type="datetime-local" /></label>
                <button class="button subtle" type="submit">Apply</button>
              </form>
            </div>
            <div v-if="loading.decisions" class="page-state"><span class="spinner"></span>Loading review queue…</div>
            <div v-else-if="errors.decisions" class="inline-error"><strong>Queue unavailable.</strong> {{ errors.decisions }} <button @click="loadDecisions">Retry</button></div>
            <div v-else-if="!decisions.length" class="empty-state">
              <span class="empty-glyph">✓</span><h3>No decisions match this view</h3><p>New uncertain, high-risk, or disputed decisions will appear here.</p>
            </div>
            <div v-else class="decision-list">
              <button v-for="decision in decisions" :key="itemId(decision)" class="decision-row" @click="openDecision(decision)">
                <span class="priority-mark" :class="`tier-${decision.review_priority || 0}`"></span>
                <span class="decision-copy"><strong>{{ decisionTitle(decision) }}</strong><small>{{ reviewSignals(decision).sources.join(', ') || 'No feedback source yet' }} · {{ displayTime(decision.created_at || decision.observed_at) }}</small><ReviewSignals :decision="decision" /></span>
                <span class="row-arrow">→</span>
              </button>
            </div>
            <div class="queue-pagination">
              <button class="button subtle" :disabled="loading.decisions || queueOffset === 0" @click="changeQueuePage(-1)">Previous</button>
              <span>Page {{ queueOffset / 50 + 1 }} · {{ decisions.length }} decisions</span>
              <button class="button subtle" :disabled="loading.decisions || decisions.length < 50" @click="changeQueuePage(1)">Next</button>
            </div>
          </div>
        </section>

        <section v-if="activeTab === 'detail'" class="detail-page">
          <div v-if="loading.detail" class="page-state"><span class="spinner"></span>Loading decision evidence…</div>
          <div v-else-if="errors.detail" class="inline-error"><strong>Decision unavailable.</strong> {{ errors.detail }}</div>
          <div v-else-if="!selectedDecision" class="empty-state large">
            <span class="empty-glyph">↖</span><h2>Select a decision from the review queue</h2><p>The request, original assessment, execution evidence, and review history will be shown together.</p><button class="button primary" @click="activeTab = 'queue'">Open review queue</button>
          </div>
          <template v-else>
            <div class="detail-title">
              <button class="text-button" @click="activeTab = 'queue'">← Queue</button>
              <p class="eyebrow">Decision {{ itemId(selectedDecision) }}</p>
              <h2>{{ decisionTitle(selectedDecision) }}</h2>
              <ReviewSignals :decision="selectedDecision" show-events />
              <div class="tags"><em>{{ selectedDecision.status || 'pending' }}</em><em v-if="selectedDecision.recording_status">{{ selectedDecision.recording_status }}</em></div>
            </div>
            <div class="detail-columns">
              <div class="evidence-column">
                <article class="panel evidence-card">
                  <div class="panel-head compact"><div><p class="eyebrow">Original signal</p><h3>Laya assessment</h3></div><span class="revision">{{ selectedDecision.reviews?.length || 0 }} review revisions</span></div>
                  <div class="label-grid">
                    <div v-for="key in ['complexity', 'risk', 'certainty']" :key="key"><span>{{ key }}</span><strong>{{ selectedDecision.result?.advice?.assessment?.[key]?.choice || selectedDecision.advice?.assessment?.[key]?.choice || selectedDecision.labels?.[key] || 'Unknown' }}</strong><small v-if="(selectedDecision.result?.advice?.assessment?.[key]?.confidence ?? selectedDecision.advice?.assessment?.[key]?.confidence) != null">confidence {{ selectedDecision.result?.advice?.assessment?.[key]?.confidence ?? selectedDecision.advice.assessment[key].confidence }}</small></div>
                  </div>
                  <dl class="facts"><div><dt>Recommended</dt><dd>{{ selectedDecision.result?.advice?.recommendation?.model || selectedDecision.advice?.recommendation?.model || selectedDecision.recommended_model || 'None' }}</dd></div><div><dt>Effective model</dt><dd>{{ selectedDecision.context?.effective_model || selectedDecision.effective_model || selectedDecision.execution?.effective_model || 'Unverified' }}</dd></div><div><dt>Memory cases</dt><dd>{{ selectedDecision.result?.meta?.case_ids?.join(', ') || selectedDecision.meta?.case_ids?.join(', ') || selectedDecision.case_ids?.join(', ') || 'None' }}</dd></div><div><dt>First score</dt><dd>{{ JSON.stringify(selectedDecision.feedback?.[0]?.payload?.score ?? selectedDecision.feedback?.[0]?.payload?.scores ?? selectedDecision.feedback?.[0]?.score ?? 'No score recorded') }}</dd></div></dl>
                </article>
                <article class="panel evidence-card">
                  <div class="panel-head compact"><div><p class="eyebrow">Collected record</p><h3>Evidence & revisions</h3></div></div>
                  <div v-if="evidenceGroups.length" class="evidence-groups">
                    <section v-for="group in evidenceGroups" :key="group.label" class="evidence-block">
                      <h4>{{ group.label }} <span>{{ group.items.length }}</span></h4>
                      <pre>{{ JSON.stringify(group.items, null, 2) }}</pre>
                    </section>
                  </div>
                  <pre v-else>{{ JSON.stringify(selectedDecision.evidence || selectedDecision, null, 2) }}</pre>
                </article>
              </div>
              <form class="panel review-form" @submit.prevent="saveReview">
                <div><p class="eyebrow">Human conclusion</p><h3>Record a review revision</h3><p>Use observed evidence. A successful test alone does not prove the original risk judgment was correct.</p></div>
                <label>Status<select v-model="review.status"><option value="confirmed">Confirmed</option><option value="corrected">Corrected</option><option value="insufficient">Evidence insufficient</option><option value="excluded">Exclude sample</option></select></label>
                <div class="form-trio">
                  <label>Complexity<select v-model="review.complexity"><option value="">Choose…</option><option value="low">Low</option><option value="medium">Medium</option><option value="high">High</option></select></label>
                  <label>Risk<select v-model="review.risk"><option value="">Choose…</option><option value="low">Low</option><option value="medium">Medium</option><option value="high">High</option></select></label>
                  <label>Certainty<select v-model="review.certainty"><option value="">Choose…</option><option value="clear">Clear</option><option value="uncertain">Uncertain</option></select></label>
                </div>
                <div class="form-pair">
                  <label>Task family<input v-model="review.task_family" placeholder="general" /></label>
                  <label>Language<select v-model="review.language"><option value="en">English</option><option value="zh">Chinese</option><option value="multilingual">Multilingual</option><option value="other">Other</option></select></label>
                </div>
                <label>Case applicability<select v-model="review.applicability"><option value="task-fact">Task fact — independent of execution model</option><option value="configuration-dependent">Execution configuration dependent</option></select></label>
                <label v-if="review.applicability === 'configuration-dependent'">Recorded execution assignment<select v-model="review.validation_assignment_event_id"><option value="">Unknown — retain as historical evidence</option><option v-for="attempt in selectedDecision.execution_attempts || []" :key="attempt.event_id" :value="attempt.event_id">{{ attempt.attempt_ref }} · {{ attempt.effective?.model || 'Unverified model' }} / {{ attempt.effective?.reasoning_effort || 'Unknown effort' }} · {{ attempt.event_id }}</option></select><small>The service validates recorded identity, environment and score provenance. Choosing an assignment does not certify missing evidence.</small></label>
                <label>Applicability reason<textarea v-model="review.applicability_reason" rows="2" placeholder="Which execution conditions limit reuse?"></textarea></label>
                <label>Reason<textarea v-model="review.reason" rows="5" placeholder="What evidence supports this conclusion?"></textarea></label>
                <div v-if="errors.review" class="form-error">{{ errors.review }}</div>
                <div class="form-actions"><button class="button danger ghost" type="button" @click="deleteDecision">Delete decision</button><button class="button primary" :disabled="loading.review">{{ loading.review ? 'Saving…' : 'Save revision' }}</button></div>
              </form>
            </div>
          </template>
        </section>

        <section v-if="activeTab === 'cases'" class="cases-page">
          <div class="split-heading"><div><p class="eyebrow">Memory is reviewed context, not training</p><h2>Build, evaluate, then activate</h2></div><button class="button subtle" @click="loadCases">Refresh records</button></div>
          <div v-if="errors.cases || errors.versions" class="inline-error"><strong>Some memory data is unavailable.</strong> {{ errors.cases || errors.versions }}</div>
          <div class="cases-layout">
            <article class="panel">
              <div class="panel-head"><div><p class="eyebrow">Reviewed source material</p><h3>Cases</h3></div><span class="count">{{ selectedCases.length }} selected</span></div>
              <div v-if="loading.cases" class="page-state"><span class="spinner"></span>Loading cases…</div>
              <div v-else-if="!cases.length" class="empty-state compact"><h3>No reviewed cases yet</h3><p>Confirmed or corrected reviews with complete labels can become candidate cases.</p></div>
              <div v-else class="case-list">
                <label v-for="item in cases" :key="itemId(item)" class="case-row">
                  <input v-model="selectedCases" type="checkbox" :value="itemId(item)" />
                  <span><strong>{{ item.summary || item.task_summary || itemId(item) }}</strong><small>{{ item.task_family || item.language || 'General' }} · {{ item.status || item.applicability || 'reviewed' }}</small><small v-if="item.applicability === 'configuration-dependent'">Configuration evidence: {{ item.verification_status || 'unknown' }} · Last validated: {{ displayTime(item.last_validated_at) }}</small><small v-if="item.applicability_reason">{{ item.applicability_reason }}</small></span>
                  <button class="mini danger" type="button" @click.prevent="deleteCase(item)">Delete</button>
                </label>
              </div>
              <div v-if="errors.createVersion" class="form-error">{{ errors.createVersion }}</div>
              <button class="button primary full" :disabled="loading.createVersion || !cases.length" @click="createVersion">Freeze selected as candidate</button>
            </article>
            <div class="stack">
              <article class="panel">
                <div class="panel-head"><div><p class="eyebrow">Immutable candidates</p><h3>Memory versions</h3></div></div>
                <div v-if="loading.versions" class="page-state"><span class="spinner"></span>Loading versions…</div>
                <div v-else-if="!versions.length" class="empty-state compact"><h3>No memory versions</h3><p>Select reviewed cases to create the first candidate.</p></div>
                <div v-else class="version-list">
                  <div v-for="item in versions" :key="itemId(item)" class="version-row">
                    <div><strong>{{ item.name || itemId(item) }}</strong><small>{{ item.case_count ?? item.case_ids?.length ?? '—' }} cases · {{ displayTime(item.created_at) }}</small></div>
                    <span class="status-stamp" :class="item.status">{{ item.status || 'candidate' }}</span>
                    <div class="row-actions"><button class="mini" @click="startEvaluation(item)">Evaluate</button><button class="mini accent" :disabled="!['evaluated', 'ready', 'active'].includes(item.status)" @click="activateVersion(item)">Activate</button></div>
                  </div>
                </div>
              </article>
              <article class="panel">
                <div class="panel-head"><div><p class="eyebrow">Background work</p><h3>Jobs</h3></div></div>
                <div v-if="!jobs.length" class="empty-state compact"><p>No evaluations or exports are queued.</p></div>
                <div v-else class="job-list"><div v-for="item in jobs" :key="itemId(item)" class="job-row"><span><strong>{{ item.kind || 'Job' }}</strong><small v-if="exportArtifact(item)">{{ exportArtifact(item).recordCount ?? '—' }} records · {{ exportArtifact(item).mediaType }}</small><small v-else>{{ itemId(item) }}</small></span><span>{{ typeof item.progress === 'number' ? `${item.progress}%` : item.status }}</span><button v-if="['queued', 'running'].includes(item.status)" class="mini" @click="cancelJob(item)">Cancel</button><a v-else-if="exportArtifact(item)" class="mini accent" :href="exportArtifact(item).href" :download="exportArtifact(item).name">Download</a></div></div>
              </article>
            </div>
          </div>
        </section>

        <section v-if="activeTab === 'settings'" class="settings-page">
          <div class="summary-strip status-strip">
            <article><span>Service</span><strong>{{ serviceHealthy ? 'Ready' : (status?.status || status?.service_status || 'Unknown') }}</strong></article>
            <article><span>Worker</span><strong>{{ workerLabel }}</strong></article>
            <article><span>Storage</span><strong>{{ displayBytes(status?.storage_bytes ?? status?.database_bytes) }}</strong></article>
            <article><span>Pending review</span><strong>{{ status?.counts?.pending_reviews ?? '—' }}</strong></article>
          </div>
          <div v-if="errors.settings || errors.status" class="inline-error"><strong>Settings or status unavailable.</strong> {{ errors.settings || errors.status }}</div>
          <div v-if="status?.evidence_cleanup_error" class="inline-error"><strong>Evidence cleanup pending.</strong> {{ status.evidence_cleanup_error }} Retry the deletion after resolving the filesystem problem; the service also retries on restart.</div>
          <div v-if="restoreWarning" class="inline-error">{{ restoreWarning }}</div>
          <div class="settings-grid">
            <article class="panel setting-panel">
              <div><p class="eyebrow">Consent controls</p><h3>Collection & memory</h3><p>These switches affect future decisions. Pausing collection does not delete existing records.</p></div>
              <button class="switch-row" :disabled="!settings" :aria-pressed="settings?.recording_enabled" @click="toggleSetting('recording_enabled', 'decision recording')"><span><strong>Decision recording</strong><small>Store redacted requests, outputs, and linked evidence.</small></span><span class="switch" :class="{ on: settings?.recording_enabled }"><i></i></span></button>
              <button class="switch-row" :disabled="!settings" :aria-pressed="settings?.memory_enabled" @click="toggleSetting('memory_enabled', 'case memory')"><span><strong>Case memory</strong><small>Allow the active reviewed version to inform advisor calls.</small></span><span class="switch" :class="{ on: settings?.memory_enabled }"><i></i></span></button>
              <button class="switch-row" :disabled="!settings" :aria-pressed="settings?.replay_enabled" @click="toggleSetting('replay_enabled', 'pending evidence delivery')"><span><strong>Pending evidence delivery</strong><small>Pause retries without deleting durable queued evidence.</small></span><span class="switch" :class="{ on: settings?.replay_enabled }"><i></i></span></button>
              <div v-if="errors.saveSettings" class="form-error">{{ errors.saveSettings }}</div>
            </article>
            <article class="panel setting-panel">
              <div><p class="eyebrow">Bounded storage</p><h3>Retention & limits</h3></div>
              <p v-if="status?.storage_pressure" class="inline-error">Storage exceeds its soft limit. Protected evidence will not be removed automatically.</p>
              <label>Ordinary record retention (days)<input v-model="settingsDraft.retention_days" type="number" min="1" step="1" /></label>
              <label>Storage soft limit (bytes)<input v-model="settingsDraft.soft_limit_bytes" type="number" min="1048576" step="1048576" /><small>{{ displayBytes(settingsDraft.soft_limit_bytes) }}</small></label>
              <button class="button primary" :disabled="loading.saveSettings" @click="saveLimits">Save limits</button>
            </article>
            <article class="panel backup-panel">
              <div class="panel-head"><div><p class="eyebrow">Recovery</p><h3>Backups</h3></div><button class="button subtle" @click="createBackup">Create backup</button></div>
              <div v-if="loading.backups" class="page-state"><span class="spinner"></span>Loading backups…</div>
              <div v-else-if="!backups.length" class="empty-state compact"><p>No managed backups are available.</p></div>
              <div v-else class="backup-list"><div v-for="item in backups" :key="itemId(item)" class="backup-row"><span><strong>{{ item.name || itemId(item) }}</strong><small>{{ displayTime(item.created_at) }} · {{ item.complete === false ? 'Incomplete safety archive — damaged evidence preserved where available' : displayBytes(item.size_bytes) }}</small></span><button class="mini danger" :disabled="item.complete === false" @click="restoreBackup(item)">Restore…</button><button class="mini danger" @click="deleteBackup(item)">Delete…</button></div></div>
              <div class="export-row"><div><strong>Portable export</strong><small>Creates a background export job; it does not upload data.</small></div><button class="button subtle" @click="startExport">Start export</button></div>
            </article>
            <article class="panel diagnostic-panel">
              <p class="eyebrow">Durable delivery</p><h3>Pending evidence</h3>
              <p v-if="!outbox.length">No pending or quarantined feedback.</p>
              <p v-if="errors.outbox" class="inline-error">{{ errors.outbox }}</p>
              <details v-for="item in outbox" :key="item.event_id"><summary>{{ item.state }} · {{ item.event_id }}</summary><pre>{{ JSON.stringify(item, null, 2) }}</pre><button class="button subtle" @click="retryEvent(item)">Retry unchanged event</button></details>
            </article>
            <article class="panel diagnostic-panel">
              <p class="eyebrow">Observed service state</p><h3>Diagnostics</h3><pre>{{ JSON.stringify(status, null, 2) }}</pre>
            </article>
          </div>
        </section>
      </template>
    </main>

    <transition name="toast"><div v-if="notice" class="toast" role="status"><span>✓</span>{{ notice }}</div></transition>
  </div>
</template>
