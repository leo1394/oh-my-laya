import { t, locale } from './i18n.js'

const number = value => Number.isSafeInteger(value) && value >= 0 ? value.toLocaleString(locale.value) : null
const unknown = () => t('Not reported', '未上报')

export function observationRecords(value) {
  return Array.isArray(value) ? value.filter(item => item?.contract === 'efficiency_observation_v1' && item.complete_task_coverage === false) : []
}

export function inputSize(item) {
  const size = item.context?.input_size
  const value = number(size?.value)
  const unit = { bytes: t('bytes', '字节'), characters: t('characters', '字符'), native_tokens: t('native tokens', '原生 Token') }[size?.unit]
  return value !== null && unit && typeof size?.source === 'string' && size.source.trim() ? `${value} ${unit}` : unknown()
}

export function isolation(item) {
  return {
    isolated: typeof item.context?.evidence_ref === 'string' && item.context.evidence_ref.trim() ? t('Reported isolated', '上报为已隔离') : unknown(),
    unsupported: t('Unsupported by host', '宿主不支持'),
    unknown: unknown()
  }[item.context?.isolation] || unknown()
}

export function duration(item) {
  const value = number(item.outcome?.duration_ms)
  return value === null ? unknown() : `${value} ms`
}
