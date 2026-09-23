# Oh My Laya

[English](README.md) | [简体中文](README-ZH.md)

> 一条命令，把本地 Laya 决策能力接入你的编码 Agent。

Oh My Laya 基于 [laya-mlx](https://github.com/mizorewww/laya-mlx)，自动下载并校验 Hugging Face 权重，随后注册 `laya_tell_me` 工具。它适合做分类、评分、风险分流和是非判断；模型下载完成后，推理完全在本机运行。

支持 Codex、Claude Code、DeepSeek Harness（DSH）和 pi-agent。安装器会检测本机已有工具，允许单选、多选或选择全部。

## 一键安装

要求：Apple Silicon、macOS 14+、Python 3.11+。默认多语言 FP16 权重约 678 MB。

一键安装并注册到所有已检测到的客户端：

```bash
sh -c "$(curl -fsSL https://raw.githubusercontent.com/leo1394/oh-my-laya/master/tools/oh-my-laya.sh)"
```

没有 curl 时使用 wget：

```bash
sh -c "$(wget -qO- https://raw.githubusercontent.com/leo1394/oh-my-laya/master/tools/oh-my-laya.sh)"
```

请先安装你要使用的 Agent 客户端，Oh My Laya 会自动接入本机已有的客户端。

已克隆项目？在项目根目录运行交互式安装器：

```bash
./install.sh
```

无需交互：

```bash
./install.sh --targets codex
./install.sh --targets codex,claude
./install.sh --targets all
```

`all` 和 `both` 都表示注册到所有已检测到的客户端。安装完成后，请重启对应 Agent 会话。

## 开始使用

直接告诉 Agent：

```text
使用 Laya 将当前改动判断为低、中、高风险，并判断是否需要人工审查。
```

Oh My Laya 提供以下工具：

| 工具 | 用途 |
| --- | --- |
| `laya_tell_me` | `choice` 分类、`score` 评分、`noul` 是非概率 |
| `laya_advisor_preferences` | 查看或修改模型建议偏好 |

Laya 不生成代码，也不应被用于授权删除、发布等高风险操作。多语言模型总上下文为 1,024 tokens，长内容请先让 Agent 摘要。

## Codex 模型建议

使用 `--targets codex` 安装时会同时安装建议 Skill。重启 Codex 后输入：

```text
使用 $laya-model-advisor 为当前会话的每个新任务评估模型与推理档位，先让我选择建议授权策略。
```

支持**每次询问**、**仅高复杂度／高风险／不确定时询问**、**全自动接受建议**。偏好跨会话保存，随时可在对话中修改。询问时会列出已核实的可用模型，允许选择其他模型及其支持的推理档位。未配置模型分档时，默认保持当前模型、仅建议推理档位；仍可手动选择列表中的其他模型。

这是建议版：接受建议**不等于切换当前模型**，请在 Codex 模型选择器中应用。客户端支持时使用弹窗，否则在对话中询问。会话评估由 Skill 驱动，不是保证每条消息触发的钩子。无法核实模型列表时，会请你提供列表。这些偏好不会绕过执行权限审批。

也可在安装时指定策略（默认首次使用时询问）：

```bash
./install.sh --targets codex --advice-policy conditional
# 其他选项：always（每次询问）、auto（全自动接受建议）
```

## 常用选项

```bash
# 选择其他权重
./install.sh --targets all --model english
./install.sh --targets dsh --model typed-decisions

# 只预览，不下载、不修改配置
./install.sh --targets codex --dry-run
```

默认安装到 `~/.local/share/oh-my-laya/`。每个 Agent 会启动独立的惰性 MCP 进程；同时调用时，每个进程都会占用一份统一内存。

## 开发验证

```bash
PYTHONPATH=src python3 -m unittest discover -s tests -v
```

项目受 [MIT License](LICENSE) 许可。
