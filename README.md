# Oh My Laya

[English](README.md) | [中文](README-ZH.md)

> One command to bring local Laya decision-making to your coding agents.

Oh My Laya builds on [laya-mlx](https://github.com/mizorewww/laya-mlx). It downloads and verifies Hugging Face weights, then registers the `laya_tell_me` tool for classification, scoring, risk routing, and yes/no decisions. Inference runs entirely on your Mac after the model is downloaded.

It supports Codex, Claude Code, DeepSeek Harness (DSH), and pi-agent. The installer detects available clients and lets you select one, several, or all of them.

## Install

Requirements: Apple Silicon, macOS 14+, and Python 3.11+. The default multilingual FP16 checkpoint is approximately 678 MB.

Install and register with all detected clients:

```bash
sh -c "$(curl -fsSL https://raw.githubusercontent.com/leo1394/oh-my-laya/master/tools/oh-my-laya.sh)"
```

Without curl, use wget:

```bash
sh -c "$(wget -qO- https://raw.githubusercontent.com/leo1394/oh-my-laya/master/tools/oh-my-laya.sh)"
```

Install your preferred agent client first; Oh My Laya connects to clients already on your machine.

Already cloned the repository? Run the interactive installer from its root:

```bash
./install.sh
```

For non-interactive installation:

```bash
./install.sh --targets codex
./install.sh --targets codex,claude
./install.sh --targets all
```

Both `all` and `both` register every detected client. Restart the affected agent sessions after installation.

## Use It

Ask your agent:

```text
Use Laya to classify the current change as low, medium, or high risk, and decide whether human review is needed.
```

Oh My Laya provides these tools:

| Tool | Purpose |
| --- | --- |
| `laya_tell_me` | `choice` classification, `score` ranking, and `noul` yes/no probability |
| `laya_advisor_preferences` | Read or change model recommendation preferences |

Laya does not generate code and must not authorize destructive, publishing, or other consequential actions. The multilingual model has a total context budget of 1,024 tokens, so ask the agent to summarize long inputs first.

## Codex Model Advisor

Installing with `--targets codex` also installs the advisor skill. Restart Codex, then ask:

```text
Use $laya-model-advisor for each new task in this session. Ask me to choose a recommendation policy.
```

Choose **always ask**, **ask only for high complexity, high risk or uncertainty**, or **automatically accept recommendations**. You can change this preference in chat at any time; it persists across sessions. When asking, the advisor lists verified available models and lets you choose a model and its supported reasoning effort. Without a user-defined model tier mapping, it recommends effort changes on the current model; you can still choose another listed model.

This is advisory: accepting a recommendation does **not** switch the active model. Apply it in Codex's model picker. Popups require host support; otherwise the agent asks in chat. Session advice is skill-driven, not a guaranteed per-message hook. If the model list cannot be verified, the advisor asks you to provide it. These preferences never bypass execution approvals.

Optional installation preference (default: ask on first use):

```bash
./install.sh --targets codex --advice-policy conditional
# Other values: always, auto
```

## Common Options

```bash
# Select another checkpoint
./install.sh --targets all --model english
./install.sh --targets dsh --model typed-decisions

# Preview without downloading or changing configuration
./install.sh --targets codex --dry-run
```

The default installation directory is `~/.local/share/oh-my-laya/`. Each agent starts its own lazy MCP process; concurrent callers each consume a separate unified-memory allocation.

## Development

```bash
PYTHONPATH=src python3 -m unittest discover -s tests -v
```

Licensed under the [MIT License](LICENSE).
