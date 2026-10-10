<script setup>
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { api } from './api.js'
import { label, locale, t } from './i18n.js'
import { activityBounds, activityCharts, activityLogTime, activityPair, activityPath, activityRecommendation, activityRequestGate, activityTime, activityWorker, pausedSnapshotSafe } from './liveActivity.js'

const props = defineProps({ eventVersion: Number, eventState: String, resetVersion: Number, range: Object, embedded: Boolean })
const emit = defineEmits(['open', 'clock'])
const displayed = ref(null)
const paused = ref(false)
const loading = ref(true)
const error = ref('')
const refreshedAt = ref(null)
const now = ref(Date.now())
const searchInput = ref('')
const search = ref('')
const page = ref(0)
const logPanel = ref(null)
const fullscreen = ref(false)
let searchTimer
const pageCount = computed(() => Math.max(1, Math.ceil((displayed.value?.pagination?.total || 0) / 10)))
const bounds = computed(() => activityBounds(props.range))
const charts = computed(() => activityCharts(displayed.value, (refreshedAt.value || now.value) / 1000))
const chartTitle = computed(() => charts.value.singleDay ? t('Recorded decisions on selected date', '所选日期的已记录决策') : t('Recorded decisions in selected range', '所选日期范围的已记录决策'))
const rows = computed(() => Array.isArray(displayed.value?.items) ? displayed.value.items.slice(0, 50) : [])
const number = value => Number.isSafeInteger(value) && value >= 0 ? value.toLocaleString(locale.value) : '—'
const pair = activityPair
const dispatchStatus = value => value === 'started' ? t('Started', '已启动') : value === 'failed' ? label('failed') : value === 'unknown' ? label('unknown') : t('Not recorded', '未记录')
const decisionStatus = value => value === 'pending' ? t('Waiting for result', '等待结果') : label(value)
const modeLabel = value => ({ direct: t('Direct', '直接处理'), delegate: t('Delegate', '委派'), needs_context: t('Needs context', '需要上下文') })[value] || value
const workerState = computed(() => activityWorker(displayed.value, props.eventState, error.value))
const workerStatus = computed(() => ({ error: t('Activity unavailable', '活动不可用'), unloaded: t('Unloaded / unknown', '未加载 / 未知'), busy: t('Busy or queued', '忙碌或排队'), idle: t('Idle', '空闲') })[workerState.value])
const workerHint = computed(() => `${workerStatus.value} · ${t('Queued work', '排队工作')}: ${number(displayed.value?.worker?.queued)}`)
const chartTime = point => charts.value.singleDay ? `${String(Math.floor(Math.floor(point.x * 60 / 25) / 60)).padStart(2, '0')}:${String(Math.floor(point.x * 60 / 25) % 60).padStart(2, '0')}` : logTime(bounds.value.start + Math.floor(point.x * (bounds.value.end - bounds.value.start) / 600)).slice(0, 10)
const middleTick = maximum => maximum > 1 ? maximum / 2 : ''
const list = value => Array.isArray(value) && value.length ? value.join(', ') : t('None recorded', '无记录')
const time = activityTime
const logTime = activityLogTime
let latest = null
let latestAt = null
let timer
let throttle
const requestGate = activityRequestGate()
let lastStart = 0

async function refresh(force = false) {
  if (!bounds.value) return
  if (typeof document !== 'undefined' && document.hidden && !force) return
  const elapsed = Date.now() - lastStart
  if (!force && elapsed < 1000) {
    if (!throttle) throttle = setTimeout(() => { throttle = null; refresh() }, 1000 - elapsed)
    return
  }
  lastStart = Date.now()
  const request = requestGate.start()
  try {
    const result = await api(activityPath(bounds.value, { offset: page.value * 10, search: search.value }), { signal: request.signal })
    if (!requestGate.current(request)) return
    if (result?.scope !== 'recorded_decisions' || result.items_scope !== 'selected_recorded_decisions' || result.start !== bounds.value?.start || result.end !== bounds.value?.end) throw new Error(t('Activity response is out of date.', '活动响应已过期。'))
    latest = result
    if (page.value > 0 && page.value * 10 >= result.pagination?.total) {
      page.value = Math.max(0, Math.ceil(result.pagination.total / 10) - 1)
      return
    }
    latestAt = Date.now()
    if (paused.value && !pausedSnapshotSafe(displayed.value, result)) {
      displayed.value = null
      refreshedAt.value = null
    } else if (!paused.value) {
      displayed.value = result
      refreshedAt.value = latestAt
    }
    error.value = ''
  } catch (failure) {
    if (!requestGate.current(request) || failure.name === 'AbortError') return
    error.value = failure.message || t('Activity could not be loaded.', '无法加载活动。')
  } finally {
    if (requestGate.current(request)) loading.value = false
  }
}

function resume() {
  paused.value = false
  if (latest) {
    displayed.value = latest
    refreshedAt.value = latestAt
  }
  refresh(true)
}

function togglePause() {
  if (paused.value) resume()
  else paused.value = true
}
defineExpose({ paused, togglePause })

function toggleFullscreen() {
  fullscreen.value = !fullscreen.value
  document.body.style.overflow = fullscreen.value ? 'hidden' : ''
  logPanel.value?.querySelector('.log-fullscreen')?.focus()
}
function fullscreenKeys(event) {
  if (!fullscreen.value) return
  if (event.key === 'Escape') { event.preventDefault(); toggleFullscreen() }
  if (event.key === 'Tab') {
    const controls = [...logPanel.value.querySelectorAll('button:not(:disabled), input, summary')].filter(element => element.getClientRects().length)
    const first = controls[0], last = controls[controls.length - 1]
    if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last?.focus() }
    else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first?.focus() }
  }
}
function searchLogs() {
  clearTimeout(searchTimer)
  searchTimer = setTimeout(() => { search.value = searchInput.value.trim() }, 250)
}

function tick() {
  now.value = Date.now()
  emit('clock')
  refresh()
}

function visible() {
  if (!document.hidden) {
    emit('clock')
    refresh(true)
  }
}

function clearSnapshot() {
  requestGate.cancel()
  clearTimeout(throttle)
  throttle = null
  lastStart = 0
  latest = null
  latestAt = null
  displayed.value = null
  refreshedAt.value = null
  paused.value = false
  loading.value = !!bounds.value
  error.value = ''
}

watch(() => [props.range?.valid, props.range?.created_after, props.range?.created_before], () => {
  page.value = 0
  clearSnapshot()
  refresh(true)
})
watch(search, () => { page.value = 0; paused.value = false; loading.value = true; refresh(true) })
watch(page, () => { paused.value = false; loading.value = true; refresh(true) })
watch(() => props.eventVersion, () => refresh())
watch(() => props.resetVersion, () => {
  clearSnapshot()
  refresh()
})
onMounted(() => {
  refresh()
  timer = setInterval(tick, 10000)
  document.addEventListener('visibilitychange', visible)
  document.addEventListener('keydown', fullscreenKeys)
})
onBeforeUnmount(() => {
  requestGate.cancel()
  clearInterval(timer)
  clearTimeout(throttle)
  clearTimeout(searchTimer)
  document.removeEventListener('visibilitychange', visible)
  document.removeEventListener('keydown', fullscreenKeys)
  if (fullscreen.value) document.body.style.overflow = ''
})
</script>

<template>
  <section class="live-activity" :aria-label="t('Recorded activity', '已记录活动')">
    <div v-if="!embedded" class="activity-toolbar">
      <span class="activity-toolbar-label">{{ t('Live activity', '实时观测') }}</span>
      <div class="activity-actions"><button class="button subtle" @click="paused ? resume() : paused = true">{{ paused ? t('Resume display', '继续显示') : t('Pause display', '暂停显示') }}</button><button class="button subtle" @click="refresh(true)">{{ t('Retry / refresh', '重试 / 刷新') }}</button></div>
    </div>
    <p v-if="!bounds" class="page-state" role="status">{{ t('Choose a valid date range to show activity.', '请选择有效日期范围以查看活动。') }}</p>
    <p v-if="bounds && loading && !displayed" class="page-state" role="status">{{ t('Loading recorded activity…', '正在加载已记录活动…') }}</p>
    <p v-else-if="paused && !displayed" class="page-state" role="status">{{ t('Display paused. Resume to show the latest recorded activity.', '显示已暂停。继续显示以查看最新记录活动。') }}</p>
    <div v-if="error" class="activity-message" role="alert">{{ displayed ? t('Showing the last loaded snapshot. Refresh failed:', '显示上次加载的快照。刷新失败：') : t('Activity unavailable:', '活动不可用：') }} {{ error }}</div>
    <template v-if="displayed">
      <div class="activity-message" :class="{ 'activity-message-compact': embedded }" role="status">
        <span>{{ paused ? t('Display paused; collection and refresh continue.', '显示已暂停；采集和刷新继续。') : props.eventState === 'live' && !error ? t('Updates connected', '更新已连接') : t('Updates not connected; snapshot may be stale', '更新未连接；快照可能已过期') }}</span>
        <span>{{ t('Snapshot', '快照') }}: {{ time(refreshedAt / 1000) }}</span>
        <span v-if="displayed.recording_enabled === false">{{ t('Recording is off. New decisions will not appear here.', '记录已关闭，新决策不会出现在这里。') }}</span>
      </div>
      <div class="activity-charts">
        <article class="panel"><div class="activity-chart-head"><h3>{{ chartTitle }}</h3><span class="worker-indicator" :class="`worker-${workerState}`" :title="workerHint"><span class="worker-dot"></span>{{ workerStatus }}</span></div><p class="activity-legend"><span class="line-key">{{ t('Cumulative (left)', '累计（左轴）') }}</span><span class="bar-key">{{ charts.singleDay ? t('Hourly starts (right)', '每小时开始数（右轴）') : charts.coarse ? t('Interval starts (right)', '每区间开始数（右轴）') : t('Daily starts (right)', '每日开始数（右轴）') }}</span><span>{{ t('Local time', '本地时间') }}</span></p><svg viewBox="-35 -12 670 155" role="img" :aria-label="`${chartTitle}: ${charts.total}`"><g class="activity-axis"><line x1="0" y1="120" x2="600" y2="120"/><line x1="0" y1="60" x2="600" y2="60"/><line x1="0" y1="0" x2="600" y2="0"/><text x="-7" y="124" text-anchor="end">0</text><text x="-7" y="64" text-anchor="end">{{ middleTick(charts.cumulativeMaximum) }}</text><text x="-7" y="4" text-anchor="end">{{ charts.cumulativeMaximum }}</text><text x="607" y="124">0</text><text x="607" y="64">{{ middleTick(charts.countMaximum) }}</text><text x="607" y="4">{{ charts.countMaximum }}</text><text v-for="tick in charts.ticks" :key="tick.x" :x="tick.x" y="140" :text-anchor="tick.x === 0 ? 'start' : tick.x === 600 ? 'end' : 'middle'">{{ tick.label }}</text></g><rect v-for="bar in charts.bars" :key="bar.start ?? bar.hour" :x="bar.x" :y="120 - bar.height" :width="bar.width" :height="bar.height" fill="#b6d7ec"><title>{{ bar.label || `${bar.hour}:00` }} · {{ bar.count }} {{ t('decisions', '次决策') }}</title></rect><path :d="charts.curve" fill="none" stroke="var(--orange)" stroke-width="3" stroke-linecap="round" stroke-linejoin="round"/><circle v-for="point in charts.points.filter((point, index, points) => index === points.length - 1 || point.total !== points[index - 1]?.total)" :key="point.x" :cx="point.x" :cy="120 - point.total * 120 / charts.cumulativeMaximum" r="2.5" fill="var(--orange)"><title>{{ chartTime(point) }} · {{ point.total }} {{ t('cumulative decisions', '累计决策') }}</title></circle></svg><p v-if="!charts.hasData" class="activity-empty">{{ t('No recorded decisions in selected range.', '所选日期范围内尚无已记录决策。') }}</p></article>
      </div>
      <div ref="logPanel" class="activity-log panel" :class="{ 'log-maximized': fullscreen }" :role="fullscreen ? 'dialog' : undefined" :aria-modal="fullscreen ? true : undefined" :aria-label="t('Recorded decisions', '已记录决策')">
        <div class="panel-head log-toolbar"><button class="button subtle log-fullscreen" @click="toggleFullscreen" :aria-label="fullscreen ? t('Exit fullscreen', '退出全屏') : t('Fullscreen', '全屏显示')" :title="fullscreen ? t('Exit fullscreen', '退出全屏') : t('Fullscreen', '全屏显示')">{{ fullscreen ? '⊡' : '⛶' }}</button><h3>{{ t('Recorded decisions', '已记录决策') }}</h3><input v-model="searchInput" type="search" maxlength="200" @input="searchLogs" :aria-label="t('Search decisions', '搜索决策')" :placeholder="t('Search task, ID or role…', '搜索任务、ID 或角色…')"/></div>
        <div class="activity-log-content" :aria-busy="loading">
        <p v-if="loading" class="activity-empty" role="status">{{ t('Updating results…', '正在更新结果…') }}</p>
        <p v-if="!rows.length" class="activity-empty">{{ search ? t('No matching decisions.', '没有匹配的决策。') : t('No recorded decisions yet.', '暂无已记录决策。') }}</p>
        <details v-for="item in rows" :key="item.id" class="activity-row">
          <summary><span class="activity-expand" aria-hidden="true">▸</span><span class="activity-row-main"><small>{{ logTime(item.created_at, item.created_at_ms) }} · #{{ item.id }} · {{ decisionStatus(item.status) }}</small><strong :title="item.summary || t('Untitled decision', '未命名决策')">{{ item.summary || t('Untitled decision', '未命名决策') }}</strong><span class="activity-badges"><em v-if="item.role">{{ t('Role', '角色') }}: {{ item.role }}</em><em v-if="item.risk">{{ t('Risk', '风险') }}: {{ label(item.risk) }}</em><em v-if="item.recommendation?.model">{{ t('Laya recommended', 'Laya 推荐') }}: {{ activityRecommendation(item.recommendation) }}</em></span></span></summary>
          <div class="activity-row-detail">
            <p>{{ t('Recorded', '记录时间') }}: {{ time(item.created_at) }} · {{ t('Finished', '完成时间') }}: {{ item.finished_at ? time(item.finished_at) : t('Not recorded', '未记录') }}</p>
            <p>{{ t('Role', '角色') }}: {{ item.role || '—' }} · {{ t('Complexity', '复杂度') }}: {{ label(item.complexity) }} · {{ t('Risk', '风险') }}: {{ label(item.risk) }} · {{ t('Uncertain', '不确定') }}: {{ item.uncertain == null ? '—' : item.uncertain ? t('Yes', '是') : t('No', '否') }}</p>
            <p>{{ t('Recommended model, not an execution receipt', '推荐模型，不是执行凭据') }}: {{ pair(item.recommendation) }}</p>
            <h4>{{ t('Stored orchestration plan · advisory', '已存储的编排计划 · 建议性质') }}</h4>
            <p v-if="item.orchestration">{{ t('Mode', '模式') }}: {{ modeLabel(item.orchestration.mode) || '—' }} · {{ t('Run', '运行') }}: {{ item.orchestration.run_id || '—' }} · {{ t('Stage', '阶段') }}: {{ item.orchestration.stage_id || '—' }} · {{ t('Required roles', '所需角色') }}: {{ list(item.orchestration.required_roles) }}</p>
            <p v-else>{{ t('No orchestration plan recorded.', '未记录编排计划。') }}</p>
            <h4>{{ t('Observed assignment receipts', '已观测的分配凭据') }}</h4>
            <p v-if="!item.assignments?.length">{{ t('No assignment receipts recorded. A plan does not prove dispatch.', '未记录分配凭据。计划不能证明已派发。') }}</p>
            <div v-for="(assignment, index) in item.assignments || []" :key="assignment.attempt_ref || index" class="activity-assignment"><strong>{{ assignment.role || t('Unknown role', '角色未知') }} · {{ assignment.attempt_ref || '—' }}</strong><small>{{ time(assignment.created_at) }}</small><p>{{ t('Requested', '请求') }}: {{ pair(assignment.requested) }} · {{ t('Effective, if verified', '实际使用（如已核实）') }}: {{ pair(assignment.effective) }}</p><p>{{ t('Verified dispatch receipt', '已核验的派发凭据') }}: {{ dispatchStatus(assignment.status) }}</p></div>
            <p v-if="item.assignments_truncated">{{ t('Assignment receipts truncated in this view.', '此视图中的分配凭据已截断。') }}</p>
            <button class="button subtle" @click="$emit('open', item.id)">{{ t('Open details', '打开详情') }}</button>
          </div>
        </details>
        </div>
        <div class="log-pagination"><span>{{ t('Newest first', '最新在前') }} · {{ number(displayed.pagination?.total) }} {{ t('results', '条结果') }}</span><button class="mini" :disabled="page === 0" @click="page--">{{ t('Previous', '上一页') }}</button><span>{{ page + 1 }} / {{ pageCount }}</span><button class="mini" :disabled="page + 1 >= pageCount" @click="page++">{{ t('Next', '下一页') }}</button></div>
      </div>
    </template>
  </section>
</template>

<style scoped>
.live-activity { padding-top: 12px; }
.live-activity .activity-message-compact { border: 0; background: transparent; padding: 0; margin-bottom: 8px; }
.activity-toolbar { display: flex; justify-content: space-between; align-items: center; gap: 12px; }
.activity-toolbar-label { color: var(--ink-soft); font-size: 11px; }
.activity-actions { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; }
.activity-chart-head { display: flex; justify-content: space-between; align-items: center; gap: 12px; }
.activity-legend { display: flex; flex-wrap: wrap; gap: 6px 16px; }
.line-key { color: var(--orange-dark); }
.bar-key { color: #4388ad; }
.worker-indicator { display: inline-flex; align-items: center; gap: 6px; color: var(--ink-soft); font-size: 11px; }
.worker-dot { width: 9px; height: 9px; border-radius: 50%; background: #888; }
.worker-error .worker-dot { background: #d2493b; }
.worker-busy .worker-dot { background: #d7a331; }
.worker-idle .worker-dot { background: #398f55; }
.activity-message { margin: 0 0 14px; padding: 10px 14px; display: flex; flex-wrap: wrap; gap: 8px 20px; border: 1px solid var(--line); border-radius: 9px; color: var(--ink-soft); background: var(--paper-deep); font-size: 11px; }
.activity-charts p, .activity-row small, .activity-row-detail { color: var(--ink-soft); font-size: 11px; line-height: 1.5; }
.activity-charts { margin: 16px 0; }
.activity-charts article { min-width: 0; padding: 18px; }
.activity-charts h3 { margin: 0; font-size: 15px; }
.activity-charts p { margin: 4px 0 15px; }
.activity-charts svg { display: block; width: 100%; height: auto; overflow: visible; }
.activity-axis line { stroke: var(--line); stroke-width: 1; }
.activity-axis text { fill: var(--ink-soft); font-size: 12px; }
.activity-empty { margin: 0; padding: 35px 20px; color: var(--ink-soft); font-size: 12px; text-align: center; }
.activity-log { overflow: hidden; }
.activity-log.log-maximized { position: fixed; inset: 0; z-index: 100; display: flex; flex-direction: column; background: white; border: 0; border-radius: 0; padding: 20px; }
.activity-log.log-maximized .activity-log-content { flex: 1; min-height: 0; max-height: none; }
.log-toolbar { justify-content: flex-start; gap: 12px; background: white; }
.log-toolbar h3 { margin: 0; font-size: 16px; white-space: nowrap; }
.log-toolbar input { max-width: 320px; order: 1; }
.log-toolbar h3 { order: 2; }
.log-fullscreen { font-size: 20px; padding: 5px 10px; }
.activity-log-content { background: #f2f6f9; max-height: 560px; overflow: auto; scrollbar-gutter: stable; }
.log-pagination { display: flex; justify-content: flex-end; align-items: center; gap: 12px; padding: 12px 20px; border-top: 1px solid var(--line); font-size: 11px; background: white; }
.log-pagination > span:first-child { margin-right: auto; color: var(--ink-soft); }
.activity-row { border-top: 1px solid var(--line); }
.activity-row summary { padding: 15px 20px; display: flex; justify-content: space-between; gap: 12px; align-items: center; cursor: pointer; }
.activity-row summary:hover { background: #e7eff5; }
.activity-row summary::-webkit-details-marker { display: none; }
.activity-row-main { min-width: 0; flex: 1; display: grid; gap: 3px; }
.activity-row-main small { overflow-wrap: anywhere; }
.activity-row-main strong { overflow: hidden; font-size: 12px; text-overflow: ellipsis; white-space: nowrap; }
.activity-badges { display: flex; flex-wrap: wrap; gap: 5px; }
.activity-badges em { min-width: 0; overflow-wrap: anywhere; padding: 2px 5px; border-radius: 4px; color: var(--ink-soft); background: var(--paper-deep); font-size: 10px; font-style: normal; }
.activity-expand { color: var(--ink-soft); font-size: 16px; flex: 0 0 12px; }
.activity-row[open] > summary .activity-expand { transform: rotate(90deg); }
.activity-row[open] { background: #eaf1f6; }
.activity-row-detail { padding: 2px 20px 18px; overflow-wrap: anywhere; }
.activity-row-detail h4 { margin: 14px 0 4px; color: var(--ink); font-size: 12px; }
.activity-row-detail p { margin: 5px 0; }
.activity-assignment { margin: 7px 0; padding: 9px 12px; border-left: 2px solid var(--orange); background: var(--paper-deep); }
.activity-assignment strong, .activity-assignment small { display: block; }
@media (max-width: 740px) { .activity-row summary { align-items: flex-start; } }
@media (max-width: 560px) { .log-toolbar { flex-wrap: wrap; } .log-toolbar input { max-width: none; flex-basis: 100%; } .log-pagination { gap: 8px; padding: 12px; } }
</style>
