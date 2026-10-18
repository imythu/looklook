# Codex 对话界面设计方案

> 状态：方案，未实现。依据：客户端 1.17.2 的 Claude Code 对话界面（`src/chat/`），以及 2026-10-18 在 Windows 上对 Codex CLI 0.159.3 的实测（见 §2）。
> 目标读者：实现和以后维护这块代码的人。§10 是 Codex 升级时怎么跟进，单独看也能用。

## 1. 目标与范围

**目标**：Codex 会话也有“对话”标签，和 Claude Code 用同一套界面、同一套行为——发消息、看回复和工具调用、在对话里审批、停止、会话列表上的状态和提醒。终端里的 Codex 照常可用，两边是同一个进程的两个视图。

**原则**（和 Claude 一样）：

- 不替 Codex 做决定：认不出的界面一律交给终端（`Phase::Tui`），**任何异常都不自动放行**。
- 不改用户自己的 Codex 配置（`~/.codex/config.toml`、`hooks.json`），需要的东西都在启动命令里带上。
- Codex 更新快（两周内 0.159 → 0.163），所有“认格式”的地方都要能单独更新、有样本测试（§10）。

**不做**（以后可以单独立项）：

- 逐字显示 Codex 正在写的回复的“精确版”：rollout 里只有写完的段落。第一版和 Claude 一样从屏幕上取正在输出的内容（§7.2）；要更精确得走 app-server（§12）。
- Codex Cloud、`codex exec` 这类非交互用法。

## 2. 实测结论（Codex 0.159.3，Windows 11）

| # | 结论 | 怎么验证的 |
|---|---|---|
| 1 | Codex 有 hooks，`codex features list` 里 `hooks` 是 stable、默认开 | 命令输出 |
| 2 | 事件名、输入字段和 Claude Code 一致：`SessionStart`（`source`）、`UserPromptSubmit`（`prompt`）、`PreToolUse` / `PostToolUse`（`tool_name`、`tool_input`、`tool_use_id`、`tool_response`）、`PermissionRequest`、`Stop`（`last_assistant_message`），都带 `session_id`、`turn_id`、`transcript_path`、`model`、`permission_mode` | 记录 hook 输入 |
| 3 | 运行命令报成 `tool_name: "Bash"`、`tool_input: {"command": …}`，和 Claude 一样（rollout 里实际是 JS 形式的 `exec`，见 §6） | 同上 |
| 4 | `transcript_path` 是这个会话的 rollout 文件（`$CODEX_HOME/sessions/YYYY/MM/DD/rollout-…-<id>.jsonl`） | 同上 |
| 5 | hooks 可以用 `-c hooks.<事件>=[{matcher="*",hooks=[{type="command",command="…",timeout=N}]}]` 在启动时传入，不用写任何文件 | `codex exec` 和交互式 TUI 都触发了 |
| 6 | 项目里的 `.codex/hooks.json` 没生效（未信任的目录），不能靠它 | 同一目录对比 |
| 7 | hooks 需要“信任”才运行；测试用了 `--dangerously-bypass-hook-trust`（会把这次运行里**所有**已启用但未信任的 hooks 都放行，见 §4.3） | 不带这个参数时 hooks 不运行 |
| 8 | `PermissionRequest` 在 TUI 弹确认框时触发；hook 按 Claude 的格式答 `{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow"}}}`，Codex 照办，TUI 不再弹框 | 只读沙箱里建文件，hook 答允许后文件建出来了 |
| 9 | `PermissionRequest` 的输入里**没有** `permission_suggestions`（Claude 有），`tool_input` 多一个 `description`（沙箱给的原因） | 记录 hook 输入 |
| 10 | 环境变量（`LOOKLOOK_INSTANCE`）能传到 hook 程序 | 同上 |
| 11 | 新开的 Codex 默认连电脑上共享的后台服务；带着不同设置时会拒绝复用并退出，所以要 `--no-daemon` | 之前的 `Cannot use the shared background server` |
| 12 | Windows 上 PowerShell 会优先找 npm 生成的 `codex.ps1`，被默认执行策略拦住；要用 `codex.cmd` / `.exe` | 之前的终端报错 |
| 13 | 粘贴后马上按回车会被 Codex 吞掉，文字停在输入框里 | 之前对话看法的端到端测试 |

**还没验证、实现前要先试的**见 §11（每条都写了怎么试）。

## 3. 总体结构：共用核心 + 工具适配器

现在 `src/chat/` 是为 Claude Code 写的。做法是把“和工具无关”的部分留在核心，“和工具有关”的部分抽成适配器。**不新建目录**：Claude 的文件原地不动，Codex 的文件用 `codex_` 前缀放在同一个 `src/chat/` 里，样本放进已有的 `src/chat/testdata/`，脚本放进已有的 `scripts/`。

```text
client/src/chat/
  mod.rs               核心：Inst / State / Phase、hook 处理和状态机、审批挂起、input 送达确认、watch 循环、事件推送（小改：按 agent 分派）
  agent.rs             新文件：trait Agent + 按终端的启动类型选适配器；Claude 的实现也写在这里（调用现有文件）
  hooks.rs             不动（hook 入口两边共用）；只把 settings_json 里的 command 拼法抽成函数给 Codex 用
  transcript.rs        不动（Claude）
  screen.rs            不动（Claude）
  commands.rs          不动（Claude）
  codex_launch.rs      新文件：启动命令、hooks 注入、hook 信任（§4）
  codex_rollout.rs     新文件：rollout → 条目（§6），产出和 transcript.rs 相同的 Item / Block
  codex_code_mode.rs   新文件：从 exec 的 JS 里拆出命令、补丁（§6.4）
  codex_screen.rs      新文件：输入框 / 菜单 / 正在输出的内容（§7）
  codex_commands.rs    新文件：`/` 命令提示（§9）
  codex_compat.rs      新文件：版本探测、已验证范围、可用能力（§10.1）
  codex_patterns.rs    新文件：所有“认文字”的常量集中在这里（§10.2）
  testdata/            已有目录；Codex 样本用 codex-<版本>-<名字> 命名（§10.3）
client/scripts/record-codex.mjs   新文件，放在已有的 scripts/ 里（§10.4）
```

`trait Agent`（每个方法都是现在代码里写死 Claude 的地方）：

| 方法 | Claude 现状 | Codex |
|---|---|---|
| `launch_args(id, resume)` | `--settings <claude-hooks.json>` + `--resume` | §4 |
| `relaunch_command(id, session)` | `chat_relaunch_command` | `codex resume --no-daemon … <id>` |
| `valid_transcript(path)` | `…/projects/<项目>/<id>.jsonl` | `$CODEX_HOME/sessions/**/rollout-*.jsonl` |
| `transcript(path) -> Box<dyn TranscriptReader>` | `Transcript` | `Rollout`（§6） |
| `classify(screen) -> Screen` / `live(screen)` | `screen.rs` | §7 |
| `decision(answer, pending) -> Value` | `answer()` 里的 match | §8 |
| `commands(workdir)` | `.claude/commands`、skills | §9 |
| `caps(version) -> Caps` | 全部支持 | §10.1，界面按它显示按钮 |
| `is_agent_process(pane_command)` | 不是 shell 就算 | `codex` / `node`（npm 版是 node 跑的） |

`TranscriptReader` 就是现在 `Transcript` 的公开方法：`poll()`、`items(from,to)`、`full_item(key)`、`len()`、`tool_input(id)`、`version`、`stats`。核心只依赖这个接口。

hook 入口（`looklook hook`、`POST /hook`、令牌、`LOOKLOOK_INSTANCE`）完全共用：两边的 hook 输入格式一样，`Chat::hook()` 里按事件改状态的逻辑也一样（§5 只列出差别）。

## 4. 启动

### 4.1 命令行

```text
LOOKLOOK_INSTANCE=<id> codex --no-daemon \
  [--dangerously-bypass-hook-trust | 见 4.3] \
  -c 'hooks.SessionStart=[{matcher="*",hooks=[{type="command",command="<looklook> hook <hook.json>",timeout=10}]}]' \
  -c 'hooks.UserPromptSubmit=…' … （每个事件一个 -c）
  [--yolo] [resume <session>]
```

- 事件：`SessionStart`、`SessionEnd`、`UserPromptSubmit`、`Stop`、`Notification`、`PreToolUse`、`PostToolUse`、`PermissionRequest`、`PreCompact`、`PostCompact`、`SubagentStart`、`SubagentStop`（Codex 二进制里都有这些名字；`StopFailure`、`PostToolUseFailure`、`PermissionDenied` 有没有要在 §11 里确认，没有就不传）。`PermissionRequest` 的 timeout 600，其余 10，和 Claude 一样。
- hook 命令就是 Claude 用的那条（`hooks::settings_json` 里的 `command`），拼成 TOML 字符串时注意转义 `"` 和 `\`。
- 12 个 `-c` 约 3 KB，Windows 命令行上限 32 KB（cmd 8 KB），tmux 建会话时整串传，没问题（ttyd 那个 256 字节的限制早已绕开）。
- Windows：沿用 `instances.rs` 现在的做法，PowerShell 下直接调 `codex.cmd` / `.exe`，不走 `codex.ps1`（实测 #12）。
- 接着聊：`codex resume --no-daemon <hooks…> <id>`（参数顺序在 §11 确认）。

### 4.2 为什么不写配置文件

`--settings` 那样的“额外配置文件”Codex 没有；`-p <profile>` 会读 `$CODEX_HOME/<名>.config.toml`，要往用户的 Codex 目录里写文件，而且所有用这个 profile 的地方都会带上我们的 hooks。用 `-c` 只影响这一个进程，最干净。

### 4.3 hook 信任（要先定下来）

Codex 只运行“信任过”的 hooks。三个办法：

| 方案 | 做法 | 问题 |
|---|---|---|
| A | `--dangerously-bypass-hook-trust` | 这次运行里**所有**已启用但未信任的 hooks 都会运行，包括项目里别人写进 `.codex/` 的。打开别人的仓库时有风险 |
| B（推荐，待验证） | 信任状态也用 `-c` 传：二进制里有 `HookStateToml.trusted_hash`，如果 `-c hooks.state.<我们的 hook>.trusted_hash=<哈希>` 能生效，就只信任我们自己这一条 | 要弄清哈希怎么算（对什么内容、什么算法），Codex 改算法时要跟进（§10） |
| C | 第一次开对话界面时，在终端里让用户确认一次信任，Codex 自己记下 | 要用户操作一次；命令行（含 hook.json 路径）变了要重新确认 |

做法：先按 §11 试 B；B 不行就用 C，A 只作为用户在设置里明确打开的“高级选项”，并写清楚风险。

注意：B 即使今天能用，本质上也是从命令行绕过 Codex 的信任机制，Codex 很可能把它当成漏洞修掉。所以就算 B 能行，也要同时实现 C 作为退路，并在 `codex_compat.rs` 里按版本判断 B 还能不能用。看看是远程控制工具，A 不能默认打开。

### 4.4 启动类型和开关

- `chat_ui` 开关从“只对 claude”放宽到 `claude | codex`（`instances.rs` 的 `chat_settings`、`TerminalView.jsx` 的 `hasChat`、`Terminals.jsx` 的 `canChat`）。
- 新建的 Codex 会话默认打开对话界面（和 Claude 一样）；已有的会话在“编辑”里打开，重启生效。
- `chat_ui` 打开时不管 `full_access`，`--no-daemon` 一律带上（实测 #11）。

## 5. 状态（和 Claude 的差别）

`Chat::hook()` 的状态机照用。差别：

| 事件 / 情况 | Claude | Codex |
|---|---|---|
| `SessionStart.source` | `startup` / `resume` / `clear` / `compact` | 已见 `startup`；`resume`、`/new` 之后是什么要确认（§11），影响“换会话”的判断（`rebind`） |
| 中断（Esc） | 没有 hook，靠抓屏 | 同样没有 hook；rollout 里有 `event_msg: turn_aborted`，可以作为补充依据 |
| 一轮结束 | `Stop` | `Stop`；rollout 的 `task_complete` 带用时，用来生成 `Turn` 条目 |
| `Notification` | `permission_prompt` 等 | 类型名要确认；认不出的忽略 |
| 审批超时 | 590 秒后交给 TUI | 同 |
| 没接上 hooks | 10 秒内没有 SessionStart → `not_attached` | 同；另外再看 `features.hooks` 和信任状态，给出具体原因（“hooks 被关了”“hook 没被信任”） |
| 并行 | 基本是单线程：事件按顺序到达，`Stop` 就是一轮结束 | `multi_agent` 默认开，子代理和并行工具调用的 hook 会交错到达。状态要按 `turn_id`（以及 agent 标识，如果有的话）分开记，只有主线的 `Stop` 才算一轮结束；挂起的审批按 `tool_use_id` 对应，不能假设同一时间只有一个（S11） |
| hook 出错 | 空输出 = 不做决定，交给 TUI | 要实测（S0）。在确认 Codex 也把空输出当成“交给 TUI”之前，不能上线 |

## 6. 对话内容：rollout → 条目

### 6.1 读法

- 和 `transcript.rs` 一样：按字节偏移跟读、行写到一半先留着、文件变短就重读（`Change::Reset`）。
- **只认白名单**里的记录，其余计入 `stats.unknown`，诊断信息里能看到（§10.4）。

### 6.2 映射表（0.159.3 一个 9 轮会话里实际出现的记录）

| rollout 记录 | 条目 | 说明 |
|---|---|---|
| `session_meta` | —（取 `id`、`cwd`、`cli_version` → `version`） | 一个文件里可能有多条（resume），只取第一条的 id |
| `turn_context` | —（取 `model`） | |
| `response_item: message`（user） | `User` | 去掉 Codex 注入的上下文：整段被同一个标签包起来的（`<environment_context>…`）、`# AGENTS.md instructions` 开头的；`input_image` 计入 `images` |
| `response_item: message`（assistant） | `Assistant { blocks: [Text] }` | 连续的 assistant 文字合成一条 |
| `response_item: message`（developer / system） | — | 跳过 |
| `response_item: reasoning` | `Assistant { blocks: [Thinking] }`，只用 `summary` 里的文字 | `encrypted_content` 不读；没有 summary 就不显示 |
| `response_item: custom_tool_call`（`exec`） | `Assistant { blocks: [ToolUse…] }` | 0.159 起工具调用是一段 JS，拆法见 §6.4，翻成什么见 §6.3 |
| `response_item: function_call` / `local_shell_call` | `ToolUse`（`name` 规范成 `Bash` / `apply_patch`…） | 旧版本和部分工具还是这种 |
| `…_call_output` | `ToolResult` | 输出里的图片计入 `images`；超长截断（沿用 `TEXT_LIMIT`） |
| `event_msg: patch_apply_end` | 给对应的 `ToolUse` 加 `extra`（改了哪些文件、增删行数） | 界面用它显示 diff 卡片 |
| `event_msg: turn_aborted` | `Interrupt` | |
| `event_msg: task_complete` | `Turn { ms }` | |
| `compacted` / `response_item: compaction` | `Divider { kind: "compact" }`（有可读摘要时加 `Summary`） | |
| `event_msg: user_message` / `agent_message` | — | 和 `response_item` 重复，跳过（只在 response_item 缺失的老版本里用） |
| `event_msg: token_count`、`token_usage_record`、`thread_settings_applied`、`world_state`、`task_started` | — | 跳过（`token_count` 以后可以用来显示上下文占用） |

条目的 `key`：`r:<行号>:<序号>`（rollout 没有 uuid；只追加不改写，行号稳定）。assistant 合并时用第一行的 key。

### 6.3 原则：翻成 Claude 的说法，界面原样复用

Codex 的工具调用在服务端（`codex_rollout.rs`）先翻成 Claude Code 的工具名和参数，界面不需要认识 Codex 的工具，直接复用 Claude 的卡片（效果见画布“照着 Claude Code 现在的对话界面做”一行）：

| Codex | 翻成 | 界面上 |
|---|---|---|
| `exec_command` / `shell` | `Bash {command}` | 命令卡片 |
| `apply_patch` 里的每个文件（Update） | `Edit {file_path}`，结果的 `extra.structuredPatch` 由补丁的块生成 | 和 Claude 一样的 diff、`+增 −删` |
| `apply_patch`（Add） | `Write {file_path, content}` | 新文件 |
| `apply_patch`（Delete） | `Bash {command: "rm <文件>"}`，标记来自补丁 | 删除 |
| `update_plan {plan:[{step,status}]}` | `TodoWrite {todos:[{content: step, activeForm: step, status}]}` | 勾选清单，输入框上方“待办 2/3” |
| `view_image` | `Read {file_path}` | 读文件 |
| `web_search` / `web__run` | `WebSearch {query}` / `WebFetch {url}` | 网页 |
| 子代理 | `Agent {description, prompt}` | 子代理卡片 |
| 认不出的 | `exec {code}` | 写明“没认出来的工具调用”，展开是原始代码 |

### 6.4 code mode 的工具调用

0.159 的 `custom_tool_call` 名叫 `exec`，`input` 是一段 JS，例如：

```js
text(await tools.exec_command({cmd:"New-Item -ItemType File b.txt",max_output_tokens:1000}));
await tools.apply_patch("*** Begin Patch\n*** Update File: src/a.rs\n…");
```

`codex_code_mode.rs` 只做一件事：找出每个 `tools.<名字>(` 调用，读出第一个参数里的 `cmd` 字符串或补丁字符串，拆成若干个 `ToolUse`：

- `exec_command` → `ToolUse { name: "Bash", input: {"command": …} }`（和 hook 报的一致，界面复用 Claude 的命令卡片）；
- `apply_patch` → `ToolUse { name: "apply_patch", input: {"patch": …, "files": […]} }`（界面显示改了哪些文件）；
- `write_stdin`、`web__run`、`view_image` 等 → 对应名字；认不出的 → `ToolUse { name: "exec", input: {"code": 原文} }`，界面显示原始代码，不瞎猜。

（管理端仓库 `ttyd-ai-control-plane` 的 `looklook-client/src/explain.rs` 里有一版这个解析和测试，可以参考。）

## 7. 屏幕

### 7.1 输入框还是别的界面

Codex 的 TUI 底部：

```text
› 用户正在输入的文字            ← 以 `›` 开头（空着时是灰色的提示“Ask Codex to do anything”）
  GPT-6-Astra medium · ~\path   ← 状态行
  ? for shortcuts      ⚠ …      ← 提示行；运行中是 “• Working (12s • esc to interrupt)”
```

`codex_screen.rs::classify`：

- 最后几行里有输入框行（`›` 开头、下面是状态行）且不是菜单 → `Screen::Input { text, busy }`；`busy` = 有 `esc to interrupt`。灰色提示文字不算 `text`（和 Claude 一样靠 SGR 的暗淡属性区分）。
- 底部是编号菜单（某一行以 `›` + 数字开头，下面最多几行边线或 “enter … / esc …” 提示）→ `Screen::Modal`。信任文件夹、更新提示、后台服务设置、`/model`、`/permissions`、审批确认框都是这样。
- 都不是（回到了 shell、Codex 崩了）→ 由 `is_shell` 判断，和 Claude 一样。

`ttyd-ai-control-plane` 的 `looklook-client/src/screen.rs` 里有一版菜单解析和样本（审批框、折行选项、信任文件夹、更新提示、没有提示行的菜单），可以参考。

### 7.2 正在输出的内容

和 Claude 的 `screen::live` 一样：运行中把屏幕上最后一段 `•` 开头的回复和状态行推给界面，transcript 写完后被正式条目替换。

## 8. 发消息、停止、审批

### 8.1 发消息

- 先确认是输入框（§7.1），不是就转 `Phase::Tui`，和 Claude 一样。
- 文字：沿用分块“打字”（64 字 / 30 毫秒、换行用 `C-j`）。Codex 的“连续快速输入当作粘贴”判断和 Claude 不同，**块大小和间隔放进 `codex_patterns.rs` 按工具配置**，§11 实测后定。
- 回车：等输入框里出现刚打的文字的开头再按（抓屏确认），不靠固定延时（实测 #13）。没收到 `UserPromptSubmit` 时补按一次回车的逻辑照用。
- 图片：Codex TUI 粘贴图片文件路径能不能变成附件要实测（§11）；不行就在界面上对 Codex 隐藏发图片（`caps.images = false`）。

### 8.2 停止

Esc，同 Claude。

### 8.3 审批

| 界面上的选择 | Claude 的答复 | Codex |
|---|---|---|
| 允许 | `{"behavior":"allow"}` | 同（实测 #8） |
| 拒绝（可附理由） | `{"behavior":"deny","message":…}` | 格式相同，`message` 会不会转给模型要实测 |
| 本会话都允许 | `updatedPermissions` 加规则 | Codex 不给建议（实测 #9），`updatedPermissions` 认不认要实测。不认就由客户端记：这个终端、这个会话里，同一条命令（或同一前缀）再请求时直接答允许；规则只在内存里，会话一换就清空，界面上列出来可以撤销。**只按完整命令匹配，不按前缀**：前缀写宽了就是漏洞（允许 `npm` 等于允许 `npm exec <任意命令>`） |
| 允许访问目录 | `addDirectories` 建议 | 没有建议，不提供这个按钮（`caps.allow_dirs = false`） |
| 回答问题（AskUserQuestion） | `updatedInput.answers` | Codex 的提问工具（`request_user_input`）走不走 `PermissionRequest` 要实测；不走就是 TUI 菜单 → `Phase::Tui` |
| 计划确认 | 抓屏核对后按数字键 | Codex 的计划模式（“Yes, implement this plan”）同样做法，菜单文字放 `codex_patterns.rs` |
| 到终端里回答 | `terminal` | 同 |

审批卡片上显示 `tool_input.description`（沙箱给的原因，实测 #9），比只显示命令好懂。

## 9. `/` 命令提示

- 内置命令（固定列表，放 `codex_patterns.rs`）：`/model`、`/permissions`、`/new`、`/resume`、`/compact`、`/diff`、`/mention`、`/status`、`/review`、`/init`、`/plan`… 其中会打开选择器的（`/model`、`/permissions`、`/resume`）标成“会切到终端”，和 Claude 一样处理。
- 自定义：`$CODEX_HOME/prompts/*.md`（命令名是 `prompts:<文件名>`，以实际版本为准）、`$CODEX_HOME/skills/*/SKILL.md`、项目的 `.codex/`、`.agents/skills/`。扫描方式沿用 `commands.rs`，目录列表放 `codex_patterns.rs`。

## 10. 方便更新：Codex 升级时怎么跟进

Codex 的 rollout 格式、菜单文字、hook 字段都可能随版本变。设计上做到四件事：**知道是哪个版本、改一处就行、有样本能回归、出了问题能看出来**。

### 10.1 版本和能力（`codex_compat.rs`）

- 版本：从 hook 输入里没有，取 rollout 第一条 `session_meta.cli_version`；没有 rollout 时启动前跑一次 `codex --version`（按程序路径缓存）。
- 一张表：

  ```rust
  const TESTED: &[(&str, &str)] = &[("0.159.0", "0.163.x")]; // 验证过的范围
  ```

  在范围内正常用；范围外照样尝试，但状态里带 `untested_version`，界面在对话标签上显示一个小提示“这个 Codex 版本没验证过，遇到问题可以切到终端”。
- 能力 `Caps { images, allow_session, allow_dirs, questions, plan, live_text }`：按版本和启动时的探测（`codex features list` 里 `hooks` 是否开启）得出，随状态推给界面，**界面只按 caps 显示按钮**，不在前端写版本判断。

### 10.2 认文字的地方集中（`codex_patterns.rs`）

所有和 Codex 界面文字、记录名有关的常量放一个文件，按版本分组（新版本只写变了的部分，其余继承上一版）：

```rust
pub struct Patterns {
    pub input_prompt: &'static str,        // "›"
    pub busy_marker: &'static str,          // "esc to interrupt"
    pub idle_hint: &'static [&'static str], // "? for shortcuts", "Ask Codex"
    pub menu_footer: &'static [&'static str],
    pub plan_question: &'static [&'static str],
    pub injected_prefixes: &'static [&'static str],
    pub type_chunk: usize, pub type_gap_ms: u64,
    pub builtin_commands: &'static [(&'static str, bool /* 会打开选择器 */)],
    …
}
```

升级时大多数改动只是这个文件加一组值。

### 10.3 样本和回放测试

- 放在已有的 `src/chat/testdata/`，每个版本一组文件，用 `codex-<版本>-` 开头，不建子目录：
  - `codex-0.159.3-rollout.jsonl`：一段覆盖各类记录的真实会话（命令、改文件、图片、中断、压缩、子代理）；
  - `codex-0.159.3-hooks.jsonl`：同一段会话里收到的全部 hook 输入；
  - `codex-0.159.3-screen-<状态>.txt`：各状态的抓屏（`capture-pane -p -e`，带颜色）：`idle`、`typed`、`busy`、`approval`、`menu`、`trust`、`update`、`plan`；
  - `codex-0.159.3-expected.json`：解析结果的期望值。
- 测试遍历所有 `codex-*` 样本：rollout 解析出的条目和 `expected.json` 一致、`stats.unknown == 0`；每张抓屏分类正确；hook 序列喂给状态机后的 Phase 序列正确。
- 新版本只要放进一组新样本，跑 `cargo test` 就知道哪里要改。

### 10.4 录样本的工具

把 §2 里手工做的实验固化成脚本 `scripts/record-codex.mjs`（开发用，不随包）：

1. 在临时目录建一个 git 仓库和一个隔离的 tmux / psmux 服务；
2. 用 §4.1 的命令启动 Codex，hook 指向一个“只记录、全部放行”的小程序（审批那一步按脚本里的预期答复）；
3. 按剧本送几句话（运行命令、在只读沙箱里建文件触发审批、改文件、中断、`/compact`），每一步抓屏；
4. 把 rollout、hook 记录、抓屏按上面的文件名存进 `src/chat/testdata/`，生成待人工核对的 `codex-<版本>-expected.json`。

一次大约几分钟、几次小的模型调用。Codex 发新版本时跑一遍，再跑测试。

### 10.5 运行时能看出问题

- 诊断信息（`/diag/bundle`）里带上每个对话终端的：Codex 版本、`stats`（行数、解析失败、未知记录类型及计数）、最近一次抓屏分类结果、hooks 有没有接上。
- 对话界面底部（开发者模式下）可以显示“有 N 条记录没认出来”。
- 未知记录不丢：显示成“其他内容”，点开能看原始 JSON（Claude 那边已有这个做法）。

### 10.6 升级检查清单（给维护的人）

1. `npm i -g @openai/codex@<新版本>`，跑 `scripts/record-codex.mjs`；
2. `cargo test chat::codex`，看哪几组失败；
3. 改 `codex_patterns.rs`（菜单文字、提示）或 `codex_rollout.rs` 的映射表（新记录类型）；
4. 确认 hook 事件和字段没变（`hooks.jsonl` 对比上一版）；
5. 更新 `codex_compat.rs` 的验证范围，写进发布说明。

## 11. 动手前要先试的（spike）

| # | 问题 | 怎么试 | 结果决定什么 |
|---|---|---|---|
| S0 | **hook 出错时 Codex 怎么处理**：`looklook hook` 出任何错都退出 0、不输出。Claude 把它当“不做决定、交给 TUI”；Codex 对 `PermissionRequest` 的空输出、非 0 退出、超时、输出不是 JSON，各算允许、拒绝还是交给 TUI | hook 程序分别：不输出退出 0、退出 1、退出 2、sleep 超过 timeout、输出乱码；每种都在只读沙箱里触发一次建文件，看文件建没建、TUI 弹没弹框 | 能不能守住“任何异常都不自动放行”。有哪种情况等于允许，就要改 `hooks.rs` 的出错处理（例如对 Codex 明确答 `ask` 或交给 TUI），否则不能上线 |
| S1 | hook 信任能不能用 `-c hooks.state…trusted_hash` 传；哈希怎么算 | 在 TUI 里信任一条 hook，看 `config.toml` 里写了什么；改命令后哈希怎么变；再用 `-c` 传同样的值、不带 bypass 参数启动 | §4.3 用 B 还是 C |
| S2 | `resume` 时参数顺序；`SessionStart.source` 在 resume、`/new`、`/compact` 后的值 | 录样本时加这几步 | `rebind`、`relaunch` |
| S3 | `StopFailure`、`PostToolUseFailure`、`PermissionDenied`、`Notification` 的类型名 | 二进制字符串 + 录样本 | §4.1 事件列表 |
| S4 | `deny` 的 `message` 会不会给模型；`updatedPermissions` 认不认 | 审批时分别答复，看 rollout 里模型收到什么、下次同样命令还问不问 | §8.3 |
| S5 | 打字块大小和间隔：多快会被当成粘贴、回车被吞 | 不同速度送 200 字中英文混合 | `codex_patterns.rs` 的 `type_chunk` / `type_gap_ms` |
| S6 | 粘贴图片路径能不能成为附件 | 粘贴一张 png 的路径 | `caps.images` |
| S7 | Codex 提问（`request_user_input`）、计划模式走不走 hook | 让它提问、进计划模式 | `caps.questions`、`caps.plan` |
| S8 | Windows：hook 命令在什么 shell 里跑；`looklook.exe` 装在带空格路径（`C:\Program Files\…`）时能不能正常调用 | 换一个带空格的路径放 hook 程序 | §4.1 的引号写法（Claude 在 1.17.1 修过同类问题） |
| S9 | macOS / Linux 上同样流程 | 在 Mac 上跑录样本脚本 | 平台差异 |
| S10 | **app-server 能不能替代 rollout + 抓屏 + 打字**：0.159.3 的 `codex features list` 里 `tui_app_server` 是“removed / true”（开关去掉、默认生效），说明 TUI 很可能已经跑在 app-server 上 | 起一个 `codex app-server`，让终端里的 TUI 用 `--remote` 连上去，看客户端同时连上时：能不能收到逐条 item / 逐字输出；`turn/start` 发的消息 TUI 里看不看得到；审批请求发给谁、两边谁先答算谁；TUI 退出、app-server 重启时会话还在不在 | 第一版走 hooks 方案还是 app-server（§12）。能行的话，§6、§7、§8.1 的大部分工作和风险都不需要了 |
| S11 | 并行时 hook 的顺序：子代理（`multi_agent` 默认开）、一轮里并行的多个工具调用 | 让 Codex 开子代理、同时跑几条命令，记录 hook 输入的顺序和 `turn_id` / `session_id` / 有没有 agent 标识 | `Chat::hook()` 的状态机要不要按 turn / agent 分开（§5）；子代理的 `Stop` 不能当成主线一轮结束 |
| S12 | 哪些请求会走到 `PermissionRequest`：`guardian_approval` 默认开，会先自动审查一部分请求 | 默认配置下分别触发：沙箱外命令、写工作区外的文件、联网；看哪些被 guardian 直接处理、哪些到了 hook | 审批卡片什么时候出现；界面要不要显示“已被 Codex 自动审查放行” |
| S13 | hook 的开销：每个 Pre/PostToolUse 都同步启动一次 `looklook.exe` | Windows（开着 Defender）上让 Codex 连续跑 50 条短命令，对比挂 hooks 和不挂的总用时 | 可以接受就照做；明显变慢就只挂必要的事件（PreToolUse / PostToolUse 改成异步或去掉，状态靠 rollout 补） |

**S0、S1 任何一个不过，hooks 方案都不能上线**；S10 能行的话优先考虑 app-server。这三项放在最前面做。（S0、S10 已实测通过，见 §11.1；**建议第一版改走 app-server**，hooks 方案留作退路，S1 只在走 hooks 时才需要做。）

### 11.1 实测结果（2026-10-18，Codex 0.159.3，Windows 11）

实验脚本：S0 在隔离的 psmux（单独的 `PSMUX_DATA_DIR`）里驱动交互式 TUI，hook 是一个只记录、按模式出错的小程序；S10 自己起 `codex app-server --listen ws://127.0.0.1:<端口>`，TUI 用 `--remote` 连上去，再用一个 WebSocket 客户端（模拟看看）连同一个 app-server。都在临时目录里跑，没碰用户的 `~/.codex` 配置。

**S10：通过，app-server 可以替代 rollout + 抓屏 + 打字。**

| 验证项 | 结果 |
|---|---|
| 客户端找到 TUI 的会话 | `thread/loaded/list` 返回 TUI 的 thread id。**注意**：只列出来不会收到事件，要先 `thread/resume {threadId}` 订阅 |
| 客户端发消息 | `turn/start {threadId, input:[{type:"text",text}]}` 成功；**TUI 里同步显示了这条用户消息**、回复和命令 |
| 结构化事件 | `turn/started`、`item/started` / `item/completed`（`userMessage`、`agentMessage`、`commandExecution`）、`turn/completed`，命令是现成的字符串，不用解析 code mode 的 JS |
| 逐字输出 | `item/agentMessage/delta`（一轮 19 条），不用抓屏 |
| 审批 | `item/commandExecution/requestApproval` 作为请求同时发给客户端和 TUI（TUI 弹出了审批框）；客户端答 `{decision:"accept"}` 后命令执行，**TUI 的审批框自动关掉**，所有客户端收到 `serverRequest/resolved` |
| 审批内容 | 带 `reason`（沙箱给的原因）、`command`、`cwd`、`commandActions`、`proposedExecpolicyAmendment`、`availableDecisions`（`accept` / `acceptWithExecpolicyAmendment` / `cancel`）。协议里还有 `acceptForSession`，“本会话都允许”不用客户端自己记规则（§8.3） |
| 其他请求 | `item/fileChange/requestApproval`、`item/tool/requestUserInput`（提问）、`item/permissions/requestApproval`、`mcpServer/elicitation/request` 都是协议里的请求，对应 §8.3 的提问、改文件审批 |
| 停止 / 插话 | 协议有 `turn/interrupt`、`turn/steer`（这次没测） |

另外看到：交互式 `codex` 默认连本机共享的 app-server 后台服务（`codex agents` 可以列出上面的会话，`--no-daemon` 才不连）；`codex app-server proxy` 能把 stdio 接到这个后台服务的控制 socket。也就是说，看看甚至可以不自己起 app-server，直接接用户平时就在用的那个——这一点要另外评估（会不会看到用户别的会话、权限怎么控制）。

**S0：通过，hook 出错时 Codex 退回 TUI，不会自动放行。**

在只读沙箱里让 Codex 申请沙箱外建文件，`PermissionRequest` hook 按下面的方式回答；看到 TUI 弹审批框时脚本按 Esc 拒绝（用来区分“退回 TUI”和“自动放行”）：

| hook 的回答 | 结果 |
|---|---|
| `{"decision":{"behavior":"allow"}}` | 照办，命令执行，TUI 不弹框 |
| `{"decision":{"behavior":"deny","message":…}}` | 照办，TUI 显示 “Blocked by hook”；**`message` 会转给模型**（rollout 里能看到原文，S4 的一半） |
| 不输出、退出 0 | 退回 TUI 弹审批框 |
| 退出 1 | 退回 TUI 弹审批框（TUI 先显示 “Hook failed — hook exited with code 1”） |
| 退出 2（带 stderr） | 退回 TUI 弹审批框 |
| 输出不是 JSON | 退回 TUI 弹审批框 |
| 超过 timeout（8 秒）没回答 | 退回 TUI 弹审批框 |

所以 `hooks.rs` 现在“出什么错都不输出、退出 0”的做法对 Codex 也成立，“任何异常都不自动放行”守得住。

顺带发现：在 TUI 的审批框里按 Esc 会**中断整轮**（“Conversation interrupted”），而且**不触发 `Stop` hook**——只靠 hooks 的话，这种中断要靠抓屏或 rollout 的 `turn_aborted` 才知道（§5 的“中断”一行）。app-server 有 `turn/completed`，不存在这个问题。

**hooks 方案的实测副产品**（S10 也会碰到的不算）：

- `codex exec` 会把审批策略强制设成 never（`-c approval_policy=…` 覆盖不了），`PermissionRequest` 只能在交互式 TUI 里测；
- PowerShell 下 `-c "hooks.X=[{…}]"` 的双引号会被吃掉（经 psmux 传参时），只能写进 `.ps1` 再 `-File` 运行——S8 的引号问题是真的；
- 粘贴后马上回车会被吞（实测 #13 复现），要等输入框稳定后再按，而且多按的回车会落在随后弹出的审批框上、默认选中“Yes, proceed”——**模拟打字有误批准的风险**，这是 hooks 方案特有的。

## 12. 以后的选项：走 app-server

Codex 有 `codex app-server`（JSON-RPC，`thread/start`、`turn/start`、逐条 item 事件、审批作为请求发给客户端），TUI 也可以 `--remote` 连到一个 app-server。如果终端里的 TUI 和客户端连同一个 app-server，就能拿到逐字输出、结构化的工具调用和审批，不用读文件、不用抓屏。

没有选它作为第一版，因为：接口标着 experimental，变动比 hooks 和 rollout 更频繁；两个客户端同时连一个会话时审批发给谁、谁先答，还没有文档；还要管理 app-server 进程的生命周期。等 hooks 方案稳定后，可以把“逐字输出”这一项单独用 app-server 做，其余不变。

**S10 已经实测通过（§11.1）**，下面这段是当时的分析，结论成立：hooks 方案实际上是 hooks + rollout + 抓屏 + 模拟打字四样东西，其中三样是 Codex 的内部格式，Codex 两周就从 0.159 升到 0.163，四样一起跟版本变，并不比 app-server 稳。而 0.159.3 里 `tui_app_server` 已经默认生效，TUI 本身可能就是 app-server 的一个客户端。如果 S10 证明两个客户端可以连同一个会话，app-server 能一次解决：结构化的条目（不用解析 code mode 的 JS）、逐字输出（不用抓屏）、审批请求（不用 hook 信任）、发消息（不用模拟打字）。所以动手前先花一两天做 S10，再决定第一版走哪条路。

## 13. 分期

| 期 | 内容 | 估计 |
|---|---|---|
| 0 | spike：先做 S0、S1、S10，据此定走 hooks 还是 app-server；再做 S2–S5、S8、S11–S13；录第一组样本 | 2–3 天 |
| 1 | 抽出 `trait Agent`（新文件 `agent.rs`），Claude 的文件不挪、行为不变，现有测试全过 | 1–2 天 |
| 2 | Codex：启动和 hooks、rollout 读取、屏幕、发消息、停止、允许 / 拒绝、会话列表状态和提醒、界面开关；`codex_compat.rs`、`codex_patterns.rs`、样本测试、录样本脚本 | 4–5 天 |
| 3 | 本会话都允许、`/` 命令提示、图片、计划 / 提问、diff 卡片、诊断信息 | 2–3 天 |
| 4（可选） | app-server 逐字输出 | 另行评估 |

## 14. 测试

- 单元：`codex_code_mode.rs`、`codex_rollout.rs`、`screen.rs` 用 §10.3 的样本；`codex_launch.rs` 检查各平台、各 shell 的命令行和引号。
- 状态机：把 `hooks.jsonl` 按顺序喂给 `Chat::hook()`，对比 Phase 序列。
- 端到端（和现在的 `chat_e2e` 一样，默认忽略、手动跑）：真的启动 Codex，发一句话、触发一次审批并从接口答复、停止一次，检查条目和状态。
- 手动：Windows（PowerShell、cmd）、macOS 各跑一遍；电脑和手机同时打开同一个对话。

## 15. 界面设计（详细）

效果图在设计画布“Codex 对话界面（方案）”两行里（7 张）。界面沿用 1.17 的 Claude 对话界面（`src-ui/pages/ChatView.jsx`），**不新做一套**：所有组件按服务端推来的 `agent`（`claude` / `codex`）和 `caps`（§10.1）决定显示什么，前端不判断 Codex 版本。

### 15.1 对话进行中（画板 CodexChat、CodexPhone）

| 区域 | 组件 | Codex 下的行为 |
|---|---|---|
| 顶栏 | `TerminalView` 的标签 | 和 Claude 一样：终端 / 对话 / 改动 / 文件；`hasChat` 放宽到 `launch === 'codex' && chat_ui` |
| 状态行 | `StatusLine` | 文字同 Claude（`chat.phase_*`、`chat.running_tool`）；右侧徽标：权限模式改用 Codex 的取值（`default` → “默认”、只读沙箱 → “只读沙箱”、`bypassPermissions` → “全部权限”），新增 `chat.mode_codex_*` 文案；模型名徽标 |
| 用户消息 | `Item` → user | 同 Claude；注入的上下文已在服务端去掉 |
| 回复 | `Item` → assistant 的 text | 同 Claude，Markdown |
| 思考 | `Thinking` | 只显示 Codex 给的摘要；没有摘要的不显示（不出现空的“思考过程”） |
| 命令、改文件、待办、读图、网页、子代理 | 现有的 `ToolCard` / `ToolBody`（Bash、Edit、Write、TodoWrite、Read、WebSearch、Agent） | **不改**：服务端已经翻成 Claude 的说法（§6.3），卡片、diff、待办清单和 Claude 完全一样 |
| 认不出的 | `ToolBody` 新增 `exec` 分支 | 写明“没认出来的工具调用”，展开是原始代码（`Pre`），不猜它做了什么 |
| 正在输出 | `Live` | 同 Claude：灰色的那段来自抓屏；活动文字用屏幕上的 “Working (12s …)” 换成“正在工作（12 秒）” |
| 一轮结束 | `Turn` | “这一轮用了 38 秒”（来自 `task_complete`） |
| 中断 | `Interrupt` | 同 Claude |
| 输入框 | `Composer` | 同 Claude；`/` 列出 Codex 的命令（§9）；`caps.images = false` 时附件按钮置灰并提示“这个 Codex 版本还不能在对话里发图片”；运行中发送按钮变“停止”（Esc） |

手机：同一组件，审批条从底部弹起（画板 CodexPhoneApproval），按钮纵向排列、高度 ≥44px。

### 15.2 审批（画板 CodexApproval、CodexPhoneApproval）

`ApprovalBar` 的 `permission` 分支：

- 标题：`chat.perm_title`（“Codex 想运行命令（Bash）”——工具名来自 hook）。
- 命令：`$ …`；改文件时是 diff。
- **沙箱的说明**：`tool_input.description` 单独成一块（Codex 特有，比只看命令好懂）。
- 按钮：允许 / 本会话都允许 / 拒绝… / 在终端里回答。
  - “本会话都允许”：`caps.allow_session` 为 `client` 时，提示文字改成“这个会话里同样的命令以后直接允许（看看记住，换会话就失效）”；对话区上方出现“本会话已允许：…”的小标签，点 × 撤销（新接口 `DELETE /instances/{id}/chat/rules/{n}`）。
  - “允许访问目录”：Codex 没有，不显示（`caps.allow_dirs = false`）。
  - “拒绝…”：展开输入理由，和 Claude 一样。
- 提问、计划：`caps.questions` / `caps.plan` 为 false 时不会出现这两种卡片，Codex 弹的这类菜单走 `Phase::Tui`（下一节）。

### 15.3 连接状态和退路（画板 CodexStates）

`Detached` 组件按 `status.reason` 显示一张说明卡片（同一时间一张）：

| reason | 标题 / 按钮 | 新增？ |
|---|---|---|
| `starting` | 正在连接 Codex… / 打开终端 | 文案按 agent 换名字 |
| `not_trusted` | 需要在终端里确认一次 / 去终端里确认、为什么要确认 | 新增（§4.3 方案 C） |
| `hooks_disabled` | 你的 Codex 关掉了 hooks / 打开终端、怎么打开 | 新增 |
| `not_running`、`ended` | Codex 已经退出了 / 接着上次的会话启动 | 文案按 agent |
| `not_attached` | 这个会话没开对话界面 / 编辑会话 | 文案按 agent |
| Phase `tui` | Codex 在终端里等你选择 / 去终端里选 | 同 Claude |

另外，`status.untested_version` 为真时，对话区顶部一条黄色提示“Codex x.y.z 还没在看看里验证过……”，带“报告问题”。

文案键：现有的 `chat.detached_*` 里写死了 “Claude Code”，改成插值 `{{agent}}`；新增 `chat.detached_not_trusted_*`、`chat.detached_hooks_disabled_*`、`chat.untested_version`、`chat.mode_codex_*`、`chat.tool_patch`、`chat.tool_unknown_exec`、`chat.allow_session_client_hint`、`chat.session_rules`、`form.chat_ui_codex_hint`、`form.chat_ui_codex_old`。中英文都要加（`npm run check:locales` 会检查）。

### 15.4 新建 / 编辑会话（画板 CodexNewSession）

`Terminals.jsx` 的新建弹窗：

- `canChat` 放宽到 `launch === 'claude' || launch === 'codex'`。
- Codex 的“对话视图”说明换成 `form.chat_ui_codex_hint`（会用 `--no-daemon` 单独运行、带上看看的 hooks、不改 Codex 配置；第一次可能要在终端里确认信任）。
- Codex 没装：仍是原来的一键安装；版本低于 0.159：开关置灰，写 `form.chat_ui_codex_old` 并给更新命令。需要 `GET /tools` 多返回 `codex_version`（客户端 `tools.rs`）。
- 编辑已有会话：沿用 `form.chat_ui_hint_restart`。

### 15.5 会话列表和提醒

不用改界面：角标（`badge_permission`、`badge_tui`…）和浏览器通知（`notify_*`）由 `Chat::brief()` 给出，Codex 接入后自动有。通知文字里的 “Claude” 改成插值。

## 16. 代码放在哪（客户端 / 管理端，别改错）

| | 仓库 | 这次改什么 | 不改什么 |
|---|---|---|---|
| **客户端** | `looklook`（本仓库，`F:\GitProject\looklook`），代码在 `client/` | `client/src/chat/` 下的新文件和小改（§3）、`client/src/instances.rs`（启动命令、`chat_settings` 放宽到 Codex）、`client/src/tools.rs`（`codex_version`）、`client/src/gateway/chat.rs`（“本会话已允许”的撤销接口）、`client/src-ui/pages/ChatView.jsx`、`client/src-ui/pages/TerminalView.jsx`（`hasChat`）、`client/src-ui/pages/Terminals.jsx`（新建 / 编辑弹窗）、`client/src-ui/shared/chat.js`、`client/src-ui/locales/*/common.json`、`client/src/chat/testdata/codex-*`、`client/scripts/record-codex.mjs`、本文档 `client/CODEX_UI.md` | `protocol/`（对话界面的数据只在客户端和浏览器之间，中转原样转发，协议不用改）；**不新建目录** |
| **管理端** | `ttyd-ai-control-plane`（`F:\GitProject\ttyd-ai-control-plane`） | 只改用户网站的文档：`looklook-server/server-web/src/docs/zh-CN/terminals.md`、`en-US/terminals.md`（“对话视图”一节，画板 ServerDocs），可选 `faq.md` | `looklook-server/server/`（Rust）、数据库迁移、`protocol/`、管理后台 `server-web/src/admin/`（模型服务商已支持 Codex）、`server-web/src/user/` 的页面；这个仓库里的 `looklook-client/` 也不动（客户端以 `looklook` 为准） |
