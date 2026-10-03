const API_ROOT = '/api/v1'

export async function api(path, options = {}) {
  const response = await fetch(`${API_ROOT}${path}`, {
    credentials: 'same-origin',
    ...options,
    headers: {
      Accept: 'application/json',
      ...(options.body ? { 'Content-Type': 'application/json' } : {}),
      ...options.headers
    }
  })
  const contentType = response.headers.get('content-type') || ''
  const payload = response.status === 204
    ? null
    : contentType.includes('application/json')
      ? await response.json()
      : await response.text()
  if (!response.ok) {
    const detail = payload && typeof payload === 'object' ? payload.error : payload
    const message = typeof detail === 'object' ? detail.message : detail
    throw new Error(message || `Request failed (${response.status})`)
  }
  return payload
}

export function jsonBody(value) {
  return JSON.stringify(value)
}

export function eventsUrl(lastEventId) {
  const query = lastEventId ? `?last_event_id=${encodeURIComponent(lastEventId)}` : ''
  return `${API_ROOT}/events${query}`
}
