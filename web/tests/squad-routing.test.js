import test from 'node:test'
import assert from 'node:assert/strict'
import { fileURLToPath } from 'node:url'
import { createServer } from 'vite'
import { createSSRApp } from 'vue'
import { renderToString } from '@vue/server-renderer'

test('routing settings show persisted preferences in both languages without assuming defaults', async () => {
  const server = await createServer({ root: fileURLToPath(new URL('..', import.meta.url)), server: { middlewareMode: true, ws: false }, appType: 'custom' })
  try {
    const { default: component } = await server.ssrLoadModule('/src/SquadRouting.vue')
    const { setLanguage } = await server.ssrLoadModule('/src/i18n.js')
    const render = (preferences) => renderToString(createSSRApp(component, { preferences }))
    setLanguage('en')
    assert.match(await render(null), /Preferences unavailable/)
    assert.doesNotMatch(await render(null), /Configure routing/)
    const preferences = { policy: 'auto', ceiling: { model: 'host-model', reasoning_effort: 'medium' }, squad: { enabled: true, reviewer: null } }
    assert.match(await render(preferences), /host-model \/ medium/)
    assert.match(await render(preferences), /Enabled/)
    setLanguage('zh')
    assert.match(await render(preferences), /Squad 角色路由/)
    assert.match(await render(preferences), /当前主会话/)
    assert.match(await render({ ...preferences, squad: { enabled: false, reviewer: null } }), /未启用/)
  } finally { await server.close() }
})
