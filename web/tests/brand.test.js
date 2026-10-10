import test from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'

test('dashboard, favicon and plugin share the Return geometry', () => {
  const read = path => readFileSync(new URL(path, import.meta.url), 'utf8')
  const mark = read('../public/logo.svg')
  const favicon = read('../public/favicon.svg')
  const plugin = read('../../src/laya_tell_me/plugin/oh-my-laya/assets/icon.svg')
  const geometry = svg => svg.match(/<path d="([^"]+)"/)[1]
  assert.equal(geometry(favicon), geometry(mark))
  assert.equal(geometry(plugin), geometry(mark))
  assert.equal((geometry(mark).match(/M/g) || []).length, 1, 'single unbranched return retained')
  assert.match(favicon, /stroke-width="7"/)
  for (const svg of [favicon, plugin]) assert.match(svg, /fill="#eaf5ff"/)
  assert.match(read('../index.html'), /rel="icon"[^>]+href="\/favicon.svg"/)
  assert.match(read('../src/App.vue'), /class="brand-mark" src="\/logo.svg"/)
  assert.ok(read('../src/App.vue').includes('Oh My Laya — Reflect. Route. Refine.'))
  assert.match(read('../src/styles.css'), /\.brand-tagline \{[^}]*text-transform: none;/)
})
