import { ref } from 'vue'

export const languageKey = 'laya.workbench.language'

export function resolveLanguage(saved, languages = []) {
  if (saved === 'zh' || saved === 'en') return saved
  const primary = String(languages[0] || 'en').toLowerCase().split(/[-_]/)[0]
  return primary === 'zh' ? 'zh' : 'en'
}

function initialLanguage() {
  let saved
  try { saved = globalThis.localStorage?.getItem(languageKey) } catch { /* Storage can be disabled. */ }
  return resolveLanguage(saved, globalThis.navigator?.languages || [globalThis.navigator?.language])
}

export const locale = ref(initialLanguage())

export function setLanguage(value) {
  locale.value = resolveLanguage(value)
  try { globalThis.localStorage?.setItem(languageKey, locale.value) } catch { /* Keep this session usable. */ }
  if (globalThis.document) {
    document.documentElement.lang = locale.value === 'zh' ? 'zh-CN' : 'en'
    document.title = locale.value === 'zh' ? 'Oh My Laya — 决策工作台' : 'Oh My Laya — Decision workbench'
  }
}

export function t(english, chinese) {
  return locale.value === 'zh' ? chinese : english
}

export function label(value) {
  const values = {
    low: ['Low', '低'], medium: ['Medium', '中'], high: ['High', '高'],
    clear: ['Clear', '明确'], uncertain: ['Uncertain', '不确定'],
    unknown: ['Unknown', '未知'], pending: ['Pending', '待处理'],
    confirmed: ['Confirmed', '已确认'], corrected: ['Corrected', '已调整'],
    insufficient: ['Insufficient evidence', '证据不足'], excluded: ['Excluded', '已排除'],
    complexity: ['Complexity', '复杂度'], risk: ['Risk', '风险'], certainty: ['Certainty', '确定性'],
    model_fit: ['Model fit', '模型匹配'], judgment_quality: ['Judgment quality', '判断质量'], outcome_quality: ['Outcome quality', '结果质量'],
    ready: ['Ready', '就绪'], stopped: ['Stopped', '已停止'], busy: ['Busy', '忙碌'],
    live: ['Connected', '已连接'], connecting: ['Connecting', '连接中'],
    disconnected: ['Disconnected', '已断开'], candidate: ['Candidate', '候选'],
    reconnecting: ['Reconnecting', '重新连接中'], unsupported: ['Live updates unavailable', '不支持实时更新'],
    passed: ['Passed', '通过'], failed: ['Failed', '失败'], active: ['Active', '已启用'],
    invalidated: ['Invalidated', '已失效'], queued: ['Queued', '排队中'], running: ['Running', '运行中'],
    completed: ['Completed', '已完成'], cancelled: ['Cancelled', '已取消']
  }
  return values[value] ? t(...values[value]) : value || t('Unknown', '未知')
}
