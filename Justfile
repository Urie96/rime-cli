set shell := ["bash", "-euo", "pipefail", "-c"]

RIME_SHARED_DATA_DIR:= justfile_directory() / "rime-ice"
RIME_USER_DATA_DIR:= justfile_directory() / "user_data"
RIME_LOG_DIR:= justfile_directory() / "log"
SOCKET := RIME_USER_DATA_DIR / "rime-daemon.sock"
DAEMON := justfile_directory() / "target" / "debug" / "rime-daemon"
CLI := justfile_directory() / "target" / "debug" / "rime-cli"

build:
    cargo build -p rime-daemon -p rime-cli -q

# ---------------------------------------------------------------------------
# 常用命令
# ---------------------------------------------------------------------------

# 前台常驻运行 rime-daemon（Ctrl-C 退出；另开一个终端用 `just cli` 连接）。
server:
    #!/usr/bin/env bash
    export RIME_SHARED_DATA_DIR="{{ RIME_SHARED_DATA_DIR }}"
    export RIME_USER_DATA_DIR="{{ RIME_USER_DATA_DIR }}"
    export RIME_LOG_DIR="{{ RIME_LOG_DIR }}"
    export RIME_SOCKET="{{ SOCKET }}"
    echo "rime-daemon 前台运行中（Ctrl-C 退出）"
    exec "{{ DAEMON }}"

cli *args:
    #!/usr/bin/env bash
    export RIME_SHARED_DATA_DIR="{{ RIME_SHARED_DATA_DIR }}"
    export RIME_USER_DATA_DIR="{{ RIME_USER_DATA_DIR }}"
    export RIME_LOG_DIR="{{ RIME_LOG_DIR }}"
    export RIME_SOCKET="{{ SOCKET }}"
    export RIME_DAEMON_BIN="{{ DAEMON }}"
    "{{ CLI }}" {{ args }}

# 一键打开 tmux 三屏开发环境：左 rime-daemon / 右上 shell / 右下 rime-cli。
# 已存在同名会话则直接附加。tmux 配置若用 base-index 1（首窗口索引为 1），
# 脚本全程以 pane_id（%N）定位，不写死窗口/面板索引。
run: build
    #!/usr/bin/env bash
    set -euo pipefail
    SESSION="rime"

    if tmux has-session -t "$SESSION" 2>/dev/null; then
      echo "tmux 会话 '$SESSION' 已存在，直接附加。"
      exec tmux attach-session -t "$SESSION"
    fi

    echo "创建 tmux 会话 '$SESSION'：左 = rime-daemon，右上 = shell，右下 = rime-cli"
    # 先建空窗口、再 send-keys 启动 server：即使 `just server` 秒挂
    # （如缺 Squirrel 配置、编译失败），布局也照常建立。
    tmux new-session -d -s "$SESSION" -n dev
    tmux send-keys -t "$SESSION" 'just server' Enter

    # 右上 pane：默认 shell（切分后新 pane 自动成为活动 pane）
    tmux split-window -h -t "$SESSION"
    TARGET_PANE=$(tmux display-message -t "$SESSION" -p '#{pane_id}')
    # 右下 pane：cli，注入 TARGET_PANE = 右上 pane 的 TMUX_PANE
    sleep 1
    tmux split-window -v -t "$SESSION" "RIME_EXEC='tmux send-keys -t $TARGET_PANE {key}' just cli"

    exec tmux attach-session -t "$SESSION"
