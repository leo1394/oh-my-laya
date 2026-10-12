import test from 'node:test'
import assert from 'node:assert/strict'
import { decisionRoute, rememberRoute, routeUrl } from '../src/decisionNavigation.js'

test('details keep their origin and safely encode decision IDs', () => {
  const location = { href: 'http://localhost/?other=keep' }
  const url = routeUrl(location, { tab: 'detail', id: 'a/b & c', from: 'queue' })
  assert.deepEqual(decisionRoute({ href: 'http://localhost' + url }), { tab: 'detail', id: 'a/b & c', from: 'queue' })
  assert.equal(new URL('http://localhost' + url).searchParams.get('other'), 'keep')
  assert.deepEqual(decisionRoute({ href: 'http://localhost/?decision=one' }), { tab: 'detail', id: 'one', from: 'cases' })
  assert.equal(routeUrl({ href: 'http://localhost/?decision=one&from=queue' }, { tab: 'queue' }), '/')
})

test('opening details adds one history entry; browser back and forward restore list and detail', () => {
  const stack = [{ url: '/', state: null }]
  let index = 0
  const browser = {
    get location() { return { href: 'http://localhost' + stack[index].url } },
    history: {
      get state() { return stack[index].state },
      replaceState(state, _, url) { stack[index] = { state, url } },
      pushState(state, _, url) { stack.splice(++index, stack.length, { state, url }) }
    }
  }
  for (const from of ['queue', 'cases']) {
    rememberRoute(browser, { tab: from, scrollY: 520 })
    rememberRoute(browser, { tab: 'detail', id: 'one', from, returnEntry: true }, true)
    assert.equal(decisionRoute(browser.location).tab, 'detail')
    index -= 1
    assert.equal(decisionRoute(browser.location).tab, from)
    assert.equal(browser.history.state.layaRoute.scrollY, 520)
    index += 1
    assert.equal(decisionRoute(browser.location).from, from)
  }
})
