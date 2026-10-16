//! Structured terminal views of directory listings and Git evidence.
use super::*;

const ADDED: Color = crate::tui::theme::THEME.patch_added;
const REMOVED: Color = crate::tui::theme::THEME.diff_removed;

// Format metadata literally before layout so wrapped continuations retain the tool gutter.
struct View {
    lines: Vec<Line<'static>>,
    expanded: bool,
    width: u16,
    hidden: bool,
}

impl View {
    fn new(expanded: bool, width: u16) -> Self {
        Self {
            lines: vec![],
            expanded,
            width,
            hidden: false,
        }
    }

    fn push(&mut self, text: &str, color: Color) {
        let available = self.width.saturating_sub(4);
        let text = safe_output(text);
        self.hidden |= text.width() > usize::from(available);
        let parts = if self.expanded && available >= 2 {
            super::super::transcript::wrap_line(&Line::from(text), available).0
        } else {
            vec![Line::from(clipped(&text, usize::from(available)))]
        };

        for line in parts {
            self.lines.push(Line::from(vec![
                Span::raw(" ".repeat(usize::from(self.width).min(4))),
                Span::styled(line.to_string(), Style::default().fg(color)),
            ]));
        }
    }

    fn push_spans(&mut self, spans: Vec<Span<'static>>) {
        let available = self.width.saturating_sub(4);
        let prefix_width = usize::from(self.width).min(4);
        let prefix = " ".repeat(prefix_width);

        let sanitized: Vec<Span<'static>> = spans
            .into_iter()
            .map(|mut s| {
                s.content = safe_output(&s.content).into();
                s
            })
            .collect();

        let raw_line = Line::from(sanitized);
        let total_w = raw_line.width();
        self.hidden |= total_w > usize::from(available);

        if available == 0 {
            self.lines
                .push(Line::from(vec![Span::raw(prefix)]));
            return;
        }

        let parts = if self.expanded && available >= 2 {
            super::super::transcript::wrap_line(&raw_line, available).0
        } else {
            let mut clipped_spans = Vec::new();
            let mut remaining = usize::from(available);
            for span in raw_line.spans {
                let sw = span.width();
                if sw <= remaining {
                    remaining -= sw;
                    clipped_spans.push(span);
                } else if remaining > 0 {
                    let clipped_text = clipped(&span.content, remaining);
                    clipped_spans.push(Span::styled(clipped_text, span.style));
                    break;
                } else {
                    break;
                }
            }

            vec![Line::from(clipped_spans)]
        };

        for p in parts {
            let mut line_spans = vec![Span::raw(prefix.clone())];
            line_spans.extend(p.spans);
            self.lines.push(Line::from(line_spans));
        }
    }

    fn source(&mut self, renderer: &mut Renderer, text: &str) {
        self.hidden |= safe_output(text).width() > usize::from(self.width.saturating_sub(4));
        self.lines.extend(renderer.render(
            text,
            super::super::code::Options {
                gutter: super::super::code::Gutter::Indent(4),
                width: Some(self.width),
                overflow: if self.expanded {
                    super::super::code::Overflow::Wrap
                } else {
                    super::super::code::Overflow::Clip
                },
                ..Default::default()
            },
        ));
    }

    fn note(&mut self, text: &str) {
        self.push(text, SECONDARY);
    }

    fn omitted(&mut self, count: usize, unit: &str) {
        if count > 0 {
            self.hidden = true;
            self.note(&format!("另有 {count} {unit}"));
        }
    }

    fn paging(&mut self, value: &Value) {
        if value["has_more"] == true {
            self.note(&match value["next_offset"].as_u64() {
                Some(offset) => format!("... 另有后续记录 (offset={offset})"),
                None => "... 另有后续记录".into(),
            });
        }

        if value["scan_truncated"] == true {
            self.note("已达到扫描上限，可缩小目录范围继续查询");
        }

        if value["output_truncated"] == true {
            self.note("工具输出已截断，可缩小查询范围");
        }
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        if self.expanded || self.hidden {
            self.lines
                .push(toggle_line(self.expanded, self.width));
        }

        self.lines
    }
}

pub(super) fn files(row: &ToolRow, expanded: bool, width: u16) -> Option<Vec<Line<'static>>> {
    let value: Value = serde_json::from_str(&row.output).ok()?;
    let entries = value["entries"].as_array()?;
    let arguments: Value = serde_json::from_str(&row.arguments).unwrap_or_default();
    let base = arguments["path"]
        .as_str()
        .unwrap_or(".")
        .trim_end_matches('/');
    let base = base.strip_prefix("./").unwrap_or(base);
    let prefix = format!("{base}/");
    let mut view = View::new(expanded, width);

    if entries.is_empty() {
        view.note("(空目录)");
        return Some(view.finish());
    }

    let limit = if expanded { entries.len() } else { 6 };

    for entry in entries.iter().take(limit) {
        let path = entry["path"].as_str()?;
        let path = path
            .strip_prefix(&prefix)
            .unwrap_or(path);

        if entry["kind"] == "directory" {
            view.push(&format!("{path}/"), crate::tui::theme::THEME.md_h4_plus);
        } else {
            view.push(path, crate::tui::theme::THEME.output_foreground);
        }
    }

    if !expanded && entries.len() > limit {
        view.omitted(entries.len().saturating_sub(limit), "个条目");
    }

    Some(view.finish())
}

pub(super) fn summary(arguments: &str) -> String {
    let value: Value = serde_json::from_str(arguments).unwrap_or_default();
    let label = match value["view"]
        .as_str()
        .unwrap_or("status")
    {
        "history" => "提交历史",
        "diff" if value["staged"] == true => "暂存区差异",
        "diff" => "工作区差异",
        _ => "工作区状态",
    };

    match value["path"].as_str() {
        Some(path) => format!(
            "{label}  {}",
            super::super::render::compact_preview(path, 100)
        ),

        None => label.into(),
    }
}

pub(super) fn git(row: &ToolRow, expanded: bool, width: u16) -> Option<Vec<Line<'static>>> {
    let value: Value = serde_json::from_str(&row.output).ok()?;
    let mut view = View::new(expanded, width);

    if let Some(commits) = value["commits"].as_array() {
        if !commits.is_empty() {
            view.note(&format!("提交历史 ({} 条):", commits.len()));
        } else {
            view.note("无提交历史记录");
        }

        let limit = if expanded { commits.len() } else { 4 };

        for commit in commits.iter().take(limit) {
            let id = commit["id"].as_str()?;
            let short = commit["short_id"].as_str()?;

            if !matches!(id.len(), 40 | 64)
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit())
                || short.len() < 4
                || !id.starts_with(short)
            {
                view.push("历史记录字段异常，请重新查询提交历史", REMOVED);

                if expanded {
                    for line in serde_json::to_string_pretty(commit)
                        .ok()?
                        .lines()
                    {
                        view.note(line);
                    }
                }

                view.hidden = true;
                continue;
            }

            let subject = commit["subject"].as_str()?;
            view.push(
                &format!(
                    "{short}  {}",
                    if subject.is_empty() {
                        "无提交标题"
                    } else {
                        subject
                    }
                ),
                Color::Reset,
            );

            if expanded {
                view.note(&format!(
                    "  {}  {}",
                    commit["author"].as_str()?,
                    commit["authored_at"].as_str()?
                ));
                view.note(&format!("  {id}"));
                view.lines.push(Line::default());
            }
        }

        // Author, time and full ID are details available on expansion.
        view.hidden |= !commits.is_empty();
        view.omitted(commits.len().saturating_sub(limit), "条提交");
    } else if let Some(entries) = value["entries"].as_array() {
        let branch = &value["branch"];
        let head = branch["head"]
            .as_str()
            .unwrap_or("未知分支");
        view.push(head, ACCENT);

        if let Some(upstream) = branch["upstream"].as_str() {
            view.note(&format!(
                "跟踪 {upstream}  领先 {}  落后 {}",
                branch["ahead"], branch["behind"]
            ));
        }

        if expanded && let Some(id) = branch["oid"].as_str() {
            view.note(&format!("HEAD  {id}"));
        }

        view.note(if entries.is_empty() {
            "工作区干净 (无变更)"
        } else {
            "索引 / 工作区  路径"
        });
        let limit = if expanded { entries.len() } else { 6 };

        for entry in entries.iter().take(limit) {
            let (status, color) = match entry["kind"].as_str()? {
                "untracked" => ("??", ADDED),
                "ignored" => ("!!", SECONDARY),
                "unmerged" => (entry["xy"].as_str().unwrap_or("UU"), REMOVED),
                _ => (entry["xy"].as_str().unwrap_or(".."), ACCENT),
            };

            let path = entry["path"].as_str()?;
            let path = entry["original_path"]
                .as_str()
                .map(|old| format!("{old} → {path}"))
                .unwrap_or_else(|| path.into());
            view.push(&format!("{status:<2}  {path}"), color);
        }

        view.hidden |= branch["oid"].is_string();
        view.omitted(entries.len().saturating_sub(limit), "个变更");
    } else if let Some(lines) = value["lines"].as_array() {
        if lines.is_empty() {
            view.note(if value["staged"] == true {
                "暂存区无差异"
            } else {
                "工作区无差异"
            });
        } else {
            view.note(&format!(
                "{} ({} 行):",
                if value["staged"] == true {
                    "暂存区差异"
                } else {
                    "工作区差异"
                },
                lines.len()
            ));
        }

        let limit = if expanded { lines.len() } else { 8 };
        let mut renderer = Renderer::new("diff");
        for line in lines.iter().take(limit) {
            let text = line.as_str()?;
            view.source(&mut renderer, text);
        }

        view.omitted(lines.len().saturating_sub(limit), "行差异");
    } else {
        return fallback(row, expanded, width);
    }

    view.paging(&value);

    Some(view.finish())
}

pub(super) fn format_bytes(bytes: u64) -> String {
    if bytes >= 1_024 * 1_024 * 1_024 {
        format!("{:.1} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    } else if bytes >= 1_024 * 1_024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1_024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

pub(super) fn command_output(
    row: &ToolRow,
    expanded: bool,
    width: u16,
) -> Option<Vec<Line<'static>>> {
    let value: Value = serde_json::from_str(&row.output).ok()?;
    let content = value["content"].as_str()?;
    let stream = value["stream"]
        .as_str()
        .unwrap_or("stdout");
    let offset = value["offset"].as_u64().unwrap_or(0);
    let next_offset = value["next_offset"]
        .as_u64()
        .unwrap_or(0);
    let total = value["total_retained_bytes"]
        .as_u64()
        .unwrap_or(0);
    let truncated = value["truncated"]
        .as_bool()
        .unwrap_or(false);

    let theme = &crate::tui::theme::THEME;
    let mut view = View::new(expanded, width);

    let stream_color = if stream == "stderr" {
        theme.error
    } else {
        theme.accent
    };
    let stream_badge = if stream == "stderr" {
        "[STDERR]"
    } else {
        "[STDOUT]"
    };
    let range_str = if total > 0 {
        let pct = format!(
            " · {:.0}%",
            (next_offset as f64 / total as f64 * 100.0).min(100.0)
        );
        format!(
            "字节 {}..{} (共 {}{pct})",
            format_bytes(offset),
            format_bytes(next_offset),
            format_bytes(total)
        )
    } else {
        format!(
            "字节 {}..{}",
            format_bytes(offset),
            format_bytes(next_offset)
        )
    };

    view.push_spans(vec![
        Span::styled(
            format!("{stream_badge} "),
            Style::default()
                .fg(stream_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(range_str, Style::default().fg(theme.tool_detail)),
    ]);

    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        view.note("(空输出)");
    } else {
        let limit = if expanded { lines.len() } else { 6 };
        for line in lines.iter().take(limit) {
            let spans = super::super::render::parse_ansi_to_spans(line);
            view.push_spans(spans);
        }

        if !expanded && lines.len() > limit {
            view.omitted(lines.len().saturating_sub(limit), "行输出");
        }
    }

    if truncated || (total > 0 && next_offset < total) {
        view.push_spans(vec![Span::styled(
            format!("… 尚有后续输出 (next_offset = {next_offset}, 可使用 CommandOutput 继续查询)"),
            Style::default().fg(theme.tool_running),
        )]);
    }

    Some(view.finish())
}

pub(super) fn web_search(row: &ToolRow, expanded: bool, width: u16) -> Option<Vec<Line<'static>>> {
    let value: Value = serde_json::from_str(&row.output).ok()?;
    let results = value["results"].as_array()?;
    let provider = value["provider"]
        .as_str()
        .unwrap_or("Web");
    let theme = &crate::tui::theme::THEME;
    let mut view = View::new(expanded, width);

    let count = results.len();
    view.push_spans(vec![
        Span::styled(
            format!("{} 搜索结果 · ", icons::MAGNIFY),
            Style::default().fg(theme.accent),
        ),
        Span::styled(
            format!("来源: {provider} (共 {count} 条)"),
            Style::default().fg(theme.tool_detail),
        ),
    ]);

    if value["query_match_unconfirmed"].as_bool() == Some(true) {
        view.push_spans(vec![Span::styled(
            "相关性未确认：结果标题、摘要或链接缺少查询匹配线索，请调整搜索词或打开网页核实。",
            Style::default().fg(theme.tool_running),
        )]);
    }

    if expanded {
        if let Some(warnings) = value["warnings"].as_array() {
            for warning in warnings
                .iter()
                .filter_map(Value::as_str)
            {
                view.push_spans(vec![Span::styled(
                    format!("注意: {warning}"),
                    Style::default().fg(theme.tool_running),
                )]);
            }
        }
        if let Some(url) = value["response_url"].as_str() {
            view.push_spans(vec![Span::styled(
                format!("搜索页面: {url}"),
                Style::default().fg(theme.md_link),
            )]);
        }
    }

    if results.is_empty() {
        view.note("未找到相关搜索结果");
    } else {
        let limit = if expanded { results.len() } else { 3 };
        for (i, item) in results.iter().take(limit).enumerate() {
            let title = item["title"]
                .as_str()
                .unwrap_or("(无标题)");
            let url = item["url"].as_str().unwrap_or("");
            let snippet = item["snippet"].as_str().unwrap_or("");

            view.push_spans(vec![
                Span::styled(
                    format!("{}. ", i + 1),
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    title.to_owned(),
                    Style::default()
                        .fg(theme.output_foreground)
                        .add_modifier(Modifier::BOLD),
                ),
            ]);

            if !url.is_empty() {
                view.push_spans(vec![Span::styled(
                    format!("   {url}"),
                    Style::default().fg(theme.md_link),
                )]);
            }

            if !snippet.is_empty() {
                view.push_spans(vec![Span::styled(
                    format!("   {snippet}"),
                    Style::default().fg(theme.output_muted),
                )]);
            }
        }

        if !expanded && results.len() > limit {
            view.omitted(results.len().saturating_sub(limit), "条结果");
        }
    }

    if let Some(reason) = value["fallback_reason"].as_str() {
        view.push_spans(vec![Span::styled(
            format!("注意: {reason}"),
            Style::default().fg(theme.tool_running),
        )]);
    }

    Some(view.finish())
}

pub(super) fn document(row: &ToolRow, expanded: bool, width: u16) -> Option<Vec<Line<'static>>> {
    document_with_links(row, expanded, width).map(|lines| {
        lines
            .into_iter()
            .flat_map(|linked| linked.wrap(width))
            .map(|linked| {
                if linked.line.width() > usize::from(width) {
                    Line::from(super::super::render::fit_label(
                        &linked.line.to_string(),
                        usize::from(width),
                        false,
                    ))
                } else {
                    linked.line
                }
            })
            .collect()
    })
}

pub(super) fn document_with_links(
    row: &ToolRow,
    expanded: bool,
    width: u16,
) -> Option<Vec<LinkedLine>> {
    let value: Value = serde_json::from_str(&row.output).ok()?;
    let path = value["path"]
        .as_str()
        .unwrap_or_default();
    let format = value["format"]
        .as_str()
        .unwrap_or("doc");
    let content = value["content"].as_str()?;
    let start_char = value["start_char"]
        .as_u64()
        .unwrap_or(0);
    let total_chars = value["total_chars"]
        .as_u64()
        .unwrap_or(0);
    let truncated = value["truncated"]
        .as_bool()
        .unwrap_or(false);
    let next_start = value["next_start_char"].as_u64();

    let theme = &crate::tui::theme::THEME;
    let mut view = View::new(expanded, width);

    let format_badge = format.to_ascii_uppercase();
    let end_char = start_char + content.chars().count() as u64;
    view.push_spans(vec![
        Span::styled(
            format!("[{format_badge}] "),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            path.to_owned(),
            Style::default().fg(theme.output_foreground),
        ),
        Span::styled(
            format!(" · 字符 {start_char}..{end_char} (共 {total_chars})"),
            Style::default().fg(theme.tool_detail),
        ),
    ]);

    if let (Some(start), Some(end), Some(total)) = (
        value["pdf"]["start_page"].as_u64(),
        value["pdf"]["end_page"].as_u64(),
        value["pdf"]["total_pages"].as_u64(),
    ) {
        view.note(&format!("第 {start}–{end} 页 / 共 {total} 页"));
    }
    if value["pdf"]["has_encoding_issues"].as_bool() == Some(true) {
        view.note("部分文字编码异常：缺失片段已标注，严重乱码页已省略文字");
    }
    if value["pdf"]["partial"].as_bool() == Some(true) {
        view.note("文字提取不完整，缺失内容已按页标注");
    }
    if let Some(pages) = value["pdf"]["vision"]["attached_pages"]
        .as_array()
        .filter(|p| !p.is_empty())
    {
        let pages = pages
            .iter()
            .filter_map(|p| p.as_u64())
            .map(|p| p.to_string())
            .collect::<Vec<_>>()
            .join("、");
        view.note(&format!("已附页面图片供模型读取：第 {pages} 页"));
    }
    if let Some(next) = value["pdf"]["vision"]["next_image_start_page"].as_u64() {
        view.note(&format!("后续页面图片从第 {next} 页继续读取"));
    }
    if value["pdf"]["vision"]["status"].as_str() == Some("unavailable") {
        view.note("页面图片不可用：当前模型不支持视觉、图片能力关闭或渲染服务未配置");
    }
    if value["pdf"]["vision"]["status"].as_str() == Some("failed")
        || value["pdf"]["vision"]["failures"]
            .as_array()
            .is_some_and(|p| !p.is_empty())
    {
        view.note("部分页面图片未能生成，已保留文字结果");
    }
    if let Some(pages) = value["pdf"]["pages"].as_array() {
        let incomplete = pages
            .iter()
            .filter(|page| page["status"].as_str() != Some("text"))
            .filter_map(|page| page["page"].as_u64())
            .map(|page| page.to_string())
            .collect::<Vec<_>>();
        if !incomplete.is_empty() {
            view.note(&format!("需核对页面：{}", incomplete.join("、")));
        }
    }
    if let Some(pages) = value["pdf"]["pages_needing_ocr"]
        .as_array()
        .filter(|pages| !pages.is_empty())
    {
        view.note(&format!("此范围内有 {} 页需结合页面图片读取", pages.len()));
    }
    let mut out: Vec<_> = view
        .lines
        .drain(..)
        .map(LinkedLine::plain)
        .collect();
    let lines = markdown_preview(content, width);
    let total_lines = lines.len();
    let limit = if expanded { lines.len() } else { 6 };
    out.extend(lines.into_iter().take(limit));
    if content.is_empty() {
        view.note("(空文档)");
    } else if !expanded {
        view.omitted(total_lines.saturating_sub(limit), "行");
    }

    if truncated && let Some(next) = next_start {
        view.push_spans(vec![Span::styled(
            format!("… 尚有后续字符 (next_start_char = {next}, 可使用 ReadDocument 继续查看)"),
            Style::default().fg(theme.tool_running),
        )]);
    }

    if let Some(next) = value["next_start_page"].as_u64() {
        view.note(&format!("后续内容从第 {next} 页继续读取"));
    }

    out.extend(
        view.finish()
            .into_iter()
            .map(LinkedLine::plain),
    );
    Some(out)
}

pub(super) fn capability(row: &ToolRow, _expanded: bool, width: u16) -> Option<Vec<Line<'static>>> {
    let value: Value = serde_json::from_str(&row.output).ok()?;
    let status = value["status"]
        .as_str()
        .unwrap_or("unknown");
    let theme = &crate::tui::theme::THEME;
    let mut view = View::new(true, width);

    if status == "granted" {
        view.push_spans(vec![Span::styled(
            format!("{} 外部目录访问授权已授予 (Granted)", icons::CHECK),
            Style::default()
                .fg(theme.success)
                .add_modifier(Modifier::BOLD),
        )]);
        if let Some(cap) = value.get("capability") {
            let path = cap["path"].as_str().unwrap_or("");
            let access = cap["access"].as_str().unwrap_or("");
            let access_label = match access {
                "read_write" | "write" => "读写 (read_write)",
                "read" => "只读 (read)",
                other => other,
            };

            view.push(&format!("  路径: {path}"), theme.output_foreground);
            view.push(&format!("  模式: {access_label}"), theme.tool_detail);
        }
    } else {
        view.push_spans(vec![Span::styled(
            format!("{} 外部目录访问授权被拒绝 (Denied)", icons::CLOSE),
            Style::default()
                .fg(theme.error)
                .add_modifier(Modifier::BOLD),
        )]);
        if let Some(msg) = value["message"].as_str() {
            view.push(&format!("  原因: {msg}"), theme.output_muted);
        }
    }

    Some(view.finish())
}

pub(super) fn read_image(row: &ToolRow, _expanded: bool, width: u16) -> Option<Vec<Line<'static>>> {
    let value: Value = serde_json::from_str(&row.output).ok()?;
    let path = value["path"]
        .as_str()
        .unwrap_or_default();
    let theme = &crate::tui::theme::THEME;
    let mut view = View::new(true, width);

    view.push_spans(vec![
        Span::styled(
            format!("{} 图像已载入上下文 · ", icons::IMAGE_OUTLINE),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            path.to_owned(),
            Style::default().fg(theme.output_foreground),
        ),
    ]);

    Some(view.finish())
}

pub(super) fn fallback(row: &ToolRow, expanded: bool, width: u16) -> Option<Vec<Line<'static>>> {
    let value: Value = serde_json::from_str(&row.output).ok()?;

    // Preserve existing command, write receipt and text-content renderers.
    if !value.is_object() && !value.is_array()
        || value.get("stdout").is_some()
        || value.get("written_bytes").is_some()
        || value["content"].is_string()
    {
        return None;
    }

    let pretty = serde_json::to_string_pretty(&value).ok()?;
    let lines: Vec<_> = pretty.lines().collect();
    let limit = if expanded { lines.len() } else { 6 };

    let mut view = View::new(expanded, width);

    let mut renderer = Renderer::new("json");
    for line in lines.iter().take(limit) {
        view.source(&mut renderer, line);
    }

    view.omitted(lines.len().saturating_sub(limit), "行");

    Some(view.finish())
}
