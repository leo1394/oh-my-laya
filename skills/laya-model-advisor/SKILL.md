---
name: laya-model-advisor
description: Use when the user asks Oh My Laya to recommend a coding-agent model or reasoning effort, configure recommendation permissions, or assess each new task in this session. Supports Codex and capability-checked Claude Code, DSH and pi hosts. Do not trigger on ordinary coding tasks unless the user enabled session advice. Does not switch the main model.
---

# Laya model advisor

Assess a short task summary through the installed `laya_tell_me` MCP tool. Never
invent a Laya result, model catalog, popup response, or successful model switch.
Use the user's language for questions and explanations.

## Non-Codex hosts

For Claude Code, DeepSeek Harness (DSH), and pi, use this host's live capabilities
instead of the Codex-specific discovery and configuration steps below. Do not
read Codex logs, require CODEX_THREAD_ID, apply Codex desktop effort settings, or
call Codex tools. Resolve the current model, supported effort controls and available
models through this host's verified metadata. Never translate effort names or rank
models without evidence. If required model/effort data is unavailable, report the
limitation and keep advice manual; do not fabricate a catalog or automatic route.

Use the same Laya tool schemas and confirmed policy/ceiling rules. Different hosts
may share saved preferences: revalidate the saved pair against the current host
before automatic acceptance. A null or incompatible recommendation requires setup.
On pi the extension exposes both laya_tell_me (including advisor mode) and
laya_advisor_preferences. On Claude/DSH use the registered MCP tools.

Use a single multi-step selection with final confirmation when the native UI
supports it. If required UI or subagent binding is unavailable, explain and ask
whether the user wants supported manual/single-agent use; do not pretend a popup
or model switch occurred. Respect Alpha Squad's capability gate if it is active.
Use only native usage telemetry for this host, never Codex's log collector.
Claude/DSH retain their native /goal implementation when available. Pi's installed
/goal is a user-invoked prompt template, not a persistent background goal engine.
The remaining policy, bounded-advice and permission boundaries apply unchanged.

## Question / answer gate

Use ONE window with four required steps submitted together: policy, model,
reasoning, Final confirmation. Never open separate policy or confirmation windows.

- Transition from `ready` to `awaiting_answer` when sending a question. A tool
  response such as `accepted: true` acknowledges delivery, NOT a user answer.
- While awaiting an asynchronous answer, keep the turn active. Do not send a
  final response, even one saying "waiting for your answer": ending the turn may
  dismiss the host's question UI. Use an available interruptible wait (for example
  `clock.sleep` for at most 60 seconds per call), then check for new user input.
  A wait timeout is not an answer; continue waiting without resending the question.
  Do not poll MCP preferences, call inference, save a default, or open the next
  question while the gate is closed. Use brief status updates only as required by
  the host; never repeat the question or replace the pending UI automatically.
- Advance only after an explicit answer to the pending question has been received
  and validated. Partial, unrelated, empty or invalid input leaves the gate closed;
  ask only for the missing clarification. Honor cancellation or a new overriding
  request immediately. A preselection, dismissal or elapsed time is never consent.
- If the host offers a blocking question tool in the current mode, it can supply
  this wait directly. Do not call a tool restricted to another mode. If neither
  blocking input nor interruptible waiting is available, use a plain-text question
  and wait for a new user turn; do not promise a persistent popup.
- This is workflow waiting, not a failed task or permission to mark a Goal blocked.
  On resumption, preserve the pending question and never infer an unrecorded answer.
  Track `selection_pending` and `confirmed`; preserve all four answers across
  interruptions. No setting is saved until all answers validate and the fourth
  answer explicitly says **Confirm and continue**.

## One-window setup and confirmation

Read `laya_advisor_preferences` without arguments. Resolve the current model and
locally allowed catalog below BEFORE opening a question. If a task is supplied,
call Laya using existing preferences to obtain advisory information; do not save
new settings or accept the result while setup is incomplete. A setup-only request
does not require inventing a task or a Laya recommendation.

On first use, policy changes, missing/invalid automatic ceilings, an explicit
selection request, or `advice.ask_user=true`, send ONE structured request with
`questions: [policy, model, reasoning, confirmation]`, in that order:

1. **建议策略:** always ask; ask only for high complexity/risk/uncertainty;
   automatically accept within the selected ceiling. For Chinese, use the title:
   "请选择Laya Tell Me建议策略。选择会跨会话保存，仅控制是否确认建议，不自动切换模型，也不授予执行权限。提交前流程保持等待。"
2. **Step 1 — Model:** each locally eligible model once, recommendation first if
   present. Explain: for always/conditional this is the current advisory choice;
   for auto it is the ONLY model authorized for automatic recommendations. Models
   have no universal strength ordering: do not assume other models are below it.
   A "Keep current model" option is allowed only when the current ID is verified.
3. **Step 2 — Reasoning:** distinct locally allowed efforts. Default the proposed
   ceiling to `high` if locally allowed and supported; otherwise propose the highest
   verified effort below high, never a higher fallback. This is a preselection,
   not consent. Higher ceilings require an explicit user choice and local enablement.
   Explain: for auto this is a MAXIMUM, not a fixed effort; Laya may
   recommend lower supported efforts on the approved model. Model and effort are
   still required when auto is selected. Annotate model-specific restrictions
   where needed; a static window cannot dynamically change its choices. Validate
   the exact pair after submission. Do not list the Cartesian product of pairs.
4. **Final confirmation:** offer **Confirm and continue**, **Revise selections**,
   and **Cancel**. State explicitly that confirmation covers the policy, model and
   effort entered in the previous steps; auto saves them as its ceiling. A static
   question cannot interpolate answers that have not arrived: do not fabricate a
   live summary or open a second confirmation popup. The host controls the outer
   submit-button label; require this explicit fourth answer and full submission.

Wait until all four answers arrive. Recheck local restrictions and the exact pair.
Incomplete or invalid submissions do not save anything; reopen the same four-step
window explaining the problem and preserving valid choices. **Revise selections**
reopens that same window without saving; **Cancel** leaves preferences unchanged.
Only after a valid **Confirm and continue** save once:

- always/conditional: `laya_advisor_preferences(policy=...)`.
- auto: `laya_advisor_preferences(policy="auto", ceiling={"model": selected_id,
  "reasoning_effort": selected_effort}, models=filtered_catalog)`.

The MCP must expose the ceiling parameters; if an older installed version does
not, request an update/restart instead of silently saving uncapped auto consent.
Preferences persist across sessions. Advisory choices in always/conditional do
not change the host's model. Do not persist session-only preferences. Do not save
partially answered or preselected defaults. If the native input tool cannot accept
four questions in one request, report that limitation instead of silently splitting
the flow. Use `request_user_input_async` where available; do not use Plan-only tools
in other modes or substitute execution-permission dialogs.

## Assessment and model choices

Before asking about the current model, first inspect local session metadata
read-only. Identify this exact conversation using `CODEX_THREAD_ID` when available;
locate its matching session file under the configured Codex home (`CODEX_HOME`,
otherwise `~/.codex`), and inspect the latest `turn_context` record's `model` and
`effort` (or `reasoning_effort`) fields. Read only the current session and extract
only the needed metadata, not message history or credentials. Check the timestamp
and session identity: a previous turn's record describes its last execution and
must not be presented as proof of the current picker setting. Do not substitute
global defaults, another session's settings, or a previous advisory choice.

If local metadata is missing, stale or incomplete, check available live host
metadata for this conversation. Do not ask the user to manually fill in the model
as the default fallback. If it still cannot be verified, explicitly mark it
unknown and omit `current_model`; continue with the verified available-model
catalog without inventing a current model or automatic recommendation. A catalog
of available models alone does not identify which model this session is using.

### Local reasoning restrictions

Before inference, before showing the four-step window, and after submission, read only
`[desktop].enabled-reasoning-efforts` from the local Codex home's `config.toml`
(parse TOML; do not print unrelated configuration or credentials). Intersect that
allowlist with EACH model's live supported reasoning efforts, preserving the host's
effort order. Pass the filtered catalog to `laya_tell_me` and use the same catalog
for UI options and answer validation. Remove models with no allowed efforts.

If the setting is absent, hide `max`, `ultra`, and `xmax` by default. Show one of
those exact values only if explicitly enabled locally AND supported by that model.
Do not infer enablement from a current-session effort or from the host advertising
the capability. Do not equate `xmax` with `max` or `xhigh`; never invent aliases.
If the setting exists but is malformed or cannot be read, report the problem and
pause selection rather than bypassing it. An explicit empty allowlist permits no
choices. Local-only values such as `persistent` must not appear unless the model
also advertises them. Never edit Codex configuration to enable hidden efforts.

Revalidate after the user submits: if availability or local restrictions changed,
explain and reopen selection instead of silently substituting a different effort.

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
   Use the four-step window above when confirmation or setup is required.
5. In auto mode, only accept a recommendation within the saved ceiling and current
   filtered catalog. The backend restricts advice to the approved model and the
   effort prefix ending at the ceiling. If that model or ceiling effort becomes
   unavailable, ask for a new ceiling in the same four-step window; do not use a
   replacement model or broaden the ceiling automatically.
6. With valid setup and `ask_user=false`, state that the bounded recommendation
   was automatically accepted; do not open a window for every task in auto mode.
   An explicit request to revise selections still opens the four-step window.
   A null recommendation is not acceptance: keep current settings and explain why.

## Applying and repeating

### Optional Alpha Squad integration

When explicitly configuring subagent routing with `alpha-squad-coding-craft`,
use its `references/laya-routing.md` in place of this skill's standalone setup
window. Check that skill is available before reading it; advice works without it.
One combined window includes execution policy/model/effort, a reviewer model and
effort, and Final confirmation LAST. Never run both setup gates. Preferences
must support `squad`; older MCP versions require an update or standalone manual
Alpha Squad, never uncapped automatic routing.

Only after explicit confirmation save `squad={enabled: true, reviewer: pair or
null}` alongside policy and the auto ceiling when applicable. Old advice-only
preferences leave squad disabled. Omitting squad preserves it; `enabled: false`
disables routing. Reviewer configuration is independent of the execution ceiling.
High complexity, high risk or uncertainty routes reviews to the exact orchestrator;
ordinary reviews use the configured pair (or orchestrator by default).

For role advice pass role and the verified current_reasoning_effort as well as
current_model and the filtered models. The returned `advice.delegation` is only
validated spawn guidance: Alpha Squad must apply its own gate, task authorization,
and actual native spawn call. It can assign SUBAGENT models, never this main
session. The following manual-switch instructions apply to standalone session
advice, not delegated role recommendations.

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
