// Opt-in isolated real-browser acceptance; uses existing Playwright and Chrome.
import assert from 'node:assert/strict'
import { spawn } from 'node:child_process'
import { mkdtempSync, rmSync } from 'node:fs'
import { createConnection } from 'node:net'
import { resolve } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'
import { setTimeout as delay } from 'node:timers/promises'

const { chromium } = await import(pathToFileURL(resolve(process.env.LAYA_PLAYWRIGHT_MODULE)).href)
const root = fileURLToPath(new URL('../', import.meta.url))
const directory = mkdtempSync('/private/tmp/laya-date-browser-')
const service = spawn(resolve(process.env.LAYA_TEST_BINARY), ['service'], {
  env: { ...process.env, LAYA_WORKBENCH_DIR: directory, LAYA_PORT: '0', LAYA_PYTHON: resolve(root, 'tests/fixtures/workbench_worker.py') }, stdio: 'ignore'
})
let browser
function rpc(method) {
  return new Promise((resolveReply, reject) => {
    const socket = createConnection(resolve(directory, 'service.sock'))
    let data = ''
    socket.setTimeout(5000, () => socket.destroy(new Error('RPC timeout')))
    socket.on('error', reject)
    socket.on('connect', () => socket.write(JSON.stringify({ protocol_version: 1, request_id: `dates-${method}`, method, params: {} }) + '\n'))
    socket.on('data', chunk => {
      data += chunk
      if (!data.includes('\n')) return
      socket.end()
      const reply = JSON.parse(data.split('\n')[0])
      if (reply.error) reject(new Error(JSON.stringify(reply.error)))
      else resolveReply(reply.result)
    })
  })
}
try {
  let status
  for (let n = 0; n < 100; n++) {
    try { status = await rpc('status'); break } catch { await delay(50) }
  }
  assert.ok(status?.ok, 'isolated service ready')
  browser = await chromium.launch({ executablePath: process.env.LAYA_CHROME_BIN, headless: true })
  const page = await browser.newPage({ locale: 'en-US', timezoneId: 'Asia/Shanghai', viewport: { width: 1440, height: 1000 } })
  await page.addInitScript(() => localStorage.setItem('laya.workbench.language', 'en'))
  const requests = []
  page.on('request', req => {
    const url = new URL(req.url())
    if (['/api/v1/overview', '/api/v1/cases'].includes(url.pathname)) requests.push({ path: url.pathname, query: url.searchParams.toString() })
  })
  await page.goto((await rpc('pair')).url)
  await page.getByRole('button', { name: 'Last 7 days', exact: true }).waitFor()
  assert.equal(await page.title(), 'Oh My Laya — Decision workbench')
  const languageSelect = page.getByLabel('Language / 语言')
  const languageStyle = await languageSelect.evaluate(el => { const css = getComputedStyle(el); return { appearance: css.appearance, padding: css.paddingRight, position: css.backgroundPosition } })
  assert.equal(languageStyle.appearance, 'none')
  assert.equal(languageStyle.padding, '32px')
  assert.ok(languageStyle.position.includes('12px'), 'language arrow keeps its right inset')
  await languageSelect.selectOption('zh')
  await page.getByRole('button', { name: '最近 7 天', exact: true }).waitFor()
  await languageSelect.focus()
  await page.keyboard.press('e')
  await page.getByRole('button', { name: 'Last 7 days', exact: true }).waitFor()
  assert.equal(await languageSelect.inputValue(), 'en')
  assert.equal(await page.locator('link[rel="icon"]').getAttribute('href'), '/favicon.svg')
  assert.equal(await page.locator('.brand-mark').evaluate(image => image.complete && image.naturalWidth > 0), true)
  assert.equal(await page.locator('.rail').evaluate(rail => getComputedStyle(rail).backgroundColor), 'rgb(234, 245, 255)')
  const favicon = await page.request.get(new URL('/favicon.svg', page.url()).href)
  assert.ok(favicon.ok())
  assert.ok((await favicon.text()).includes('#eaf5ff'))
  if (process.env.LAYA_BRAND_SCREENSHOT) await page.screenshot({ path: process.env.LAYA_BRAND_SCREENSHOT, fullPage: true })
  for (const width of [981, 1024, 1440]) {
    await page.setViewportSize({ width, height: 1000 })
    assert.ok(await page.locator('.brand strong').evaluate(label => label.getBoundingClientRect().right <= document.querySelector('.rail').getBoundingClientRect().right), 'brand stays within sidebar')
    assert.equal(await page.locator('.brand strong').evaluate(label => getComputedStyle(label).whiteSpace), 'nowrap')
  }
  assert.equal(await page.getByRole('button', { name: 'Last 7 days', exact: true }).getAttribute('aria-pressed'), 'true')
  const nextOverview = page.waitForResponse(res => res.url().includes('/api/v1/overview?') && res.ok())
  await page.getByRole('button', { name: 'Last 30 days', exact: true }).click()
  await nextOverview
  const query = requests.filter(item => item.path.endsWith('/overview')).at(-1).query
  const caseResponse = page.waitForResponse(res => res.url().includes('/api/v1/cases?') && res.ok())
  await page.getByRole('button', { name: 'Case study', exact: true }).click()
  await caseResponse
  assert.equal(await page.getByRole('button', { name: 'Last 30 days', exact: true }).getAttribute('aria-pressed'), 'true')
  const latestCases = new URLSearchParams(requests.filter(item => item.path.endsWith('/cases')).at(-1).query)
  const overviewQuery = new URLSearchParams(query)
  for (const key of ['created_after', 'created_before']) assert.equal(latestCases.get(key), overviewQuery.get(key))
  await page.getByRole('button', { name: 'Custom', exact: true }).click()
  await page.getByLabel('Start date', { exact: true }).fill('2099-12-31')
  await page.getByRole('alert').waitFor()
  await delay(100)
  const count = requests.length
  await page.getByLabel('End date', { exact: true }).fill('2000-01-01')
  await delay(150)
  assert.equal(requests.length, count, 'invalid custom range causes no data request')
  await page.getByLabel('Start date', { exact: true }).fill('2026-10-01')
  const customResponse = page.waitForResponse(res => res.url().includes('/api/v1/cases?') && res.ok())
  await page.getByLabel('End date', { exact: true }).fill('2026-10-05')
  await customResponse
  const customQuery = new URLSearchParams(requests.filter(item => item.path.endsWith('/cases')).at(-1).query)
  assert.equal(Number(customQuery.get('created_after')), Date.parse('2026-10-01T00:00:00+08:00') / 1000)
  assert.equal(Number(customQuery.get('created_before')), Date.parse('2026-10-06T00:00:00+08:00') / 1000 - 1)
  await page.locator('.nav-item').filter({ hasText: 'Overview' }).click()
  assert.equal(await page.locator('main').evaluate(el => getComputedStyle(el).backgroundColor), 'rgb(255, 255, 255)')
  assert.equal(await page.locator('main').evaluate(el => getComputedStyle(el).backgroundImage), 'none')
  const dates = await page.locator('.custom-dates').evaluate(el => {
    const boxes = [...el.querySelectorAll('input'), el.querySelector(':scope > span')].map(node => node.getBoundingClientRect())
    return boxes.map(box => ({ y: box.y, height: box.height }))
  })
  assert.ok(dates.every(box => Math.abs(box.y - dates[0].y) < 1 && box.height === 38), 'date inputs and separator share vertical center')
  if (process.env.LAYA_STYLE_SCREENSHOT) await page.screenshot({ path: process.env.LAYA_STYLE_SCREENSHOT, fullPage: true })
  for (const width of [1024, 768, 390, 320]) {
    await page.setViewportSize({ width, height: 900 })
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), `no page overflow at ${width}`)
    assert.ok(await page.locator('.custom-dates input').evaluateAll(inputs => inputs.every(input => input.getBoundingClientRect().right <= innerWidth)), `date controls fit at ${width}`)
  }
  await page.setViewportSize({ width: 1440, height: 1000 })
  assert.equal(await page.getByLabel('Start date', { exact: true }).inputValue(), '2026-10-01')
  assert.equal(await page.getByLabel('End date', { exact: true }).inputValue(), '2026-10-05')
  console.log('PASS default last7; shared last30; invalid range withheld; custom inclusive bounds and reverse navigation')
} finally {
  if (browser) await browser.close()
  try { await rpc('stop') } catch { service.kill('SIGTERM') }
  if (service.exitCode === null) await new Promise(resolveExit => service.once('exit', resolveExit))
  rmSync(directory, { recursive: true, force: true })
}
