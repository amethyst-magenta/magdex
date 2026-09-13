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
    app::{format_duration, relative_time, SLASH_COMMANDS},
    model::{
        layout_composer, ActionStatus, AppState, ApprovalKind, BlockKind, CommandAction,
        CommandActionKind, ComposerLayout, FileChange, FileChangeKind, ImageAttachment, Popup,
        TranscriptBlock, UserInputRequest,
    },
};

const DIM: Color = Color::DarkGray;
const ACCENT: Color = Color::Cyan;
const USER_BACKGROUND: Color = Color::Rgb(48, 48, 48);
const COMPOSER_BACKGROUND: Color = Color::Rgb(38, 38, 38);
const DIFF_ADD_BACKGROUND: Color = Color::Rgb(28, 65, 46);
const DIFF_REMOVE_BACKGROUND: Color = Color::Rgb(78, 37, 34);

pub fn draw(frame: &mut Frame, state: &mut AppState) {
    if state.resume_picker.is_some() {
        draw_resume_workspace(frame, state);
        return;
    }
    let area = frame.area();
    // The composer has two cells of horizontal padding on each side and a
    // two-cell prompt (`› `), so wrap against its real text width.
    state.composer_width = area.width.saturating_sub(6).max(1) as usize;
    let composer = layout_composer(
        &state.composer.text,
        state.composer.cursor,
        state.composer_width,
    );
    let composer_lines = composer.lines.len().clamp(1, 8) as u16;
    let attachment_rows = attachment_row_count(state.image_attachments.len());
    let slash_suggestions = slash_command_suggestions(&state.composer.text);
    let suggestion_rows = slash_suggestions.len() as u16;
    let composer_height = (composer_lines + attachment_rows + suggestion_rows + 4)
        .min(area.height.saturating_sub(4).max(5));
    let panel_gap = u16::from(state.popup.is_some() && area.height > 4);
    let bottom_height = state
        .popup
        .as_ref()
        .map(|popup| bottom_panel_height(state, popup, area.width))
        .unwrap_or(composer_height)
        .min(area.height.saturating_sub(5 + panel_gap).max(1));
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
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
        draw_composer(frame, state, &composer, &slash_suggestions, chunks[3]);
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
        .min(area.height.saturating_sub(5 + panel_gap));
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
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
    let width = area.width.saturating_sub(2) as usize;
    let left_width = state.project.width();
    let right_width = right.width();
    let gap = width.saturating_sub(left_width + right_width).max(1);
    let line = Line::from(vec![
        Span::styled(format!(" {}", state.project), Style::default().bold()),
        Span::raw(" ".repeat(gap)),
        Span::styled(right, Style::default().fg(DIM)),
    ]);
    let text_area = Rect::new(area.x, area.y.saturating_add(1), area.width, 1);
    frame.render_widget(Paragraph::new(line), text_area);
    let separator = "▔".repeat(area.width as usize);
    let sep_area = Rect::new(area.x, area.y.saturating_add(3), area.width, 1);
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
            "  ↓ new output · Ctrl+G to latest",
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
    let mut block_offsets = vec![None; state.blocks.len()];
    let mut in_activity = false;
    let current_action = state.blocks.iter().rposition(|block| {
        matches!(
            block.kind,
            BlockKind::Command | BlockKind::File | BlockKind::Web
        )
    });
    for (index, block) in state.blocks.iter().enumerate() {
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
        block_offsets[index] = Some(lines.len());
        append_block(
            &mut lines,
            block,
            content_width as usize,
            render_width as usize,
            &state.cwd,
            current_action == Some(index) && block.kind == BlockKind::Command,
        );
    }
    if in_activity {
        lines.push(section_separator(render_width as usize));
        lines.push(Line::default());
    }
    state.transcript_cache_width = width;
    state.transcript_cache_revision = state.transcript_revision;
    state.transcript_cache_lines = lines;
    state.transcript_block_offsets = block_offsets;
}

fn append_block(
    lines: &mut Vec<Line<'static>>,
    block: &TranscriptBlock,
    width: usize,
    render_width: usize,
    cwd: &str,
    can_expand: bool,
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
        BlockKind::Command => append_command(lines, block, width, can_expand),
        BlockKind::File => append_file_changes(lines, block, width, render_width, cwd),
        BlockKind::Web => append_web_action(lines, block, width),
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

fn append_command(
    lines: &mut Vec<Line<'static>>,
    block: &TranscriptBlock,
    width: usize,
    can_expand: bool,
) {
    let command = block.title.strip_prefix("$ ").unwrap_or(&block.title);
    let status = block.action_status.unwrap_or(ActionStatus::InProgress);
    let explored = !block.command_actions.is_empty()
        && block
            .command_actions
            .iter()
            .all(|action| action.kind != CommandActionKind::Unknown);
    let heading = match (explored, status) {
        (true, ActionStatus::InProgress) => "Exploring",
        (true, _) => "Explored",
        (false, ActionStatus::InProgress) => "Running",
        (false, ActionStatus::Declined) => "Declined",
        (false, _) => "Ran",
    };
    if explored {
        lines.push(Line::from(Span::styled(
            format!("  {heading}"),
            Style::default().bold(),
        )));
        let last = block.command_actions.len().saturating_sub(1);
        for (index, action) in block.command_actions.iter().enumerate() {
            append_command_action(lines, action, index == last, width);
        }
    } else {
        let mut spans = vec![Span::styled(
            format!("  {heading} "),
            Style::default().bold(),
        )];
        spans.extend(shell_command_spans(command));
        push_wrapped_line(lines, spans, width);
    }

    let failed = status == ActionStatus::Failed || block.exit_code.is_some_and(|code| code != 0);
    let expanded = can_expand && block.expanded;
    if (!explored || expanded || failed) && !block.text.is_empty() {
        let mut output_rows = Vec::new();
        for (index, line) in block.text.lines().enumerate() {
            let prefix = if index == 0 { "    └ " } else { "      " };
            push_wrapped_line(
                &mut output_rows,
                vec![Span::styled(
                    format!("{prefix}{line}"),
                    Style::default().fg(DIM),
                )],
                width,
            );
        }
        append_collapsible_rows(lines, output_rows, expanded, 6, can_expand);
    }

    if failed {
        let message = block
            .exit_code
            .map(|code| format!("exit {code}"))
            .unwrap_or_else(|| "command failed".to_string());
        push_wrapped_line(
            lines,
            vec![Span::styled(
                format!("    {message}"),
                Style::default().fg(Color::Red),
            )],
            width,
        );
    } else if status == ActionStatus::Declined {
        lines.push(Line::from(Span::styled(
            "    declined",
            Style::default().fg(Color::Red),
        )));
    }
}

fn append_command_action(
    lines: &mut Vec<Line<'static>>,
    action: &CommandAction,
    last: bool,
    width: usize,
) {
    let branch = if last { "    └ " } else { "    ├ " };
    let mut spans = vec![Span::styled(branch, Style::default().fg(DIM))];
    let verb = match action.kind {
        CommandActionKind::Read => Some("Read"),
        CommandActionKind::ListFiles => Some("List"),
        CommandActionKind::Search => Some("Search"),
        CommandActionKind::Unknown => None,
    };
    if let Some(verb) = verb {
        spans.push(Span::styled(verb.to_string(), Style::default().fg(ACCENT)));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            action.label.clone(),
            Style::default().fg(Color::Gray),
        ));
    } else {
        spans.extend(shell_command_spans(&action.label));
    }
    push_wrapped_line(lines, spans, width);
}

fn shell_command_spans(command: &str) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut next_is_program = true;
    for (index, token) in command.split_whitespace().enumerate() {
        if index > 0 {
            spans.push(Span::raw(" "));
        }
        let operator = matches!(token, "|" | "||" | "&&" | ";");
        let style = if operator {
            Style::default().fg(Color::Yellow)
        } else if next_is_program {
            Style::default().fg(ACCENT)
        } else if token.starts_with('-') {
            Style::default().fg(DIM)
        } else {
            Style::default().fg(Color::Gray)
        };
        spans.push(Span::styled(token.to_string(), style));
        next_is_program = operator;
    }
    spans
}

fn append_collapsible_rows(
    lines: &mut Vec<Line<'static>>,
    rows: Vec<Line<'static>>,
    expanded: bool,
    collapsed_rows: usize,
    show_shortcut: bool,
) {
    let hidden = rows.len().saturating_sub(collapsed_rows);
    if expanded || hidden == 0 {
        lines.extend(rows);
        return;
    }
    lines.extend(rows.into_iter().take(collapsed_rows));
    let summary = if show_shortcut {
        format!("    … +{hidden} lines · Ctrl+O to expand")
    } else {
        format!("    … +{hidden} lines")
    };
    lines.push(Line::from(Span::styled(summary, Style::default().fg(DIM))));
}

fn append_file_changes(
    lines: &mut Vec<Line<'static>>,
    block: &TranscriptBlock,
    width: usize,
    render_width: usize,
    cwd: &str,
) {
    if block.file_changes.is_empty() {
        lines.push(Line::from(Span::styled(
            "  Changed files",
            Style::default().bold(),
        )));
        return;
    }

    for change in &block.file_changes {
        let status = block.action_status.unwrap_or(ActionStatus::Completed);
        let verb = file_change_verb(change, status);
        let path = display_action_path(&change.path, cwd);
        let mut heading = vec![
            Span::styled(format!("  {verb} "), Style::default().bold()),
            Span::styled(path, Style::default().fg(ACCENT)),
        ];
        if let Some(move_path) = &change.move_path {
            heading.push(Span::styled(" → ", Style::default().fg(DIM)));
            heading.push(Span::styled(
                display_action_path(move_path, cwd),
                Style::default().fg(ACCENT),
            ));
        }
        let (added, removed) = diff_stats(&change.diff);
        if added > 0 || removed > 0 {
            heading.push(Span::styled(" (", Style::default().fg(DIM)));
        }
        if added > 0 {
            heading.push(Span::styled(
                format!("+{added}"),
                Style::default().fg(Color::Green),
            ));
        }
        if added > 0 && removed > 0 {
            heading.push(Span::raw(" "));
        }
        if removed > 0 {
            heading.push(Span::styled(
                format!("-{removed}"),
                Style::default().fg(Color::Red),
            ));
        }
        if added > 0 || removed > 0 {
            heading.push(Span::styled(")", Style::default().fg(DIM)));
        }
        push_wrapped_line(lines, heading, width);

        if !change.diff.is_empty() {
            append_diff(lines, &change.diff, render_width);
        }
    }

    match block.action_status {
        Some(ActionStatus::Failed) => lines.push(Line::from(Span::styled(
            "    file change failed",
            Style::default().fg(Color::Red),
        ))),
        Some(ActionStatus::Declined) => lines.push(Line::from(Span::styled(
            "    file change declined",
            Style::default().fg(Color::Red),
        ))),
        _ => {}
    }
}

fn file_change_verb(change: &FileChange, status: ActionStatus) -> &'static str {
    match (status, change.kind, change.move_path.is_some()) {
        (ActionStatus::InProgress, FileChangeKind::Add, _) => "Adding",
        (ActionStatus::InProgress, FileChangeKind::Delete, _) => "Deleting",
        (ActionStatus::InProgress, _, true) => "Moving",
        (ActionStatus::InProgress, FileChangeKind::Update, _) => "Editing",
        (ActionStatus::InProgress, FileChangeKind::Unknown, _) => "Changing",
        (_, FileChangeKind::Add, _) => "Added",
        (_, FileChangeKind::Delete, _) => "Deleted",
        (_, _, true) => "Moved",
        (_, FileChangeKind::Update, _) => "Edited",
        (_, FileChangeKind::Unknown, _) => "Changed",
    }
}

fn display_action_path(path: &str, cwd: &str) -> String {
    let path_value = std::path::Path::new(path);
    if let Ok(relative) = path_value.strip_prefix(cwd) {
        if !relative.as_os_str().is_empty() {
            return relative.to_string_lossy().into_owned();
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if path == home {
            return "~".to_string();
        }
        if let Some(suffix) = path
            .strip_prefix(&home)
            .and_then(|suffix| suffix.strip_prefix('/'))
        {
            return format!("~/{suffix}");
        }
    }
    path.to_string()
}

fn diff_stats(diff: &str) -> (usize, usize) {
    let added = diff
        .lines()
        .filter(|line| line.starts_with('+') && !line.starts_with("+++"))
        .count();
    let removed = diff
        .lines()
        .filter(|line| line.starts_with('-') && !line.starts_with("---"))
        .count();
    (added, removed)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DiffRowKind {
    Context,
    Added,
    Removed,
    Metadata,
}

struct DiffRow<'a> {
    line_number: Option<usize>,
    kind: DiffRowKind,
    content: &'a str,
}

fn append_diff(lines: &mut Vec<Line<'static>>, diff: &str, width: usize) {
    let rows = parse_diff_rows(diff);
    let gutter_width = rows
        .iter()
        .filter_map(|row| row.line_number)
        .map(|line| line.to_string().len())
        .max()
        .unwrap_or(1);

    for row in rows {
        append_diff_row(lines, row, gutter_width, width);
    }
}

fn parse_diff_rows(diff: &str) -> Vec<DiffRow<'_>> {
    let mut rows = Vec::new();
    let mut old_line = None;
    let mut new_line = None;

    for line in diff.lines() {
        if line.starts_with("@@") {
            if let Some((old, new)) = parse_hunk_positions(line) {
                old_line = Some(old);
                new_line = Some(new);
            }
            continue;
        }
        if line.starts_with("diff --git ")
            || line.starts_with("index ")
            || line.starts_with("--- ")
            || line.starts_with("+++ ")
        {
            continue;
        }

        let (kind, content, line_number) = if let Some(content) = line.strip_prefix('+') {
            let number = new_line;
            new_line = new_line.map(|line| line.saturating_add(1));
            (DiffRowKind::Added, content, number)
        } else if let Some(content) = line.strip_prefix('-') {
            let number = old_line;
            old_line = old_line.map(|line| line.saturating_add(1));
            (DiffRowKind::Removed, content, number)
        } else if let Some(content) = line.strip_prefix(' ') {
            let number = new_line.or(old_line);
            old_line = old_line.map(|line| line.saturating_add(1));
            new_line = new_line.map(|line| line.saturating_add(1));
            (DiffRowKind::Context, content, number)
        } else {
            (DiffRowKind::Metadata, line, None)
        };
        rows.push(DiffRow {
            line_number,
            kind,
            content,
        });
    }

    rows
}

fn parse_hunk_positions(header: &str) -> Option<(usize, usize)> {
    let mut fields = header.split_whitespace();
    (fields.next()? == "@@").then_some(())?;
    let old = parse_hunk_position(fields.next()?, '-')?;
    let new = parse_hunk_position(fields.next()?, '+')?;
    Some((old, new))
}

fn parse_hunk_position(field: &str, prefix: char) -> Option<usize> {
    field.strip_prefix(prefix)?.split(',').next()?.parse().ok()
}

fn append_diff_row(
    lines: &mut Vec<Line<'static>>,
    row: DiffRow<'_>,
    gutter_width: usize,
    width: usize,
) {
    let marker = match row.kind {
        DiffRowKind::Added => '+',
        DiffRowKind::Removed => '-',
        DiffRowKind::Context | DiffRowKind::Metadata => ' ',
    };
    let marker_style = match row.kind {
        DiffRowKind::Added => Style::default().fg(Color::Green),
        DiffRowKind::Removed => Style::default().fg(Color::Red),
        DiffRowKind::Context | DiffRowKind::Metadata => Style::default().fg(DIM),
    };
    let content_style = match row.kind {
        DiffRowKind::Added | DiffRowKind::Removed => {
            Style::default().fg(Color::Gray).add_modifier(Modifier::DIM)
        }
        DiffRowKind::Context | DiffRowKind::Metadata => Style::default().fg(DIM),
    };
    let background = match row.kind {
        DiffRowKind::Added => Some(DIFF_ADD_BACKGROUND),
        DiffRowKind::Removed => Some(DIFF_REMOVE_BACKGROUND),
        DiffRowKind::Context | DiffRowKind::Metadata => None,
    };
    let prefix_width = gutter_width.saturating_add(5);
    let content_width = width.saturating_sub(prefix_width).max(1);
    let wrapped = hard_wrap_preserving(row.content, content_width);

    for (index, content) in wrapped.into_iter().enumerate() {
        let number = if index == 0 {
            row.line_number
                .map(|line| line.to_string())
                .unwrap_or_default()
        } else {
            String::new()
        };
        let row_start = lines.len();
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {number:>gutter_width$} "),
                Style::default().fg(DIM),
            ),
            Span::styled(
                if index == 0 { marker } else { ' ' }.to_string(),
                marker_style,
            ),
            Span::raw(" "),
            Span::styled(content, content_style),
        ]));
        if let Some(background) = background {
            apply_background_band(&mut lines[row_start..], width, background);
        }
    }
}

fn hard_wrap_preserving(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let width = width.max(1);
    let mut rows = Vec::new();
    let mut row = String::new();
    let mut used: usize = 0;
    for ch in text.chars() {
        let char_width = ch.width().unwrap_or(0);
        if !row.is_empty() && used.saturating_add(char_width) > width {
            rows.push(std::mem::take(&mut row));
            used = 0;
        }
        row.push(ch);
        used = used.saturating_add(char_width);
        if used >= width {
            rows.push(std::mem::take(&mut row));
            used = 0;
        }
    }
    if !row.is_empty() {
        rows.push(row);
    }
    rows
}

fn append_web_action(lines: &mut Vec<Line<'static>>, block: &TranscriptBlock, width: usize) {
    for (index, detail) in block.text.lines().enumerate() {
        let mut spans = vec![Span::raw("  ")];
        if index == 0 {
            spans.push(Span::styled(
                format!("{} ", block.title),
                Style::default().bold(),
            ));
        }
        spans.push(Span::styled(
            detail.to_string(),
            Style::default().fg(Color::Gray),
        ));
        push_wrapped_line(lines, spans, width);
    }
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

fn slash_command_suggestions(input: &str) -> Vec<(&'static str, &'static str)> {
    if !input.starts_with('/') || input.chars().any(char::is_whitespace) {
        return vec![];
    }
    SLASH_COMMANDS
        .iter()
        .copied()
        .filter(|(command, _)| command.starts_with(input))
        .collect()
}

fn draw_composer(
    frame: &mut Frame,
    state: &AppState,
    layout: &ComposerLayout,
    suggestions: &[(&str, &str)],
    area: Rect,
) {
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
    let suggestions_y = inner.y.saturating_add(attachment_height);
    let suggestion_height = (suggestions.len() as u16).min(
        inner
            .height
            .saturating_sub(attachment_height)
            .saturating_sub(1),
    );
    if suggestion_height > 0 {
        let lines = suggestions
            .iter()
            .take(suggestion_height as usize)
            .map(|(command, description)| {
                Line::from(vec![
                    Span::styled(format!("{command:<12}"), Style::default().fg(ACCENT)),
                    Span::styled(*description, Style::default().fg(DIM)),
                ])
            })
            .collect::<Vec<_>>();
        frame.render_widget(
            Paragraph::new(lines),
            Rect::new(inner.x, suggestions_y, inner.width, suggestion_height),
        );
    }
    let content_height = attachment_height.saturating_add(suggestion_height);
    let input_area = Rect::new(
        inner.x,
        inner.y.saturating_add(content_height),
        inner.width,
        inner.height.saturating_sub(content_height),
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
        Popup::History { .. } => {
            list_height(
                state
                    .blocks
                    .iter()
                    .filter(|block| block.kind == BlockKind::User)
                    .count(),
                14,
            ) + 1
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
            0,
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
            0,
            area,
        ),
        Popup::Reasoning { selected } => {
            let efforts = state
                .model
                .as_ref()
                .and_then(|id| state.models.iter().find(|model| &model.id == id))
                .map(|model| model.efforts.clone())
                .unwrap_or_default();
            draw_list_panel(frame, "Reasoning", efforts, selected, 0, area)
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
            draw_list_panel(frame, "Resume", entries, selected, 0, area);
        }
        Popup::History { selected } => {
            let messages = state
                .blocks
                .iter()
                .filter(|block| block.kind == BlockKind::User)
                .collect::<Vec<_>>();
            let entries = if messages.is_empty() {
                vec!["No messages yet".to_string()]
            } else {
                messages
                    .iter()
                    .enumerate()
                    .map(|(index, block)| {
                        let preview = block.text.split_whitespace().collect::<Vec<_>>().join(" ");
                        format!("{}  {}", index + 1, truncate(&preview, 72))
                    })
                    .collect()
            };
            draw_list_panel(frame, "History · Enter to jump", entries, selected, 1, area);
        }
        Popup::Login { url, error } => {
            draw_text_panel(
                frame,
                "Account",
                account_text(url.as_deref(), error.as_deref()),
                area,
            );
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
        format!("Sign-in failed\n\n{error}\n\nRestart Magdex to retry.")
    } else if let Some(url) = url {
        format!("Complete sign-in in your browser.\n\nIf it did not open:\n{url}")
    } else {
        "Sign-in required\n\nStarting ChatGPT login…".to_string()
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
    bottom_padding: u16,
    area: Rect,
) {
    let block = panel_block(title, bottom_padding);
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
    fn header_has_symmetric_vertical_padding_and_omits_context_percent() {
        let backend = ratatui::backend::TestBackend::new(80, 4);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let mut state = AppState::new("/home/user/magdex".into(), true);
        state.project = "~/magdex".into();
        state.model = Some("gpt-test".into());
        state.effort = Some("high".into());

        terminal
            .draw(|frame| draw_header(frame, &state, frame.area()))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let row = |y| (0..80).map(|x| buffer[(x, y)].symbol()).collect::<String>();
        assert!(row(0).trim().is_empty());
        assert!(row(1).contains("~/magdex"));
        assert!(row(1).contains("gpt-test · high · default"));
        assert!(!row(1).contains('%'));
        assert!(row(2).trim().is_empty());
        assert_eq!(row(3), "▔".repeat(80));
    }

    #[test]
    fn exploratory_commands_show_semantics_instead_of_shell_output() {
        let mut block = TranscriptBlock::new(
            BlockKind::Command,
            "rg -n needle src",
            "src/app.rs:1:needle",
        );
        block.action_status = Some(ActionStatus::Completed);
        block.exit_code = Some(0);
        block.command_actions = vec![CommandAction {
            kind: CommandActionKind::Search,
            label: "needle in src".into(),
        }];
        let mut lines = Vec::new();

        append_command(&mut lines, &block, 80, true);

        let rendered = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(rendered.contains("Explored"));
        assert!(rendered.contains("Search needle in src"));
        assert!(!rendered.contains("Shell"));
        assert!(!rendered.contains("src/app.rs:1:needle"));
        let search = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .find(|span| span.content == "Search")
            .unwrap();
        assert_eq!(search.style.fg, Some(ACCENT));
    }

    #[test]
    fn file_changes_render_relative_paths_and_colored_diffs() {
        let mut block = TranscriptBlock::new(BlockKind::File, "Files", "");
        block.action_status = Some(ActionStatus::Completed);
        let diff = std::iter::once("@@ -10,3 +10,16 @@".to_string())
            .chain(std::iter::once(" context before".to_string()))
            .chain(std::iter::once("-old".to_string()))
            .chain((0..14).map(|index| format!("+line{index}")))
            .chain(std::iter::once(" context after".to_string()))
            .collect::<Vec<_>>()
            .join("\n");
        block.file_changes = vec![FileChange {
            kind: FileChangeKind::Update,
            path: "/project/src/ui.rs".into(),
            move_path: None,
            diff,
        }];
        let mut lines = Vec::new();

        append_file_changes(&mut lines, &block, 80, 80, "/project");

        let rendered = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(rendered.contains("Edited src/ui.rs (+14 -1)"));
        assert!(!rendered.contains("@@"));
        assert!(!rendered.contains("Ctrl+O"));
        let removed = lines
            .iter()
            .find(|line| line.spans.iter().any(|span| span.content == "old"))
            .unwrap();
        let added = lines
            .iter()
            .find(|line| line.spans.iter().any(|span| span.content == "line0"))
            .unwrap();
        let last_added = lines
            .iter()
            .find(|line| line.spans.iter().any(|span| span.content == "line13"))
            .unwrap();
        let context = lines
            .iter()
            .find(|line| {
                line.spans
                    .iter()
                    .any(|span| span.content == "context before")
            })
            .unwrap();
        assert_eq!(removed.style.bg, Some(DIFF_REMOVE_BACKGROUND));
        assert_eq!(added.style.bg, Some(DIFF_ADD_BACKGROUND));
        assert_eq!(removed.width(), 80);
        assert_eq!(added.width(), 80);
        assert_eq!(context.style.bg, None);
        assert!(removed
            .spans
            .iter()
            .find(|span| span.content == "old")
            .unwrap()
            .style
            .add_modifier
            .contains(Modifier::DIM));
        let line_text = |line: &Line<'_>| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        };
        assert!(line_text(context).contains("10   context before"));
        assert!(line_text(removed).contains("11 - old"));
        assert!(line_text(added).contains("11 + line0"));
        assert!(line_text(last_added).contains("24 + line13"));
    }

    #[test]
    fn long_single_line_command_output_collapses_by_visual_rows() {
        let mut block = TranscriptBlock::new(
            BlockKind::Command,
            "cargo metadata --locked",
            "x".repeat(200),
        );
        block.action_status = Some(ActionStatus::Completed);
        block.exit_code = Some(0);
        let mut lines = Vec::new();

        append_command(&mut lines, &block, 24, true);

        let rendered = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(rendered.contains("Ran"));
        assert!(rendered.contains("Ctrl+O to expand"));
        let flag = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .find(|span| span.content == "--locked")
            .unwrap();
        assert_eq!(flag.style.fg, Some(DIM));
        assert!(lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .any(|span| { span.content.contains('x') && span.style.fg == Some(DIM) }));

        block.expanded = true;
        let mut old_lines = Vec::new();
        append_command(&mut old_lines, &block, 24, false);
        let old_rendered = old_lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(old_rendered.contains("… +"));
        assert!(!old_rendered.contains("Ctrl+O"));
    }

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
            "/project",
            false,
        );
        append_block(
            &mut lines,
            &TranscriptBlock::new(BlockKind::Assistant, "Codex", "answer"),
            76,
            80,
            "/project",
            false,
        );
        append_block(
            &mut lines,
            &TranscriptBlock::new(BlockKind::TurnEnd, "", "9m 55s"),
            76,
            80,
            "/project",
            false,
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
            .draw(|frame| draw_composer(frame, &state, &layout, &[], frame.area()))
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
            .draw(|frame| draw_composer(frame, &state, &layout, &[], frame.area()))
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
    fn message_history_lists_compact_previews() {
        let backend = ratatui::backend::TestBackend::new(60, 10);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let mut state = AppState::new("/project".into(), true);
        state.blocks = vec![
            TranscriptBlock::new(BlockKind::User, "You", "first message"),
            TranscriptBlock::new(BlockKind::Assistant, "Codex", "answer"),
            TranscriptBlock::new(BlockKind::User, "You", "second\nmessage"),
        ];

        terminal
            .draw(|frame| {
                draw_bottom_panel(frame, &state, Popup::History { selected: 1 }, frame.area())
            })
            .unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("History · Enter to jump"));
        assert!(rendered.contains("1  first message"));
        assert!(rendered.contains("2  second message"));
    }

    #[test]
    fn transcript_cache_tracks_user_messages_for_history_jumps() {
        let mut state = AppState::new("/project".into(), true);
        state.blocks = vec![
            TranscriptBlock::new(BlockKind::User, "You", "first"),
            TranscriptBlock::new(BlockKind::Assistant, "Codex", "answer"),
            TranscriptBlock::new(BlockKind::User, "You", "second"),
        ];
        state.mark_transcript_dirty();

        rebuild_transcript_cache(&mut state, 60);
        let expected = state.transcript_block_offsets[2].unwrap();

        assert!(state.jump_to_user_message(1));
        assert_eq!(state.scroll, expected);
        assert!(expected > state.transcript_block_offsets[0].unwrap());
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
    fn slash_command_suggestions_filter_as_the_user_types() {
        let all = slash_command_suggestions("/");
        assert_eq!(all.len(), SLASH_COMMANDS.len());

        let filtered = slash_command_suggestions("/re")
            .into_iter()
            .map(|(command, _)| command)
            .collect::<Vec<_>>();
        assert_eq!(filtered, ["/resume", "/reasoning"]);
        assert!(slash_command_suggestions("hello").is_empty());
        assert!(slash_command_suggestions("/resume now").is_empty());
    }

    #[test]
    fn slash_command_suggestions_render_above_the_composer() {
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let mut state = AppState::new("/project".into(), true);
        state.composer.replace("/re".into());

        terminal.draw(|frame| draw(frame, &mut state)).unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("/resume"));
        assert!(rendered.contains("Resume conversation"));
        assert!(rendered.contains("/reasoning"));
        assert!(rendered.contains("› /re"));
        assert!(!rendered.contains("Change model"));
    }

    #[test]
    fn composer_cursor_wraps_at_exact_edge() {
        let layout = layout_composer("abcd", 4, 4);
        assert_eq!(layout.lines, ["abcd", ""]);
        assert_eq!((layout.cursor_row, layout.cursor_col), (1, 0));
    }
}
