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

export function decisionQuery(filters = {}, offset = 0) {
  const query = new URLSearchParams({ limit: '50', offset: String(offset) })
  query.set('filter', filters.status || 'pending')
  if (filters.risk) query.set('risk', filters.risk)
  if (filters.source?.trim()) query.set('source', filters.source.trim())
  for (const [input, output] of [['after', 'created_after'], ['before', 'created_before']]) {
    if (!filters[input]) continue
    const value = new Date(filters[input]).getTime()
    if (!Number.isFinite(value)) throw new Error('Choose a valid date and time.')
    query.set(output, String(Math.floor(value / 1000)))
  }
  if (query.has('created_after') && query.has('created_before') && Number(query.get('created_after')) > Number(query.get('created_before'))) {
    throw new Error('Start time must be before end time.')
  }
  return query.toString()
}

export function queueNeedsRefresh(changes) {
  return ['decision', 'review', 'feedback'].some((kind) => changes.includes(kind))
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
  return decision?.summary || decision?.task_summary || decision?.task_ref || decision?.id || 'Untitled decision'
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
