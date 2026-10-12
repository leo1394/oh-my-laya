// Opt-in browser regression against an isolated service and synthetic decisions.
import assert from 'node:assert/strict'
import { spawn } from 'node:child_process'
import { mkdtempSync, rmSync } from 'node:fs'
import { createConnection } from 'node:net'
import { resolve } from 'node:path'
import { pathToFileURL } from 'node:url'
import { setTimeout as delay } from 'node:timers/promises'

const { chromium } = await import(pathToFileURL(resolve(process.env.LAYA_PLAYWRIGHT_MODULE)).href)
const directory = mkdtempSync('/private/tmp/laya-navigation-')
let service = spawn(resolve(process.env.LAYA_TEST_BINARY), ['service'], {
  env: { ...process.env, LAYA_WORKBENCH_DIR: directory, LAYA_PORT: '0', LAYA_NATIVE_USAGE_SYNC: '0', LAYA_PYTHON: resolve('tests/fixtures/workbench_worker.py') }, stdio: 'ignore'
})
let browser
function rpc(method, params = {}) {
  return new Promise((resolveReply, reject) => {
    const socket = createConnection(resolve(directory, 'service.sock'))
    let data = ''
    socket.setTimeout(5000, () => socket.destroy(new Error('RPC timeout')))
    socket.on('error', reject)
    socket.on('connect', () => socket.write(JSON.stringify({ protocol_version: 1, request_id: `navigation-${Date.now()}-${Math.random()}`, method, params }) + '\n'))
    socket.on('data', chunk => {
      data += chunk
      if (!data.includes('\n')) return
      socket.end()
      const reply = JSON.parse(data.split('\n')[0])
      if (reply.error) reject(new Error(reply.error))
      else resolveReply(reply.result)
    })
  })
}
try {
  for (let i = 0; i < 100; i++) {
    try { await rpc('status'); break } catch { await delay(50) }
  }
  browser = await chromium.launch({ executablePath: process.env.LAYA_CHROME_BIN, headless: true })
  const page = await browser.newPage({ viewport: { width: 1440, height: 1000 }, locale: 'en-US' })
  const errors = []
  page.on('pageerror', error => errors.push(error.message))
  await page.addInitScript(() => localStorage.setItem('laya.workbench.language', 'en'))
  const pairingUrl = (await rpc('pair')).url
  await page.goto(new URL(pairingUrl).origin)
  await page.getByRole('alert').filter({ hasText: 'This browser is not paired' }).waitFor()
  assert.equal(await page.getByLabel('Search decisions').count(), 0)
  await page.goto(pairingUrl)
  await page.getByLabel('Search decisions').waitFor()
  const origin = new URL(page.url()).origin
  const stopped = new Promise(resolveExit => service.once('exit', resolveExit))
  await rpc('stop')
  await stopped
  service = spawn(resolve(process.env.LAYA_TEST_BINARY), ['service'], {
    env: { ...process.env, LAYA_WORKBENCH_DIR: directory, LAYA_PORT: new URL(origin).port, LAYA_NATIVE_USAGE_SYNC: '0', LAYA_PYTHON: resolve('tests/fixtures/workbench_worker.py') }, stdio: 'ignore'
  })
  for (let i = 0; i < 100; i++) {
    try { await rpc('status'); break } catch { await delay(50) }
  }
  await page.reload()
  await page.getByLabel('Search decisions').waitFor()
  // Reopening a bookmarked, already-used pairing URL keeps valid browser trust.
  await page.goto(pairingUrl)
  await page.getByLabel('Search decisions').waitFor()
  assert.equal(new URL(page.url()).hash, '')
  const enabled = await page.request.patch(origin + '/api/v1/settings', { data: { recording_enabled: true }, headers: { Origin: origin } })
  assert.equal(enabled.status(), 200)
  for (let i = 0; i < 12; i++) await rpc('predict', { state: `navigation-fixture ${i}`, questions: { risk: {} } })
  await page.getByRole('button', { name: 'Refresh', exact: true }).click()
  await page.getByLabel('Search decisions').fill('navigation-fixture')
  await page.getByText('1 / 2', { exact: true }).waitFor()
  await page.getByRole('button', { name: 'Next', exact: true }).click()
  await page.getByText('2 / 2', { exact: true }).waitFor()
  await page.getByText('Updating results…', { exact: true }).waitFor({ state: 'hidden' })
  const row = page.locator('.activity-row').first()
  await row.locator('summary').click()
  const contentX = await row.locator('.activity-row-main').evaluate(el => el.getBoundingClientRect().x)
  const detailX = await row.locator('.activity-row-detail > p').first().evaluate(el => el.getBoundingClientRect().x)
  assert.ok(Math.abs(contentX - detailX) < 1, 'expanded content aligns with decision text')
  await row.getByRole('button', { name: 'Open details' }).scrollIntoViewIfNeeded()
  const previousY = await page.evaluate(() => window.scrollY)
  await row.getByRole('button', { name: 'Open details' }).click()
  await page.getByRole('button', { name: 'Back to recorded decisions' }).waitFor()
  assert.equal(new URL(page.url()).searchParams.get('from'), 'queue')
  await page.goBack()
  await page.locator('.activity-log').waitFor({ state: 'visible' })
  assert.equal(await page.getByLabel('Search decisions').inputValue(), 'navigation-fixture')
  await page.getByText('2 / 2', { exact: true }).waitFor()
  assert.equal(await page.locator('.activity-row[open]').count(), 1)
  assert.ok(Math.abs(await page.evaluate(() => window.scrollY) - previousY) < 2, 'back restores page position')
  await page.goForward()
  await page.getByRole('button', { name: 'Back to recorded decisions' }).click()
  await page.locator('.activity-log').waitFor({ state: 'visible' })
  await page.getByRole('navigation').getByRole('button', { name: /Case study/ }).click()
  await page.getByLabel('Case filter').selectOption('all')
  await page.locator('.decision-row').first().click()
  await page.getByRole('button', { name: 'Back to Case study' }).waitFor()
  await page.goBack()
  await page.locator('.cases-page').waitFor({ state: 'visible' })
  assert.deepEqual(errors, [])
  console.log('PASS: alignment, source-aware back button, browser back/forward, list search/page/expansion preservation')
} finally {
  await browser?.close()
  try { await rpc('stop') } catch { service.kill('SIGTERM') }
  await new Promise(resolveExit => service.exitCode == null ? service.once('exit', resolveExit) : resolveExit())
  rmSync(directory, { recursive: true, force: true })
}
