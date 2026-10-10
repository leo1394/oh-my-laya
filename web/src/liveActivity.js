import { locale, t } from './i18n.js'

export function localDayBounds(now = new Date()) {
  const start = new Date(now.getFullYear(), now.getMonth(), now.getDate())
  const end = new Date(now.getFullYear(), now.getMonth(), now.getDate() + 1)
  return { start: Math.floor(start.getTime() / 1000), end: Math.floor(end.getTime() / 1000) }
}

export function activityBounds(range) {
  if (!range?.valid || !Number.isSafeInteger(range.created_after) || !Number.isSafeInteger(range.created_before) || range.created_before < range.created_after) return null
  return { start: range.created_after, end: range.created_before + 1 }
}

export function activityPath(bounds, options) {
  const base = `/activity?created_after=${bounds.start}&created_before=${bounds.end}&limit=${options ? 10 : 50}`
  return options ? `${base}&offset=${options.offset}&q=${encodeURIComponent(options.search)}` : base
}

export function activityRequestGate() {
  let version = 0
  let controller
  return {
    start() {
      controller?.abort()
      controller = new AbortController()
      return { version: ++version, signal: controller.signal }
    },
    current(request) { return request.version === version },
    cancel() { version++; controller?.abort() },
  }
}

export function activityTime(value) {
  if (value == null || value === '') return t('Time not recorded', '未记录时间')
  const date = new Date(typeof value === 'number' ? value * 1000 : value)
  return Number.isFinite(date.getTime()) ? date.toLocaleString(locale.value === 'zh' ? 'zh-CN' : 'en', { dateStyle: 'medium', timeStyle: 'medium' }) : t('Time not recorded', '未记录时间')
}

export function activityLogTime(value, milliseconds = null) {
  if (Number.isSafeInteger(milliseconds) && milliseconds >= 0) {
    const seconds = Math.floor(milliseconds / 1000)
    return `${activityLogTime(seconds)}.${String(milliseconds % 1000).padStart(3, '0')}`
  }
  if (value == null || value === '') return t('Time not recorded', '未记录时间')
  const date = new Date(typeof value === 'number' ? value * 1000 : value)
  if (!Number.isFinite(date.getTime())) return t('Time not recorded', '未记录时间')
  const fraction = typeof value === 'number' ? String(value).match(/\.(\d+)/)?.[1] : typeof value === 'string' ? value.match(/T\d{2}:\d{2}:\d{2}\.(\d+)/)?.[1] : null
  const pad = number => String(number).padStart(2, '0')
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}${fraction ? `.${fraction}` : ''}`
}

export function activityPair(value) {
  return value?.model ? `${value.model} / ${value.reasoning_effort || t('Not recorded', '未记录')}` : t('Not verified', '未核实')
}

export function activityRecommendation(value) {
  if (!value?.model) return ''
  const effort = value.reasoning_effort
  return `${value.model}${effort ? ` ${effort.charAt(0).toUpperCase()}${effort.slice(1)}` : ''}`
}

export function pausedSnapshotSafe(displayed, next) {
  if (!displayed) return true
  return typeof displayed.privacy_revision === 'string' && typeof next?.privacy_revision === 'string' &&
    typeof displayed.service_instance === 'string' && typeof next.service_instance === 'string' &&
    displayed.privacy_revision === next.privacy_revision &&
    displayed.service_instance === next.service_instance &&
    displayed.recording_enabled === next.recording_enabled
}

const axisMaximum = value => value <= 1 ? 1 : Math.ceil(value / 2) * 2

export function activityCurve(points, maximum) {
  if (!points.length) return ''
  const y = point => 120 - point.total * 120 / maximum
  let path = `M ${points[0].x},${y(points[0])}`
  for (let index = 1; index < points.length; index++) {
    const previous = points[index - 1]
    const point = points[index]
    const third = (point.x - previous.x) / 3
    // Horizontal endpoint tangents keep each smooth segment inside its real values.
    path += ` C ${previous.x + third},${y(previous)} ${point.x - third},${y(point)} ${point.x},${y(point)}`
  }
  return path
}

export function activityWorker(snapshot, eventState, error) {
  if (error) return 'error'
  if (eventState !== 'live' || !Number.isSafeInteger(snapshot?.worker?.pid) || snapshot.worker.pid <= 0) return 'unloaded'
  if (snapshot.worker.busy === true || snapshot.worker.queued > 0) return 'busy'
  return snapshot.worker.busy === false ? 'idle' : 'unloaded'
}

export function activityCharts(snapshot, nowSeconds = Date.now() / 1000) {
  const buckets = Array.isArray(snapshot?.buckets) ? snapshot.buckets : []
  const end = Number.isFinite(snapshot?.end) ? snapshot.end : Infinity
  const start = Number.isFinite(snapshot?.start) ? snapshot.start : -Infinity
  const singleDay = Number.isFinite(start) && Number.isFinite(end) && new Date(start * 1000).toDateString() === new Date((end - 1) * 1000).toDateString()
  if (!singleDay) {
    const cutoff = Math.max(start, Math.min(nowSeconds, end))
    const duration = end - start
    const coarse = snapshot?.bucket_seconds > 900
    const days = []
    for (const bucket of buckets) {
      if (!Number.isFinite(bucket.start) || !Number.isFinite(bucket.end) || bucket.start < start || bucket.start >= cutoff || bucket.end > end || !Number.isSafeInteger(bucket.count) || bucket.count < 0) continue
      const day = new Date(bucket.start * 1000).toDateString()
      const previous = days[days.length - 1]
      if (!coarse && previous?.day === day) {
        previous.end = bucket.end
        previous.count += bucket.count
      } else days.push({ day, start: bucket.start, end: bucket.end, count: bucket.count })
    }
    let total = 0
    const points = [{ x: 0, total: 0 }]
    const countMaximum = axisMaximum(Math.max(1, ...days.map(day => day.count)))
    const bars = days.map(day => {
      total += day.count
      points.push({ x: Math.min(600, (Math.min(day.end, cutoff) - start) * 600 / duration), total })
      const x = (day.start - start) * 600 / duration + 2
      const date = value => activityLogTime(value).slice(0, 10)
      const label = coarse ? `${date(day.start)}–${date(day.end - 1)}` : date(day.start)
      return { ...day, label, x, width: Math.max(1, (Math.min(day.end, cutoff) - day.start) * 600 / duration - 4), height: day.count * 120 / countMaximum }
    })
    const cumulativeMaximum = axisMaximum(Math.max(1, total))
    const ticks = [0, 0.25, 0.5, 0.75, 1].map((fraction, index) => ({ x: fraction * 600, label: activityLogTime(start + Math.floor(duration * fraction) - (index === 4 ? 1 : 0)).slice(0, 10) }))
    return { hours: [], points, curve: activityCurve(points, cumulativeMaximum), bars, total, countMaximum, cumulativeMaximum, ticks, singleDay, coarse, hasData: total > 0 }
  }
  const hours = Array.from({ length: 24 }, (_, hour) => ({ hour, count: 0, observed: false }))
  for (const bucket of buckets) {
    if (!Number.isFinite(bucket.start) || bucket.start < start || bucket.start >= Math.min(nowSeconds, end) || !Number.isSafeInteger(bucket.count) || bucket.count < 0) continue
    const hour = new Date(bucket.start * 1000).getHours()
    hours[hour].count += bucket.count
    hours[hour].observed = true
  }
  const cutoff = Math.max(start, Math.min(nowSeconds, end))
  const current = new Date(cutoff * 1000)
  const cutoffHour = cutoff >= end ? 24 : current.getHours() + (current.getMinutes() * 60 + current.getSeconds()) / 3600
  let total = 0
  const points = [{ x: 0, total: 0 }]
  for (const hour of hours) {
    total += hour.count
    if (hour.hour < Math.floor(cutoffHour)) points.push({ x: (hour.hour + 1) * 25, total })
    else if (hour.hour === Math.floor(cutoffHour) && cutoffHour < 24) points.push({ x: cutoffHour * 25, total })
  }
  const maximum = axisMaximum(Math.max(1, ...hours.map(hour => hour.count)))
  const cumulativeMaximum = axisMaximum(Math.max(1, total))
  const height = 120
  return {
    hours,
    points,
    curve: activityCurve(points, cumulativeMaximum),
    bars: hours.filter(hour => hour.observed).map(hour => ({ ...hour, x: hour.hour * 25 + 2, width: 21, height: hour.count * height / maximum })),
    total,
    countMaximum: maximum,
    cumulativeMaximum,
    cutoffHour,
    ticks: [0, 6, 12, 18, 24].map(hour => ({ x: hour * 25, label: `${hour}:00` })),
    singleDay,
    hasData: total > 0,
  }
}
