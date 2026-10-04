import test from 'node:test'
import assert from 'node:assert/strict'
import { resolveLanguage, locale, setLanguage, t } from '../src/i18n.js'
import { originalLabels, reviewLabels, reviewPayloadLabels } from '../src/review.js'

test('language follows the primary system language and persists explicit overrides', () => {
  assert.equal(resolveLanguage(null, ['zh-CN']), 'zh')
  assert.equal(resolveLanguage(null, ['zh-Hant-TW']), 'zh')
  assert.equal(resolveLanguage(null, ['en-GB']), 'en')
  assert.equal(resolveLanguage(null, ['fr-FR', 'zh-CN']), 'en')
  assert.equal(resolveLanguage('zh', ['en-US']), 'zh')
  assert.equal(resolveLanguage('invalid', []), 'en')
  const previous = Object.getOwnPropertyDescriptor(globalThis, 'localStorage')
  const calls = []
  Object.defineProperty(globalThis, 'localStorage', { configurable: true, value: { setItem: (...args) => calls.push(args) } })
  try {
    setLanguage('zh')
    assert.equal(t('Review', '复核'), '复核')
    assert.deepEqual(calls[0], ['laya.workbench.language', 'zh'])
    Object.defineProperty(globalThis, 'localStorage', { configurable: true, get() { throw new Error('disabled') } })
    assert.doesNotThrow(() => setLanguage('en'))
    assert.equal(locale.value, 'en')
  } finally {
    if (previous) Object.defineProperty(globalThis, 'localStorage', previous)
    else delete globalThis.localStorage
  }
})

test('prefill uses original native answers but latest human labels take precedence', () => {
  const decision = { result: { laya_result: { answers: { complexity: { choice: 'low' }, risk: { choice: 'high' }, certainty: { choice: 'clear' } } } } }
  assert.deepEqual(originalLabels(decision), { complexity: 'low', risk: 'high', certainty: 'clear' })
  decision.reviews = [{ labels: { complexity: 'medium', risk: 'high', certainty: 'uncertain', model_tier: 'low' } }]
  assert.equal(reviewLabels(decision).complexity, 'medium')
  const labels = reviewPayloadLabels(reviewLabels(decision))
  assert.equal(labels.model_tier, 'low')
  assert.equal(labels.risk, 'high')
  assert.equal(decision.result.laya_result.answers.complexity.choice, 'low')
  assert.equal(originalLabels({}).risk, '')
  assert.equal(Object.hasOwn(reviewPayloadLabels({ risk: 'high' }), 'model_tier'), false)
})
