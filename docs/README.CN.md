# Grapha

[English](../README.md)

Grapha 是一个高速代码智能 CLI 和 MCP 服务器，用来把代码库构建成规范化、可查询的图。它面向开发者和 AI 智能体每天都会问的问题：

- 这个符号在哪里声明？
- 谁调用它、读取它、写入它，或者实现它？
- 修改它会影响什么？
- 哪些入口点会触达这个 API、数据库写入、缓存访问或事件发布？
- 开始改代码前，应该先看哪些文件、模块和业务概念？

Grapha 不把源码当普通文本处理。Swift 会优先通过二进制 FFI 读取 Xcode 预构建的 Index Store，然后回退到 SwiftSyntax 和 tree-sitter。Rust 使用带 Cargo 工作空间感知的专用 tree-sitter 提取器。其他受支持语言使用 best-effort tree-sitter 提取，覆盖符号、包含关系、导入和基于名称的关系。

> 基准：生产级 iOS 应用，1,991 个 Swift 文件，约 30 万行，131K 节点，784K 边，8.7 秒完成索引。

## 为什么选 Grapha

| 需求 | Grapha 的回答 |
|------|---------------|
| 高置信度 Swift 关系 | 可用时读取 Xcode Index Store USR，并为图边记录置信度 |
| 快速兜底解析 | SwiftSyntax 和内置 tree-sitter 路径，在没有成功构建时仍提供有用结果 |
| 面向智能体的上下文 | CLI 和 MCP 工具覆盖搜索、上下文、影响分析、数据流、代码味道和业务概念 |
| 修改前规划 | 影响分析、反向入口追踪、仓库变更和架构规则检查 |
| 产品语言 | 跨绑定、别名、国际化文本、资源和符号的业务概念查找 |
| 本地优先工作流 | 持久化 `.grapha/` 存储、增量索引、watch 模式和本地注解同步 |

## 安装

```bash
brew tap oops-rs/tap
brew install grapha
```

```bash
cargo install grapha
```

## 快速上手

```bash
# 构建或刷新项目图
grapha index .

# 检查已存图是否新鲜
grapha repo status

# 搜索符号
grapha symbol search "ViewModel" --kind struct --context --fields full
grapha symbol search "send" --kind function --module Room --fuzzy --declarations-only
grapha symbol search "ProfileAPI" --repo FrameUI --fields file,repo,locator

# 查看符号邻域
grapha symbol context RoomPage --format tree
grapha symbol context File.swift::helper --fields full

# 修改前估算影响范围
grapha symbol impact GiftPanelViewModel --depth 2 --format tree

# 正向追踪终端操作，或反向追踪入口点
grapha flow trace RoomPage --format tree
grapha flow trace sendGift --direction reverse
grapha flow origin UserProfileView --terminal-kind network --format tree

# 扫描仓库健康度
grapha repo smells --module Room --format brief
grapha repo modules
grapha repo arch --format brief

# 用产品语言找到代码
grapha concept search "送礼横幅" --format tree
grapha concept bind "送礼横幅" --symbol GiftBannerPage --symbol GiftBannerViewModel

# 通过 MCP 提供给 AI 智能体
grapha mcp --watch
```

## CLI 指南

### 索引与服务

```bash
grapha index <path> [--format sqlite|json] [--store-dir DIR] [--full-rebuild] [--timing]
grapha migrate [-p PATH] [--from OTHER_WORKTREE_OR_STORE] [--force]
grapha analyze <path> [--output FILE] [--compact] [--filter fn,struct]
grapha serve [-p PATH] [--host HOST] [--port N]
grapha mcp [-p PATH] [--watch[=true|false]]
```

- `index` 是常用入口。默认把图数据、搜索数据、国际化快照、资源快照和新鲜度元数据写入 `.grapha/`。
- `migrate` 可以从另一个本地 Grapha store 引导当前 worktree，让新分支在完整重建前也能回答查询。
- `analyze` 用于一次性输出图，方便临时检查。
- `serve` 运行 HTTP 图浏览器。
- `mcp` 通过 stdio 为 AI 智能体运行 MCP 服务器。旧的 `grapha serve --mcp` 形式仍兼容可用。

### 符号智能

```bash
grapha symbol search "query" [--limit N] [--kind K] [--module M] [--repo R] [--file GLOB] [--role ROLE]
grapha symbol search "query" [--fuzzy] [--exact-name] [--declarations-only] [--public-only]
grapha symbol search "query" [--context] [--fields file,id,locator,module,repo,snippet]
grapha symbol context <symbol> [--format json|tree|brief] [--fields full] [--limit N]
grapha symbol impact <symbol> [--depth N] [--format json|tree|brief] [--fields file,module,repo] [--limit N]
grapha symbol complexity <symbol>
grapha symbol file <path>
grapha symbol annotate <symbol> "note" [--by agent]
grapha symbol annotation <symbol>
```

当多个符号同名时，可以使用精确 ID、locator，或 `File.swift::helper` 这类消歧形式。`tree` 和 `brief` 适合终端阅读，`json` 适合脚本和智能体。

### 数据流

```bash
grapha flow trace <symbol> [--direction forward|reverse] [--depth N] [--format json|tree|brief]
grapha flow graph <symbol> [--depth N] [--format json|tree]
grapha flow origin <symbol> [--terminal-kind network|persistence|cache|event|keychain|search]
grapha flow entries [--module M] [--file PATH] [--limit N] [--format json|tree]
```

正向追踪从入口点或符号出发，寻找终端操作。反向追踪从符号或终端出发，寻找能够触达它的入口点。`origin` 面向 UI 到数据源的问题，例如“这个页面由哪个 API 提供数据？”

### 仓库健康度

```bash
grapha repo status
grapha repo changes [unstaged|staged|all|REF] [--limit N]
grapha repo map [--module M]
grapha repo modules
grapha repo smells [--module M | --file PATH | --symbol QUERY] [--format json|brief] [--no-cache]
grapha repo arch [--format json|brief]
grapha repo infer [--format json|brief]
grapha repo doctor [--format json|brief]
grapha repo history add --kind test --title "cargo test" [--file PATH] [--module M] [--symbol QUERY]
grapha repo history list [--kind test] [--file PATH] [--module M] [--symbol QUERY] [--limit N]
```

这些命令帮助你在大型仓库中快速定位：新鲜度、文件地图、模块耦合、架构规则违规、结构性代码味道、推断元数据健康度，以及带类型的项目历史。

### 业务概念

```bash
grapha concept search "gift banner" [--limit N] [--format json|tree]
grapha concept show "gift banner" [--format json|tree]
grapha concept bind "gift banner" --symbol GiftBannerPage --symbol GiftBannerViewModel
grapha concept alias "gift banner" --add "送礼横幅" --add "gift banner page"
grapha concept remove "gift banner"
grapha concept prune
```

概念搜索会组合确认过的绑定、别名、国际化文本、资源名和符号搜索信号。绑定是本地项目数据，因此人工确认一次后，智能体后续可以复用产品词汇。

### 国际化与资源

```bash
grapha l10n symbol <symbol> [--format json|tree]
grapha l10n usages <key> [--table TABLE] [--format json|tree]
grapha asset list [--unused]
grapha asset usages <name> [--format json|tree]
```

国际化查询把 SwiftUI 符号子树连接到文案和使用点。资源查询索引 `.xcassets` 目录和源码引用。

### 注解

```bash
grapha annotation serve --port 8080
grapha annotation list [-p PATH]
grapha annotation sync [-p PATH]
grapha annotation sync --server http://HOST:8080
```

注解是本地优先的笔记，按项目身份而不是分支作用域存储。项目身份来自 `[repo].name`、Git 元数据，或项目路径兜底。`sync` 会按顺序从 `--server`、`GRAPHA_ANNOTATION_SERVER`、项目 `grapha.toml` 和全局 Grapha 配置解析服务地址。

## MCP 服务器

```bash
grapha index .
grapha mcp --watch -p .
```

添加到 MCP 客户端：

```json
{
  "mcpServers": {
    "grapha": {
      "command": "grapha",
      "args": ["mcp", "--watch", "-p", "."]
    }
  }
}
```

可用 MCP 工具：

| 工具 | 功能 |
|------|------|
| `search_symbols` | BM25 符号搜索，支持 kind/module/file/role/fuzzy 过滤 |
| `get_index_status` | 索引时间戳、仓库快照元数据和陈旧结果提示 |
| `get_symbol_context` | 360 度符号上下文：调用者、被调用者、读取、实现、包含关系 |
| `get_impact` | 可配置深度的影响范围分析 |
| `get_file_map` | 按模块和目录组织的文件与符号地图 |
| `trace` | 正向追踪到终端操作，或反向追踪到入口点 |
| `get_file_symbols` | 按源码位置列出文件中的所有声明 |
| `batch_context` | 单次调用获取最多 20 个符号的上下文 |
| `analyze_complexity` | 类型结构指标和严重度评级 |
| `detect_smells` | 按仓库、模块、文件或符号扫描代码味道 |
| `get_module_summary` | 模块指标和跨模块耦合比例 |
| `search_concepts` | 跨绑定、国际化、资源和符号的业务概念查找 |
| `get_concept` | 查看已存概念别名和绑定符号 |
| `bind_concept` | 持久化确认后的概念到符号映射 |
| `add_concept_alias` | 为概念添加别名 |
| `remove_concept` | 从项目概念库删除概念 |
| `reload` | 不重启服务器，从磁盘重载图数据 |

MCP 服务器会在会话内记住符号解析。如果 `helper` 有歧义，用 `File.swift::helper` 消歧一次后，后续裸 `helper` 查询会解析到同一符号。服务器未启用 `--watch` 时，手动运行 `grapha index .` 后可调用 `reload`。

## 配置

项目根目录可以放置可选的 `grapha.toml`：

```toml
[repo]
name = "MobileApp"

[annotations]
server = "http://192.168.1.10:8080"

[serve]
host = "0.0.0.0"
port = 18081
watch = true

[swift]
index_store = true

[output]
default_fields = ["file", "module", "repo"]

[inferred]
enabled = false

[[external]]
name = "FrameUI"
path = "/path/to/local/frameui"

[[architecture.layers]]
name = "ui"
patterns = ["AppUI*", "Features/*/View*"]

[[architecture.layers]]
name = "infra"
patterns = ["Networking*", "Persistence*"]

[[architecture.deny]]
from = "infra"
to = "ui"
reason = "Infrastructure must not depend on UI."

[[classifiers]]
pattern = "FirebaseFirestore.*setData"
terminal = "persistence"
direction = "write"
operation = "set"
```

全局开发者默认值可以放在 `$GRAPHA_CONFIG`、`$XDG_CONFIG_HOME/grapha/config.toml`、`~/.config/grapha/config.toml` 或 `~/.grapha/config.toml`。项目配置会覆盖全局配置中的仓库专属值，例如 `[annotations].server` 和 `[serve].port`。

## 架构

```text
grapha-core/     共享图、提取、语义、选择器和插件类型
grapha-swift/    Swift 提取：Index Store -> SwiftSyntax -> tree-sitter
grapha-engine/   库引擎：提取、查询引擎、持久化和语言插件
grapha/          CLI、MCP 服务器和 Web UI
nodus/           智能体工具包，包含 skills、rules 和 commands
```

### Swift 提取瀑布

```text
Xcode Index Store（二进制 FFI） -> 编译器解析 USR，置信度 1.0
  fallback
SwiftSyntax（JSON FFI）         -> 精确语法解析，无类型解析，置信度 0.9
  fallback
tree-sitter-swift              -> 快速解析兜底，置信度 0.6-0.8
```

Index Store 提取后，tree-sitter 会在共享解析中补充文档注释、SwiftUI 视图层级、国际化元数据和资源引用。

### 图模型

- 节点类型包括文件、函数、方法、类型、模块、导入/导出、路由、组件、Swift protocol/extension、SwiftUI 视图节点和分支节点。
- 边类型包括 calls、uses、imports、exports、implements、contains、type_ref、reads、writes、publishes、subscribes、inherits、extends、instantiates、overrides、decorates、returns 和 references。
- 数据流注解记录 direction、operation、condition、async boundary 和源码 provenance。
- 节点角色区分 entry point、terminal operation 和 internal symbol。
- 终端类型包括 `network`、`persistence`、`cache`、`event`、`keychain` 和 `search`。

## 性能

生产级 iOS 应用实测：1,991 个 Swift 文件，约 30 万行。

| 阶段 | 耗时 |
|------|------|
| 提取，包括 Index Store 和 tree-sitter 增强 | 3.5s |
| 合并和模块感知的跨文件解析 | 0.3s |
| 入口点和终端分类 | 1.7s |
| SQLite 持久化，延迟建索引 | 2.0s |
| Tantivy BM25 搜索索引 | 1.0s |
| 合计 | 8.7s |

最终图包含 131,185 个节点、783,793 条边、2,983 个入口点和 11,149 个终端操作。可以在自己的项目上运行 `grapha index . --timing` 查看阶段耗时。

## 支持的语言

| 语言 | 提取方式 | 类型解析 |
|------|----------|----------|
| Swift | Xcode Index Store、SwiftSyntax、tree-sitter | Index Store 可用时提供编译器级 USR |
| Rust | 专用 tree-sitter 提取器 | 基于名称 |
| TypeScript / TSX / JavaScript | 通用 tree-sitter 提取器 | 基于名称 |
| Python / Go / Java / C / C++ / C# | 通用 tree-sitter 提取器 | 基于名称 |
| PHP / Ruby / Kotlin / Dart / Pascal | 通用 tree-sitter 提取器 | 基于名称 |

Swift 和 Rust 是第一等提取路径。其他语言提供有用的结构覆盖，但不声称具备编译器级语义。

## 开发

```bash
cargo build                    # 构建所有 workspace crate
cargo test                     # 运行 workspace 测试套件
cargo clippy                   # Lint
cargo fmt -- --check           # 检查格式
```

## 许可证

MIT
