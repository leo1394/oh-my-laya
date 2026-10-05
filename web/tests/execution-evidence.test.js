import test from 'node:test'
import assert from 'node:assert/strict'
import { setLanguage } from '../src/i18n.js'
import { observationRecords, inputSize, isolation, duration } from '../src/executionEvidence.js'
import { fileURLToPath } from 'node:url'
import { createServer } from 'vite'
import { createSSRApp } from 'vue'
import { renderToString } from '@vue/server-renderer'

test('execution evidence preserves units, zero, unknown values and source requirements', () => {
  setLanguage('en')
  const record = { contract: 'efficiency_observation_v1', complete_task_coverage: false }
  assert.deepEqual(observationRecords([null, {}, record]), [record])
  assert.deepEqual(observationRecords({}), [])
  assert.deepEqual(observationRecords([{ ...record, complete_task_coverage: true }]), [])
  const size = (value, unit, source = 'host input packet') => inputSize({ context: { input_size: { value, unit, source } } })
  assert.equal(size(1200, 'bytes'), '1,200 bytes')
  assert.equal(size(400, 'characters'), '400 characters')
  assert.equal(size(0, 'native_tokens'), '0 native tokens')
  for (const value of [null, -1, 0.5, '40', Number.MAX_SAFE_INTEGER + 1]) assert.equal(size(value, 'bytes'), 'Not reported')
  assert.equal(size(2, 'unknown'), 'Not reported')
  assert.equal(size(2, 'bytes', ''), 'Not reported')
  assert.equal(isolation({ context: { isolation: 'isolated' } }), 'Not reported')
  assert.equal(isolation({ context: { isolation: 'isolated', evidence_ref: 'host snapshot' } }), 'Reported isolated')
  assert.equal(isolation({ context: { isolation: 'unsupported' } }), 'Unsupported by host')
  assert.equal(duration({ outcome: { duration_ms: 0 } }), '0 ms')
  assert.equal(duration({ outcome: { duration_ms: null } }), 'Not reported')
  setLanguage('zh')
  assert.equal(size(12, 'bytes'), '12 字节')
  assert.equal(size(12, 'native_tokens'), '12 原生 Token')
  assert.equal(duration({}), '未上报')
  assert.equal(isolation({ context: { isolation: 'unsupported' } }), '宿主不支持')
})

test('execution evidence leaves rows unrendered until expanded and tolerates old services', async () => {
  const server = await createServer({ root: fileURLToPath(new URL('..', import.meta.url)), server: { middlewareMode: true, ws: false }, appType: 'custom' })
  try {
    const { default: component } = await server.ssrLoadModule('/src/ExecutionEvidence.vue')
    const { setLanguage } = await server.ssrLoadModule('/src/i18n.js')
    setLanguage('en')
    const observations = Array.from({ length: 51 }, (_, index) => ({
      contract: 'efficiency_observation_v1', complete_task_coverage: false,
      attempt_ref: `unrendered-attempt-${index}`, identity: { role: 'worker' },
    }))
    const render = (records) => renderToString(createSSRApp(component, { observations: records }))
    const html = await render(observations)
    assert.match(html, /Execution context &amp; duration · 51/)
    assert.doesNotMatch(html, /unrendered-attempt-|<article|<dl/)
    for (const records of [undefined, [], [{ contract: 'future', complete_task_coverage: false }]]) {
      assert.doesNotMatch(await render(records), /<details/)
    }
  } finally {
    await server.close()
  }
})
