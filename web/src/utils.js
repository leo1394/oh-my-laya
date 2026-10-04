export function normalizeList(payload) {
  if (Array.isArray(payload)) return payload
  if (!payload || typeof payload !== 'object') return []
  for (const key of ['items', 'results', 'data', 'decisions', 'cases', 'versions', 'jobs', 'backups']) {
    if (Array.isArray(payload[key])) return payload[key]
  }
  return []
}

export function displayTime(value) {
  if (!value) return 'Not recorded'
  const date = new Date(typeof value === 'number' ? value * 1000 : value)
  return Number.isNaN(date.getTime()) ? String(value) : date.toLocaleString()
}

function localDate(value) {
  if (typeof value !== 'string' || !/^\d{4}-\d{2}-\d{2}$/.test(value)) return null
  const [year, month, day] = value.split('-').map(Number)
  const date = new Date(year, month - 1, day)
  if (date.getFullYear() !== year || date.getMonth() !== month - 1 || date.getDate() !== day) return null
  return date
}

export function localDateKey(date) {
  const pad = (value) => String(value).padStart(2, '0')
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`
}

export function presetDateRange(preset = 'last7', now = new Date()) {
  const end = new Date(now.getFullYear(), now.getMonth(), now.getDate())
  const start = new Date(end)
  if (preset === 'last7') start.setDate(start.getDate() - 6)
  else if (preset === 'last30') start.setDate(start.getDate() - 29)
  else if (preset !== 'today') throw new Error('Unknown date preset.')
  return { preset, start: localDateKey(start), end: localDateKey(end) }
}

export function refreshPresetRange(range, now = new Date()) {
  if (!range || range.preset === 'custom') return range
  const next = presetDateRange(range.preset, now)
  return next.start === range.start && next.end === range.end ? range : next
}

export function dateRangeBounds(range) {
  const start = localDate(range?.start)
  const end = localDate(range?.end)
  if (!start || !end) return { valid: false, error: 'Choose valid start and end dates.' }
  if (start.getTime() > end.getTime()) return { valid: false, error: 'Start date must be on or before end date.' }
  const nextDay = new Date(end.getFullYear(), end.getMonth(), end.getDate() + 1)
  return {
    valid: true,
    created_after: Math.floor(start.getTime() / 1000),
    created_before: Math.floor(nextDay.getTime() / 1000) - 1
  }
}

export function rangeQuery(bounds) {
  if (!bounds?.valid) throw new Error(bounds?.error || 'Choose a valid date range.')
  return new URLSearchParams({ created_after: String(bounds.created_after), created_before: String(bounds.created_before) }).toString()
}

export function decisionQuery(filters = {}, offset = 0, bounds = null) {
  const query = new URLSearchParams({ limit: '50', offset: String(offset) })
  query.set('filter', filters.status || 'pending')
  if (filters.risk) query.set('risk', filters.risk)
  if (filters.source?.trim()) query.set('source', filters.source.trim())
  if (bounds) {
    if (!bounds.valid) throw new Error(bounds.error)
    query.set('created_after', String(bounds.created_after))
    query.set('created_before', String(bounds.created_before))
  }
  return query.toString()
}

export function queueNeedsRefresh(changes) {
  return ['decision', 'review', 'feedback'].some((kind) => changes.includes(kind))
}

export function reviewScope(decision = {}, latestReview = {}) {
  const advisor = decision.request?.advisor || {}
  const field = (key) => Object.hasOwn(latestReview, key) ? latestReview[key] : (key === 'language' ? decision[key] : advisor[key] ?? decision[key])
  return {
    task_family: field('task_family') || 'general',
    task_lineage: field('task_lineage') || '',
    language: field('language') || (/\p{Script=Han}/u.test(JSON.stringify(decision.request?.state || '')) ? 'zh' : 'en')
  }
}

export function reviewSignals(decision = {}) {
  const labels = {
    high_risk: 'High risk', uncertain: 'Uncertain', decision_error: 'Decision error',
    problem_score: 'Problem score', test_failed: 'Test failed',
    review_disagreement: 'Reviewer disagreement', user_declined: 'User declined',
    user_modified: 'User changed selection', outcome_failed: 'Task failed',
    model_changed: 'Model changed', high_risk_correction: 'Reported high-risk correction',
    reported_high_risk_correction: 'Reported high-risk correction', invalid_result: 'Invalid result',
    user_choice_changed: 'User changed selection', model_upgrade: 'Model upgrade',
    reported_label_change: 'Reviewer proposed label correction'
  }
  const priorities = ['No review signal', 'Needs review', 'Problem feedback', 'Uncertain with problem feedback', 'Reported safety / missed-risk concern']
  const strings = (value) => Array.isArray(value) ? value.filter((item) => typeof item === 'string') : []
  const rank = Number(decision.review_priority) || 0
  return {
    rank,
    priority: priorities[rank] || 'Needs review',
    reasons: strings(decision.review_reasons).map((reason) => labels[reason] || reason.replaceAll('_', ' ')),
    triggerCount: Number.isInteger(decision.review_trigger_count) ? decision.review_trigger_count : 0,
    eventIds: strings(decision.review_trigger_event_ids),
    sources: strings(decision.review_sources),
    risk: decision.risk || decision.result?.advice?.assessment?.risk?.choice || decision.result?.answers?.risk?.choice || decision.result?.laya_result?.answers?.risk?.choice || decision.result?.laya_result?.risk?.label || null
  }
}

export function displayBytes(value) {
  const bytes = Number(value)
  if (!Number.isFinite(bytes) || bytes < 0) return 'Unknown'
  if (bytes < 1024) return `${bytes} B`
  const units = ['KiB', 'MiB', 'GiB']
  let amount = bytes / 1024
  let unit = units[0]
  for (let index = 1; index < units.length && amount >= 1024; index += 1) {
    amount /= 1024
    unit = units[index]
  }
  return `${amount.toFixed(amount >= 10 ? 0 : 1)} ${unit}`
}

export function errorMessage(error) {
  if (typeof error === 'string') return error
  if (error && typeof error.message === 'string') return error.message
  return 'The local service did not return a usable response.'
}

export function decisionTitle(decision) {
  return decision?.summary || decision?.task_summary || decision?.task_ref || (typeof decision?.request?.state === 'string' ? decision.request.state : null) || decision?.id || 'Untitled decision'
}

export function itemId(item) {
  return item?.id || item?.decision_id || item?.case_id || item?.version_id || item?.job_id || item?.backup_id
}

export function exportArtifact(job) {
  const result = job?.result
  if (job?.kind !== 'export' || job?.status !== 'completed' || !result?.artifact_id) return null
  return {
    href: `/api/v1/exports/${encodeURIComponent(result.artifact_id)}`,
    name: result.artifact_name || 'laya-export.ndjson',
    mediaType: result.media_type || 'application/x-ndjson',
    recordCount: result.record_count
  }
}
