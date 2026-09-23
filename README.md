# Oh My Laya

[English](README.md) | [Chinese](README-ZH.md)

> One command to bring local Laya decision-making to your coding agents.

Oh My Laya builds on [laya-mlx](https://github.com/mizorewww/laya-mlx). It downloads and verifies Hugging Face weights, then registers the `laya_tell_me` tool for classification, scoring, risk routing, and yes/no decisions. Inference runs entirely on your Mac after the model is downloaded.

It supports Codex, Claude Code, DeepSeek Harness (DSH), and pi-agent. The installer detects available clients and lets you select one, several, or all of them.

## Install

Requirements: Apple Silicon, macOS 14+, and Python 3.11+. The default multilingual FP16 checkpoint is approximately 678 MB.

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

Oh My Laya provides one tool:

| Tool | Purpose |
| --- | --- |
| `laya_tell_me` | `choice` classification, `score` ranking, and `noul` yes/no probability |

Laya does not generate code and must not authorize destructive, publishing, or other consequential actions. The multilingual model has a total context budget of 1,024 tokens, so ask the agent to summarize long inputs first.

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
