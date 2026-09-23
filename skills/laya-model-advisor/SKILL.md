---
name: laya-model-advisor
description: Use when the user asks Oh My Laya to recommend a Codex model or reasoning effort, configure recommendation permissions, or assess each new task in this session. Do not trigger on ordinary coding tasks unless the user enabled session advice. This is an advisory workflow, not an automatic model-switching hook.
---

# Laya model advisor

Assess a short task summary through the installed `laya_tell_me` MCP tool. Never
invent a Laya result, model catalog, popup response, or successful model switch.
Use the user's language for questions and explanations.

## Consent

1. Read `laya_advisor_preferences` with no arguments. On first use or when the
   user requests a policy change, offer three choices using the host's structured
   question UI (`request_user_input_async` if available):
   - Always ask (recommended).
   - Ask only for high complexity, high risk, or uncertainty; otherwise accept.
   - Automatically accept all recommendations, including uncertain assessments.
2. Explain that preferences persist across sessions on this machine, affect only
   recommendations, and grant no execution permissions. Save the exact user's
   choice with `laya_advisor_preferences(policy="always"|"conditional"|"auto")`.
   An unanswered, preselected or dismissed choice is NOT consent. Wait for the
   response; do not change the policy while awaiting it. For session-only requests,
   do not persist a policy: use always-ask or explain the persisted setting.

## Assessment and model choices

1. Obtain the current host's available model IDs and supported reasoning efforts
   from live host metadata or an accessible `model/list` API. Do not launch or
   modify another session to get access. Do not use a generic provider model list
   or cached assumptions about account access. If unavailable, explain that the
   list cannot be verified and ask the user to supply it; no fabricated choices.
2. Pass `advisor={"current_model": "verified-current-id", "models": [...]}` to
   `laya_tell_me`, omitting `questions`. Each model entry has `id` and
   `reasoning_efforts` ordered least to most. Omit current_model if unknown.
   Optional `tier` values fast/balanced/strong must come from the user's explicit
   preferences; never infer a ranking from model names. Without a tier mapping,
   advice retains the known current model and adjusts only reasoning. Explain
   this fallback; users can still choose any listed model themselves.
3. Keep `state` a short, factual summary of requirements, impact and unknowns
   (roughly 200 tokens). Never include credentials, full source files or the model
   catalog in state. Catalog/policy are processed outside Laya. If the tool fails,
   report the failure and keep the current settings; do not auto-accept a guess.
4. Summarize returned complexity, risk, uncertainty, recommendation and rationale.
   Confidence below 0.6 is a conservative heuristic, not calibrated accuracy.
   Respect `advice.ask_user`: when true, present a structured model question with
   the recommended model first (if present), every remaining verified model, and
   "Keep current settings". Include the assessment and recommended effort in the
   question. If the UI limits options, show the complete numbered catalog in the
   question body and accept a model ID via free text. Do not silently truncate.
5. After a model is selected, offer ONLY its supported reasoning efforts in a
   second question, recommended effort first if valid, plus keeping current
   settings. Validate free-text choices against the verified catalog. Do not
   continue as if consent was granted while either answer is pending. If no
   structured UI exists, ask in text and wait; do not claim a popup was shown.
6. When ask_user is false, state that the recommendation was automatically
   accepted under the chosen policy. No permission prompt is necessary. A null
   recommendation is not acceptance: retain current settings and explain what
   information is missing.

## Applying and repeating

This integration does NOT switch the active Codex model. After an accepted choice,
tell the user the exact model and effort to select in Codex's model picker. Do not
claim a switch occurred, edit global Codex configuration, restart/resume/fork the
thread, or send a message to the active thread as a workaround. If the user keeps
current settings or declines, continue without changing them. Any actual switch
requires a supported host control and verifiable success; this skill supplies none.

If the user enables advice for this session, repeat once per new substantive task
or material scope change, not per message or tool call. Stop when asked. Skill
matching is not a guaranteed per-turn hook. This policy never authorizes file
deletion, publication, purchases, shell execution, or bypassing host approvals.
