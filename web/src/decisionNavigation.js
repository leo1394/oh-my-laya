export function decisionRoute(location) {
  const params = new URL(location.href).searchParams
  const id = params.get('decision')
  if (id) return { tab: 'detail', id, from: params.get('from') === 'queue' ? 'queue' : 'cases' }
  return { tab: ['queue', 'cases', 'settings'].includes(params.get('tab')) ? params.get('tab') : 'queue' }
}

export function routeUrl(location, route) {
  const url = new URL(location.href)
  for (const key of ['decision', 'from', 'tab']) url.searchParams.delete(key)
  if (route.tab === 'detail') {
    url.searchParams.set('decision', route.id)
    url.searchParams.set('from', route.from)
  } else if (route.tab !== 'queue') url.searchParams.set('tab', route.tab)
  return url.pathname + url.search + url.hash
}

export function rememberRoute(browser, route, push = false) {
  browser.history[push ? 'pushState' : 'replaceState'](
    { ...browser.history.state, layaRoute: route }, '', routeUrl(browser.location, route))
}
