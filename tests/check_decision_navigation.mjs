// Opt-in browser regression against an isolated service and synthetic decisions.
import assert from 'node:assert/strict'
import { spawn } from 'node:child_process'
import { mkdtempSync, readFileSync, rmSync } from 'node:fs'
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
let page
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
  page = await browser.newPage({ viewport: { width: 1440, height: 1000 }, locale: 'en-US' })
  // Allow UI-only checks against an existing binary, without rebuilding Rust.
  if (process.env.LAYA_TEST_WEB_DIST) {
    const dist = resolve(process.env.LAYA_TEST_WEB_DIST)
    await page.route('http://127.0.0.1:*/**', route => {
      const path = new URL(route.request().url()).pathname
      if (path !== '/' && !/^\/assets\/[\w.-]+\.(js|css)$/.test(path)) return route.continue()
      const contentType = path === '/' ? 'text/html' : path.endsWith('.js') ? 'text/javascript' : 'text/css'
      return route.fulfill({ contentType, body: readFileSync(dist + (path === '/' ? '/index.html' : path)) })
    })
  }
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
  if (!process.env.LAYA_STYLE_ONLY) {
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
  }
  const enabled = await page.request.patch(origin + '/api/v1/settings', { data: { recording_enabled: true }, headers: { Origin: origin } })
  assert.equal(enabled.status(), 200)
  for (let i = 0; i < 12; i++) await rpc('predict', { state: `navigation-fixture ${i}`, questions: { risk: {} } })
  await page.getByRole('button', { name: 'Refresh', exact: true }).click()
  if (!process.env.LAYA_STYLE_ONLY) {
  await page.getByLabel('Search decisions').fill('navigation-fixture')
  await page.getByText('1 / 2', { exact: true }).waitFor()
  async function checkChartHover() {
    const chart = page.locator('.activity-chart-plot svg')
    const hoverAt = async x => {
      const point = await chart.evaluate((svg, x) => {
        const point = svg.createSVGPoint()
        point.x = x
        point.y = 60
        const screen = point.matrixTransform(svg.getScreenCTM())
        return { x: screen.x, y: screen.y }
      }, x)
      await page.mouse.move(point.x, point.y)
    }
    const lastX = Number(await chart.locator(':scope > circle').last().getAttribute('cx'))
    await hoverAt(Math.max(0, lastX - 0.01))
    const tooltip = page.getByRole('tooltip')
    await tooltip.waitFor()
    assert.match(await tooltip.innerText(), /Cumulative decisions\s+12/)
    assert.equal(await chart.locator('.activity-crosshair line').count(), 1)
    await hoverAt(Math.min(600, lastX + 5))
    await tooltip.waitFor({ state: 'hidden' })
    await page.setViewportSize({ width: 390, height: 844 })
    await chart.scrollIntoViewIfNeeded()
    await hoverAt(Math.max(0, lastX - 0.01))
    await tooltip.waitFor()
    const tipBox = await tooltip.boundingBox()
    const plotBox = await page.locator('.activity-chart-plot').boundingBox()
    assert.ok(tipBox.x >= plotBox.x && tipBox.x + tipBox.width <= plotBox.x + plotBox.width + 1, 'tooltip stays inside the narrow chart')
    await page.mouse.move(0, 0)
    await tooltip.waitFor({ state: 'hidden' })
    await page.setViewportSize({ width: 1440, height: 1000 })
  }
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
  await page.getByRole('navigation').getByRole('button', { name: /Overview/ }).click()
  await checkChartHover()
  }
  for (const locale of ['en', 'zh']) {
    await page.getByLabel('Language / 语言').selectOption(locale)
    for (const width of [1440, 390]) {
      await page.setViewportSize({ width, height: 1000 })
      for (const tab of ['cases', 'settings', 'queue']) {
        await page.goto(origin + '/?tab=' + tab)
        await page.locator('.top-actions select').waitFor()
        await page.getByLabel('Language / 语言').selectOption(locale)
        await page.locator(tab === 'cases' ? '.queue-panel' : tab === 'settings' ? '.setting-panel' : '.activity-log').first().waitFor()
        await page.locator('details').evaluateAll(elements => elements.forEach(el => { el.open = true }))
        const controls = await page.locator('select').evaluateAll(elements => elements.filter(el => el.getClientRects().length).map(el => {
          const css = getComputedStyle(el)
          return { label: el.getAttribute('aria-label') || el.parentElement.textContent.slice(0, 50), appearance: css.appearance, padding: parseFloat(css.paddingRight), image: css.backgroundImage }
        }))
        for (const control of controls) {
          assert.equal(control.appearance, 'none', `${tab}/${locale}/${width}: ${control.label} uses native edge arrow`)
          assert.ok(control.padding >= 32, `${control.label}: reserve arrow space`)
          assert.notEqual(control.image, 'none', `${control.label}: shared arrow missing`)
        }
        assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), `${tab}/${locale}/${width}: page overflows horizontally`)
        if (tab === 'cases') {
          await page.screenshot({ path: `/private/tmp/laya-controls-${locale}-${width}.png`, fullPage: true })
          await page.locator('.queue-panel .panel-head > select').selectOption('all')
          await page.locator('.decision-row').first().click()
          await page.locator('.review-form').waitFor()
          await page.locator('.review-form details').evaluateAll(elements => elements.forEach(el => { el.open = true }))
          for (const select of await page.locator('.review-form select').all()) {
            assert.equal(await select.evaluate(el => getComputedStyle(el).appearance), 'none')
            const box = await select.boundingBox()
            assert.ok(box && box.x >= 0 && box.x + box.width <= width + 1, 'review control fits viewport')
          }
        }
      }
    }
  }
  assert.deepEqual(errors, [])
  console.log(process.env.LAYA_STYLE_ONLY ? 'PASS: shared select arrows, review controls, English/Chinese, desktop/mobile layouts' : 'PASS: chart hover, navigation, and shared control styles')
} catch (error) {
  if (page) {
    console.error(await page.locator('body').innerText())
    await page.screenshot({ path: '/private/tmp/laya-navigation-failure.png', fullPage: true })
  }
  throw error
} finally {
  await browser?.close()
  try { await rpc('stop') } catch { service.kill('SIGTERM') }
  await new Promise(resolveExit => service.exitCode == null ? service.once('exit', resolveExit) : resolveExit())
  rmSync(directory, { recursive: true, force: true })
}
