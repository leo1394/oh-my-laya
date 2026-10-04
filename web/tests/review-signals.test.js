import test from 'node:test'
import assert from 'node:assert/strict'
import { fileURLToPath } from 'node:url'
import { createServer } from 'vite'
import { createSSRApp } from 'vue'
import { renderToString } from '@vue/server-renderer'

test('review signal component renders triage and escapes feedback evidence', async () => {
  const server = await createServer({ root: fileURLToPath(new URL('..', import.meta.url)), server: { middlewareMode: true, hmr: false, ws: false }, appType: 'custom' })
  try {
    const { default: component } = await server.ssrLoadModule('/src/ReviewSignals.vue')
    const html = await renderToString(createSSRApp(component, {
      decision: {
        review_priority: 4,
        review_reasons: ['review_disagreement', '<script>bad</script>'],
        review_trigger_count: 2,
        review_trigger_event_ids: ['event-1', 'event-2']
      },
      showEvents: true
    }))
    assert.match(html, /Reported safety \/ missed-risk concern/)
    assert.match(html, /2 review triggers/)
    assert.match(html, /Reviewer disagreement/)
    assert.match(html, /Trigger events: event-1, event-2/)
    assert.match(html, /&lt;script&gt;/)
    assert.doesNotMatch(html, /<script>bad/)
  } finally {
    await server.close()
  }
})
