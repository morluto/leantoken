<!-- 与根目录 README 同步更新。英文版是权威技术规范。 -->
<div align="center">

# LeanToken

**面向智能体的代码智能，提供明确的源代码 token 预算。**

`mcp-name: io.github.morluto/leantoken`

**语言：** [English](../../../README.md) · 简体中文 · [日本語](../ja-JP/README.md) · [한국어](../ko-KR/README.md)

<img src="../../../assets/leantoken-hero-v3.jpg" alt="LeanToken 将代码库缩小到智能体需要阅读的代码" width="100%">

[![npm](https://img.shields.io/npm/v/leantoken?logo=npm&label=npm)](https://www.npmjs.com/package/leantoken)
[![Rust 1.95+](https://img.shields.io/badge/Rust-1.95%2B-000000?logo=rust)](https://www.rust-lang.org/)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#许可证)

</div>

LeanToken 在本机构建不可变的仓库 generation，并从同一 generation 提供有界的
搜索、结构和读取操作。CLI 和 MCP 服务器调用相同的应用服务；源码不会离开本机。

英文 README 和链接的参考文档是完整、权威的技术规范。本页提供日常使用所需的当前说明。

## 快速开始

为支持的编程客户端配置 LeanToken：

```bash
npx leantoken setup
```

重启客户端，在仓库中验证连接：

```bash
npx leantoken doctor
```

也可以直接使用 CLI：

```bash
npx leantoken refresh
npx leantoken search RepositoryGeneration
npx leantoken outline src/storage/snapshot.rs
npx leantoken read src/storage/snapshot.rs --lines 1:120
```

除非明确传入 `--allow-broad-root`，LeanToken 会拒绝文件系统根目录、主目录以及主目录的
父目录。安装前会预览将要修改的文件并要求确认；自动化操作必须明确选择客户端。

## 检索模型

`refresh` 是发布边界：

```text
仓库文件 -> 有界采集 -> 完整的派生 generation
         -> 原子发布 -> search / outline / read
```

一次检索只观察一个已提交的 generation，不会混合两次发布中的文件。监听器和兼容性协调模式
可以请求刷新，但正确性不依赖与文件系统事件竞速。工作树变更后请调用 `refresh`。

公开的 `read` 返回已索引 generation 中的内容。需要读取未提交工作树字节的库调用方，可显式使用
`Services::read_worktree`，并接受它较弱的保证。

一个 MCP 进程对应一个仓库根目录；多个仓库应启动多个进程，以获得清晰的资源和故障隔离。

## 可用工具

| 工具 | 用途 |
| --- | --- |
| `leantoken.files` | 发现已索引路径，不加载源码。 |
| `leantoken.search` | 搜索文本、正则、标识符、符号和引用。 |
| `leantoken.outline` | 查看定义、签名、导入和范围。 |
| `leantoken.read` | 读取精确的已索引范围、符号或标题。 |
| `leantoken.history` | 查看有界的 Git 历史和差异。 |
| `leantoken.json` | 查询有界的实时 JSON 结构。 |
| `leantoken.context` | 为任务组合排序后的证据。 |
| `leantoken.receipt_rebase` | 将不可变证据变基到更新的 generation。 |
| `leantoken.savings` | 报告检索与 token 的观测统计。 |

`files`、`search`、`outline` 与 `read` 是检索内核；`context` 在这些原语之上编排。
Git、JSON、安装、更新、缓存管理和离线分析分别有自己的所有者。

## Token、游标与生命周期

源代码预算与序列化响应预算彼此独立：`max_tokens` 限制返回源码，
`max_response_tokens` 限制完整 JSON 响应。分页游标绑定仓库、generation、规范化请求和位置，
不能无声地继续到不同内容或参数。客户端可提供已持有内容的哈希以避免重复传输；检索证据和
查询证明是不可变、内容寻址的制品，而不是可变的会话状态。

使用 `--dry-run` 预览安装、缓存或私有运行时清理。受管理的客户端配置固定所选版本，不会自动升级：

```bash
npx leantoken setup --claude --codex --yes
npx --yes leantoken@latest setup --refresh --yes
```

## 文档

| 文档 | 内容 |
| --- | --- |
| [使用说明](../../usage.md) | 当前 CLI 与 MCP 行为 |
| [架构](../../architecture.md) | generation、存储和有界性约定 |
| [开发](../../development.md) | 测试所有权与贡献流程 |
| [基准测试](../../../benchmarks/README.md) | 可选评测工具与证据规则 |
| [发布](../../releases.md) | 发布和恢复流程 |
| [变更日志](../../../CHANGELOG.md) | 已发布的改动 |

规划应放在 GitHub issue 和 pull request 中；历史设计和实验叙述保留在 Git 历史中。

## 许可证

可按你的选择使用 MIT 许可证或 Apache License 2.0。
