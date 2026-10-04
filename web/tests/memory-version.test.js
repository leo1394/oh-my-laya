import test from 'node:test'
import assert from 'node:assert/strict'
import { fileURLToPath } from 'node:url'
import { createServer } from 'vite'
import { createSSRApp } from 'vue'
import { renderToString } from '@vue/server-renderer'

test('activation follows server eligibility rather than lifecycle status', async () => {
  const server = await createServer({ root: fileURLToPath(new URL('..', import.meta.url)), server: { middlewareMode: true, hmr: false, ws: false }, appType: 'custom' })
  try {
    const { default: component } = await server.ssrLoadModule('/src/MemoryVersion.vue')
    const render = (item) => renderToString(createSSRApp(component, { item }))
    const passed = { id: 'v1', status: 'candidate', evaluation_status: 'passed', activation_eligible: true, evaluation: { passed: true, sample_count: 16, candidate_memory_exposure: 1 } }
    const html = await render(passed)
    assert.match(html, /<button class="mini accent">Activate<\/button>/)
    assert.match(html, /<button class="mini" disabled>Evaluate<\/button>/)
    for (const item of [
      { ...passed, activation_eligible: false },
      { ...passed, activation_eligible: undefined },
      { ...passed, status: 'active' }
    ]) assert.match(await render(item), /<button class="mini accent" disabled>Activate<\/button>/)
    assert.match(await render({ id: 'new', status: 'candidate', evaluation_status: 'pending' }), /<button class="mini">Evaluate<\/button>/)
    const failed = await render({ ...passed, evaluation_status: 'failed', activation_eligible: false, evaluation: { passed: false, candidate_memory_exposure: 0, note: '<script>bad</script>' } })
    assert.match(failed, /No candidate cases were injected/)
    assert.match(failed, /Reports are immutable/)
    assert.doesNotMatch(failed, /<script>bad<\/script>/)
  } finally {
    await server.close()
  }
})
