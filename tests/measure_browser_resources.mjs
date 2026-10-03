// Opt-in macOS measurement. Supply existing Playwright and Chrome paths; installs nothing.
import assert from 'node:assert/strict'
import { execFileSync, spawn } from 'node:child_process'
import { createHash } from 'node:crypto'
import { mkdtempSync, readFileSync, rmSync } from 'node:fs'
import { createConnection } from 'node:net'
import { resolve } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'
import { setTimeout as delay } from 'node:timers/promises'

assert.equal(process.platform, 'darwin', 'This measurement uses macOS ps RSS')
assert.ok(process.env.LAYA_PLAYWRIGHT_MODULE, 'Set LAYA_PLAYWRIGHT_MODULE to an existing Playwright index.mjs')
assert.ok(process.env.LAYA_CHROME_BIN, 'Set LAYA_CHROME_BIN to an existing Chrome executable')
const { chromium } = await import(pathToFileURL(resolve(process.env.LAYA_PLAYWRIGHT_MODULE)).href)
const root = fileURLToPath(new URL('../', import.meta.url))
const binary = resolve(root, 'target/release/laya')
const binarySha256 = createHash('sha256').update(readFileSync(binary)).digest('hex')
const populate = process.argv.includes('--populate')
const populatedRows = 75
// Keep Unix socket paths below macOS sockaddr_un's path length limit.
const directory = mkdtempSync('/private/tmp/laya-browser-resource-')
const env = { ...process.env, LAYA_WORKBENCH_DIR: directory,
  LAYA_PYTHON: resolve(root, 'tests/fixtures/workbench_worker.py'), LAYA_IDLE_SECONDS: '1' }
const service = spawn(binary, ['service'], { env, stdio: 'ignore' })
let serviceError
service.on('error', (error) => { serviceError = error })
let browser
let report
const command = (args) => execFileSync(binary, args, { env, encoding: 'utf8', timeout: 10000 })
const status = () => JSON.parse(command(['status']))

function rpc(method, params = {}, requestId = `browser-resource-${Date.now()}`) {
  return new Promise((resolveRequest, reject) => {
    const socket = createConnection(resolve(directory, 'service.sock'))
    let response = ''
    let settled = false
    const finish = (action, value) => {
      if (settled) return
      settled = true
      action(value)
    }
    socket.setEncoding('utf8')
    socket.setTimeout(10000)
    socket.on('connect', () => socket.write(`${JSON.stringify({ protocol_version: 1, request_id: requestId, method, params })}\n`))
    socket.on('data', (chunk) => {
      response += chunk
      const newline = response.indexOf('\n')
      if (newline === -1) return
      socket.end()
      try {
        const decoded = JSON.parse(response.slice(0, newline))
        if (decoded.error) finish(reject, new Error(JSON.stringify(decoded.error)))
        else finish(resolveRequest, decoded.result)
      } catch (error) {
        finish(reject, error)
      }
    })
    socket.on('end', () => finish(reject, new Error(`RPC ${method} ended without a complete response`)))
    socket.on('timeout', () => socket.destroy(new Error(`RPC ${method} timed out`)))
    socket.on('error', (error) => finish(reject, error))
  })
}

function rss(pid) {
  try {
    return Number(execFileSync('ps', ['-p', String(pid), '-o', 'rss='], { encoding: 'utf8', timeout: 2000 }).trim())
  } catch {
    return null // Browser processes may exit between discovery and sampling.
  }
}

function requireBrowserSamples(rows) {
  for (const type of ['browser', 'renderer']) {
    assert.ok(rows.some((row) => row.type === type && Number.isFinite(row.rss_kib) && row.rss_kib > 0),
      `No valid ${type} RSS measurement; resource observation is unavailable`)
  }
}

try {
  let ready = false
  for (let attempt = 0; attempt < 100; attempt++) {
    if (serviceError) throw serviceError
    if (service.exitCode !== null) throw new Error('Owned service exited before readiness')
    if (status().ok === true) { ready = true; break }
    await delay(50)
  }
  assert.ok(ready, 'Service did not become ready')
  browser = await chromium.launch({ executablePath: process.env.LAYA_CHROME_BIN, headless: true })
  const session = await browser.newBrowserCDPSession()
  const page = await browser.newPage({ viewport: { width: 1440, height: 1000 } })
  const errors = []
  page.on('pageerror', (error) => errors.push(error.message))
  page.on('response', (response) => {
    if (response.url().includes('/api/') && response.status() >= 400) errors.push(`${response.status()} ${response.url()}`)
  })
  async function sample() {
    const { processInfo } = await session.send('SystemInfo.getProcessInfo')
    return processInfo.map(({ id, type }) => ({ pid: id, type, rss_kib: rss(id) }))
  }
  await page.goto('about:blank')
  await delay(2000)
  const baseline = await sample()
  requireBrowserSamples(baseline)
  const url = command(['dashboard', '--no-open']).trim()
  const response = await page.goto(url)
  assert.equal(response.status(), 200)
  await page.getByRole('button', { name: /Settings & status/ }).click()
  await page.locator('.settings-page').waitFor()
  await page.waitForFunction(() => !location.hash.includes('pair='))
  let queuePages = null
  let queueResponseBytes = null
  if (populate) {
    const recording = page.getByRole('button', { name: /Decision recording/ })
    page.once('dialog', (dialog) => dialog.accept())
    await recording.click()
    await page.waitForFunction(() => [...document.querySelectorAll('button')].some((button) =>
      button.textContent.includes('Decision recording') && button.getAttribute('aria-pressed') === 'true'))
    const stateSuffix = 'x'.repeat(4096)
    for (let row = 0; row < populatedRows; row++) {
      const result = await rpc('predict', {
        state: `Populated browser resource fixture decision ${String(row).padStart(3, '0')} ${stateSuffix}`,
        advisor: { models: [] }
      }, `browser-resource-row-${row}`)
      assert.equal(result.meta.recording_status, 'stored')
    }
    for (let attempt = 0; attempt < 100 && status().worker.pid !== 0; attempt++) await delay(50)
    assert.equal(status().worker.pid, 0, 'Fixture worker did not exit before browser measurement')
    const firstResponsePromise = page.waitForResponse((response) =>
      response.url().includes('/api/v1/decisions?') && response.url().includes('offset=0'))
    await page.getByRole('button', { name: /Review queue/ }).click()
    const firstResponse = await firstResponsePromise
    await page.locator('.decision-list').waitFor()
    await page.getByText('Page 1 · 50 decisions').waitFor()
    const firstPageIds = await page.locator('.decision-row').evaluateAll((rows) => rows.map((row) => row.textContent))
    assert.equal(firstPageIds.length, 50)
    const secondResponsePromise = page.waitForResponse((response) =>
      response.url().includes('/api/v1/decisions?') && response.url().includes('offset=50'))
    await page.getByRole('button', { name: 'Next' }).click()
    const secondResponse = await secondResponsePromise
    await page.getByText('Page 2 · 25 decisions').waitFor()
    const secondPageIds = await page.locator('.decision-row').evaluateAll((rows) => rows.map((row) => row.textContent))
    assert.equal(secondPageIds.length, 25)
    assert.equal(new Set([...firstPageIds, ...secondPageIds]).size, populatedRows)
    queuePages = [firstPageIds.length, secondPageIds.length]
    queueResponseBytes = [(await firstResponse.body()).byteLength, (await secondResponse.body()).byteLength]
    await page.getByRole('button', { name: 'Previous' }).click()
    await page.getByText('Page 1 · 50 decisions').waitFor()
  }
  const peaks = new Map()
  const started = performance.now()
  let samples = 0
  for (let second = 0; second < 60; second++) {
    await delay(1000)
    const rows = await sample()
    requireBrowserSamples(rows)
    for (const row of rows) {
      if (row.rss_kib !== null) {
        const previous = peaks.get(row.pid)
        peaks.set(row.pid, { ...row, peak_rss_kib: Math.max(previous?.peak_rss_kib || 0, row.rss_kib) })
      }
    }
    const current = status()
    assert.equal(current.settings.recording_enabled, populate)
    assert.equal(current.worker.pid, 0, 'History/status browsing must not load a model')
    samples++
  }
  const finalStatus = status()
  if (populate) {
    assert.equal(finalStatus.counts.decisions, populatedRows)
    assert.equal(finalStatus.counts.pending_reviews, populatedRows)
  }
  const serviceRss = rss(service.pid)
  assert.ok(Number.isFinite(serviceRss) && serviceRss > 0, 'Service RSS unavailable')
  report = { browser: browser.version(), binary_sha256: binarySha256, headless: true,
    dataset: populate ? `${populatedRows} fixture decisions in disposable database` : 'empty disposable database',
    state_characters_per_decision: populate ? `Populated browser resource fixture decision NNN `.length + 4096 : 0,
    dataset_counts: finalStatus.counts, database_bytes: finalStatus.database_bytes,
    wal_bytes: finalStatus.wal_bytes, evidence_bytes: finalStatus.evidence_bytes,
    storage_bytes: finalStatus.storage_bytes, queue_page_rows: queuePages,
    queue_response_bytes: queueResponseBytes,
    duration_seconds: (performance.now() - started) / 1000, samples, baseline,
    dashboard_processes: [...peaks.values()], service_rss_kib: serviceRss,
    recording_enabled_during_sampling: populate, worker_loaded_during_sampling: false,
    fixture_worker: populate ? 'tests/fixtures/workbench_worker.py; exited before sampling; no real model loaded' : 'not started',
    ui_errors: errors,
    scope: 'macOS ps per-process RSS for CDP-reported processes (may exclude crash helpers); separate isolated Chrome; no summed unique or GPU memory; browser RAM is an optimization observation, not a hard threshold' }
  if (populate) {
    await page.getByRole('button', { name: /Settings & status/ }).click()
    await page.locator('.settings-page').waitFor()
    const recording = page.getByRole('button', { name: /Decision recording/ })
    page.once('dialog', (dialog) => dialog.accept())
    await recording.click()
    await page.waitForFunction(() => [...document.querySelectorAll('button')].some((button) =>
      button.textContent.includes('Decision recording') && button.getAttribute('aria-pressed') === 'false'))
    assert.equal(status().settings.recording_enabled, false)
  }
  assert.deepEqual(errors, [])
} finally {
  try {
    if (browser) await browser.close()
  } finally {
    try { command(['stop']) } catch { if (service.pid) service.kill('SIGTERM') }
    for (let attempt = 0; attempt < 100 && service.exitCode === null && service.signalCode === null && !serviceError; attempt++) await delay(50)
    if (service.exitCode !== null || service.signalCode !== null || serviceError) rmSync(directory, { recursive: true })
    else throw new Error(`Owned service did not stop; preserved temporary data at ${directory}`)
  }
}
console.log(JSON.stringify({ ...report, cleanup: populate ? 'recording disabled; owned browser, service, and temporary directory removed' : 'owned browser, service, and temporary directory removed' }, null, 2))
