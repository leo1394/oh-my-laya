import test from 'node:test'
import assert from 'node:assert/strict'
import { fileURLToPath } from 'node:url'
import { readFile } from 'node:fs/promises'
import { createServer } from 'vite'
import { createSSRApp } from 'vue'
import { renderToString } from '@vue/server-renderer'

test('date picker is bilingual and keeps one selected range shape across pages', async () => {
  const server = await createServer({ root: fileURLToPath(new URL('..', import.meta.url)), server: { middlewareMode: true, hmr: false, ws: false }, appType: 'custom' })
  try {
    const { default: component } = await server.ssrLoadModule('/src/DateRangePicker.vue')
    const { setLanguage } = await server.ssrLoadModule('/src/i18n.js')
    const selected = { preset: 'custom', start: '2026-03-08', end: '2026-03-10' }
    setLanguage('en')
    const overview = await renderToString(createSSRApp(component, { modelValue: selected }))
    const cases = await renderToString(createSSRApp(component, { modelValue: selected }))
    for (const html of [overview, cases]) {
      assert.match(html, /Decision date/)
      assert.match(html, /Today/)
      assert.match(html, /Last 7 days/)
      assert.match(html, /Last 30 days/)
      assert.match(html, /2026-03-08/)
      assert.match(html, /2026-03-10/)
    }
    setLanguage('zh')
    const chinese = await renderToString(createSSRApp(component, { modelValue: selected }))
    assert.match(chinese, /决策时间/)
    assert.match(chinese, /最近 7 天/)
    assert.match(chinese, /自定义/)
  } finally {
    await server.close()
  }
})

test('workbench renders the shared default range on overview without a service request during SSR', async () => {
  const server = await createServer({ root: fileURLToPath(new URL('..', import.meta.url)), server: { middlewareMode: true, hmr: false, ws: false }, appType: 'custom' })
  try {
    const { default: component } = await server.ssrLoadModule('/src/App.vue')
    const { setLanguage } = await server.ssrLoadModule('/src/i18n.js')
    setLanguage('en')
    const html = await renderToString(createSSRApp(component))
    assert.match(html, /Decision date/)
    assert.match(html, /Last 7 days/)
    assert.match(html, /<option value="today" selected>Today/)
    assert.doesNotMatch(html, /type="datetime-local"/)
  } finally {
    await server.close()
  }
})

test('ranged overview owns its errors and summary totals without status fallbacks', async () => {
  const source = await readFile(fileURLToPath(new URL('../src/App.vue', import.meta.url)), 'utf8')
  const globalErrors = source.match(/Object\.entries\(errors\)\.filter\([^\n]+/)?.[0] || ''
  assert.match(globalErrors, /'overview'/)
  assert.match(globalErrors, /'dateRange'/)
  assert.match(source, /overview\?\.counts\?\.decisions/)
  assert.match(source, /overview\?\.counts\?\.pending_reviews/)
  assert.match(source, /overview\?\.counts\?\.feedback/)
  assert.match(source, /overview\?\.risk_counts/)
  assert.doesNotMatch(source, /status\?\.counts\?\.(?:decisions|pending_reviews|feedback)/)
  assert.doesNotMatch(source, /status\?\.risk_counts/)
  assert.match(source, /case list follows decision dates; memory and version management remains all time/)
})
