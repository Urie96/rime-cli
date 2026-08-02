//! 2-line display state + ANSI rendering.
//!
//! Line 1: preedit（带光标）
//! Line 2: 候选词（带序号与注释）
//!
//! 已上屏文本不再由 CLI 维护：rime 的上屏文本与未被消费的按键都实时转发到
//! stdout（数据通道，可接入 tmux pane 等），界面只负责展示 preedit/候选。

use crate::client::Client;

#[derive(Default)]
pub struct State {
    /// 当前 preedit（拼音串等）。
    pub preedit: String,
    /// preedit 内光标位置（字符下标）。
    pub cursor: usize,
    /// (候选词, 注释)。
    pub candidates: Vec<(String, String)>,
    /// 高亮候选的下标；-1 表示无高亮。
    pub highlighted: i64,
    /// 当前页是否还有更多候选（末尾显示 …）。
    pub has_more: bool,
    /// 当前方案显示名（无 preedit 时显示在方括号里）。
    pub schema: Option<String>,
}

impl State {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn has_preedit(&self) -> bool {
        !self.preedit.is_empty()
    }

    /// 从 daemon 拉取最新 commit 与 context。
    /// 返回本次新上屏的文本（空串 = 无），由调用方转发到 stdout。
    pub fn refresh(&mut self, client: &mut Client) -> String {
        let text = client.get_commit().unwrap_or_default();
        match client.get_context() {
            Ok(ctx) => {
                let comp = &ctx["composition"];
                self.preedit = comp["preedit"].as_str().unwrap_or("").to_string();
                self.cursor = comp["cursor_pos"].as_i64().unwrap_or(0).max(0) as usize;
                let menu = &ctx["menu"];
                self.candidates = menu["candidates"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .map(|c| {
                                (
                                    c["text"].as_str().unwrap_or("").to_string(),
                                    c["comment"].as_str().unwrap_or("").to_string(),
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                self.highlighted = menu["highlighted_candidate_index"].as_i64().unwrap_or(0);
                self.has_more = !menu["is_last_page"].as_bool().unwrap_or(true);
            }
            Err(_) => {
                self.preedit.clear();
                self.candidates.clear();
                self.cursor = 0;
                self.highlighted = -1;
                self.has_more = false;
            }
        }
        text
    }
}

/// Render the 2-line frame (full clear + home + 2 lines).
pub fn render(s: &State) -> String {
    let mut out = String::new();
    out.push_str("\x1b[2J\x1b[H");

    // 第 1 行：preedit（空闲时显示方案名占位）
    out.push_str("\x1b[2K");
    if s.preedit.is_empty() {
        match s.schema.as_deref() {
            Some(name) if !name.is_empty() => {
                out.push_str("\x1b[90m[");
                out.push_str(name);
                out.push_str("]\x1b[0m");
            }
            _ => out.push_str("\x1b[90m…\x1b[0m"),
        }
    } else {
        let before: String = s.preedit.chars().take(s.cursor).collect();
        let after: String = s.preedit.chars().skip(s.cursor).collect();
        out.push_str("\x1b[36m");
        out.push_str(&before);
        out.push_str("\x1b[33;1m|");
        out.push_str("\x1b[36m");
        out.push_str(&after);
        out.push_str("\x1b[0m");
    }
    out.push_str("\r\n");

    // 第 2 行：候选词
    out.push_str("\x1b[2K");
    for (i, (text, comment)) in s.candidates.iter().enumerate() {
        if i > 0 {
            out.push_str("  ");
        }
        let highlighted = s.highlighted >= 0 && i as i64 == s.highlighted;
        if highlighted {
            out.push_str("\x1b[7m");
        }
        out.push_str("\x1b[33m");
        out.push_str(&(i + 1).to_string());
        out.push('.');
        out.push_str("\x1b[0m");
        out.push(' ');
        out.push_str(text);
        if !comment.is_empty() {
            out.push(' ');
            out.push_str("\x1b[2m");
            out.push_str(comment);
            out.push_str("\x1b[0m");
        }
        if highlighted {
            out.push_str("\x1b[0m");
        }
    }
    if s.has_more {
        out.push_str("\x1b[90m …\x1b[0m");
    }
    out.push_str("\x1b[0m");
    out
}
