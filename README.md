# rime-cli

在终端 / tmux 里使用 [Rime](https://rime.im) 输入法的 Rust 实现：一个常驻的
librime 引擎服务（`rime-daemon`）+ 一个终端输入客户端（`rime-cli`），把按键转
发给 librime 处理，并把上屏的中文文本与未被消费的按键实时转发到 tmux pane。

## 特性

- **常驻 daemon，独占 librime**：librime 只在 `rime-daemon` 进程里初始化一次，
  LevelDB 用户词典只被一个进程打开，多客户端共享同一份用户数据（词频、用户词）。
- **Unix socket + JSON-RPC 2.0**：按行分隔（newline-delimited）的 JSON-RPC 协议，
  每个客户端连接对应一个 librime session（例如一个编辑器实例）。
- **启动自动部署**：后台线程执行 librime 部署——`build/` 目录没有 `.bin` 产物时
  全量构建，否则增量检测；客户端连接时部署通常已完成，输入前自动等待首次部署。
- **终端客户端 `rime-cli`**：
  - raw 终端模式读取按键，解析 xterm 风格转义序列与 **kitty keyboard protocol**
    （tmux 3.4+ extended-keys / kitty / wezterm），组合键归一化为传统终端字节转发；
  - 两行界面画在 stderr：第 1 行 preedit（拼音串 + 光标），第 2 行候选词；
  - 未消费的按键与上屏文本按**原始字节**转发，可接入 `tmux send-keys` 等；
  - 未运行 daemon 时自动拉起（detached，`setsid`）。
- **无需 bindgen**：`rime-sys` 是手写的 librime C API FFI 绑定，仅声明用到的函数。

## 架构

```
┌──────────────┐   unix socket + JSON-RPC 2.0 (newline-delimited)   ┌──────────────┐
│  rime-cli    │ ────────────────────────────────────────────────▶ │  rime-daemon │
│  (终端客户端) │ ◀──────────────────────────────────────────────── │  (独占 librime│
│              │    每个连接 = 一个 librime session                 │   引擎服务)   │
└──────┬───────┘                                                   └──────┬───────┘
       │ stdout / --exec                                               │ 部署
       ▼                                                               ▼
  tmux pane (shell / nvim …)                             rime-ice（共享词库）+ user_data
```

`rime-cli` 把键盘事件送给 daemon 里的 librime 处理：

- librime **消费**了按键 → 上屏文本（`session_get_commit`）实时转发给目标；
- librime **未消费**（纯英文输入、组合键等）→ 按键的原始字节原样转发。

转发目标二选一：

- **stdout**（默认）：上屏文本与原始字节直接写入 stdout；
- **`--exec <模板>`**：每次转发直接执行解析后的命令（不经 sh），模板中 `{}`
  替换为字面负载，`{key}` 替换为 tmux 键名（如 `C-d`、`Up`、`Enter`），由 tmux
  按目标 pane 的键盘协议编码发送——对启用了 kitty keyboard protocol 的程序
  （fish 4、nvim 等）必需。

## 构建

依赖：

- [librime](https://github.com/rime/librime)（`rime-sys/build.rs` 依次查找
  `RIME_LIB_DIR`/`RIME_INCLUDE_DIR` → `pkg-config --libs rime` →
  Homebrew `/opt/homebrew/opt/librime` → `/usr/local`）
- Rust 工具链（edition 2021）
- `just`（可选，用于快捷命令）

clone 时记得带上子模块（`rime-ice` 词库即共享数据目录）：

```bash
git clone --recurse-submodules https://github.com/<you>/rime-cli
cd rime-cli
```

用 [devenv](https://devenv.sh)（Nix）进入开发环境：

```bash
devenv shell
```

或直接构建：

```bash
cargo build -p rime-daemon -p rime-cli
just build   # 等价
```

## 快速开始

一键 tmux 三屏开发环境（左 = daemon，右上 = shell，右下 = cli，`rime-cli` 以
`--exec 'tmux send-keys …'` 转发到右上 pane）：

```bash
just run
```

手动两步：

```bash
# 终端 1：前台运行 daemon（Ctrl-C 退出）
just server

# 终端 2：连接 daemon，进入输入模式（Ctrl-C 退出）
just cli
```

> `just server` 通过环境变量把共享数据目录指向仓库内的 `rime-ice`，用户数据与
> 日志分别落在 `user_data/`、`log/`。首次启动会自动全量部署（数秒～数十秒，
> 取决于词库），期间 cli 会提示“正在部署词库”并等待完成。

## 用法

```
rime-cli [--exec <命令模板>]
```

| 参数 | 说明 |
| --- | --- |
| `--exec <模板>`（或 `-e`，或环境变量 `RIME_EXEC`） | 每次转发直接执行解析后的命令，模板中 `{}` = 字面负载（上屏文本/原始字节），`{key}` = tmux 键名（由 tmux 按目标 pane 的键盘协议编码）。例：`rime-cli --exec 'tmux send-keys -t %1 {key}'`、`rime-cli --exec 'tmux send-keys -t %1 -l {}'` |
| `-h, --help` | 帮助 |

### 按键

| 按键 | 行为 |
| --- | --- |
| 拼音/字母 | 交给 librime（进入 preedit） |
| `1`–`9` 等选字键 | 按方案配置上屏候选 |
| `↑` / `↓` | 映射为 PageUp / PageDown 翻候选页；无候选时原样透传 |
| `Enter` | 有 preedit 且 librime 未消费时，按输入法惯例本地上屏 |
| `Ctrl-Space` | 中英切换（交给 librime） |
| `Ctrl-C` | 退出（不转发） |
| 其他组合键 / 未识别序列 | librime 未消费则按原始字节透传 |

界面（2 行，画在 stderr）：第 1 行 preedit（光标为终端竖线光标，空闲时显示方案名），
第 2 行候选词（数字选字、注释、末尾 `…` 表示还有下一页）。

## 环境变量

配置完全由 `rime-daemon` 在启动时解析（继承启动者的环境变量）；`rime-cli`
拉起 daemon 时**不设置任何环境变量**，仅用 `RIME_DAEMON_BIN` 指定可执行文件
路径，因此通过 rime-cli 拉起的 daemon 与直接启动配置完全一致。解析规则：

| 变量 | 默认值 | 说明 |
| --- | --- | --- |
| `RIME_SHARED_DATA_DIR` | `~/.config/rime`（若存在）否则 `~/.local/share/rime` | 共享数据目录（词库、schemas），`just server` 下指向仓库内 `rime-ice` |
| `RIME_USER_DATA_DIR` | `~/.local/share/rime.nvim` | 用户数据目录（`build/` 部署产物、用户词典） |
| `RIME_LOG_DIR` | `~/.local/state/rime.nvim` | 日志目录（`rime-daemon.log`） |
| `RIME_SOCKET` | `$XDG_RUNTIME_DIR/rime-daemon.sock`，否则日志目录下 | daemon 的 unix socket 路径 |
| `RIME_MIN_LOG_LEVEL` | `3`（FATAL） | librime 最小日志级别 |
| `RIME_DAEMON_BIN` | 与 cli 同目录 → PATH | cli 拉起 daemon 时使用的可执行文件路径 |
| `RIME_EXEC` | — | `--exec` 模板的备选来源 |

> 注释：`RIME_USER_DATA_DIR` 的默认名沿用了 rime.nvim 的路径约定，可自由覆盖。

## JSON-RPC 协议

每行一个 JSON 对象（JSON-RPC 2.0 请求/响应）。方法：

| 方法 | 参数 | 说明 |
| --- | --- | --- |
| `ping` | — | 探活，返回 `"pong"` |
| `maintenance_start` | `{ full: bool }` | 触发部署（全量/增量） |
| `maintenance_join` | — | 等待部署线程结束 |
| `maintenance_mode` | — | 是否处于部署中（含启动自动部署的同步准备阶段） |
| `schema_list` | — | 方案列表 `[{ schema_id, name }]` |
| `session_current_schema` | — | 当前方案 id |
| `session_select_schema` | `{ schema_id }` | 切换方案 |
| `session_process_key` | `{ code, mask }` | 处理按键（X11 keysym + 修饰掩码），返回是否被消费 |
| `session_commit_composition` | — | 上屏当前 preedit |
| `session_clear_composition` | — | 清空 preedit |
| `session_get_commit` | — | 取待上屏文本 `{ text }` |
| `session_get_context` | — | 取 `{ composition, menu }`（preedit、候选等） |

## 目录结构

```
crates/
  rime-sys/    手写 librime C API FFI 绑定（无 bindgen）+ build.rs 定位 librime
  rime-daemon/ 引擎服务：独占 librime、unix socket、JSON-RPC、启动自动部署
  rime-cli/    终端客户端：raw 终端、按键解析/kitty 协议、转发、2 行界面
rime-ice/      iDvel/rime-ice 词库（git submodule，作为共享数据目录）
config/        示例 Rime 配置（最简 luna_pinyin）
user_data/     运行生成的用户数据（build/ 部署产物、用户词典；已 gitignore）
Justfile       build / server / cli / run 快捷命令
devenv.yaml    Nix 开发环境（librime、pkg-config、cargo、rustc）
default.nix    Nix 打包（RIME_LIB_DIR/RIME_INCLUDE_DIR 指向 store 里的 librime）
```

## 开发

```bash
just build    # cargo build -p rime-daemon -p rime-cli
just server   # 前台运行 daemon（Ctrl-C 退出）
just cli      # 运行 cli（可带 --exec 等参数）
just run      # tmux 三屏：左 daemon / 右上 shell / 右下 cli
cargo test    # 单元测试（cli 的 shlex 分词、tmux 键名、kitty 序列解析等）
```

设计要点（实现细节见各 crate 的文档注释）：

- librime C API **非线程安全**：daemon 内所有调用经单一全局锁串行化；维护线程
  使用 librime 自己的 deployer 线程（设计上允许与 session 调用并行）。
- session 懒创建：首个 `session_*` RPC 才建 librime session，保证部署先于引擎
  启动完成，避免引擎加载不存在的 `build/` 并缓存空结果。
- daemon 是**常驻进程**（SIGTERM/SIGINT 退出），与客户端生命周期无关；同 socket
  已有存活 daemon 时自动退出，遗留的 stale socket 会被清理重绑。

## 致谢

- [librime](https://github.com/rime/librime) — 输入法引擎
- [rime-ice](https://github.com/iDvel/rime-ice) — 共享词库（submodule）
- [Rime](https://rime.im) — 输入法方案社区
