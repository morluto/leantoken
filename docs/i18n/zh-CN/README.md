<!-- Translated from README.md at d1f1bdcc10e38894b0151c67f80990199305bf26. -->
<div align="center">

<h1>LeanToken</h1>

**让每一个 AI 编程 token 发挥更大作用**

面向编程智能体的本地优先代码智能工具。通过 CLI 和 MCP 服务器搜索代码、
查看结构、读取精确范围并探索 Git 历史。

**语言：** [English](../../../README.md) · 简体中文 · [日本語](../ja-JP/README.md) · [한국어](../ko-KR/README.md)

<img src="../../../assets/leantoken-hero-v3.jpg" alt="LeanToken 将大型代码库缩小到 AI 智能体所需的文件和代码" width="100%">

[![npm](https://img.shields.io/npm/v/leantoken?logo=npm&label=npm)](https://www.npmjs.com/package/leantoken)
[![npm downloads](https://img.shields.io/npm/dm/leantoken?logo=npm&label=downloads)](https://www.npmjs.com/package/leantoken)
[![Rust 1.95+](https://img.shields.io/badge/Rust-1.95%2B-000000?logo=rust)](https://www.rust-lang.org/)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#许可证)

[快速开始](#快速开始) · [为什么选择-leanToken](#为什么选择-leantoken) · [工具](#可用工具) · [CLI](#cli-用法) · [工作原理](#工作原理) · [文档](#文档)

</div>

---

> **翻译说明：** [英文 README](../../../README.md) 是最新、最完整的规范来源。
> 本页涵盖安装和日常使用所需的核心内容；版本、安全和高级检索行为以英文文档为准。

> **实测 token 节省：** 在一项包含 60 次受控运行的研究中，与智能体内置工具相比，
> LeanToken 在有限代码库探索中减少了 20.1% 的模型输入 token，在广泛探索中减少了
> 37.6%。完整方法参见[测量方法](../../measurement.md)。

## 快速开始

将 LeanToken 添加到 Claude Code、Cursor、OpenCode、Codex、Gemini CLI 或
Antigravity：

```bash
npx leantoken setup
```

安装向导会显示检测到的客户端，但默认不选中任何客户端。你可以准确选择哪些编程智能体
接收 LeanToken；写入前，向导会列出配置路径和 MCP 启动命令并要求确认。通过 `npx`
安装时，配置会固定到执行安装的确切 LeanToken 版本，重启客户端不会静默升级。

重新启动或重新加载已配置的客户端，然后在代码库中验证连接和首次检索：

```bash
npx leantoken doctor
```

可以尝试这样的宽泛任务：*在编辑前找到与请求取消相关的代码。* LeanToken 会帮助智能体
先调用 `leantoken.context`；编辑、构建和测试仍由智能体的常规工具完成。

查看 LeanToken 记录的当前代码库 token 使用情况：

```bash
npx leantoken savings
```

| 特性 | 说明 |
| --- | --- |
| **默认本地运行** | 源代码在本机数据库中建立索引。LeanToken 是只读的发现和检索层。 |
| **明确的 token 预算** | 每个响应都有明确的 token 上限，大文件不会占满整个请求。 |
| **面向智能体工作流** | 通过专注的工具查找文件、搜索代码、查看结构、读取精确范围、追踪历史、查询 JSON，并统计 token。 |

### 自动化安装与移除

跳过向导并明确选择客户端，或配置所有支持的客户端：

```bash
npx leantoken setup --claude --codex --yes
npx leantoken setup --all --yes
```

在不修改文件的情况下预览同一安装计划：

```bash
npx leantoken setup --codex --cursor --dry-run
```

移除由 LeanToken 管理的集成：

```bash
npx leantoken remove
```

## 常见智能体工作流

LeanToken 最适合作为一个小型证据循环，而不是一次性转储整个代码库：

1. **用一次调用完成自主定位。** 对不确定的广泛任务，先使用 `context` 和
   `plan_only: false`，并直接使用返回的源码。只有覆盖结果明确指出缺少实现或
   回归测试归属时，才进行至多一次定向后续调用。人工审查昂贵或高风险的检索时，
   仍可先用 `plan_only: true` 预览。
2. **继续工作而不重复发送源码。** 在下一次 context 调用中传入之前的
   `receipt_id`，或将返回的片段哈希作为 `known_hashes` 传入。
3. **调查已观察到的故障。** 使用 `investigation` 工作流，并在
   `workflow_evidence` 中只提供直接观察到的错误、路径、符号或测试意图。
4. **审查变更。** 使用 `review` 工作流，将 `base_revision` 设为
   `BASE..HEAD`，并设置 `strict_changed_paths: true`。

## 为什么选择 LeanToken

大多数智能体会先广泛搜索，再读取整个文件。LeanToken 将这个过程分阶段缩小：

| 常见的代码库探索方式 | 使用 LeanToken |
| --- | --- |
| 扫描大量目录列表 | 在紧凑的目录树中找到相关路径 |
| 读取整个文件来了解结构 | 不加载整个文件即可查看定义和导入 |
| 每轮都重新发送相同代码 | 避免重复发送未变化的证据 |
| 让大文件占满请求 | 将源码限制在精确预算内，并单独报告响应开销 |
| 猜测哪些文件重要 | 按任务对可能相关的代码进行排序 |

编程智能体仍负责编辑、命令、测试和对话；LeanToken 负责找到并返回这些任务所需的代码。

## 可用工具

| 工具 | 用途 |
| --- | --- |
| `leantoken.context` | 宽泛任务的默认入口；在 token 预算内预览或获取排序后的证据。 |
| `leantoken.search` | 对文本、正则、标识符、符号或引用进行排序搜索。 |
| `leantoken.files` | 紧凑、遵循忽略规则的路径发现。 |
| `leantoken.outline` | 无需读取整个文件即可查看定义、签名、导入和范围。 |
| `leantoken.read` | 读取一个精确符号或闭合行范围。 |
| `leantoken.history` | 在不可变 Git 修订之间读取、批量比较或追踪解析后的符号。 |
| `leantoken.json` | 查询、汇总或比较有界的实时 JSON。 |
| `leantoken.savings` | 报告响应统计、哈希抑制、失败和观测限制。 |

## CLI 用法

通过 `npx` 直接运行：

```bash
npx leantoken status
npx leantoken savings
npx leantoken doctor
npx leantoken --root /path/to/repo search handle_request
```

或安装全局二进制：

```bash
npm install --global leantoken@latest

leantoken --root /path/to/repo index
leantoken --root /path/to/repo search handle_request --mode identifier --max-tokens 800
leantoken --root /path/to/repo context \
  --task "fix request cancellation during shutdown" \
  --budget 2000
```

## 安装与更新

npm 包含以下平台的原生二进制：

- macOS ARM64 和 x64
- glibc Linux ARM64 和 x64
- Windows x64

其他目标（包括 musl Linux）需要从源码构建。安装 Rust 1.95 或更高版本以及原生
C/C++ 工具链，然后运行：

```bash
cargo install --git https://github.com/morluto/leantoken --package leantoken leantoken
```

显式更新已有的客户端集成：

```bash
npx --yes leantoken@latest setup --refresh --yes
```

固定版本的 MCP 配置绝不会静默切换到 `@latest`。完整的回滚、缓存和版本说明参见
[使用指南](../../usage.md)。

## 工作原理

```text
代码库
  │
  ▼
文件发现 ──► 代码结构提取 ──► 本地搜索索引
                                  │
                                  ▼
智能体请求 ──► 排序 / 精确检索 ──► token 预算内的目标代码
```

LeanToken 对源码建立一次索引，然后提供紧凑路径、排序匹配、结构提纲、精确源码范围和
任务相关上下文，并避免在多轮对话中重复发送未变化的证据。

## 文档

| 指南 | 内容 |
| --- | --- |
| [使用和工具参考](../../usage.md) | 命令、MCP 工具、请求选项和示例 |
| [架构与可靠性](../../architecture.md) | 组件、数据流、存储和故障行为 |
| [路线图](../../roadmap.md) | 当前方向和计划工作 |
| [开发与测试](../../development.md) | 本地设置、验证和发布流程 |
| [基准测试方法](../../../benchmarks/README.md) | token 经济性测量与解读 |
| [测量工具](../../measurement.md) | 实验、传输成本和性能分析工具 |

## 许可证

你可以选择以下任一许可证：

- [Apache License, Version 2.0](../../../LICENSE-APACHE)
- [MIT License](../../../LICENSE-MIT)
