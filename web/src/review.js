export function originalLabels(decision = {}) {
  const assessment = decision.result?.advice?.assessment || decision.advice?.assessment
    || decision.result?.laya_result?.answers || decision.result?.answers || {}
  return Object.fromEntries(['complexity', 'risk', 'certainty'].map((key) => [key, assessment[key]?.choice || '']))
}

export function reviewLabels(decision = {}) {
  const latest = decision.reviews?.at(-1)
  return { ...originalLabels(decision), ...(decision.labels || {}), ...(decision.review?.labels || {}), ...(latest?.labels || {}) }
}

export function reviewPayloadLabels(review) {
  return {
    complexity: review.complexity, risk: review.risk, certainty: review.certainty,
    ...(review.model_tier ? { model_tier: review.model_tier } : {})
  }
}
