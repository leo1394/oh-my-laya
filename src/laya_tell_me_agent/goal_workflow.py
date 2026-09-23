"""Opt-in per-client Goal instructions; never enables execution permissions."""

import os
from pathlib import Path
import re
import stat
import sys
import tempfile


TITLE = "## Goal workflow (STRICT)"
CLIENTS = {
    "codex": ("CODEX_HOME", "~/.codex", "AGENTS.md"),
    "claude": ("CLAUDE_CONFIG_DIR", "~/.claude", "CLAUDE.md"),
    "dsh": ("DSH_HOME", "~/.dsh", "AGENTS.md"),
    "pi": ("PI_CODING_AGENT_DIR", "~/.pi/agent", "AGENTS.md"),
}


def client_home(client):
    variable, default, _ = CLIENTS[client]
    return Path(os.environ.get(variable) or default).expanduser().resolve()


def codex_home():
    return client_home("codex")


def select_goal_workflow(choice, dry_run=False, *, client="codex"):
    if choice is not None:
        return choice == "yes"
    if dry_run:
        print(f"+ would ask whether to enable {client} /goal integration (no changes in dry-run)")
        return False
    if not sys.stdin.isatty():
        print("! no interactive terminal; leaving Goal rules unchanged (use --goal-workflow yes to opt in)")
        return False
    print(f"Optional {client} /goal integration updates '{TITLE}' in {client_home(client) / CLIENTS[client][2]}.")
    if client == "pi":
        print("Pi also installs prompts/goal.md: a workflow prompt, not a persistent Goal loop.")
    print("Existing rules in that section will be replaced after backup; other sections are preserved.")
    while True:
        try:
            answer = input("Use Alpha Squad + Laya with /goal? [y/N]: ").strip().lower()
        except (EOFError, KeyboardInterrupt):
            return False
        if answer in ("y", "yes"):
            return True
        if answer in ("", "n", "no"):
            return False
        print("Please enter y or n.")


def skill_path(home, agents_skills, name, *, planned=False):
    candidates = list(dict.fromkeys([
        home / "skills" / name / "SKILL.md", agents_skills / name / "SKILL.md",
    ]))
    existing = [path for path in candidates if path.exists() or path.is_symlink()]
    if len(existing) > 1:
        raise RuntimeError(f"Ambiguous installed skill locations: {name}")
    if existing:
        if not existing[0].is_file():
            raise RuntimeError(f"Not a readable skill file: {existing[0]}")
        return existing[0].resolve()
    if planned:
        return candidates[0].resolve()
    raise RuntimeError(f"Cannot enable /goal: skill not installed: {name}")


def render_rules(alpha, advisor, client="codex"):
    for path in (alpha, advisor):
        if any(character in str(path) for character in ("\n", "\r", "`")):
            raise ValueError("Skill path cannot be safely embedded in Markdown")
    if client != "codex":
        entry = ("The user invokes the Oh My Laya /goal prompt template (which explicitly requests Alpha Squad)"
                 if client == "pi" else f"The user starts /goal, or {client} supplies verified native active-goal context")
        return f'''{TITLE}
- Activate `alpha-squad-coding-craft` ONLY when {entry}, or the user explicitly asks to use the skill. Quoted commands, documentation and ordinary tasks do not activate it.
- Read `{alpha}` before substantive work. Prefer its optional Laya routing with `{advisor}` and compatible local Laya tools. If unavailable, use standalone manual selection; never invent an assessment.
- Use THIS host's verified model catalog, model/effort controls, user-input UI, delegation and usage APIs. Do not read Codex session logs/configuration or invoke Codex-only tools. Never guess model IDs or claim a subagent assignment succeeded without host evidence. Keep the main session model unchanged.
- Follow the skill's host adapter and capability checks. Use one selection window with model, reasoning and Final confirmation as the last step when supported. Wait for explicit Confirm and continue before proceeding. If required controls or subagents are unavailable, explain the limitation and ask whether to use a supported manual/single-agent fallback; never fabricate compatibility or bypass the gate.
- Read the advisor's non-Codex guidance before applying advice. Advice is not execution authorization. Preserve host approvals and the user's chosen model/effort ceiling.
- Announce exact confirmed Agent models at the start of each user task while active; do not repeat solely for an automatic continuation. Apply the delegation gate rather than spawning every role mechanically.
- Preserve native active-goal context across continuation and compaction; do not require a literal /goal after the UI consumes it. Pending user input is waiting, not goal failure. Never start, complete or fail a native goal without the host's supported controls and appropriate evidence.
- Pi's prompt entry is not a native persistent Goal engine and adds no automatic restarts, scheduling or background execution. Do not claim otherwise.
- Honor explicit user instructions to stop or not use this workflow. Do not create a goal solely to activate the skill.
'''
    return f'''{TITLE}
- **ONLY** load and follow the `alpha-squad-coding-craft` skill when **ONE** of the following is true:
  1. The user starts a goal with `/goal`, or Codex supplies native active-goal execution context (including `<codex_internal_context source="goal">` with an objective to pursue).
  2. The user explicitly instructs you to use the `alpha-squad-coding-craft` skill.
- Native active-goal execution context is sufficient evidence of Goal mode. Do **NOT** require the literal `/goal` text to appear in the user message; the UI may consume the command before sending the objective to the model.
- When triggered, read `{alpha}` before substantive work. Resolve or revalidate the exact model and reasoning assignments as the skill requires. Use ONE native structured user-input window for the required model and reasoning selection steps, with Final confirmation as the LAST step in that SAME window. Do not open a separate confirmation popup or replace the window with ordinary chat when structured input is available. Do not begin substantive work until all selections validate and the user submits **Confirm and continue**. The first task-status sentence after confirmation must be the skill's `Agent models:` assignment line. If the skill cannot be read or exact assignments cannot be resolved, report the failure instead of silently starting work.
- Prefer Alpha Squad's optional Laya routing when `{advisor}` and compatible Laya MCP tools are available. Read that advisor skill and Alpha Squad's `references/laya-routing.md`, then follow their combined setup gate instead of opening duplicate selection windows. If the optional adapter, advisor or MCP is absent/incompatible, use Alpha Squad's standalone manual selection. Never invent a Laya assessment. Keep the orchestrator's current model unchanged; model advice does not authorize task actions or bypass host approvals.
- Treat an unanswered, dismissed, or still-pending model-selection window as waiting for user input. Do not mark the Goal `blocked`, complete, or failed merely because the user has not completed selection and Final confirmation.
- Repeat the `Agent models:` assignment line as the first task-status sentence for every new user-started task while the skill remains active, even when the cached assignment is unchanged or work remains orchestrator-only. Do not repeat it merely for an automatic continuation with no new user request.
- Keep following the skill during automatic continuations of that goal. If context was compacted, use native active-goal context to re-establish the trigger and reload the skill as needed.
- Follow the skill's delegation gate; small tasks may remain orchestrator-only. Loading the skill and spawning agents are separate decisions.
- In **ALL other cases**, do **NOT** load or follow the skill. Ordinary tasks, quoted examples, logs, documentation, and discussions about `/goal` or this skill do not trigger it.
- Do not create a goal merely to activate the skill. The user's explicit instruction not to use it takes precedence.
'''


def replace_goal_section(existing, rules):
    # Ignore headings inside fenced examples. Replace only the exact named section.
    headings = []
    offset = 0
    fence = None
    for line in existing.splitlines(keepends=True):
        marker = re.match(r"^ {0,3}(`{3,}|~{3,})", line)
        if marker:
            token = marker.group(1)
            if fence is None:
                fence = token
            elif token[0] == fence[0] and len(token) >= len(fence):
                fence = None
        elif fence is None and re.match(r"^#{1,2}\s", line):
            headings.append((offset, line.strip()))
        offset += len(line)
    matches = [index for index, (_, heading) in enumerate(headings) if heading == TITLE]
    if len(matches) > 1:
        raise RuntimeError("Multiple Goal workflow sections; resolve duplicates before enabling")
    newline = "\r\n" if "\r\n" in existing else "\n"
    rules = rules.replace("\n", newline)
    if matches:
        index = matches[0]
        start = headings[index][0]
        end = headings[index + 1][0] if index + 1 < len(headings) else len(existing)
        return existing[:start] + rules + (newline if end < len(existing) else "") + existing[end:]
    separator = "" if not existing else (newline if existing.endswith(newline) else newline * 2)
    return existing + separator + rules


def install_goal_workflow(dry_run=False, *, home=None, agents_skills=None, client="codex"):
    home = (home or client_home(client)).expanduser().resolve()
    agents_skills = agents_skills or (Path.home() / ".agents" / "skills" if client == "codex" else home / "skills")
    target = home / CLIENTS[client][2]
    override = home / "AGENTS.override.md"
    if client in ("codex", "pi") and override.exists() and override.read_text().strip():
        raise RuntimeError(f"{override} overrides AGENTS.md; resolve it before enabling /goal")
    if target.is_symlink() or (target.exists() and not target.is_file()):
        raise RuntimeError(f"Refusing to replace a non-regular instructions file: {target}")
    alpha = skill_path(home, agents_skills, "alpha-squad-coding-craft", planned=dry_run)
    # Codex shares advisor skills; other clients use their own skills directory.
    advisor = skill_path(home, agents_skills, "laya-model-advisor", planned=dry_run)
    if client == "codex" and dry_run and not advisor.exists():
        advisor = (agents_skills / "laya-model-advisor" / "SKILL.md").resolve()
    before = target.read_bytes() if target.exists() else None
    updated = replace_goal_section((before or b"").decode("utf-8"), render_rules(alpha, advisor, client)).encode("utf-8")
    if client == "pi":
        install_pi_goal_prompt(home, alpha, True)
    if updated == before:
        if client == "pi" and not dry_run:
            install_pi_goal_prompt(home, alpha, False)
        print(f"+ /goal rules already up to date in {target}")
        return
    print(f"+ {'would update' if dry_run else 'update'} /goal rules in {target}; Alpha Squad: {alpha}")
    if dry_run:
        return
    home.mkdir(parents=True, exist_ok=True)
    mode = stat.S_IMODE(target.stat().st_mode) if before is not None else 0o600
    with tempfile.NamedTemporaryFile(dir=home, prefix=".goal-workflow-", delete=False) as stream:
        temporary = Path(stream.name)
        stream.write(updated)
    try:
        temporary.chmod(mode)
        if target.is_symlink() or (target.read_bytes() if target.exists() else None) != before:
            raise RuntimeError(f"{target.name} changed during setup; refusing to overwrite")
        if before is not None:
            with tempfile.NamedTemporaryFile(dir=home, prefix=f"{target.name}.oh-my-laya-backup-", delete=False) as backup:
                backup.write(before)
            print(f"+ backup: {backup.name}")
        temporary.replace(target)
    finally:
        temporary.unlink(missing_ok=True)
    if client == "pi":
        install_pi_goal_prompt(home, alpha, False)


def install_pi_goal_prompt(home, alpha, dry_run):
    import hashlib

    target = home / "prompts" / "goal.md"
    marker = home / "prompts" / ".oh-my-laya-goal.sha256"
    if target.is_symlink() or marker.is_symlink():
        raise RuntimeError(f"Refusing symlinked pi /goal prompt: {target}")
    if target.exists() and (not target.is_file() or not marker.is_file()
                           or hashlib.sha256(target.read_bytes()).hexdigest() != marker.read_text().strip()):
        raise RuntimeError(f"Existing pi /goal prompt is unmanaged or modified: {target}")
    content = ("---\ndescription: Start an Alpha Squad + Laya workflow (not a persistent goal loop)\n---\n"
               f"Explicitly use alpha-squad-coding-craft at `{alpha}` for this task.\n"
               "Follow the Goal workflow (STRICT) instructions and confirm model choices before work.\n"
               "This is a user-invoked workflow prompt, not a native background Goal loop.\n"
               "Task: $ARGUMENTS\n").encode("utf-8")
    print(f"+ {'would install' if dry_run else 'install'} pi /goal prompt at {target}")
    if not dry_run:
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(content)
        marker.write_text(hashlib.sha256(content).hexdigest() + "\n")
