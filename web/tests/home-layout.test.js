import test from 'node:test'
import assert from 'node:assert/strict'
import { fileURLToPath } from 'node:url'
import { createServer } from 'vite'
import { createSSRApp } from 'vue'
import { renderToString } from '@vue/server-renderer'

test('dashboard opens on compact overview with live activity, not configuration or case queue', async () => {
  const server = await createServer({ root: fileURLToPath(new URL('..', import.meta.url)), server: { middlewareMode: true, hmr: false, ws: false }, appType: 'custom' })
  try {
    const { default: component } = await server.ssrLoadModule('/src/App.vue')
    const { setLanguage } = await server.ssrLoadModule('/src/i18n.js')
    for (const language of ['en', 'zh']) {
      setLanguage(language)
      const html = await renderToString(createSSRApp(component))
      assert.equal((html.match(/class="[^"]*\bnav-item\b/g) || []).length, 3)
      assert.match(html, /class="live-activity"/)
      assert.match(html, /class="token-overview"/)
      assert.doesNotMatch(html, /class="learning-overview"|class="panel tier-panel"|class="panel queue-panel"/)
      assert.ok(html.indexOf('date-range-compact') < html.indexOf('class="summary-strip"'))
      assert.match(html, /class="overview-metrics panel"/)
      assert.ok(html.indexOf('class="token-overview"') < html.indexOf('class="live-activity"'))
    }
  } finally {
    await server.close()
  }
})
