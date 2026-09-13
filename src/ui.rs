use ratatui::style::Stylize;
use ratatui::{
    layout::{Constraint, Direction, Layout, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, List, ListItem, ListState, Padding, Paragraph, Wrap},
    Frame,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::{
    app::{format_duration, palette_items, relative_time},
    model::{
        AppState, ApprovalKind, BlockKind, ImageAttachment, Popup, TranscriptBlock,
        UserInputRequest,
    },
};

const DIM: Color = Color::DarkGray;
const ACCENT: Color = Color::Cyan;
const USER_BACKGROUND: Color = Color::Rgb(48, 48, 48);
const COMPOSER_BACKGROUND: Color = Color::Rgb(38, 38, 38);

pub fn draw(frame: &mut Frame, state: &mut AppState) {
    if state.resume_picker.is_some() {
        draw_resume_workspace(frame, state);
        return;
    }
    let area = frame.area();
    // The composer has two cells of horizontal padding on each side and a
    // two-cell prompt (`› `), so wrap against its real text width.
    let composer = layout_composer(
        &state.composer.text,
        state.composer.cursor,
        area.width.saturating_sub(6).max(1) as usize,
    );
    let composer_lines = composer.lines.len().clamp(1, 8) as u16;
    let attachment_rows = attachment_row_count(state.image_attachments.len());
    let composer_height =
        (composer_lines + attachment_rows + 4).min(area.height.saturating_sub(4).max(5));
    let panel_gap = u16::from(state.popup.is_some() && area.height > 4);
    let bottom_height = state
        .popup
        .as_ref()
        .map(|popup| bottom_panel_height(state, popup, area.width))
        .unwrap_or(composer_height)
        .min(area.height.saturating_sub(3 + panel_gap).max(1));
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(panel_gap),
            Constraint::Length(bottom_height),
        ])
        .split(area);

    draw_header(frame, state, chunks[0]);
    draw_transcript(frame, state, chunks[1]);
    if let Some(popup) = &state.popup {
        draw_bottom_panel(frame, state, popup.clone(), chunks[3]);
    } else {
        draw_composer(frame, state, &composer, chunks[3]);
    }
}

fn draw_resume_workspace(frame: &mut Frame, state: &AppState) {
    let area = frame.area();
    let panel_gap = u16::from(state.popup.is_some() && area.height > 4);
    let panel_height = state
        .popup
        .as_ref()
        .map(|popup| bottom_panel_height(state, popup, area.width))
        .unwrap_or(0)
        .min(area.height.saturating_sub(3 + panel_gap));
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(panel_gap),
            Constraint::Length(panel_height),
        ])
        .split(area);

    draw_header(frame, state, chunks[0]);
    draw_resume_picker(frame, state, chunks[1]);
    if let Some(popup) = &state.popup {
        draw_bottom_panel(frame, state, popup.clone(), chunks[3]);
    }
}

fn draw_resume_picker(frame: &mut Frame, state: &AppState, area: Rect) {
    let Some(picker) = state.resume_picker.as_ref() else {
        return;
    };
    let block = Block::default().padding(Padding::new(2, 2, 1, 1));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(inner);

    frame.render_widget(
        Paragraph::new(vec![
            Line::styled("Recent conversations", Style::default().bold()),
            Line::styled(state.cwd.clone(), Style::default().fg(DIM)),
        ]),
        chunks[0],
    );

    let entries = if picker.loading {
        vec![ListItem::new("Loading conversations…")]
    } else if let Some(error) = &picker.error {
        vec![ListItem::new(Line::styled(
            error.clone(),
            Style::default().fg(Color::Red),
        ))]
    } else if state.threads.is_empty() {
        vec![ListItem::new("No conversations found in this directory")]
    } else {
        let title_width = chunks[1].width.saturating_sub(16).max(12) as usize;
        state
            .threads
            .iter()
            .map(|thread| {
                ListItem::new(Line::from(vec![
                    Span::raw(truncate(&thread.title, title_width)),
                    Span::styled(
                        format!("  · {}", relative_time(thread.updated_at)),
                        Style::default().fg(DIM),
                    ),
                ]))
            })
            .collect()
    };
    let list = List::new(entries)
        .highlight_symbol("› ")
        .highlight_style(Style::default().fg(ACCENT).bold());
    let selection = (!picker.loading && picker.error.is_none() && !state.threads.is_empty())
        .then_some(picker.selected);
    let mut list_state = ListState::default().with_selected(selection);
    frame.render_stateful_widget(list, chunks[1], &mut list_state);

    frame.render_widget(
        Paragraph::new("j/k or ↑/↓ move · Enter resume · Ctrl+C quit")
            .style(Style::default().fg(DIM)),
        chunks[2],
    );
}

fn draw_header(frame: &mut Frame, state: &AppState, area: Rect) {
    let right = [
        state.model.as_deref().unwrap_or("default"),
        state.effort.as_deref().unwrap_or("default"),
        &state.collaboration_mode,
    ]
    .join(" · ");
    let right = if let Some(percent) = state.context_percent {
        format!("{right} · {percent}%")
    } else {
        right
    };
    let width = area.width.saturating_sub(2) as usize;
    let left_width = state.project.width();
    let right_width = right.width();
    let gap = width.saturating_sub(left_width + right_width).max(1);
    let line = Line::from(vec![
        Span::styled(format!(" {}", state.project), Style::default().bold()),
        Span::raw(" ".repeat(gap)),
        Span::styled(right, Style::default().fg(DIM)),
    ]);
    frame.render_widget(Paragraph::new(line), area);
    let separator = "─".repeat(area.width as usize);
    let sep_area = Rect::new(area.x, area.y + 1, area.width, 1);
    frame.render_widget(
        Paragraph::new(separator).style(Style::default().fg(DIM)),
        sep_area,
    );
}

fn draw_transcript(frame: &mut Frame, state: &mut AppState, area: Rect) {
    rebuild_transcript_cache(state, area.width);

    let mut tail = Vec::with_capacity(3);
    if let Some(started) = state.turn_started_at {
        tail.push(working_indicator(started));
        tail.push(Line::default());
    }
    if state.new_output {
        tail.push(Line::from(Span::styled(
            "  ↓ new output · End to follow",
            Style::default().fg(ACCENT),
        )));
    }
    let visual_height = state.transcript_cache_lines.len() + tail.len();
    let max_scroll = visual_height.saturating_sub(area.height as usize);
    state.transcript_max_scroll = max_scroll;
    if state.scroll == usize::MAX || state.scroll >= max_scroll {
        state.scroll = max_scroll;
        state.at_bottom = true;
        state.new_output = false;
    }
    let scroll = state.scroll;
    let visible_end = (scroll + area.height as usize).min(visual_height);
    let lines = (scroll..visible_end)
        .map(|index| {
            if index < state.transcript_cache_lines.len() {
                state.transcript_cache_lines[index].clone()
            } else {
                tail[index - state.transcript_cache_lines.len()].clone()
            }
        })
        .collect::<Vec<_>>();

    for (viewport_row, line) in lines.iter().enumerate() {
        if line.style.bg != Some(USER_BACKGROUND) {
            continue;
        }
        frame.render_widget(
            Block::default().style(Style::default().bg(USER_BACKGROUND)),
            Rect::new(
                area.x,
                area.y.saturating_add(viewport_row as u16),
                area.width,
                1,
            ),
        );
    }
    frame.render_widget(Paragraph::new(Text::from(lines)), area);
}

fn rebuild_transcript_cache(state: &mut AppState, width: u16) {
    if state.transcript_cache_width == width
        && state.transcript_cache_revision == state.transcript_revision
    {
        return;
    }

    let content_width = width.saturating_sub(4).max(1);
    let render_width = width.max(1);
    let mut lines = Vec::new();
    let mut in_activity = false;
    for block in &state.blocks {
        if matches!(
            block.kind,
            BlockKind::Reasoning | BlockKind::Assistant | BlockKind::Commentary
        ) && block.text.trim().is_empty()
        {
            continue;
        }
        let activity = matches!(
            block.kind,
            BlockKind::Commentary
                | BlockKind::Reasoning
                | BlockKind::Command
                | BlockKind::File
                | BlockKind::Web
                | BlockKind::Error
        );
        if activity && !in_activity {
            lines.push(section_separator(render_width as usize));
            lines.push(Line::default());
            in_activity = true;
        } else if !activity && in_activity {
            lines.push(section_separator(render_width as usize));
            lines.push(Line::default());
            in_activity = false;
        }
        append_block(
            &mut lines,
            block,
            content_width as usize,
            render_width as usize,
        );
    }
    if in_activity {
        lines.push(section_separator(render_width as usize));
        lines.push(Line::default());
    }
    state.transcript_cache_width = width;
    state.transcript_cache_revision = state.transcript_revision;
    state.transcript_cache_lines = lines;
}

fn append_block(
    lines: &mut Vec<Line<'static>>,
    block: &TranscriptBlock,
    width: usize,
    render_width: usize,
) {
    match block.kind {
        BlockKind::User => {
            let start = lines.len();
            lines.push(Line::default());
            append_markdown(lines, &block.text, "  ", Style::default(), width);
            lines.push(Line::default());
            apply_background_band(&mut lines[start..], render_width, USER_BACKGROUND);
        }
        BlockKind::Assistant => {
            append_markdown(lines, &block.text, "  ", Style::default(), width);
        }
        BlockKind::Commentary => {
            append_markdown(lines, &block.text, "  ", Style::default().fg(DIM), width);
        }
        BlockKind::Reasoning => {
            lines.push(Line::from(Span::styled(
                "  Thinking",
                Style::default().fg(DIM).italic(),
            )));
            for line in block.text.lines() {
                push_wrapped_line(
                    lines,
                    vec![Span::styled(
                        format!("    {line}"),
                        Style::default().fg(DIM).italic(),
                    )],
                    width,
                );
            }
        }
        BlockKind::Command => append_command(lines, block, width),
        BlockKind::File => {
            lines.push(Line::from(Span::styled(
                "  Files",
                Style::default().fg(Color::Magenta).bold(),
            )));
            for line in block.text.lines() {
                let style = match line.chars().next() {
                    Some('+') => Style::default().fg(Color::Green),
                    Some('-') => Style::default().fg(Color::Red),
                    _ => Style::default().fg(Color::Magenta),
                };
                push_wrapped_line(
                    lines,
                    vec![Span::styled(format!("    {line}"), style)],
                    width,
                );
            }
        }
        BlockKind::Web => {
            lines.push(Line::from(Span::styled(
                "  Web search",
                Style::default().fg(Color::Blue).bold(),
            )));
            for line in block.text.lines() {
                push_wrapped_line(lines, vec![Span::raw(format!("    {line}"))], width);
            }
        }
        BlockKind::Error => {
            lines.push(Line::from(Span::styled(
                "  Error",
                Style::default().fg(Color::Red).bold(),
            )));
            for line in block.text.lines() {
                push_wrapped_line(
                    lines,
                    vec![Span::styled(
                        format!("    {line}"),
                        Style::default().fg(Color::Red),
                    )],
                    width,
                );
            }
        }
        BlockKind::Status => {
            push_wrapped_line(
                lines,
                vec![
                    Span::styled("  · ", Style::default().fg(DIM)),
                    Span::styled(block.text.clone(), Style::default().fg(DIM)),
                ],
                width,
            );
        }
        BlockKind::TurnEnd => {
            let label = format!("─ Worked for {} ", block.text);
            let fill = "─".repeat(render_width.saturating_sub(label.width()));
            lines.push(Line::from(Span::styled(
                format!("{label}{fill}"),
                Style::default().fg(Color::Gray),
            )));
        }
    }
    lines.push(Line::default());
}

fn apply_background_band(lines: &mut [Line<'static>], width: usize, background: Color) {
    for line in lines {
        let padding = width.saturating_sub(line.width());
        line.style = line.style.patch(Style::default().bg(background));
        if padding > 0 {
            line.spans.push(Span::styled(
                " ".repeat(padding),
                Style::default().bg(background),
            ));
        }
    }
}

fn section_separator(width: usize) -> Line<'static> {
    Line::from(Span::styled(
        "─".repeat(width),
        Style::default().fg(Color::Gray),
    ))
}

fn working_indicator(started: std::time::Instant) -> Line<'static> {
    let elapsed = started.elapsed();
    let word = "Working";
    let phase = (elapsed.as_millis() / 90) as isize % (word.chars().count() as isize + 6) - 3;
    let mut spans = vec![Span::raw("  ")];
    for (index, ch) in word.chars().enumerate() {
        let color = match (index as isize - phase).abs() {
            0 => Color::Rgb(220, 245, 240),
            1 => Color::Rgb(155, 205, 196),
            2 => Color::Rgb(105, 150, 143),
            _ => Color::Rgb(92, 102, 102),
        };
        spans.push(Span::styled(ch.to_string(), Style::default().fg(color)));
    }
    spans.push(Span::styled(
        format!(
            " ({} • esc to interrupt)",
            format_duration(elapsed.as_millis().min(u128::from(u64::MAX)) as u64)
        ),
        Style::default().fg(DIM),
    ));
    Line::from(spans)
}

fn append_command(lines: &mut Vec<Line<'static>>, block: &TranscriptBlock, width: usize) {
    let color = Color::Yellow;
    let command = block.title.strip_prefix("$ ").unwrap_or(&block.title);
    let mut output = block.text.lines();
    let status = output.next().unwrap_or("running…");
    let output = output.collect::<Vec<_>>();
    lines.push(Line::from(Span::styled(
        "  Shell",
        Style::default().fg(color).bold(),
    )));
    push_wrapped_line(
        lines,
        vec![
            Span::styled("    $ ", Style::default().fg(color)),
            Span::styled(command.to_string(), Style::default().fg(color)),
        ],
        width,
    );
    if !block.expanded && output.len() > 12 {
        lines.push(Line::from(vec![
            Span::raw("    "),
            Span::styled(
                format!("{} lines · {status} · Ctrl+O to expand", output.len()),
                Style::default().fg(DIM),
            ),
        ]));
        return;
    }
    for line in output {
        push_wrapped_line(
            lines,
            vec![Span::styled(
                format!("    {line}"),
                Style::default().fg(Color::Gray),
            )],
            width,
        );
    }
    lines.push(Line::from(vec![
        Span::raw("    "),
        Span::styled(status.to_string(), Style::default().fg(DIM)),
    ]));
}

fn append_markdown(
    lines: &mut Vec<Line<'static>>,
    source: &str,
    indent: &str,
    base_style: Style,
    width: usize,
) {
    let mut in_code = false;
    for raw in source.lines() {
        if raw.trim_start().starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code {
            push_wrapped_line(
                lines,
                vec![Span::styled(
                    format!("{indent}  {raw}"),
                    base_style.fg(Color::Yellow),
                )],
                width,
            );
            continue;
        }
        let (prefix, content, base) = if let Some(rest) = raw.strip_prefix("### ") {
            ("", rest, base_style.bold())
        } else if let Some(rest) = raw.strip_prefix("## ") {
            ("", rest, base_style.bold())
        } else if let Some(rest) = raw.strip_prefix("# ") {
            ("", rest, base_style.bold().underlined())
        } else if let Some(rest) = raw.strip_prefix("- ") {
            ("  • ", rest, base_style)
        } else if let Some(rest) = raw.strip_prefix("* ") {
            ("  • ", rest, base_style)
        } else {
            ("", raw, base_style)
        };
        let mut spans = vec![Span::styled(format!("{indent}{prefix}"), base)];
        spans.extend(inline_spans(content, base));
        push_wrapped_line(lines, spans, width);
    }
}

/// Wrap styled text before Ratatui sees it, without box-drawing decorations.
fn push_wrapped_line(lines: &mut Vec<Line<'static>>, spans: Vec<Span<'static>>, width: usize) {
    let mut indent = Vec::new();
    let mut content = Vec::new();
    let mut reading_indent = true;

    for span in spans {
        let style = span.style;
        let text = span.content.into_owned();
        if reading_indent {
            let content_start = text
                .char_indices()
                .find_map(|(index, ch)| (!ch.is_whitespace()).then_some(index));
            match content_start {
                Some(index) => {
                    if index > 0 {
                        indent.push(Span::styled(text[..index].to_string(), style));
                    }
                    content.push(Span::styled(text[index..].to_string(), style));
                    reading_indent = false;
                }
                None => indent.push(Span::styled(text, style)),
            }
        } else {
            content.push(Span::styled(text, style));
        }
    }

    let available = width.saturating_sub(indent.iter().map(Span::width).sum::<usize>());
    for chunk in wrap_styled_spans(content, available.max(1)) {
        let mut row = indent.clone();
        row.extend(chunk);
        lines.push(Line::from(row));
    }
}

fn wrap_styled_spans(spans: Vec<Span<'static>>, width: usize) -> Vec<Vec<Span<'static>>> {
    let width = width.max(1);
    let chars = spans
        .into_iter()
        .flat_map(|span| {
            let style = span.style;
            span.content
                .into_owned()
                .chars()
                .map(move |ch| (ch, style))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();

    if chars.is_empty() {
        return vec![Vec::new()];
    }

    let mut wrapped = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let mut used = 0;
        let mut end = start;
        let mut last_space = None;
        while end < chars.len() {
            let char_width = chars[end].0.width().unwrap_or(0);
            if end > start && used + char_width > width {
                break;
            }
            used += char_width;
            if chars[end].0.is_whitespace() {
                last_space = Some(end);
            }
            end += 1;
            if used >= width {
                break;
            }
        }

        let mut next = end;
        if end < chars.len()
            && !chars[end].0.is_whitespace()
            && !chars[end.saturating_sub(1)].0.is_whitespace()
        {
            if let Some(space) = last_space.filter(|space| *space > start) {
                end = space;
                next = space + 1;
            }
        }
        while end > start && chars[end - 1].0.is_whitespace() {
            end -= 1;
        }
        while next < chars.len() && chars[next].0.is_whitespace() {
            next += 1;
        }
        if end == start {
            end = (start + 1).min(chars.len());
            next = end;
        }

        let mut chunk: Vec<Span<'static>> = Vec::new();
        for (ch, style) in &chars[start..end] {
            if let Some(previous) = chunk.last_mut().filter(|span| span.style == *style) {
                previous.content.to_mut().push(*ch);
            } else {
                chunk.push(Span::styled(ch.to_string(), *style));
            }
        }
        wrapped.push(chunk);
        start = next;
    }
    wrapped
}

fn inline_spans(input: &str, base: Style) -> Vec<Span<'static>> {
    #[derive(Clone, Copy)]
    enum Marker {
        Bold,
        Code,
        Link,
    }

    let mut spans = Vec::new();
    let mut rest = input;
    while !rest.is_empty() {
        let next = [
            rest.find("**").map(|index| (index, Marker::Bold)),
            rest.find('`').map(|index| (index, Marker::Code)),
            rest.find('[').map(|index| (index, Marker::Link)),
        ]
        .into_iter()
        .flatten()
        .min_by_key(|(index, _)| *index);
        let Some((index, marker)) = next else {
            spans.push(Span::styled(rest.to_string(), base));
            break;
        };
        if index > 0 {
            spans.push(Span::styled(rest[..index].to_string(), base));
        }
        match marker {
            Marker::Bold | Marker::Code => {
                let delimiter = if matches!(marker, Marker::Bold) {
                    "**"
                } else {
                    "`"
                };
                let after = &rest[index + delimiter.len()..];
                if let Some(end) = after.find(delimiter) {
                    let style = if matches!(marker, Marker::Bold) {
                        base.add_modifier(Modifier::BOLD)
                    } else {
                        base.fg(Color::Yellow)
                    };
                    spans.push(Span::styled(after[..end].to_string(), style));
                    rest = &after[end + delimiter.len()..];
                } else {
                    spans.push(Span::styled(rest[index..].to_string(), base));
                    break;
                }
            }
            Marker::Link => {
                let after_open = &rest[index + 1..];
                if let Some(label_end) = after_open.find("](") {
                    let after_url_open = &after_open[label_end + 2..];
                    if let Some(url_end) = after_url_open.find(')') {
                        spans.push(Span::styled(
                            after_open[..label_end].to_string(),
                            base.fg(Color::Blue).underlined(),
                        ));
                        rest = &after_url_open[url_end + 1..];
                        continue;
                    }
                }
                spans.push(Span::styled("[", base));
                rest = after_open;
            }
        }
    }
    spans
}

#[derive(Debug, PartialEq, Eq)]
struct ComposerLayout {
    lines: Vec<String>,
    cursor_row: usize,
    cursor_col: usize,
}

fn layout_composer(text: &str, cursor: usize, width: usize) -> ComposerLayout {
    let width = width.max(1);
    let mut lines = vec![String::new()];
    let mut row = 0;
    let mut col = 0;
    let mut cursor_position = None;

    let mut previous = None;
    for (index, ch) in text.char_indices() {
        let char_width = ch.width().unwrap_or(0);
        let word_start = ch != '\n'
            && !ch.is_whitespace()
            && previous.is_none_or(|previous: char| previous.is_whitespace());
        if word_start {
            let word_width = text[index..]
                .chars()
                .take_while(|candidate| !candidate.is_whitespace())
                .map(|candidate| candidate.width().unwrap_or(0))
                .sum::<usize>();
            if col > 0 && col + word_width > width {
                row += 1;
                col = 0;
                lines.push(String::new());
            }
        }
        if ch != '\n' && col > 0 && col + char_width > width {
            row += 1;
            col = 0;
            lines.push(String::new());
            if ch.is_whitespace() {
                if index == cursor {
                    cursor_position = Some((row, col));
                }
                previous = Some(ch);
                continue;
            }
        }
        if index == cursor {
            cursor_position = Some((row, col));
        }
        if ch == '\n' {
            row += 1;
            col = 0;
            lines.push(String::new());
        } else {
            lines[row].push(ch);
            col += char_width;
            // Keep the insertion cursor inside the composer when a line is
            // exactly full. The next character will continue on this row.
            if col == width && index + ch.len_utf8() == cursor {
                row += 1;
                col = 0;
                lines.push(String::new());
                cursor_position = Some((row, col));
            }
        }
        previous = Some(ch);
    }
    if cursor == text.len() && cursor_position.is_none() {
        if col >= width {
            row += 1;
            col = 0;
            lines.push(String::new());
        }
        cursor_position = Some((row, col));
    }
    let (cursor_row, cursor_col) = cursor_position.unwrap_or((row, col));
    ComposerLayout {
        lines,
        cursor_row,
        cursor_col,
    }
}

fn draw_composer(frame: &mut Frame, state: &AppState, layout: &ComposerLayout, area: Rect) {
    let block = Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_style(Style::default().fg(DIM))
        .padding(Padding::new(2, 2, 1, 1))
        .style(Style::default().bg(COMPOSER_BACKGROUND));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let attachments = attachment_labels(&state.image_attachments);
    let attachment_height = (attachments.len() as u16).min(inner.height);
    if attachment_height > 0 {
        let lines = attachments
            .into_iter()
            .map(|label| Line::styled(label, Style::default().fg(DIM)))
            .collect::<Vec<_>>();
        frame.render_widget(
            Paragraph::new(lines),
            Rect::new(inner.x, inner.y, inner.width, attachment_height),
        );
    }
    let input_area = Rect::new(
        inner.x,
        inner.y.saturating_add(attachment_height),
        inner.width,
        inner.height.saturating_sub(attachment_height),
    );

    let visible_lines = input_area.height.max(1) as usize;
    let vertical_scroll = layout
        .cursor_row
        .saturating_add(1)
        .saturating_sub(visible_lines)
        .min(layout.lines.len().saturating_sub(visible_lines));

    let content = if state.composer.text.is_empty() {
        Text::from(Line::from(vec![
            Span::styled("› ", Style::default().fg(ACCENT).bold()),
            Span::styled("Ask Codex…", Style::default().fg(DIM)),
        ]))
    } else {
        let lines = layout
            .lines
            .iter()
            .enumerate()
            .map(|(index, line)| {
                Line::from(vec![
                    Span::styled(
                        if index == 0 { "› " } else { "  " },
                        Style::default().fg(ACCENT).bold(),
                    ),
                    Span::raw(line.clone()),
                ])
            })
            .collect::<Vec<_>>();
        Text::from(lines)
    };
    frame.render_widget(
        Paragraph::new(content).scroll((vertical_scroll as u16, 0)),
        input_area,
    );
    if state.popup.is_none() && input_area.height > 0 {
        let x = input_area.x.saturating_add(2).saturating_add(
            layout
                .cursor_col
                .min(input_area.width.saturating_sub(3) as usize) as u16,
        );
        let y = input_area
            .y
            .saturating_add(layout.cursor_row.saturating_sub(vertical_scroll) as u16);
        frame.set_cursor_position(Position::new(x, y));
    }
}

fn attachment_row_count(count: usize) -> u16 {
    count.min(3) as u16
}

fn attachment_labels(images: &[ImageAttachment]) -> Vec<String> {
    if images.len() <= 3 {
        return images
            .iter()
            .enumerate()
            .map(|(index, image)| format!("[Image {}] {}x{}", index + 1, image.width, image.height))
            .collect();
    }
    let mut labels = images[..2]
        .iter()
        .enumerate()
        .map(|(index, image)| format!("[Image {}] {}x{}", index + 1, image.width, image.height))
        .collect::<Vec<_>>();
    labels.push(format!("[Images 3–{}]", images.len()));
    labels
}

fn bottom_panel_height(state: &AppState, popup: &Popup, width: u16) -> u16 {
    let list_height = |len: usize, max: usize| len.clamp(1, max) as u16 + 3;
    match popup {
        Popup::Palette { .. } => list_height(palette_items().len(), 8),
        Popup::Models { .. } => list_height(state.models.len(), 12),
        Popup::CollaborationModes { .. } => list_height(state.collaboration_modes.len(), 8),
        Popup::Reasoning { .. } => {
            let len = state
                .model
                .as_ref()
                .and_then(|id| state.models.iter().find(|model| &model.id == id))
                .map(|model| model.efforts.len())
                .unwrap_or(0);
            list_height(len, 8)
        }
        Popup::Resume { loading, .. } => {
            list_height(if *loading { 1 } else { state.threads.len() }, 14)
        }
        Popup::Login { url, error } => {
            if url.is_none() && error.is_none() {
                7
            } else {
                let text = account_text(url.as_deref(), error.as_deref());
                text_panel_height(&text, width)
            }
        }
        Popup::Approval(approval) => {
            let option_count = approval_options(&approval.kind).len();
            let detail_lines = approval
                .detail
                .lines()
                .map(|line| visual_line_count(line, width.saturating_sub(6)))
                .sum::<usize>();
            (detail_lines as u16 + option_count as u16 + 4).clamp(8, 18)
        }
        Popup::UserInput(request) => {
            let Some(question) = request.current_question() else {
                return 7;
            };
            let question_lines = visual_line_count(&question.question, width) as u16;
            let body_lines = if request.is_editing() {
                3
            } else {
                (question.options.len() + usize::from(question.allow_other)) as u16
            };
            (question_lines + body_lines + 5).clamp(8, 18)
        }
        Popup::Disconnected { reason, .. } => {
            (visual_line_count(reason, width) as u16 + 6).clamp(7, 14)
        }
    }
}

fn text_panel_height(text: &str, width: u16) -> u16 {
    (visual_line_count(text, width) as u16 + 3).clamp(5, 14)
}

fn visual_line_count(text: &str, width: u16) -> usize {
    let content_width = width.saturating_sub(4).max(1) as usize;
    text.lines().fold(0usize, |total, line| {
        total + line.width().max(1).div_ceil(content_width)
    })
}

fn draw_bottom_panel(frame: &mut Frame, state: &AppState, popup: Popup, area: Rect) {
    match popup {
        Popup::Palette { selected } => draw_list_panel(
            frame,
            "Command",
            palette_items()
                .iter()
                .map(|item| item.to_string())
                .collect(),
            selected,
            area,
        ),
        Popup::Models { selected } => draw_list_panel(
            frame,
            "Model",
            state
                .models
                .iter()
                .map(|model| {
                    if model.name == model.id {
                        model.id.clone()
                    } else {
                        format!("{}  {}", model.name, model.id)
                    }
                })
                .collect(),
            selected,
            area,
        ),
        Popup::CollaborationModes { selected } => draw_list_panel(
            frame,
            "Mode",
            state
                .collaboration_modes
                .iter()
                .map(|mode| mode.name.clone())
                .collect(),
            selected,
            area,
        ),
        Popup::Reasoning { selected } => {
            let efforts = state
                .model
                .as_ref()
                .and_then(|id| state.models.iter().find(|model| &model.id == id))
                .map(|model| model.efforts.clone())
                .unwrap_or_default();
            draw_list_panel(frame, "Reasoning", efforts, selected, area)
        }
        Popup::Resume { selected, loading } => {
            let entries = if loading {
                vec!["Loading conversations…".to_string()]
            } else if state.threads.is_empty() {
                vec!["No conversations found".to_string()]
            } else {
                state
                    .threads
                    .iter()
                    .map(|thread| {
                        let project = std::path::Path::new(&thread.cwd)
                            .file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or(&thread.cwd);
                        format!(
                            "{}  · {}  · {}",
                            truncate(&thread.title, 42),
                            project,
                            relative_time(thread.updated_at)
                        )
                    })
                    .collect()
            };
            draw_list_panel(frame, "Resume", entries, selected, area);
        }
        Popup::Login { url, error } => {
            if url.is_none() && error.is_none() {
                draw_action_panel(
                    frame,
                    "Account",
                    "Not signed in",
                    &["Login with ChatGPT"],
                    0,
                    0,
                    area,
                );
            } else {
                draw_text_panel(
                    frame,
                    "Account",
                    account_text(url.as_deref(), error.as_deref()),
                    area,
                );
            }
        }
        Popup::Approval(approval) => {
            draw_approval_panel(frame, &approval, area);
        }
        Popup::UserInput(request) => draw_user_input_panel(frame, &request, area),
        Popup::Disconnected { reason, selected } => {
            draw_action_panel(
                frame,
                "Disconnected",
                &format!("Codex backend disconnected.\n\n{reason}"),
                &["Restart", "Quit"],
                selected,
                0,
                area,
            );
        }
    }
}

fn account_text(url: Option<&str>, error: Option<&str>) -> String {
    if let Some(error) = error {
        format!("Not signed in\n\n{error}")
    } else if let Some(url) = url {
        format!("Complete sign-in in your browser.\n\nIf it did not open:\n{url}")
    } else {
        "Not signed in".to_string()
    }
}

fn approval_options(kind: &ApprovalKind) -> &'static [&'static str] {
    if matches!(kind, ApprovalKind::Unsupported) {
        &["Decline"]
    } else {
        &[
            "Allow once",
            "Allow for this session",
            "Deny",
            "Deny and cancel turn",
        ]
    }
}

fn panel_block(title: &str, bottom_padding: u16) -> Block<'_> {
    Block::default()
        .title(format!(" {title} "))
        .borders(Borders::TOP)
        .border_style(Style::default().fg(DIM))
        .padding(Padding::new(2, 2, 1, bottom_padding))
        .style(Style::default().bg(COMPOSER_BACKGROUND))
}

fn draw_list_panel(
    frame: &mut Frame,
    title: &str,
    items: Vec<String>,
    selected: usize,
    area: Rect,
) {
    let block = panel_block(title, 0);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1)])
        .split(inner);
    let entries = items
        .into_iter()
        .map(ListItem::new)
        .collect::<Vec<ListItem<'_>>>();
    let list = List::new(entries)
        .highlight_symbol("› ")
        .highlight_style(Style::default().fg(ACCENT).bold());
    let mut list_state = ListState::default().with_selected(Some(selected));
    frame.render_stateful_widget(list, chunks[0], &mut list_state);
}

fn draw_text_panel(frame: &mut Frame, title: &str, text: String, area: Rect) {
    frame.render_widget(
        Paragraph::new(text)
            .block(panel_block(title, 0))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn draw_approval_panel(frame: &mut Frame, approval: &crate::model::Approval, area: Rect) {
    let options = approval_options(&approval.kind);
    let block = panel_block(&approval.title, 1);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(options.len() as u16),
        ])
        .split(inner);

    let command = matches!(approval.kind, ApprovalKind::Command)
        || matches!(approval.kind, ApprovalKind::Legacy) && approval.title == "Run command?";
    if command {
        let lines = approval
            .detail
            .lines()
            .map(|line| {
                Line::from(vec![
                    Span::styled("$ ", Style::default().fg(Color::Yellow).bold()),
                    Span::styled(line.to_string(), Style::default().fg(Color::Yellow)),
                ])
            })
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), chunks[0]);
    } else {
        frame.render_widget(
            Paragraph::new(approval.detail.clone()).wrap(Wrap { trim: false }),
            chunks[0],
        );
    }

    frame.render_widget(
        Paragraph::new("─".repeat(chunks[1].width as usize)).style(Style::default().fg(DIM)),
        chunks[1],
    );
    let entries = options
        .iter()
        .map(|option| ListItem::new(*option))
        .collect::<Vec<_>>();
    let list = List::new(entries)
        .highlight_symbol("› ")
        .highlight_style(Style::default().fg(ACCENT).bold());
    let mut list_state = ListState::default().with_selected(Some(approval.selected));
    frame.render_stateful_widget(list, chunks[2], &mut list_state);
}

fn draw_user_input_panel(frame: &mut Frame, request: &UserInputRequest, area: Rect) {
    let Some(question) = request.current_question() else {
        return;
    };
    let title = format!(
        "{} · {}/{}",
        question.header,
        request.current + 1,
        request.questions.len()
    );
    let block = panel_block(&title, 0);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if request.is_editing() {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(1),
                Constraint::Length(3),
                Constraint::Length(1),
            ])
            .split(inner);
        frame.render_widget(
            Paragraph::new(question.question.clone()).wrap(Wrap { trim: false }),
            chunks[0],
        );

        let (display, cursor) = if question.secret {
            let before_cursor = request.input.text[..request.input.cursor].chars().count();
            (
                "•".repeat(request.input.text.chars().count()),
                before_cursor * '•'.len_utf8(),
            )
        } else {
            (request.input.text.clone(), request.input.cursor)
        };
        let input_width = chunks[1].width.saturating_sub(2).max(1) as usize;
        let layout = layout_composer(&display, cursor, input_width);
        let visible_lines = chunks[1].height.max(1) as usize;
        let vertical_scroll = layout
            .cursor_row
            .saturating_add(1)
            .saturating_sub(visible_lines)
            .min(layout.lines.len().saturating_sub(visible_lines));
        let lines = layout
            .lines
            .iter()
            .enumerate()
            .map(|(index, line)| {
                Line::from(vec![
                    Span::styled(
                        if index == 0 { "› " } else { "  " },
                        Style::default().fg(ACCENT).bold(),
                    ),
                    if line.is_empty() && request.input.text.is_empty() {
                        Span::styled("Type an answer…", Style::default().fg(DIM))
                    } else {
                        Span::raw(line.clone())
                    },
                ])
            })
            .collect::<Vec<_>>();
        frame.render_widget(
            Paragraph::new(Text::from(lines)).scroll((vertical_scroll as u16, 0)),
            chunks[1],
        );
        let cursor_x = chunks[1]
            .x
            .saturating_add(2)
            .saturating_add(layout.cursor_col.min(input_width) as u16);
        let cursor_y = chunks[1]
            .y
            .saturating_add(layout.cursor_row.saturating_sub(vertical_scroll) as u16);
        frame.set_cursor_position(Position::new(cursor_x, cursor_y));
        let hint = if request.entering_other {
            "Enter answer · Alt+Enter newline · Esc back"
        } else {
            "Enter answer · Alt+Enter newline · Esc cancel"
        };
        frame.render_widget(
            Paragraph::new(hint).style(Style::default().fg(DIM)),
            chunks[2],
        );
        return;
    }

    let option_count = question.options.len() + usize::from(question.allow_other);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(option_count as u16),
            Constraint::Length(1),
        ])
        .split(inner);
    frame.render_widget(
        Paragraph::new(question.question.clone()).wrap(Wrap { trim: false }),
        chunks[0],
    );
    let mut entries = question
        .options
        .iter()
        .map(|option| ListItem::new(format!("{}  {}", option.label, option.description)))
        .collect::<Vec<_>>();
    if question.allow_other {
        entries.push(ListItem::new("Other  Type a custom answer"));
    }
    let list = List::new(entries)
        .highlight_symbol("› ")
        .highlight_style(Style::default().fg(ACCENT).bold());
    let mut list_state = ListState::default().with_selected(Some(request.selected));
    frame.render_stateful_widget(list, chunks[1], &mut list_state);
    frame.render_widget(
        Paragraph::new("↑/↓ or j/k choose · Enter answer · Esc cancel")
            .style(Style::default().fg(DIM)),
        chunks[2],
    );
}

fn draw_action_panel(
    frame: &mut Frame,
    title: &str,
    detail: &str,
    options: &[&str],
    selected: usize,
    bottom_padding: u16,
    area: Rect,
) {
    let block = panel_block(title, bottom_padding);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(options.len() as u16)])
        .split(inner);
    frame.render_widget(
        Paragraph::new(detail.to_string()).wrap(Wrap { trim: false }),
        chunks[0],
    );
    let entries = options
        .iter()
        .map(|option| ListItem::new(*option))
        .collect::<Vec<_>>();
    let list = List::new(entries)
        .highlight_symbol("› ")
        .highlight_style(Style::default().fg(ACCENT).bold());
    let mut list_state = ListState::default().with_selected(Some(selected));
    frame.render_stateful_widget(list, chunks[1], &mut list_state);
}

fn truncate(value: &str, max_width: usize) -> String {
    if value.width() <= max_width {
        return value.to_string();
    }
    let mut output = String::new();
    let mut width = 0;
    for ch in value.chars() {
        let next = ch.width().unwrap_or(0);
        if width + next + 1 > max_width {
            output.push('…');
            break;
        }
        width += next;
        output.push(ch);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_by_terminal_width() {
        assert_eq!(truncate("hello", 8), "hello");
        assert_eq!(truncate("hello world", 6), "hello…");
    }

    #[test]
    fn markdown_keeps_unknown_text() {
        let spans = inline_spans("a **bold** and `code`", Style::default());
        assert_eq!(
            spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>(),
            "a bold and code"
        );
    }

    #[test]
    fn markdown_links_show_label_without_noisy_target() {
        let spans = inline_spans(
            "read [README.md](/home/user/project/README.md)",
            Style::default(),
        );
        assert_eq!(
            spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>(),
            "read README.md"
        );
    }

    #[test]
    fn styled_text_wraps_on_words_without_losing_styles() {
        let wrapped = wrap_styled_spans(
            vec![
                Span::raw("one "),
                Span::styled("two three", Style::default().bold()),
            ],
            7,
        );
        let rows = wrapped
            .iter()
            .map(|row| {
                row.iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        assert_eq!(rows, ["one two", "three"]);
        assert!(wrapped[0]
            .last()
            .is_some_and(|span| span.style.add_modifier.contains(Modifier::BOLD)));
    }

    #[test]
    fn wrapped_lines_repeat_their_leading_indent() {
        let mut lines = Vec::new();
        push_wrapped_line(&mut lines, vec![Span::raw("  one two three")], 9);
        let rows = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        assert_eq!(rows, ["  one two", "  three"]);
    }

    #[test]
    fn conversation_uses_full_gray_user_band_and_plain_roles() {
        let mut lines = Vec::new();
        append_block(
            &mut lines,
            &TranscriptBlock::new(BlockKind::User, "You", "question"),
            76,
            80,
        );
        append_block(
            &mut lines,
            &TranscriptBlock::new(BlockKind::Assistant, "Codex", "answer"),
            76,
            80,
        );
        append_block(
            &mut lines,
            &TranscriptBlock::new(BlockKind::TurnEnd, "", "9m 55s"),
            76,
            80,
        );

        assert_eq!(lines[0].width(), 80);
        assert_eq!(lines[0].style.bg, Some(Color::Rgb(48, 48, 48)));
        let rendered = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(!rendered.contains("You"));
        assert!(!rendered.contains("Codex"));
        assert!(rendered.contains("Worked for 9m 55s"));
    }

    #[test]
    fn user_background_paints_the_last_terminal_cell() {
        let backend = ratatui::backend::TestBackend::new(80, 8);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let mut state = AppState::new("/project".into(), true);
        state.blocks = vec![TranscriptBlock::new(BlockKind::User, "You", "question")];
        terminal
            .draw(|frame| draw_transcript(frame, &mut state, frame.area()))
            .unwrap();

        let buffer = terminal.backend().buffer();
        for row in 0..3 {
            assert_eq!(buffer[(0, row)].bg, USER_BACKGROUND);
            assert_eq!(buffer[(79, row)].bg, USER_BACKGROUND);
        }
    }

    #[test]
    fn transcript_scroll_is_not_limited_to_u16_rows() {
        let backend = ratatui::backend::TestBackend::new(20, 3);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let mut state = AppState::new("/project".into(), true);
        let mut rows = vec!["filler"; 69_999];
        rows.push("target");
        state.blocks = vec![TranscriptBlock::new(
            BlockKind::Assistant,
            "Codex",
            rows.join("\n"),
        )];
        state.scroll = usize::MAX;

        terminal
            .draw(|frame| draw_transcript(frame, &mut state, frame.area()))
            .unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(state.transcript_max_scroll > u16::MAX as usize);
        assert!(rendered.contains("target"));
    }

    #[test]
    fn composer_is_a_padded_full_width_panel() {
        let backend = ratatui::backend::TestBackend::new(40, 5);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let mut state = AppState::new("/project".into(), true);
        state.composer.text = "hello".into();
        state.composer.cursor = state.composer.text.len();
        let layout = layout_composer(&state.composer.text, state.composer.cursor, 34);
        terminal
            .draw(|frame| draw_composer(frame, &state, &layout, frame.area()))
            .unwrap();

        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0, 2)].bg, COMPOSER_BACKGROUND);
        assert_eq!(buffer[(39, 2)].bg, COMPOSER_BACKGROUND);
        assert_eq!(buffer[(2, 2)].symbol(), "›");
        assert_eq!(buffer[(4, 2)].symbol(), "h");
    }

    #[test]
    fn composer_shows_image_attachments_above_the_prompt() {
        let backend = ratatui::backend::TestBackend::new(48, 7);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let mut state = AppState::new("/project".into(), true);
        state.image_attachments = vec![ImageAttachment {
            data_url: "data:image/png;base64,test".into(),
            width: 640,
            height: 480,
        }];
        let layout = layout_composer("", 0, 42);

        terminal
            .draw(|frame| draw_composer(frame, &state, &layout, frame.area()))
            .unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("[Image 1] 640x480"));
        assert!(rendered.contains("Ask Codex"));
    }

    #[test]
    fn transcript_uses_the_full_terminal_width() {
        let backend = ratatui::backend::TestBackend::new(180, 4);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let mut state = AppState::new("/project".into(), true);
        state.blocks = vec![TranscriptBlock::new(
            BlockKind::Assistant,
            "Codex",
            "x".repeat(150),
        )];

        terminal
            .draw(|frame| draw_transcript(frame, &mut state, frame.area()))
            .unwrap();

        assert_eq!(terminal.backend().buffer()[(130, 0)].symbol(), "x");
    }

    #[test]
    fn resume_picker_replaces_the_transcript_and_composer() {
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let mut state = AppState::new("/work/magdex".into(), true);
        state.resume_picker = Some(crate::model::ResumePicker {
            selected: 0,
            loading: false,
            error: None,
        });
        state.threads = vec![crate::model::ThreadSummary {
            id: "thread-1".into(),
            title: "Fix transcript width".into(),
            cwd: "/work/magdex".into(),
            updated_at: 0,
        }];

        terminal.draw(|frame| draw(frame, &mut state)).unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Recent conversations"));
        assert!(rendered.contains("Fix transcript width"));
        assert!(rendered.contains("Ctrl+C quit"));
        assert!(!rendered.contains("Esc quit"));
        assert!(!rendered.contains("Connecting to app-server"));
        assert!(!rendered.contains("Ask Codex"));
    }

    #[test]
    fn choices_render_as_a_bottom_panel_without_a_popup_box() {
        let backend = ratatui::backend::TestBackend::new(50, 8);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let mut state = AppState::new("/project".into(), true);
        state.models = vec![crate::model::ModelInfo {
            id: "gpt-test".into(),
            name: "GPT Test".into(),
            ..Default::default()
        }];

        terminal
            .draw(|frame| {
                draw_bottom_panel(frame, &state, Popup::Models { selected: 0 }, frame.area())
            })
            .unwrap();

        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(2, 2)].symbol(), "›");
        assert_eq!(buffer[(4, 2)].symbol(), "G");
        assert_eq!(buffer[(0, 3)].symbol(), " ");
        assert_eq!(buffer[(49, 3)].symbol(), " ");
        assert_eq!(buffer[(0, 3)].bg, COMPOSER_BACKGROUND);
        assert_eq!(buffer[(49, 3)].bg, COMPOSER_BACKGROUND);
    }

    #[test]
    fn secret_user_input_is_masked() {
        let backend = ratatui::backend::TestBackend::new(60, 10);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let mut request = UserInputRequest {
            id: serde_json::json!(1),
            questions: vec![crate::model::UserInputQuestion {
                id: "token".into(),
                header: "Token".into(),
                question: "Enter the token".into(),
                options: vec![],
                allow_other: false,
                secret: true,
            }],
            current: 0,
            answers: vec![],
            selected: 0,
            input: Default::default(),
            entering_other: false,
        };
        request.input.text = "hunter2".into();
        request.input.cursor = request.input.text.len();

        terminal
            .draw(|frame| draw_user_input_panel(frame, &request, frame.area()))
            .unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(!rendered.contains("hunter2"));
        assert!(rendered.contains("•••••••"));
    }

    #[test]
    fn bottom_panel_has_space_above_it_and_reaches_the_terminal_edge() {
        let backend = ratatui::backend::TestBackend::new(50, 16);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let mut state = AppState::new("/project".into(), true);
        state.models = vec![crate::model::ModelInfo {
            id: "gpt-test".into(),
            name: "GPT Test".into(),
            ..Default::default()
        }];
        state.popup = Some(Popup::Models { selected: 0 });

        terminal.draw(|frame| draw(frame, &mut state)).unwrap();

        let buffer = terminal.backend().buffer();
        assert_ne!(buffer[(0, 11)].bg, COMPOSER_BACKGROUND);
        assert_eq!(buffer[(0, 15)].bg, COMPOSER_BACKGROUND);
    }

    #[test]
    fn approval_separates_the_command_from_its_options() {
        let backend = ratatui::backend::TestBackend::new(60, 14);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let approval = crate::model::Approval {
            id: serde_json::json!(1),
            kind: ApprovalKind::Command,
            title: "Run command?".into(),
            detail: "cargo test".into(),
            params: serde_json::json!({}),
            selected: 0,
        };

        terminal
            .draw(|frame| draw_approval_panel(frame, &approval, frame.area()))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let rows = (0..14)
            .map(|y| (0..60).map(|x| buffer[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>();
        let command_row = rows
            .iter()
            .position(|row| row.contains("$ cargo test"))
            .unwrap();
        let divider_row = rows
            .iter()
            .enumerate()
            .skip(command_row + 1)
            .find_map(|(index, row)| row.contains("────").then_some(index))
            .unwrap();
        let options_row = rows
            .iter()
            .position(|row| row.contains("Allow once"))
            .unwrap();
        assert!(command_row < divider_row);
        assert_eq!(divider_row + 1, options_row);
        assert_eq!(buffer[(2, command_row as u16)].fg, Color::Yellow);
    }

    #[test]
    fn working_indicator_has_shimmer_text_without_a_spinner() {
        let line = working_indicator(std::time::Instant::now());
        let text = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(text.starts_with("  Working ("));
        assert!(text.ends_with("s • esc to interrupt)"));
        assert_eq!(line.spans[1..=7].len(), 7);
        assert!(line.spans[1..=7].iter().all(|span| span.style.fg.is_some()));
    }

    #[test]
    fn working_indicator_switches_to_minutes_after_sixty_seconds() {
        let started = std::time::Instant::now() - std::time::Duration::from_secs(65);
        let text = working_indicator(started)
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(text.contains("Working (1m 5s "));
    }

    #[test]
    fn composer_soft_wraps_and_tracks_cursor() {
        let layout = layout_composer("abcdefghij", 10, 4);
        assert_eq!(layout.lines, ["abcd", "efgh", "ij"]);
        assert_eq!((layout.cursor_row, layout.cursor_col), (2, 2));
    }

    #[test]
    fn composer_wraps_before_whole_words() {
        let layout = layout_composer("hello world", "hello world".len(), 10);
        assert_eq!(layout.lines, ["hello ", "world"]);
        assert_eq!((layout.cursor_row, layout.cursor_col), (1, 5));

        let exact = layout_composer("alpha beta gamma", "alpha beta gamma".len(), 10);
        assert_eq!(exact.lines, ["alpha beta", "gamma"]);
    }

    #[test]
    fn composer_does_not_split_russian_words_at_any_normal_width() {
        let text = "так, сообщения растягиваются на весь экран, попробуй запустить магдекс или другое приложение и посмотрим";
        for width in 20..=100 {
            let layout = layout_composer(text, text.len(), width);
            for word in text.split_whitespace() {
                assert!(
                    layout.lines.iter().any(|line| line.contains(word)),
                    "{word:?} split at width {width}: {:?}",
                    layout.lines
                );
            }
        }
    }

    #[test]
    fn composer_preserves_explicit_and_trailing_newlines() {
        let layout = layout_composer("аб\nв\n", "аб\nв\n".len(), 8);
        assert_eq!(layout.lines, ["аб", "в", ""]);
        assert_eq!((layout.cursor_row, layout.cursor_col), (2, 0));
    }

    #[test]
    fn composer_cursor_wraps_at_exact_edge() {
        let layout = layout_composer("abcd", 4, 4);
        assert_eq!(layout.lines, ["abcd", ""]);
        assert_eq!((layout.cursor_row, layout.cursor_col), (1, 0));
    }
}
